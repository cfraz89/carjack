//! One CarPlay connection: bring the Bluetooth -> WiFi -> AirPlay handshake up, then stream a
//! source to the accessory and feed its touches back. [`run_session`] (carjack's own compositor)
//! and [`run_desktop_session`] (the user's desktop via portal) share the handshake in
//! [`establish_carplay`] and the pump in [`pump_session`]; the caller re-runs them on failure.

use std::ffi::OsString;
use std::future::Future;
use std::net::SocketAddr;
use std::os::unix::process::CommandExt;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use backon::{ConstantBuilder, Retryable};
use eyre::{Result, eyre};
use libcarjack::airplay::{AirPlayConnection, EncodedFrame, VideoCodec};
use libcarjack::carplay_ctrl::wait_for_carplay_ctrl_probe;
use libcarjack::carplay_init::{TouchEvent, WifiCredentials};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

use carjack::compositor::{Compositor, ViewRect};
use carjack::config::{CodecChoice, Config, Target, ZeroCopyMode};
use carjack::display_cache::{DisplayCache, DisplayProps};
use carjack::encode::{self, Bitrate, EncodeBackend};

use crate::portal::RemoteDesktopStream;
use crate::wifi_connect::{AccessoryWifi, WifiBackend};
use crate::wireless::{IapHost, WifiOffhand, wireless_carplay};

/// The app streamed into the compositor. Owns the child process so it's killed on exit and can be
/// relaunched on a resize.
pub struct DesktopApp {
    command: String,
    socket: OsString,
    child: Option<std::process::Child>,
}

impl DesktopApp {
    pub fn new(command: String, socket: OsString) -> Self {
        DesktopApp {
            command,
            socket,
            child: None,
        }
    }

