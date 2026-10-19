//! One CarPlay connection on Android: iAP2 over the RFCOMM channel the app accepted, the
//! accessory's WiFi joined by the app, then AirPlay, with frames coming from the app's encoder and
//! touches going back to it. The same flow as the Linux binary's `session.rs`, with the platform
//! behind [`Platform`].

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use backon::{ConstantBuilder, Retryable};
use catplay_carplay::video::AvccConfig;
use eyre::{Result, eyre};
use iap2_proto::iap2::Iap2;
use libcarjack::access_unit::{NAL_SIZE_LEN, split_access_unit};
use libcarjack::airplay::{AirPlayConnection, EncodedFrame, VideoCodec};
use libcarjack::carplay_ctrl::wait_for_carplay_ctrl_probe;
use libcarjack::carplay_init::{CarplayInit, DeviceIdentity, TouchEvent, WifiCredentials};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

/// What the app does for a session. Called from runtime threads; none of these may block.
pub trait Platform: Send + Sync + 'static {
    /// Join the accessory's AP, answering through [`SessionCtl::wifi_joined`].
    fn join_wifi(&self, creds: &WifiCredentials);
    /// Start encoding at the accessory's size, feeding [`SessionCtl::push_frame`].
    fn start_video(&self, video: &VideoConfig);
    fn request_keyframe(&self);
    fn touch(&self, event: &TouchEvent);
}

/// The stream the accessory wants, and where on it the app's picture goes.
#[derive(Debug, Clone)]
pub struct VideoConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// The part of the panel left to us; the whole of it when the accessory reserves nothing.
    pub view: (u32, u32, u32, u32),
    pub codec: VideoCodec,
}

/// Where the app's WiFi join landed.
pub struct JoinedWifi {
    pub iface: String,
    pub mac: String,
}

struct FrameSink {
    tx: mpsc::Sender<EncodedFrame>,
    codec: VideoCodec,
    /// Set by a keyframe request: the next keyframe carries the parameter sets.
    answer_with_config: Arc<AtomicBool>,
    /// The encoder's last parameter sets, for a keyframe that arrives without its own.
    parameter_sets: Vec<u8>,
}

/// The app's handle on a running session.
#[derive(Default)]
pub struct SessionCtl {
    wifi: Mutex<Option<oneshot::Sender<Result<JoinedWifi, String>>>>,
    video: Mutex<Option<FrameSink>>,
    pub cancel: CancellationToken,
}

impl SessionCtl {
    pub fn wifi_joined(&self, result: Result<JoinedWifi, String>) {
        if let Some(tx) = self.wifi.lock().unwrap().take() {
            let _ = tx.send(result);
        }
    }

    /// One encoder output buffer, Annex-B. Blocks while the stream is backed up, which is the
    /// encoder's cue to drop frames.
    pub fn push_frame(&self, data: &[u8], keyframe: bool, codec_config: bool) {
        let (tx, frame) = {
            let mut video = self.video.lock().unwrap();
            let Some(sink) = video.as_mut() else { return };
            let au = split_access_unit(data, sink.codec);
            if !au.parameter_sets.is_empty() {
                if au.parameter_sets != sink.parameter_sets {
                    // To set against a reference stream when a unit shows nothing.
                    tracing::info!(sets = ?au.parameter_sets, "Encoder parameter sets");
                }
                sink.parameter_sets = au.parameter_sets;
            }
            if codec_config || au.data.is_empty() {
                return;
            }
            let config = (keyframe
                && !sink.parameter_sets.is_empty()
                && sink.answer_with_config.swap(false, Ordering::Relaxed))
            .then(|| AvccConfig {
                nal_size_len: NAL_SIZE_LEN,
                sps_pps: sink.parameter_sets.clone(),
            });
            let frame = EncodedFrame {
                data: au.data,
                nals: au.nals,
                keyframe,
                config,
            };
            (sink.tx.clone(), frame)
        };
        let _ = tx.blocking_send(frame);
    }
}

/// How hard to try the AirPlay endpoint before giving up on the session.
const AIRPLAY_CONNECT_ATTEMPTS: usize = 4;
const AIRPLAY_CONNECT_DELAY: Duration = Duration::from_millis(500);

/// How long to wait for the accessory's invite before giving up on it.
const INVITE_WAIT: Duration = Duration::from_secs(15);

/// Link-local addresses need the interface's scope id; without one, connecting fails.
fn with_scope_id(addr: SocketAddr, iface: &str) -> SocketAddr {
    let SocketAddr::V6(mut v6) = addr else {
        return addr;
    };
    if !v6.ip().is_unicast_link_local() || v6.scope_id() != 0 {
        return addr;
    }
    let Ok(name) = std::ffi::CString::new(iface) else {
        return addr;
    };
    // SAFETY: `name` is NUL-terminated and outlives the call.
    let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
    if index == 0 {
        tracing::warn!("Couldn't resolve interface index for {iface}; no scope id");
        return addr;
    }
    v6.set_scope_id(index);
    SocketAddr::V6(v6)
}

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

