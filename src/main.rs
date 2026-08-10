use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::sync::Arc;
use std::time::Duration;

use backon::{ConstantBuilder, Retryable};
use eyre::{Result, eyre};
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
    wifi_connect::{CarWifi, WifiBackend},
    wireless::{IapHost, wireless_carplay},
};

mod accessory_settings;
mod libei;
mod portal;
mod usb;
mod wifi_connect;
mod wireless;

/// The desktop app streamed into the compositor. Owns the child process so it's killed on exit
/// (Drop) and can be relaunched on a resize.
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
        // `exec` so the child *is* the app, in its own process group so `kill` reaches the tree.
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

    fn is_running(&mut self) -> bool {
        match &mut self.child {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }

    /// Kill the running app (its whole process group) and reap it.
    fn kill(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
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

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::DEBUG.into())
                .from_env_lossy(),
        )
        .init();

    spawn_sigint_watchdog()?;

    let config = Config::load(&Cli::parse())?;

    if config.bluetooth_device().is_none() {
        return Err(eyre!(
            "no head unit configured - set [bluetooth] device (its MAC or advertised name) in \
             config.toml, or pass --bt-device, or set CARJACK.BLUETOOTH.DEVICE"
        ));
    }

    // Shared HomeKit identity across both source paths and runs - in the data dir, not the cache,
    // so losing it doesn't leave every paired car with a dead pairing in its table.
    match xdg::BaseDirectories::with_prefix("carjack").place_data_file("homekit.bin") {
        Ok(path) => libcarjack::airplay::init_homekit_storage(&path)?,
        Err(e) => tracing::warn!("No HomeKit storage path ({e}); pairing from scratch every run"),
    }

    let wifi: Arc<dyn WifiBackend> = wifi_connect::build(config.wifi.backend).into();

    match config.source {
        Source::App => run_app(config, wifi).await,
        Source::Desktop => run_desktop(config, wifi).await,
    }
}

/// Make a second Ctrl-C always kill carjack, even if the graceful path is stuck.
///
/// Awaiting `ctrl_c()` replaces the process's default SIGINT disposition for good, so if the task
/// polling it stalls (a blocking GStreamer/DRM call, or teardown with no listener left), Ctrl-C
/// stops doing anything. One `signal()` stream means a SIGINT between two awaits is never missed.
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
async fn run_app(config: Config, wifi: Arc<dyn WifiBackend>) -> Result<()> {
    let render_node = carjack::config::resolve_render_node(&config.graphics.drm_device)?;
    let backend = encode::resolve_backend(config.graphics.zerocopy, &render_node)?;
    tracing::info!(
        "Render node: {render_node}, encode backend: {}",
        backend.describe()
    );

    let host = Arc::new(IapHost::start(wifi.as_ref()).await?);

    // Size the compositor from the last car's cached display, if any, so even a non-resizable app
    // can boot at the right size during the BT/WiFi/AirPlay setup rather than after.
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
        encode::importable_modifiers(&backend),
        start_w,
        start_h,
        start_fps,
    )?;
    let mut app = DesktopApp::new(
        config.desktop_command().to_owned(),
        compositor.socket_name.clone(),
    );
    // Launch up front only if the target size is already known (resizable, or a cached display) -
    // otherwise `run_session` launches it once the car's real resolution comes in.
    if config.app.resizable || cached.is_some() {
        app.launch();
    }

    // Each session runs the full flow (Bluetooth -> WiFi -> AirPlay); on failure, restart from a
    // fresh Bluetooth connection instead of exiting.
    let exit = loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Ctrl-C received, shutting down");
                break Ok(());
            }
            result = run_session(&host, &compositor, &backend, &config, &mut app, &mut cache, &wifi) => {
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

    app.kill();
    drop(compositor);
    exit
}

