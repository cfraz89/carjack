use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::time::Duration;

use backon::{ConstantBuilder, Retryable};
use eyre::{Result, eyre};
use futures::TryStreamExt;
use libcarjack::carplay_session::{TouchEvent, WifiCredentials};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;
use tokio_util::task::AbortOnDropHandle;
use tracing_subscriber::filter::{EnvFilter, LevelFilter};

use libcarjack::carplay_ctrl::wait_for_carplay_ctrl_probe;

use carjack::compositor::Compositor;
use carjack::config::{Cli, Config, Source};
use carjack::display_cache::{DisplayCache, DisplayProps};
use carjack::encode::{self, Bitrate, EncodeBackend};
use clap::Parser;

use crate::{
    wifi_connect::{CarWifi, connect_to_car_wifi},
    wireless::wireless_carplay,
};

mod libei;
mod portal;
mod usb;
mod wifi_connect;
mod wireless;

/// The desktop app streamed into the compositor. Owns the child process so it's killed on exit
/// (Drop) and can be relaunched on a resize (for apps that fix their resolution at boot).
struct DesktopApp {
    command: String,
    socket: OsString,
    child: Option<std::process::Child>,
}

impl DesktopApp {
    fn new(command: String, socket: OsString) -> Self {
        DesktopApp {
            command,
            socket,
            child: None,
        }
    }

    /// (Re)launch the app: kill any running instance first, then spawn a fresh one pointed at the
    /// compositor socket.
    fn launch(&mut self) {
        self.kill();
        tracing::info!("Launching desktop app: {0}", self.command);
        // `exec` so the child *is* the app (not an intermediate `sh`), and its own process group
        // so `kill` reaches the whole tree.
        match std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("exec {0}", self.command))
            .env("WAYLAND_DISPLAY", &self.socket)
            .process_group(0)
            .spawn()
        {
            Ok(child) => self.child = Some(child),
            Err(e) => tracing::warn!("failed to launch desktop app '{0}': {e}", self.command),
        }
    }

    /// Whether the app is currently running (spawned and not yet exited).
    fn is_running(&mut self) -> bool {
        match &mut self.child {
            // `try_wait` -> Ok(None) means still running; Ok(Some(_))/Err means it exited.
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }

    /// Kill the running app (its whole process group) and reap it.
    fn kill(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        // SIGKILL the process group (negative pid); the child leads its own group.
        unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
        let _ = child.kill(); // fallback if the group signal missed the leader
        let _ = child.wait();
    }
}

impl Drop for DesktopApp {
    fn drop(&mut self) {
        self.kill();
    }
}

#[tokio::main()]
async fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::DEBUG.into())
                .from_env_lossy(),
        )
        .init();

    // Before any GPU/portal/Bluetooth setup, so it covers the whole run.
    spawn_sigint_watchdog()?;

    let config = Config::load(&Cli::parse())?;

    // Fail fast and loudly: without a target there is nothing to connect to, and carjack no longer
    // guesses one. Checked here so both source paths get it before any GPU/compositor setup.
    if config.bluetooth_device().is_none() {
        return Err(eyre!(
            "no head unit configured - set [bluetooth] device (its MAC or advertised name) in \
             config.toml, or pass --bt-device, or set CARJACK.BLUETOOTH.DEVICE"
        ));
    }

    match config.source {
        Source::App => run_app(config).await,
        Source::Desktop => run_desktop(config).await,
    }
}