    /// Kill any running instance, then spawn a fresh one pointed at the compositor socket.
    pub fn launch(&mut self) {
        self.kill();
        tracing::info!("Launching desktop app: {0}", self.command);
        // `exec` so the child *is* the app and runs in its own group, so `kill` reaches the tree.
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

    pub fn is_running(&mut self) -> bool {
        match &mut self.child {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }

    /// Kill the running app (its whole process group) and reap it.
    pub fn kill(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        child.kill().ok();
        child.wait().ok();
    }
}

impl Drop for DesktopApp {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Result of the Bluetooth -> WiFi -> AirPlay handshake, common to both source modes.
struct Established {
    conn: AirPlayConnection,
    carplay_handle: AbortOnDropHandle<Result<()>>,
    /// Fill by the AirPlay/HID side, drained by the caller into the compositor or portal.
    touch_tx: mpsc::Sender<TouchEvent>,
    touch_rx: mpsc::Receiver<TouchEvent>,
    /// The accessory's Bluetooth MAC, resolved during the handshake; read before and after WiFi.
    accessory_addr_rx: watch::Receiver<Option<String>>,
    /// Ends the Bluetooth iAP2 session on our terms (fired on the accessory's `disableBluetooth`).
    handover: CancellationToken,
}

/// Link-local addresses need the interface's scope id; without one, connecting fails.
fn with_scope_id(addr: SocketAddr, iface: &str) -> SocketAddr {
    let SocketAddr::V6(mut v6) = addr else {
        return addr;
    };
    if !v6.ip().is_unicast_link_local() || v6.scope_id() != 0 {
        return addr;
    }
    match nix::net::if_::if_nametoindex(iface) {
        Ok(index) => {
            v6.set_scope_id(index);
            SocketAddr::V6(v6)
        }
        Err(e) => {
            tracing::warn!("Couldn't resolve interface index for {iface} ({e}); no scope id");
            addr
        }
    }
}

/// How hard to try the AirPlay endpoint before giving up (giving up costs a full Bluetooth teardown
/// and reconnect).
const AIRPLAY_CONNECT_ATTEMPTS: usize = 4;
const AIRPLAY_CONNECT_DELAY: Duration = Duration::from_millis(500);

/// How long to wait for the accessory's invite before falling back to the receiver it named.
const INVITE_WAIT: Duration = Duration::from_secs(15);

/// Connect to the endpoint the accessory's invite pointed at.
async fn connect_airplay_endpoint(
    addr: SocketAddr,
    remote_homekit_id: Option<uuid::Uuid>,
    device_id: String,
    mac_address: String,
) -> Result<AirPlayConnection> {
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

/// Run-up to the accessory's WiFi being up: its credentials as they arrive, the in-flight join, and
/// wherever it named its AirPlay receiver (0x4301). Joining is a task so the iAP2 link keeps being
/// serviced while it runs.
struct WifiJoin {
    wifi: Arc<dyn WifiBackend>,
    join: Option<tokio::task::JoinHandle<Result<AccessoryWifi>>>,
    airplay_hint: Option<(SocketAddr, Option<uuid::Uuid>)>,
    accessory_addr_rx: watch::Receiver<Option<String>>,
}

impl WifiJoin {
    /// Record the credentials and, on first receipt, kick off joining its AP.
    fn offer(&mut self, creds: WifiCredentials) {
        self.airplay_hint = self
            .airplay_hint
            .or(creds.airplay.map(|a| (a, creds.pairing_id)));
        if self.join.is_none() {
            tracing::info!("Got WiFi creds, connecting");
            let accessory_mac = self.accessory_addr_rx.borrow().clone();
            let wifi = self.wifi.clone();
            self.join = Some(tokio::spawn(async move {
                wifi.connect_to_accessory_wifi(
                    &creds.ssid,
                    creds.passphrase.as_deref().unwrap_or(""),
                    accessory_mac.as_deref(),
                )
                .await
            }));
        }
    }

    /// Await the join [`WifiJoin::offer`] started - or, if the EA session beat the credentials here,
    /// take them straight off the channel and start one.
    async fn finish(
        &mut self,
        creds_rx: &mut mpsc::Receiver<WifiCredentials>,
    ) -> Result<AccessoryWifi> {
        if self.join.is_none() {
            let creds = creds_rx
                .recv()
                .await
                .ok_or_else(|| eyre!("No WiFi credentials received from accessory"))?;
            self.offer(creds);
        }
        self.join
            .take()
            .ok_or_else(|| eyre!("No WiFi credentials received from accessory"))?
            .await
            .map_err(|e| eyre!("WiFi task panicked: {e}"))?
    }
}

/// Run the Bluetooth -> WiFi -> AirPlay flow and return once AirPlay is connected. Shared by both
/// source modes.
async fn establish_carplay(
    host: Arc<IapHost>,
    bt_device: &Target,
    wifi: &Arc<dyn WifiBackend>,
) -> Result<Established> {
    let (touch_tx, touch_rx) = mpsc::channel::<TouchEvent>(64);
    let (wifi_tx, mut wifi_rx) = mpsc::channel::<WifiCredentials>(4);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
    let (accessory_addr_tx, accessory_addr_rx) =
        tokio::sync::watch::channel::<Option<String>>(None);
    let handover = CancellationToken::new();

    tracing::info!("Starting CarPlay session");

    // Its own task so the runtime keeps servicing the iAP2 link through WiFi connect and AirPlay
    // setup; AbortOnDropHandle tears it down when dropped, not detached.
    let mut carplay_handle = AbortOnDropHandle::new(tokio::spawn(wireless_carplay(
        host.clone(),
        bt_device.clone(),
        WifiOffhand {
            creds_tx: wifi_tx,
            handover: handover.clone(),
        },
        ready_tx,
        accessory_addr_tx,
    )));

    let mut wifi_join = WifiJoin {
        wifi: wifi.clone(),
        join: None,
        airplay_hint: None,
        accessory_addr_rx: accessory_addr_rx.clone(),
    };
    let mut ready_rx = Some(ready_rx);

    // Connect WiFi the moment credentials arrive; leave when the EA session is up, whether or not
    // the join has started yet.
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
            Some(creds) = wifi_rx.recv() => wifi_join.offer(creds),
            _ = async { ready_rx.as_mut()?.await.ok() } => {
                tracing::info!("EA session ready");
                break;
            }
        }
    }
    let accessory_wifi = wifi_join.finish(&mut wifi_rx).await?;

    // The handover is done: the credentials are in hand and we are on the accessory's AP. This
    // does not end the Bluetooth session - an iPhone stays on the link and lets the accessory drop
    // it, and both units do so within seconds - it only marks the point after which that drop is
    // the expected ending rather than a failure.
    handover.cancel();

    let bluetooth_id = host.bluetooth_address().await?;
    // Either source's address comes from the accessory itself, so it can be link-local.
    let (endpoint, remote_homekit_id) =
        discover_airplay_endpoint(wifi_join.airplay_hint, &accessory_wifi.iface, &bluetooth_id)
            .await?;
    let endpoint = with_scope_id(endpoint, &accessory_wifi.iface);

    let mac_address = wifi.get_wifi_mac().await?;
    let conn =
        connect_airplay_endpoint(endpoint, remote_homekit_id, bluetooth_id, mac_address).await?;

    Ok(Established {
        conn,
        carplay_handle,
        touch_tx,
        touch_rx,
        accessory_addr_rx,
        handover,
    })
}

/// Find where the accessory's AirPlay receiver lives: the endpoint it named over iAP2, or the one
/// it invites us to on its network, or an error if it produces neither.
async fn discover_airplay_endpoint(
    airplay_hint: Option<(SocketAddr, Option<uuid::Uuid>)>,
    iface: &str,
    bluetooth_id: &str,
) -> Result<(SocketAddr, Option<uuid::Uuid>)> {
    if let Some(hint) = airplay_hint {
        tracing::info!(
            "Accessory named its AirPlay receiver over iAP2 at {}",
            hint.0
        );
        return Ok(hint);
    }
    tracing::info!("Accessory WiFi connected, waiting for the accessory's CarPlay invite...");
    match wait_for_carplay_ctrl_probe(iface, bluetooth_id, INVITE_WAIT).await {
        Ok((addr, pi)) => {
            tracing::info!("Got CarPlay ctrl invite; AirPlay peer at {addr}");
            Ok((addr, pi))
        }
        Err(e) => {
            tracing::warn!("{e}; no AirPlay receiver to connect to");
            Err(eyre!(
                "Accessory neither invited us nor named an AirPlay receiver"
            ))
        }
    }
}

/// Remember the accessory's display so the next startup can size everything up front. Normalize a
/// missing fps to 30 so the cached value is directly usable to start the compositor.
fn remember_accessory_display(
    cache: &mut DisplayCache,
    conn: &AirPlayConnection,
    accessory_addr_rx: &watch::Receiver<Option<String>>,
) {
    if conn.width > 0 && conn.height > 0 {
        if let Some(accessory_mac) = accessory_addr_rx.borrow().clone() {
            cache.remember(
                &accessory_mac,
                DisplayProps {
                    width: conn.width,
                    height: conn.height,
                    fps: if conn.max_fps == 0 { 30 } else { conn.max_fps },
                },
            );
        }
    }
}

/// The compositor's frames encoded and forwarded to the accessory for one app-source connection.
struct AppStream {
    encoder: AbortOnDropHandle<Result<()>>,
    /// Completes when the accessory hands the screen back; until then it relays what `encoder`
    /// produced. Boxed so it can be polled uniformly with the desktop stream.
    video: Pin<Box<dyn Future<Output = Result<()>> + Send>>,
    /// Keeps the keyframe-request loop alive for the session.
    _keyframe_relay: AbortOnDropHandle<()>,
}

impl AppStream {
    fn start(
        compositor: &Compositor,
        backend: &EncodeBackend,
        config: &Config,
        conn: AirPlayConnection,
        touch_tx: mpsc::Sender<TouchEvent>,
        handover: CancellationToken,
    ) -> Self {
        let frame_rx = compositor.subscribe_frames();

        let (video_tx, video_rx) = mpsc::channel::<EncodedFrame>(4);
        let (keyframe_tx, mut keyframe_rx) = mpsc::channel::<()>(1);
        let (encoder_keyframe_tx, encoder_keyframe_rx) = mpsc::channel::<()>(1);
        // A keyframe request is answered with a fresh compositor frame and an encoder keyframe, so
        // the stream can't stall on a static screen.
        let frame_requester = compositor.frame_requester();
        let _keyframe_relay = AbortOnDropHandle::new(tokio::spawn(async move {
            while keyframe_rx.recv().await.is_some() {
                frame_requester.store(true, std::sync::atomic::Ordering::Relaxed);
                let _ = encoder_keyframe_tx.try_send(());
            }
        }));

        let backend = backend.with_codec(encode::resolve_codec(
            config.graphics.codec,
            conn.hevc_expected,
        ));
        let encoder = AbortOnDropHandle::new(tokio::spawn(encode::encode_h264_to_channel(
            frame_rx,
            video_tx,
            backend.clone(),
            Bitrate::from_kbps(config.graphics.bitrate),
            encoder_keyframe_rx,
        )));
        let video = Box::pin(libcarjack::airplay::run_video_stream(
            conn,
            backend.codec(),
            video_rx,
            touch_tx,
            None,
            keyframe_tx,
            handover,
        ));
        AppStream {
            encoder,
            video,
            _keyframe_relay,
        }
    }
}

/// The portal's PipeWire stream scaled, encoded and forwarded to the accessory for one desktop
/// connection.
struct DesktopStream {
    encoder: AbortOnDropHandle<Result<()>>,
    video: Pin<Box<dyn Future<Output = Result<()>> + Send>>,
    /// The desktop's *actual* frame size, reported by the encoder's caps probe, so touch can be
    /// scaled from accessory space to it.
    desktop_size_rx: watch::Receiver<Option<(u32, u32)>>,
}

impl DesktopStream {
    fn start(
        rd: &RemoteDesktopStream,
        width: u32,
        height: u32,
        zerocopy: ZeroCopyMode,
        codec: VideoCodec,
        bitrate: Bitrate,
        conn: AirPlayConnection,
        touch_tx: mpsc::Sender<TouchEvent>,
        handover: CancellationToken,
    ) -> Self {
        let (video_tx, video_rx) = mpsc::channel::<EncodedFrame>(4);
        let (keyframe_tx, keyframe_rx) = mpsc::channel::<()>(1);
        let (desktop_size_tx, desktop_size_rx) =
            tokio::sync::watch::channel::<Option<(u32, u32)>>(None);
        let node_id = rd.node_id();
        let encoder = AbortOnDropHandle::new(tokio::spawn(encode::encode_pipewire_to_channel(
            node_id,
            width,
            height,
            zerocopy,
            codec,
            bitrate,
            video_tx,
            desktop_size_tx,
            keyframe_rx,
        )));
        let video = Box::pin(libcarjack::airplay::run_video_stream(
            conn,
            codec,
            video_rx,
            touch_tx,
            None,
            keyframe_tx,
            handover,
        ));
        DesktopStream {
            encoder,
            video,
            desktop_size_rx,
        }
    }
}

/// Drive a connected session to its end: log the Bluetooth link's lifecycle, surface the encoder's
/// first failure, and relay the accessory's touches back into the source until the video stream
/// hands over. `inject` is the source-specific touch sink - the compositor's aborts the session on
/// error, the portal's only logs.
async fn pump_session<V, S, Fut>(
    mut carplay_handle: &mut AbortOnDropHandle<Result<()>>,
    mut encoder: &mut AbortOnDropHandle<Result<()>>,
    mut video: &mut V,
    touch_rx: &mut mpsc::Receiver<TouchEvent>,
    mut inject: S,
) -> Result<()>
where
    V: Future<Output = Result<()>> + Unpin,
    S: FnMut(TouchEvent) -> Fut,
    Fut: Future<Output = Result<()>>,
{
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
                    Some(event) => inject(event).await?,
                    None => touch_done = true,
                }
            }
        }
    }
}