/// `source = desktop`: stream the user's existing desktop via xdg-desktop-portal, with touch
/// injected back over libei (see [`libei`]).
async fn run_desktop(config: Config, wifi: Arc<dyn WifiBackend>) -> Result<()> {
    // Makes the gst `va` plugin register VA-API elements for every GPU on a multi-GPU box, not
    // just the first - needed to reach an integrated GPU that isn't the primary one. Must be set
    // before gst initializes.
    // SAFETY: nothing has initialized gst or read this var yet, and no other thread races on it.
    if std::env::var_os("GST_VA_ALL_DRIVERS").is_none() {
        unsafe { std::env::set_var("GST_VA_ALL_DRIVERS", "1") };
    }

    // Build the GStreamer registry now, not in the first session - see `encode::init_gstreamer`.
    tokio::task::spawn_blocking(encode::init_gstreamer)
        .await
        .map_err(|e| eyre!("gstreamer init task panicked: {e}"))??;

    let host = Arc::new(IapHost::start(wifi.as_ref()).await?);

    // ScreenCast + RemoteDesktop access, asked for once - a consent dialog appears on first run
    // (`PersistMode::Application` remembers it after). Persists across CarPlay reconnects.
    let rd = portal::start_remote_desktop().await?;
    tracing::info!("Desktop source ready (PipeWire node {})", rd.node_id());
    let zerocopy = config.graphics.zerocopy;
    let bitrate = Bitrate::from_kbps(config.graphics.bitrate);
    let bt_device = config.bluetooth_device().unwrap_or_default().to_owned();

    let exit = loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Ctrl-C received, shutting down");
                break Ok(());
            }
            result = run_desktop_session(&host, &rd, zerocopy, bitrate, &bt_device, &wifi) => {
                // Lift anything still held: the input session outlives car sessions.
                rd.end_session();
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

    exit
}

/// Result of the shared Bluetooth -> WiFi -> AirPlay handshake, common to both source modes.
struct Established {
    conn: libcarjack::airplay::AirPlayConnection,
    carplay_handle: AbortOnDropHandle<Result<()>>,
    /// Sent to `run_video_stream` (the AirPlay/HID side pushes decoded touch events into it).
    touch_tx: mpsc::Sender<TouchEvent>,
    /// Read in the session loop and injected into the compositor or the portal.
    touch_rx: mpsc::Receiver<TouchEvent>,
    /// The car's Bluetooth MAC (for the display cache and accessory settings); resolves during
    /// the handshake. A `watch`, not a `oneshot`: `establish_carplay` reads it early (to look up
    /// accessory settings before WiFi connects) and the caller reads it again afterward.
    car_addr_rx: tokio::sync::watch::Receiver<Option<String>>,
}

/// Bind an IPv6 link-local address to the interface it has to be used on - link-local is
/// ambiguous on a multi-interface host, and connecting without a scope id just fails.
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

/// How hard to try the AirPlay endpoint the car invited us to before giving up (which costs a
/// full Bluetooth teardown and reconnect).
const AIRPLAY_CONNECT_ATTEMPTS: usize = 4;
const AIRPLAY_CONNECT_DELAY: Duration = Duration::from_millis(500);

/// How long to wait for the car's invite before giving up on it and trying the receiver it named
/// (if any) instead.
const INVITE_WAIT: Duration = Duration::from_secs(15);

/// Connect to the endpoint the car's invite pointed at.
async fn connect_airplay_endpoint(
    addr: std::net::SocketAddr,
    remote_homekit_id: Option<uuid::Uuid>,
    device_id: String,
    mac_address: String,
) -> Result<libcarjack::airplay::AirPlayConnection> {
    tracing::info!("Connecting to AirPlay endpoint {addr}");
    (move || {
        libcarjack::airplay::connect_airplay(
            addr,
            remote_homekit_id,
            device_id.clone(),
            mac_address.clone(),
        )
    })
    .retry(
        ConstantBuilder::new()
            .with_delay(AIRPLAY_CONNECT_DELAY)
            .with_max_times(AIRPLAY_CONNECT_ATTEMPTS),
    )
    .notify(|e, delay| tracing::warn!("AirPlay connect to {addr} failed ({e}); retry in {delay:?}"))
    .await
    .map_err(|e| eyre!("AirPlay endpoint {addr} never came up: {e}"))
}