/// Make a second Ctrl-C always kill carjack, however wedged the graceful path is.
///
/// The moment anything awaits `tokio::signal::ctrl_c()`, tokio installs a SIGINT handler and the
/// default "terminate the process" disposition is gone *process-wide*. From then on Ctrl-C only
/// resolves futures - so if nothing is awaiting one, or the task that is has stalled, Ctrl-C does
/// nothing at all. That happens easily here: the session `select!`s below poll the Ctrl-C branch
/// and the session future in the *same* task, so a blocking call inside a session (GStreamer
/// `set_state`, a DRM ioctl) starves the Ctrl-C branch; and the teardown *after* the loop
/// (`drop(compositor)` joins the compositor thread) can block with no listener left at all.
///
/// One `signal()` stream rather than two `ctrl_c()` awaits: a stream can't miss a SIGINT that
/// arrives between the first and second receipt.
fn spawn_sigint_watchdog() -> Result<()> {
    let mut sigint = signal(SignalKind::interrupt())?;
    tokio::spawn(async move {
        sigint.recv().await;
        tracing::info!("Ctrl-C received, shutting down - press again to exit immediately");
        sigint.recv().await;
        tracing::warn!("Second Ctrl-C, forcing exit");
        std::process::exit(130);
    });
    Ok(())
}

/// Await Ctrl-C or the retry backoff between sessions; returns `true` if Ctrl-C fired (stop).
async fn backoff_or_ctrl_c() -> bool {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Ctrl-C received, shutting down");
            true
        }
        _ = tokio::time::sleep(Duration::from_secs(2)) => false,
    }
}

/// `source = app`: run `[app] command` in carjack's own headless compositor and stream that.
async fn run_app(config: Config) -> Result<()> {
    // Resolve which GPU to render + encode on, then decide the encode backend (this is where
    // zerocopy=auto falls back to software, or zerocopy=true errors out).
    let render_node = carjack::config::resolve_render_node(&config.graphics.drm_device)?;
    let backend = encode::resolve_backend(config.graphics.zerocopy, &render_node)?;
    tracing::info!(
        "Render node: {render_node}, encode backend: {}",
        backend.describe()
    );

    // If we've seen a car before, start the compositor at its cached resolution rather than the
    // config default - so the app (even a non-resizable one like Waydroid) can boot at the right
    // size straight away, during the BT/WiFi/AirPlay setup, instead of after the display is known.
    let mut cache = DisplayCache::load();
    let cached = cache.last_props();
    let (start_w, start_h, start_fps) = match cached {
        Some(p) => (p.width, p.height, p.fps),
        None => (
            config.display.width,
            config.display.height,
            config.display.fps,
        ),
    };
    let compositor = Compositor::start(
        render_node,
        backend.is_zero_copy(),
        start_w,
        start_h,
        start_fps,
    )?;
    let mut app = DesktopApp::new(
        config.desktop_command().to_owned(),
        compositor.socket_name.clone(),
    );
    // Launch the app up front (so slow apps like Waydroid boot during the BT/WiFi phase) when we
    // already know the target size - i.e. it can live-resize, or we have a cached resolution.
    // Otherwise a non-resizable app is held back until the resolution is known (in `run_session`).
    if config.app.resizable || cached.is_some() {
        app.launch();
    }

    // Each session runs the full flow (Bluetooth -> WiFi -> AirPlay). On failure - most
    // importantly when the car tears down the screen socket - restart from a fresh Bluetooth
    // connection rather than exiting. Ctrl-C breaks out and cleans up (compositor thread joined,
    // app killed) via the drops below.
    let exit = loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Ctrl-C received, shutting down");
                break Ok(());
            }
            result = run_session(&compositor, &backend, &config, &mut app, &mut cache) => {
                match result {
                    Ok(()) => {
                        tracing::info!("Session ended cleanly, exiting");
                        break Ok(());
                    }
                    Err(e) => {
                        tracing::warn!("Session failed, restarting from Bluetooth: {e}");
                        if backoff_or_ctrl_c().await {
                            break Ok(());
                        }
                    }
                }
            }
        }
    };

    // Deterministic cleanup (Drop would also handle these, but do it before returning).
    app.kill();
    drop(compositor);
    exit
}