/// The endpoint the accessory named over iAP2, or the one it invites us to on its network.
async fn discover_airplay_endpoint(
    airplay_hint: Option<(SocketAddr, Option<uuid::Uuid>)>,
    iface: &str,
    bluetooth_id: &str,
) -> Result<(SocketAddr, Option<uuid::Uuid>)> {
    if let Some(hint) = airplay_hint {
        tracing::info!("Accessory named its AirPlay receiver over iAP2 at {}", hint.0);
        return Ok(hint);
    }
    tracing::info!("Accessory WiFi connected, waiting for the accessory's CarPlay invite...");
    wait_for_carplay_ctrl_probe(iface, bluetooth_id, INVITE_WAIT)
        .await
        .map_err(|e| eyre!("Accessory neither invited us nor named an AirPlay receiver: {e}"))
}

pub struct SessionParams {
    pub identity: DeviceIdentity,
    /// Upper-case colon form, the AirPlay device id and Bonjour id.
    pub bluetooth_id: String,
    /// Overrides what the accessory's declaration implies.
    pub codec: Option<VideoCodec>,
}

/// Run a session to its end. `iap` is the app's end of the RFCOMM relay.
pub async fn run(
    platform: Arc<dyn Platform>,
    ctl: Arc<SessionCtl>,
    iap: UnixStream,
    params: SessionParams,
) -> Result<()> {
    let (creds_tx, mut creds_rx) = mpsc::channel::<WifiCredentials>(4);
    let (ready_tx, ready_rx) = oneshot::channel::<()>();
    let handover = CancellationToken::new();

    let iap2 = Iap2::start(iap)
        .await
        .map_err(|e| eyre!("iAP2 link did not come up: {e:?}"))?;
    tracing::info!("iAP2 link established");

    let mut carplay = AbortOnDropHandle::new(tokio::spawn({
        let identity = params.identity.clone();
        let handover = handover.clone();
        async move {
            CarplayInit::new(iap2, creds_tx, ready_tx, identity, handover)
                .run()
                .await
                .map_err(|e| eyre!("{e:?}"))
        }
    }));

    // Join as soon as credentials arrive; move on once the EA session is up.
    let mut join: Option<oneshot::Receiver<Result<JoinedWifi, String>>> = None;
    let mut airplay_hint = None;
    let mut offer = |creds: WifiCredentials, join: &mut Option<_>| {
        airplay_hint = airplay_hint.or(creds.airplay.map(|a| (a, creds.pairing_id)));
        if join.is_none() {
            tracing::info!(ssid = %creds.ssid, "Got WiFi creds, joining");
            let (tx, rx) = oneshot::channel();
            *ctl.wifi.lock().unwrap() = Some(tx);
            platform.join_wifi(&creds);
            *join = Some(rx);
        }
    };
    let mut ready_rx = Some(ready_rx);
    loop {
        tokio::select! {
            biased;
            result = &mut carplay => {
                return Err(match result {
                    Ok(Ok(())) => eyre!("CarPlay session ended before it was ready"),
                    Ok(Err(e)) => eyre!("CarPlay session error: {e}"),
                    Err(e) => eyre!("CarPlay task panicked: {e}"),
                });
            }
            Some(creds) = creds_rx.recv() => offer(creds, &mut join),
            _ = async { ready_rx.as_mut()?.await.ok() } => {
                tracing::info!("EA session ready");
                break;
            }
        }
    }
    if join.is_none() {
        let creds = creds_rx
            .recv()
            .await
            .ok_or_else(|| eyre!("No WiFi credentials received from accessory"))?;
        offer(creds, &mut join);
    }
    let wifi = join
        .take()
        .ok_or_else(|| eyre!("No WiFi join started"))?
        .await
        .map_err(|_| eyre!("WiFi join abandoned"))?
        .map_err(|e| eyre!("WiFi join failed: {e}"))?;
    tracing::info!(iface = %wifi.iface, mac = %wifi.mac, "On the accessory's WiFi");
    handover.cancel();

    let (endpoint, remote_homekit_id) =
        discover_airplay_endpoint(airplay_hint, &wifi.iface, &params.bluetooth_id).await?;
    let endpoint = with_scope_id(endpoint, &wifi.iface);
    let conn = connect_airplay_endpoint(
        endpoint,
        remote_homekit_id,
        params.bluetooth_id.clone(),
        wifi.mac.clone(),
    )
    .await?;

    let codec = params.codec.unwrap_or(if conn.hevc_expected {
        VideoCodec::Hevc
    } else {
        VideoCodec::H264
    });
    let view = conn.view.as_ref().map_or((0, 0, conn.width, conn.height), |v| {
        (v.origin_x as u32, v.origin_y as u32, v.width as u32, v.height as u32)
    });
    let video = VideoConfig {
        width: conn.width,
        height: conn.height,
        fps: if conn.max_fps == 0 { 30 } else { conn.max_fps },
        view,
        codec,
    };
    tracing::info!(?video, "Streaming to the accessory");

    let (video_tx, video_rx) = mpsc::channel::<EncodedFrame>(4);
    let (keyframe_tx, mut keyframe_rx) = mpsc::channel::<()>(1);
    let (touch_tx, mut touch_rx) = mpsc::channel::<TouchEvent>(64);
    let answer_with_config = Arc::new(AtomicBool::new(true));
    *ctl.video.lock().unwrap() = Some(FrameSink {
        tx: video_tx,
        codec,
        answer_with_config: answer_with_config.clone(),
        parameter_sets: Vec::new(),
    });
    platform.start_video(&video);

    let _keyframes = AbortOnDropHandle::new(tokio::spawn({
        let platform = platform.clone();
        async move {
            while keyframe_rx.recv().await.is_some() {
                answer_with_config.store(true, Ordering::Relaxed);
                platform.request_keyframe();
            }
        }
    }));
    let _touches = AbortOnDropHandle::new(tokio::spawn({
        let platform = platform.clone();
        async move {
            while let Some(event) = touch_rx.recv().await {
                platform.touch(&event);
            }
        }
    }));

    let stream = libcarjack::airplay::run_video_stream(
        conn,
        codec,
        video_rx,
        touch_tx,
        keyframe_tx,
        handover,
    );
    tokio::pin!(stream);
    let mut carplay_done = false;
    let result = loop {
        tokio::select! {
            result = &mut carplay, if !carplay_done => {
                carplay_done = true;
                match result {
                    Ok(Ok(())) => tracing::info!("CarPlay (Bluetooth) session ended"),
                    Ok(Err(e)) => tracing::info!("CarPlay (Bluetooth) session ended: {e}"),
                    Err(e) => tracing::warn!("CarPlay task panicked: {e}"),
                }
            }
            result = &mut stream => break result,
        }
    };
    ctl.video.lock().unwrap().take();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEYFRAME: &[u8] = &[
        0, 0, 0, 1, 0x40, 0x01, 0x0c, // VPS
        0, 0, 0, 1, 0x42, 0x01, 0x01, // SPS
        0, 0, 0, 1, 0x44, 0x01, 0xc0, // PPS
        0, 0, 0, 1, 0x26, 0x01, 0xaf, // IDR
    ];
    const BARE_KEYFRAME: &[u8] = &[0, 0, 0, 1, 0x26, 0x01, 0xaf];

    fn ctl_with_sink() -> (SessionCtl, mpsc::Receiver<EncodedFrame>, Arc<AtomicBool>) {
        let (tx, rx) = mpsc::channel(8);
        let answer = Arc::new(AtomicBool::new(true));
        let ctl = SessionCtl::default();
        *ctl.video.lock().unwrap() = Some(FrameSink {
            tx,
            codec: VideoCodec::Hevc,
            answer_with_config: answer.clone(),
            parameter_sets: Vec::new(),
        });
        (ctl, rx, answer)
    }

    /// MediaCodec hands over the parameter sets once, in a buffer of their own; a keyframe that
    /// comes without them must still open the stream with them.
    #[test]
    fn a_bare_keyframe_takes_the_codec_config_buffers_parameter_sets() {
        let (ctl, mut rx, _) = ctl_with_sink();
        ctl.push_frame(&KEYFRAME[..21], false, true);
        assert!(rx.try_recv().is_err(), "codec config is not a frame");
        ctl.push_frame(BARE_KEYFRAME, true, false);
        let frame = rx.try_recv().unwrap();
        assert_eq!(frame.config.unwrap().sps_pps, &KEYFRAME[..21]);
        assert_eq!(frame.data, BARE_KEYFRAME);
    }

    /// Only a keyframe request earns the next keyframe a config.
    #[test]
    fn the_config_rides_only_on_requested_keyframes() {
        let (ctl, mut rx, answer) = ctl_with_sink();
        ctl.push_frame(KEYFRAME, true, false);
        assert!(rx.try_recv().unwrap().config.is_some());
        ctl.push_frame(KEYFRAME, true, false);
        assert!(rx.try_recv().unwrap().config.is_none());
        answer.store(true, Ordering::Relaxed);
        ctl.push_frame(KEYFRAME, true, false);
        assert!(rx.try_recv().unwrap().config.is_some());
    }
}