/// `source = app`: reconfigure the compositor to the accessory's display, encode its frames, and
/// inject CarPlay touch back into it.
pub async fn run_session(
    host: &Arc<IapHost>,
    compositor: &Compositor,
    backend: &EncodeBackend,
    config: &Config,
    target: &Target,
    app: &mut DesktopApp,
    cache: &mut DisplayCache,
    wifi: &Arc<dyn WifiBackend>,
) -> Result<()> {
    let Established {
        conn,
        mut carplay_handle,
        touch_tx,
        mut touch_rx,
        accessory_addr_rx,
        handover,
    } = establish_carplay(host.clone(), target, wifi).await?;

    // Reconfigure the already-running compositor to match the accessory's size and rate exactly.
    let view = conn.view.as_ref().map(|v| ViewRect {
        x: v.origin_x as i32,
        y: v.origin_y as i32,
        width: v.width as i32,
        height: v.height as i32,
    });
    let resized = compositor.reconfigure(conn.width, conn.height, conn.max_fps, view)?;
    remember_accessory_display(cache, &conn, &accessory_addr_rx);

    // A non-resizable app only has the right size once the accessory reports it, so launch (or
    // relaunch) it then; a resizable one was already started up front in `run_app`.
    if !config.app.resizable && (resized || !app.is_running()) {
        tracing::info!("launching desktop app at {}x{}", conn.width, conn.height);
        app.launch();
    }

    let mut stream = AppStream::start(compositor, backend, config, conn, touch_tx, handover);

    pump_session(
        &mut carplay_handle,
        &mut stream.encoder,
        &mut stream.video,
        &mut touch_rx,
        |event| async move { compositor.send_touch(&event) },
    )
    .await
}