/// `source = desktop`: stream the user's existing desktop via xdg-desktop-portal, with touch
/// injected back over libei (see [`libei`]).
async fn run_desktop(config: Config) -> Result<()> {
    // The gst `va` plugin only registers its VA-API elements (vapostproc/vah264enc) with this set
    // on multi-GPU boxes - it's how the desktop zero-copy path reaches an integrated (e.g. Intel)
    // GPU that renders the desktop. Set it before any gst init.
    // SAFETY: nothing in desktop mode has initialized gst or read this var yet, and no other
    // thread races on it here.
    if std::env::var_os("GST_VA_ALL_DRIVERS").is_none() {
        unsafe { std::env::set_var("GST_VA_ALL_DRIVERS", "1") };
    }

    // Ask the portal for ScreenCast + RemoteDesktop access once - a consent dialog appears on the
    // first run (`PersistMode::Application` remembers it after). Touch is then injected over libei
    // (see `src/libei.rs`). The PipeWire node and input session persist across CarPlay reconnects,
    // the way the compositor does in app mode.
    let rd = portal::start_remote_desktop().await?;
    tracing::info!("Desktop source ready (PipeWire node {})", rd.node_id());
    // `[graphics] zerocopy` here selects the desktop encode path: GL (GPU) vs software convert.
    let zerocopy = config.graphics.zerocopy;
    let bitrate = Bitrate::from_kbps(config.graphics.bitrate);
    let bt_device = config.bluetooth_device().unwrap_or_default().to_owned();

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Ctrl-C received, shutting down");
                return Ok(());
            }
            result = run_desktop_session(&rd, zerocopy, bitrate, &bt_device) => {
                // Lift anything still held: the input session outlives car sessions.
                rd.end_session();
                match result {
                    Ok(()) => {
                        tracing::info!("Session ended cleanly, exiting");
                        return Ok(());
                    }
                    Err(e) => {
                        tracing::warn!("Session failed, restarting from Bluetooth: {e}");
                        if backoff_or_ctrl_c().await {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
}

/// Result of the shared Bluetooth -> WiFi -> AirPlay handshake, common to both source modes: the
/// AirPlay connection, the still-running CarPlay/Bluetooth task, and the touch plumbing.
struct Established {
    conn: libcarjack::airplay::AirPlayConnection,
    carplay_handle: AbortOnDropHandle<Result<()>>,
    /// Sent to `run_video_stream` (the AirPlay/HID side pushes decoded touch events into it).
    touch_tx: mpsc::Sender<TouchEvent>,
    /// Read in the session loop and injected into the compositor or the portal.
    touch_rx: mpsc::Receiver<TouchEvent>,
    /// The car's Bluetooth MAC (for the display cache); resolves during the handshake.
    car_addr_rx: tokio::sync::oneshot::Receiver<String>,
}

/// Bind an IPv6 link-local address to the interface it has to be used on.
///
/// The accessory's AirPlay address is link-local (`fe80::/10`), which is ambiguous on a host
/// with more than one interface - connecting to one without a scope id fails outright rather
/// than picking the car's WiFi. Anything else is returned untouched.
fn with_scope_id(addr: std::net::SocketAddr, iface: &str) -> std::net::SocketAddr {
    let std::net::SocketAddr::V6(mut v6) = addr else {
        return addr;
    };
    if !v6.ip().is_unicast_link_local() || v6.scope_id() != 0 {
        return addr;
    }
    match nix::net::if_::if_nametoindex(iface) {
        Ok(index) => {
            v6.set_scope_id(index);
            std::net::SocketAddr::V6(v6)
        }
        Err(e) => {
            tracing::warn!("Couldn't resolve interface index for {iface} ({e}); no scope id");
            addr
        }
    }
}

/// Wait for the WiFi interface to have at least one non-tentative IPv6 address.
///
/// After a WiFi reconnect the kernel assigns a fresh link-local address and runs DAD on it;
/// the address is marked "tentative" until DAD finishes and the kernel refuses to use it as a
/// source for `connect()`.  Uses rtnetlink to query the kernel's address list and retries with
/// backoff via `backon`.
async fn wait_for_ipv6(iface: &str, timeout: Duration) -> Result<()> {
    let if_index = nix::net::if_::if_nametoindex(iface)
        .map_err(|e| eyre!("Interface {iface}: {e}"))?;

    let (conn, handle, _) = rtnetlink::new_connection()
        .map_err(|e| eyre!("rtnetlink connection: {e}"))?;
    tokio::spawn(conn);

    use netlink_packet_route::address::AddressHeaderFlag;

    tokio::time::timeout(
        timeout,
        (move || {
            let handle = handle.clone();
            async move {
                let mut addresses = handle
                    .address()
                    .get()
                    .set_link_index_filter(if_index)
                    .execute();
                while let Some(msg) = addresses
                    .try_next()
                    .await
                    .map_err(|e| eyre!("rtnetlink: {e}"))?
                {
                    if msg.header.index != if_index {
                        continue;
                    }
                    // IFA_F_TENTATIVE (0x40): DAD still in progress
                    if msg.header.flags.contains(&AddressHeaderFlag::Tentative) {
                        continue;
                    }
                    return Ok(());
                }
                Err(eyre!("no valid IPv6 address"))
            }
        })
        .retry(ConstantBuilder::new().with_delay(Duration::from_millis(50))),
    )
    .await
    .map_err(|_| {
        eyre!("IPv6 address did not become available on {iface} within {timeout:?}")
    })?
}

/// Run the Bluetooth -> WiFi -> AirPlay flow and return once AirPlay is connected (so the caller
/// knows the car's display and can wire up the video source + input). Shared by both source modes.
async fn establish_carplay(bt_device: &str) -> Result<Established> {
    let (touch_tx, touch_rx) = mpsc::channel::<TouchEvent>(64);
    let (wifi_tx, mut wifi_rx) = mpsc::channel::<WifiCredentials>(4);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
    let (car_addr_tx, car_addr_rx) = tokio::sync::oneshot::channel::<String>();

    tracing::info!("Starting CarPlay session");

    // Run the CarPlay/iAP2 Bluetooth session as its own task so the runtime keeps polling
    // it continuously. It must keep servicing the iAP2 link (keep-alives, ACKs) throughout
    // WiFi connect and AirPlay setup - if it only advanced when awaited in the loops below,
    // it would be starved for ~10s during screen-sharing setup and the car would reset the
    // Bluetooth link, tearing down CarPlay right as streaming starts.
    // AbortOnDropHandle guarantees the task (and its BlueZ resources) is torn down when the
    // returned handle is dropped (e.g. before a session restart) rather than detached.
    let mut carplay_handle = AbortOnDropHandle::new(tokio::spawn(wireless_carplay(
        bt_device.to_owned(),
        wifi_tx,
        ready_tx,
        car_addr_tx,
    )));

    let mut wifi_handle: Option<tokio::task::JoinHandle<Result<CarWifi>>> = None;
    let mut ready_rx = Some(ready_rx);
    // Where the accessory said its AirPlay receiver lives, if it said (0x4301 only).
    let mut airplay_hint: Option<std::net::SocketAddr> = None;

    // Phase 1: connect WiFi immediately when creds arrive; wait for the EA session.
    loop {
        tokio::select! {
            biased;
            result = &mut carplay_handle => {
                return match result {
                    Ok(Ok(())) => Err(eyre!("CarPlay session ended before it was ready")),
                    Ok(Err(e)) => Err(eyre!("CarPlay session error: {e}")),
                    Err(e) => Err(eyre!("CarPlay task panicked: {e}")),
                };
            }
            Some(creds) = wifi_rx.recv() => {
                airplay_hint = airplay_hint.or(creds.airplay);
                if wifi_handle.is_none() {
                    tracing::info!("Got WiFi creds, connecting");
                    let handle = tokio::spawn(async move {
                        connect_to_car_wifi(&creds.ssid, creds.passphrase.as_deref().unwrap_or("")).await
                    });
                    wifi_handle = Some(handle);
                }
            }
            _ = async { ready_rx.as_mut()?.await.ok() } => {
                tracing::info!("EA session ready");
                break;
            }
        }
    }

    // Phase 2: wait for WiFi connection to complete
    let car_wifi_result = if let Some(handle) = wifi_handle {
        handle.await.map_err(|e| eyre!("WiFi task panicked: {e}"))?
    } else if let Some(creds) = wifi_rx.recv().await {
        airplay_hint = airplay_hint.or(creds.airplay);
        connect_to_car_wifi(&creds.ssid, creds.passphrase.as_deref().unwrap_or("")).await
    } else {
        return Err(eyre!("No WiFi credentials received from car"));
    };
    let car_wifi = car_wifi_result?;

    // In testing, the bike sent us its airplay address, so we use that. It doesn't seem to
    // invite us on mdns anyway, so only advertise on that if we aren't given the address outright
    let (peer_addr, remote_homekit_id) = match airplay_hint {
        Some(addr) => {
            // Wait for a non-tentative IPv6 address before attempting the AirPlay connect:
            // after a WiFi reconnect the kernel needs a moment to finish DAD, and the link-local
            // source address won't be usable until then.
            if let Err(e) = wait_for_ipv6(&car_wifi.iface, Duration::from_secs(10)).await {
                tracing::warn!("{e}");
            }
            let addr = with_scope_id(addr, &car_wifi.iface);
            tracing::info!("Connecting to AirPlay endpoint given by the car: {addr}");
            (addr, None)
        }
        None => {
            tracing::info!("Car WiFi connected, waiting for car probe...");
            // Advertise _carplay-ctrl._tcp via mDNS, wait for the car's GET
            // /ctrl-int/1/connect, and use its matching _airplay._tcp Bonjour record for the
            // real peer address + HomeKit id.
            match wait_for_carplay_ctrl_probe(&car_wifi.iface, &car_wifi.iface_mac).await {
                Ok(result) => result,
                Err(e) => {
                    tracing::warn!("CarPlay Ctrl probe failed, falling back to gateway: {e}");
                    (std::net::SocketAddr::new(car_wifi.gateway, 7000), None)
                }
            }
        }
    };

    // Connect AirPlay to learn the car's display size + refresh.
    // Retry with backoff: the TCP connect can transiently fail (e.g. EADDRNOTAVAIL if IPv6
    // DAD is still in progress despite our wait, or the car's AirPlay server isn't listening
    // quite yet).
    let conn = (move || {
        libcarjack::airplay::connect_airplay(peer_addr, remote_homekit_id)
    })
    .retry(ConstantBuilder::new().with_delay(Duration::from_secs(2)))
    .await?;
    Ok(Established {
        conn,
        carplay_handle,
        touch_tx,
        touch_rx,
        car_addr_rx,
    })
}

/// `source = app`: reconfigure the compositor to the car's display, encode its frames, and
/// inject CarPlay touch back into it.
async fn run_session(
    compositor: &Compositor,
    backend: &EncodeBackend,
    config: &Config,
    app: &mut DesktopApp,
    cache: &mut DisplayCache,
) -> Result<()> {
    let Established {
        conn,
        mut carplay_handle,
        touch_tx,
        mut touch_rx,
        car_addr_rx,
    } = establish_carplay(config.bluetooth_device().unwrap_or_default()).await?;

    // Reconfigure the already-running compositor (and its client) to exactly match the car's size
    // and rate - so it renders at the car's resolution, no scaling needed downstream.
    let resized = compositor.reconfigure(conn.width, conn.height, conn.max_fps)?;

    // Remember this car's display so next startup can size everything up front (the compositor was
    // likely already at these dimensions from the cache, in which case `reconfigure` was a no-op).
    // The car's MAC was surfaced early by the Bluetooth task. Normalize a missing fps to 30 so the
    // cached value is directly usable to start the compositor (which would otherwise clamp 0 -> 1).
    if conn.width > 0 && conn.height > 0 {
        if let Ok(car_mac) = car_addr_rx.await {
            cache.remember(
                &car_mac,
                DisplayProps {
                    width: conn.width,
                    height: conn.height,
                    fps: if conn.max_fps == 0 { 30 } else { conn.max_fps },
                },
            );
        }
    }

    // A resizable app is already running and resizes itself - leave it. A non-resizable app is
    // (re)launched here at the now-known resolution: on the first session it wasn't started yet;
    // on a reconnect (or a wrong cache guess) it's only relaunched if the size actually changed
    // (else it's still running at the right size).
    if !config.app.resizable && (resized || !app.is_running()) {
        tracing::info!("launching desktop app at {}x{}", conn.width, conn.height);
        app.launch();
    }
    let frame_rx = compositor.subscribe_frames();

    // Encode the compositor's frames to H264 on its own task (so it keeps buffering through
    // AirPlay setup) and stream them to the car. The encoder and the video stream are both
    // session-lifetime, so their arms live directly in the loop below alongside the Bluetooth
    // handle and touch - one place for the whole streaming session to end or error.
    // Small on purpose: this is a real-time stream, so a shallow channel (plus the pipeline's leaky
    // queue) keeps latency low - a deep buffer just fills once and adds a permanent delay.
    let (video_tx, video_rx) = mpsc::channel::<libcarjack::airplay::EncodedFrame>(4);
    let mut encoder = AbortOnDropHandle::new(tokio::spawn(encode::encode_h264_to_channel(
        frame_rx,
        video_tx,
        backend.clone(),
        Bitrate::from_kbps(config.graphics.bitrate),
    )));
    let video = libcarjack::airplay::run_video_stream(conn, video_rx, touch_tx);
    tokio::pin!(video);

    // If the car later drops Bluetooth on its own, that's not fatal - keep streaming.
    let mut carplay_done = false;
    // touch_tx lives on the AirPlay/HID side (run_video_stream), not on carplay_handle, so it's
    // tracked separately - gating it on carplay_done meant a BT drop silently killed touch for
    // the rest of the session even though it kept working fine.
    let mut touch_done = false;

    loop {
        tokio::select! {
            result = &mut carplay_handle, if !carplay_done => {
                carplay_done = true;
                match result {
                    Ok(Ok(())) => tracing::info!("CarPlay (Bluetooth) session ended"),
                    Ok(Err(e)) => tracing::warn!("CarPlay (Bluetooth) session ended: {e}"),
                    Err(e) => tracing::warn!("CarPlay task panicked: {e}"),
                }
            }
            // Race the encoder so a GStreamer failure surfaces as a real error rather than a
            // silently closed video channel (which the stream side only sees as a clean end).
            result = &mut encoder => {
                return match result {
                    Ok(Ok(())) => Err(eyre!("video encoder task ended unexpectedly")),
                    Ok(Err(e)) => Err(eyre!("video encoder failed: {e}")),
                    Err(e) => Err(eyre!("video encoder task panicked: {e}")),
                };
            }
            result = &mut video => {
                // Propagate so the caller restarts the whole flow (e.g. car closed the
                // screen socket).
                result?;
                return Ok(());
            }
            touch = touch_rx.recv(), if !touch_done => {
                match touch {
                    Some(event) => compositor.send_touch(&event)?,
                    None => touch_done = true,
                }
            }
        }
    }
}

/// `source = desktop`: encode the portal's PipeWire stream (scaled to the car) and inject CarPlay
/// touch back into the desktop through the portal's RemoteDesktop session.
async fn run_desktop_session(
    rd: &portal::RemoteDesktopStream,
    zerocopy: carjack::config::ZeroCopyMode,
    bitrate: Bitrate,
    bt_device: &str,
) -> Result<()> {
    let Established {
        conn,
        mut carplay_handle,
        touch_tx,
        mut touch_rx,
        car_addr_rx: _,
    } = establish_carplay(bt_device).await?;
    let (width, height) = (conn.width, conn.height);
    tracing::info!("Streaming desktop to car at {width}x{height}");

    // Small on purpose: this is a real-time stream, so a shallow channel (plus the pipeline's leaky
    // queue) keeps latency low - a deep buffer just fills once and adds a permanent delay.
    let (video_tx, video_rx) = mpsc::channel::<libcarjack::airplay::EncodedFrame>(4);
    // The encoder reports the desktop's real frame size here; touch is scaled from car space to it.
    let (desktop_size_tx, desktop_size_rx) =
        tokio::sync::watch::channel::<Option<(u32, u32)>>(None);
    let node_id = rd.node_id();
    let mut encoder = AbortOnDropHandle::new(tokio::spawn(encode::encode_pipewire_to_channel(
        node_id,
        width,
        height,
        zerocopy,
        bitrate,
        video_tx,
        desktop_size_tx,
    )));
    let video = libcarjack::airplay::run_video_stream(conn, video_rx, touch_tx);
    tokio::pin!(video);

    let mut carplay_done = false;
    let mut touch_done = false;

    loop {
        tokio::select! {
            result = &mut carplay_handle, if !carplay_done => {
                carplay_done = true;
                match result {
                    Ok(Ok(())) => tracing::info!("CarPlay (Bluetooth) session ended"),
                    Ok(Err(e)) => tracing::warn!("CarPlay (Bluetooth) session ended: {e}"),
                    Err(e) => tracing::warn!("CarPlay task panicked: {e}"),
                }
            }
            result = &mut encoder => {
                return match result {
                    Ok(Ok(())) => Err(eyre!("video encoder task ended unexpectedly")),
                    Ok(Err(e)) => Err(eyre!("video encoder failed: {e}")),
                    Err(e) => Err(eyre!("video encoder task panicked: {e}")),
                };
            }
            result = &mut video => {
                result?;
                return Ok(());
            }
            touch = touch_rx.recv(), if !touch_done => {
                match touch {
                    // Inject back into the real desktop via the portal (async, unlike the
                    // compositor's synchronous seat injection).
                    Some(event) => {
                        // The encoder's negotiated pipewiresrc size, in physical pixels;
                        // `send_touch` converts to the portal's logical space itself.
                        let desktop_size = *desktop_size_rx.borrow();
                        if let Err(e) = rd.send_touch(&event, (width, height), desktop_size).await {
                            tracing::warn!("portal touch inject failed: {e}");
                        }
                    }
                    None => touch_done = true,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::with_scope_id;
    use std::net::SocketAddr;

    /// A link-local AirPlay address is unusable without knowing which interface it is on, so
    /// the car's WiFi interface index has to be stamped into it. `lo` is always index 1.
    #[test]
    fn link_local_addresses_get_the_interface_scope() {
        let addr: SocketAddr = "[fe80::9083:8eff:feef:a7d3]:7000".parse().unwrap();
        let scoped = with_scope_id(addr, "lo");
        let SocketAddr::V6(v6) = scoped else {
            panic!("expected v6, got {scoped}")
        };
        assert_eq!(v6.scope_id(), 1);
        assert_eq!(v6.port(), 7000);
        assert_eq!(v6.ip().to_string(), "fe80::9083:8eff:feef:a7d3");
    }

    /// Everything else is left exactly as the car gave it: routable addresses need no scope,
    /// and an unknown interface must not turn into a silently wrong one.
    #[test]
    fn other_addresses_are_untouched() {
        for addr in [
            "192.168.55.85:7000".parse::<SocketAddr>().unwrap(),
            "[2001:db8::1]:7000".parse::<SocketAddr>().unwrap(),
        ] {
            assert_eq!(with_scope_id(addr, "lo"), addr);
        }
        let link_local: SocketAddr = "[fe80::1]:7000".parse().unwrap();
        assert_eq!(
            with_scope_id(link_local, "definitely-not-an-iface"),
            link_local
        );
    }
}