/// Run the Bluetooth -> WiFi -> AirPlay flow and return once AirPlay is connected. Shared by both
/// source modes.
async fn establish_carplay(
    host: Arc<IapHost>,
    bt_device: &str,
    wifi: &Arc<dyn WifiBackend>,
) -> Result<Established> {
    let (touch_tx, touch_rx) = mpsc::channel::<TouchEvent>(64);
    let (wifi_tx, mut wifi_rx) = mpsc::channel::<WifiCredentials>(4);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
    let (car_addr_tx, car_addr_rx) = tokio::sync::watch::channel::<Option<String>>(None);

    tracing::info!("Starting CarPlay session");

    // Its own task so the runtime keeps servicing the iAP2 link (keep-alives, ACKs) continuously
    // through WiFi connect and AirPlay setup, rather than being starved by the loops below.
    // AbortOnDropHandle tears the task (and its BlueZ resources) down when dropped, not detached.
    let mut carplay_handle = AbortOnDropHandle::new(tokio::spawn(wireless_carplay(
        host.clone(),
        bt_device.to_owned(),
        wifi_tx,
        ready_tx,
        car_addr_tx,
    )));

    let mut wifi_handle: Option<tokio::task::JoinHandle<Result<CarWifi>>> = None;
    let mut ready_rx = Some(ready_rx);
    // Where the accessory said its AirPlay receiver lives, and its HomeKit pairing id if it named
    // one there too (0x4301 only).
    let mut airplay_hint: Option<(std::net::SocketAddr, Option<uuid::Uuid>)> = None;

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
                airplay_hint = airplay_hint.or(creds.airplay.map(|a| (a, creds.pairing_id)));
                if wifi_handle.is_none() {
                    tracing::info!("Got WiFi creds, connecting");
                    let car_mac = car_addr_rx.borrow().clone();
                    let wifi = wifi.clone();
                    let handle = tokio::spawn(async move {
                        wifi.connect_to_car_wifi(
                            &creds.ssid,
                            creds.passphrase.as_deref().unwrap_or(""),
                            car_mac.as_deref(),
                        )
                        .await
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
        airplay_hint = airplay_hint.or(creds.airplay.map(|a| (a, creds.pairing_id)));
        let car_mac = car_addr_rx.borrow().clone();
        wifi.connect_to_car_wifi(
            &creds.ssid,
            creds.passphrase.as_deref().unwrap_or(""),
            car_mac.as_deref(),
        )
        .await
    } else {
        return Err(eyre!("No WiFi credentials received from car"));
    };
    let car_wifi = car_wifi_result?;

    let bluetooth_id = host.bluetooth_address().await?;
    let picked = match airplay_hint {
        Some(hint) => {
            tracing::info!("Car named its AirPlay receiver over iAP2 at {}", hint.0);
            Some(hint)
        }
        None => {
            tracing::info!("Car WiFi connected, waiting for the car's CarPlay invite...");
            match wait_for_carplay_ctrl_probe(&car_wifi.iface, &bluetooth_id, INVITE_WAIT).await {
                Ok((addr, pi)) => {
                    tracing::info!("Got CarPlay ctrl invite; AirPlay peer at {addr}");
                    Some((addr, pi))
                }
                Err(e) => {
                    tracing::warn!("{e}; no AirPlay receiver to connect to");
                    None
                }
            }
        }
    };
    let (endpoint, remote_homekit_id) =
        picked.ok_or_else(|| eyre!("Car neither invited us nor named an AirPlay receiver"))?;
    // Either source's address comes from the car itself, so it can be link-local.
    let endpoint = with_scope_id(endpoint, &car_wifi.iface);

    let mac_address = wifi.get_wifi_mac().await?;
    let conn =
        connect_airplay_endpoint(endpoint, remote_homekit_id, bluetooth_id, mac_address).await?;
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
    host: &Arc<IapHost>,
    compositor: &Compositor,
    backend: &EncodeBackend,
    config: &Config,
    app: &mut DesktopApp,
    cache: &mut DisplayCache,
    wifi: &Arc<dyn WifiBackend>,
) -> Result<()> {
    let Established {
        conn,
        mut carplay_handle,
        touch_tx,
        mut touch_rx,
        car_addr_rx,
    } = establish_carplay(host.clone(), config.bluetooth_device().unwrap_or_default(), wifi)
        .await?;

    // Reconfigure the already-running compositor to exactly match the car's size and rate.
    let resized = compositor.reconfigure(conn.width, conn.height, conn.max_fps)?;

    // Remember this car's display so next startup can size everything up front. Normalize a
    // missing fps to 30 so the cached value is directly usable to start the compositor.
    if conn.width > 0 && conn.height > 0 {
        if let Some(car_mac) = car_addr_rx.borrow().clone() {
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

    // A resizable app already resizes itself. A non-resizable one is (re)launched here at the
    // now-known resolution, and only if it wasn't already running at the right size.
    if !config.app.resizable && (resized || !app.is_running()) {
        tracing::info!("launching desktop app at {}x{}", conn.width, conn.height);
        app.launch();
    }
    let frame_rx = compositor.subscribe_frames();

    // Built here, not at launch time: the appsrc latches its caps once, so this must follow
    // `reconfigure`. Channels are shallow - a real-time stream wants low latency, not buffering.
    let (video_tx, video_rx) = mpsc::channel::<libcarjack::airplay::EncodedFrame>(4);
    let (keyframe_tx, keyframe_rx) = mpsc::channel::<()>(1);
    let mut encoder = AbortOnDropHandle::new(tokio::spawn(encode::encode_h264_to_channel(
        frame_rx,
        video_tx,
        backend.clone(),
        Bitrate::from_kbps(config.graphics.bitrate),
        keyframe_rx,
    )));
    let video = libcarjack::airplay::run_video_stream(conn, video_rx, touch_tx, keyframe_tx);
    tokio::pin!(video);

    let mut carplay_done = false;
    // touch_tx lives on the AirPlay/HID side, not on carplay_handle - gating touch on a BT drop
    // would silently kill it for the rest of the session even though it kept working fine.
    let mut touch_done = false;

    loop {
        tokio::select! {
            // The encoder ending closes the video channel too, so `encoder` must be polled first
            // or `video` reports a closed channel instead of the error that caused it.
            biased;

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
    host: &Arc<IapHost>,
    rd: &portal::RemoteDesktopStream,
    zerocopy: carjack::config::ZeroCopyMode,
    bitrate: Bitrate,
    bt_device: &str,
    wifi: &Arc<dyn WifiBackend>,
) -> Result<()> {
    let Established {
        conn,
        mut carplay_handle,
        touch_tx,
        mut touch_rx,
        car_addr_rx: _,
    } = establish_carplay(host.clone(), bt_device, wifi).await?;
    let (width, height) = (conn.width, conn.height);
    tracing::info!("Streaming desktop to car at {width}x{height}");

    let (video_tx, video_rx) = mpsc::channel::<libcarjack::airplay::EncodedFrame>(4);
    let (keyframe_tx, keyframe_rx) = mpsc::channel::<()>(1);
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
        keyframe_rx,
    )));
    let video = libcarjack::airplay::run_video_stream(conn, video_rx, touch_tx, keyframe_tx);
    tokio::pin!(video);

    let mut carplay_done = false;
    let mut touch_done = false;

    loop {
        tokio::select! {
            // The encoder ending closes the video channel too, so `encoder` must be polled first
            // or `video` reports a closed channel instead of the error that caused it.
            biased;

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
                    // Async, unlike the compositor's synchronous seat injection.
                    Some(event) => {
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