/// `source = desktop`: encode the portal's PipeWire stream (scaled to the accessory) and inject CarPlay
/// touch back into the desktop through the portal's RemoteDesktop session.
pub async fn run_desktop_session(
    host: &Arc<IapHost>,
    rd: &RemoteDesktopStream,
    zerocopy: ZeroCopyMode,
    codec: CodecChoice,
    bitrate: Bitrate,
    bt_device: &Target,
    wifi: &Arc<dyn WifiBackend>,
) -> Result<()> {
    let Established {
        conn,
        mut carplay_handle,
        touch_tx,
        mut touch_rx,
        accessory_addr_rx: _,
        handover,
    } = establish_carplay(host.clone(), bt_device, wifi).await?;
    let (width, height) = (conn.width, conn.height);
    let codec = encode::resolve_codec(codec, conn.hevc_expected);
    tracing::info!("Streaming desktop to accessory at {width}x{height}");

    let stream = DesktopStream::start(
        rd, width, height, zerocopy, codec, bitrate, conn, touch_tx, handover,
    );
    let DesktopStream {
        mut encoder,
        mut video,
        desktop_size_rx,
    } = stream;

    pump_session(
        &mut carplay_handle,
        &mut encoder,
        &mut video,
        &mut touch_rx,
        |event| {
            // Touch is target-scaled to the desktop's real size once the encoder has negotiated it.
            let desktop_size = *desktop_size_rx.borrow();
            async move {
                if let Err(e) = rd.send_touch(&event, (width, height), desktop_size).await {
                    tracing::warn!("portal touch inject failed: {e}");
                }
                Ok(())
            }
        },
    )
    .await
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
