use std::net::SocketAddr;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use bytes::BytesMut;
use catplay_carplay::carplay_tx::{
    AirPlayTransmitter, AirPlayTransmitterBootstrap, AirPlayTransmitterProxy,
    AirPlayTransmitterProxyRef,
};
use catplay_carplay::common::AirPlayFeature;
use catplay_carplay::modes::{
    EntityEnum, Resource, ResourceConstraint, ResourceController, ResourceID,
    ResourceTransferPriority,
};
use catplay_carplay::msg::Command;
use catplay_carplay::rtsp_frame::HttpStatus;
use catplay_carplay::screen::tx::{ScreenTransmitError, ScreenTransmitSink};
use catplay_carplay::video::{AvccConfig, AvccConfigExtended, EncodedVideoFrame, Pts, VideoView};
use catplay_hap::{HomekitStorageFile, HomekitStorageRef};
use eyre::{eyre, Result};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::carplay_session::{TouchEvent, TouchEventType};

/// A single encoded access unit with optional codec configuration (present on keyframes,
/// extracted by the encoder): SPS/PPS for H264, VPS/SPS/PPS for HEVC.
pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub config: Option<AvccConfig>,
}

/// Which codec the screen stream carries. An iPhone sends HEVC to every unit captured so far;
/// H264 is what carjack sent before and what an older accessory may need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    Hevc,
}

impl VideoCodec {
    pub fn is_hevc(self) -> bool {
        matches!(self, Self::Hevc)
    }

    pub fn describe(self) -> &'static str {
        match self {
            Self::H264 => "H264",
            Self::Hevc => "HEVC",
        }
    }
}

/// Would an iPhone send this accessory HEVC?
///
/// A declaration settles it, but neither unit captured so far declares anything - the phone keys
/// off something else, and the only thing that separates them is how new the accessory's AirPlay
/// implementation is: `475.8.2` gets HEVC from a phone, `320.14` gets H264 and shows black for
/// HEVC. The threshold between the two is interpolated, not observed.
pub fn expects_hevc(hevc_info: bool, multi_codec: bool, source_version: &str) -> bool {
    const HEVC_FROM_SOURCE_VERSION: u32 = 400;
    let major = source_version
        .split('.')
        .next()
        .and_then(|m| m.parse::<u32>().ok())
        .unwrap_or(0);
    hevc_info || multi_codec || major >= HEVC_FROM_SOURCE_VERSION
}

pub struct AirPlayConnection {
    tx: AirPlayTransmitterProxyRef,
    pub width: u32,
    pub height: u32,
    pub max_fps: u32,
    /// Where our stream sits on the accessory's panel, when it reserves part of it for itself.
    pub view: Option<VideoView>,
    /// Whether an iPhone would send this accessory HEVC - see [`expects_hevc`].
    pub hevc_expected: bool,
}

/// HomeKit pairings for the whole process, so an accessory that has met us before can take
/// `/pair-verify` against stored keys instead of a full `/pair-setup`. Left unset (tests) each
/// connection gets a throwaway identity.
///
/// Inert for now: catplay's transmitter bootstrap goes straight to SETUP - its `start_pair_task`
/// and `start_auth_setup_task` calls are commented out - so nothing here is read or written until
/// that flow is enabled. Units tested so far are happy with an unencrypted control channel.
static HOMEKIT: OnceLock<HomekitStorageRef> = OnceLock::new();

/// Point HomeKit storage at a file, loading any pairings already in it. Corrupt or unreadable
/// storage is not fatal - catplay recovers it with a fresh identity, costing one re-pair.
pub fn init_homekit_storage(path: &Path) -> Result<()> {
    let path = path
        .to_str()
        .ok_or_else(|| eyre!("HomeKit storage path {path:?} is not UTF-8"))?;
    let storage = HomekitStorageFile::file(path).map_err(|e| eyre!("{e:?}"))?;
    tracing::info!(
        "HomeKit identity {} from {path} ({} accessory pairing(s) known)",
        storage.device_id(),
        storage.paired_ids().len()
    );
    let _ = HOMEKIT.set(storage);
    Ok(())
}

pub async fn connect_airplay(
    peer_addr: SocketAddr,
    remote_homekit_id: Option<Uuid>,
    device_id: String,
    mac_address: String,
) -> Result<AirPlayConnection> {
    let homekit = HOMEKIT
        .get()
        .cloned()
        .unwrap_or_else(HomekitStorageFile::memory);

    // Only pass an id we have actually paired with. Then it selects `/pair-verify` against the
    // stored keys; unknown, all it can do is make `/pair-setup` assert that the accessory's real
    // identity matches - failing the session over a guess, since not every unit's advertised id
    // is its HomeKit one. Filtering here also makes a first contact free to succeed and be
    // remembered, which is what makes the *next* connection the fast one.
    let remote_homekit_id = remote_homekit_id.filter(|id| homekit.find_paired_by_id(id).is_some());
    match remote_homekit_id {
        Some(id) => tracing::info!("Known HomeKit accessory {id}; expecting pair-verify"),
        None => tracing::info!("Unknown HomeKit accessory; expecting a full pair-setup"),
    }

    let bootstrap = AirPlayTransmitterBootstrap {
        homekit,
        peer_ip: peer_addr,
        // iAP2 over AirPlay (`iap2_tunnel`) is a negotiated feature - `ControllerFeature::IApChannel`
        // is what claims it. Not claimed yet: the only unit we can test against advertises just
        // `VocoderInfo` and never tunnels anything, so the claim is unexercised, and an unexercised
        // claim is exactly what broke the bike's WiFi request (afd6605). The `Accessory AirPlay
        // features` log below says whether a given unit supports it; add the feature here once one
        // does.
        controller_features: vec![],
        remote_homekit_id,
        device_id,
        mac_address,
        pair_verify_first: remote_homekit_id.is_some(),
    };

    let tx = AirPlayTransmitterProxy::connect(bootstrap).await?;
    tracing::info!("AirPlay bootstrap complete!");
    {
        // Whether the accessory will tunnel iAP2 here at all is a property of *its* feature set,
        // not just ours - log it so a silent tunnel can be told apart from an unsupported one.
        let info = tx.info_cached();
        tracing::info!(
            extended_features = ?info.extended_features,
            bluetooth_ids = ?info.bluetooth_ids,
            "Accessory AirPlay features"
        );
    }

    let info = tx.info_cached();
    let disp = info
        .displays
        .first()
        .ok_or_else(|| eyre!("accessory declared no display in /info"))?;
    let (width, height) = (disp.width_pixels, disp.height_pixels);
    let max_fps = disp.max_fps.unwrap_or(0);
    let disp_uuid = disp.uuid.clone();
    tracing::info!("Accessory display: {width}x{height} @ {max_fps} fps, uuid={disp_uuid}");

    let view = panel_view(&info, &disp_uuid);
    match view {
        Some(v) => tracing::info!(
            "Accessory panel reserves part of itself: video at ({}, {}) sized {}x{}",
            v.origin_x,
            v.origin_y,
            v.width,
            v.height
        ),
        None => tracing::debug!("Accessory gave no panel geometry; video covers the display"),
    }

    let multi_codec = info
        .features
        .contains(AirPlayFeature::SUPPORTS_SCREEN_MULTI_CODEC);
    let hevc_expected = expects_hevc(info.hevc_info.is_some(), multi_codec, &info.source_version);
    tracing::info!(
        hevc_expected,
        hevc_info = info.hevc_info.is_some(),
        multi_codec,
        source_version = %info.source_version,
        "Accessory codec declaration"
    );

    Ok(AirPlayConnection {
        tx,
        width,
        height,
        max_fps,
        view,
        hevc_expected,
    })
}

/// Where our stream sits on the accessory's panel, from `/info`'s `displayPanels`.
///
/// `None` when it covers the whole panel, which is the only case catplay used to handle. An
/// accessory that keeps a strip for its own instruments gives an origin below the top; the size is
/// what is left of the panel. (An iPhone sends one pixel less for the origin than the panel
/// declares - the reason is unknown, so the declared value is used.)
fn panel_view(
    info: &catplay_carplay::msg::InfoMessageResponse,
    display_uuid: &str,
) -> Option<VideoView> {
    let panel = info.display_panels.iter().find(|p| {
        p.initial_video_streams
            .iter()
            .any(|s| s.uuid == display_uuid)
    })?;
    let stream = panel
        .initial_video_streams
        .iter()
        .find(|s| s.uuid == display_uuid)?;
    let origin_x = stream.origin_x_pixels.unwrap_or(0);
    let origin_y = stream.origin_y_pixels.unwrap_or(0);
    if origin_x == 0 && origin_y == 0 {
        return None;
    }
    let width = panel.width_pixels?.saturating_sub(origin_x);
    let height = panel.height_pixels?.saturating_sub(origin_y);
    (width > 0 && height > 0).then_some(VideoView {
        origin_x: origin_x as f32,
        origin_y: origin_y as f32,
        width: width as f32,
        height: height as f32,
    })
}

pub async fn run_video_stream(
    conn: AirPlayConnection,
    codec: VideoCodec,
    mut video_rx: mpsc::Receiver<EncodedFrame>,
    touch_tx: mpsc::Sender<TouchEvent>,
    keyframe_tx: mpsc::Sender<()>,
    handover: CancellationToken,
) -> Result<()> {
    let AirPlayConnection {
        tx,
        width,
        height,
        max_fps,
        view,
        hevc_expected: _,
    } = conn;

    // CarPlay resource arbitration: claim the main screen for us (the controller) via a
    // ModesChanged command BEFORE setting up the screen stream. This ordering is mandatory
    // - without it the head unit doesn't consider the phone the screen owner and tears the
    // session down as soon as video starts arriving.
    let mut modes = ResourceController::from_change_modes(&tx.info_cached().modes);
    if modes.screen.entity() != EntityEnum::Controller {
        modes.screen.take_by_controller(&Resource::take(
            ResourceID::MainScreen,
            ResourceTransferPriority::UserInitiated,
            ResourceConstraint::Anytime,
            ResourceConstraint::Anytime,
        ));
    }
    tx.assert_modes(modes.serialize_to_state()).await?;
    tracing::info!("Asserted screen modes (controller owns screen)");

    // Service the CarPlay control commands the accessory sends back over the AirPlay event
    // channel (HID reports, mode changes, key-frame requests, ...). catplay queues each one
    // and blocks its event server waiting for our response; if we never answer, the accessory
    // tears the whole session down within ~100ms of the first command (it sends a
    // HidSendReport right after RECORD). Runs for the session lifetime.
    // iAP2 moves onto this channel when the accessory drops Bluetooth; without someone reading it
    // the control channel simply ends at the handover.
    let (tunnel_stream, tunnel) = crate::iap2_tunnel::open(tx.clone());
    let tunnel_used = tunnel.when_used();
    let _tunnel_guard = abort_on_drop(tokio::spawn(async move {
        match crate::iap2_tunnel::start_session(tunnel_stream, tunnel_used).await {
            Ok(iap2) => crate::iap2_tunnel::run(iap2).await,
            Err(e) => tracing::warn!(?e, "iAP2 tunnel session did not come up"),
        }
    }));

    let _cmd_guard = abort_on_drop(tokio::spawn(handle_accessory_commands(
        tx.clone(),
        modes,
        touch_tx,
        keyframe_tx.clone(),
        tunnel,
        handover,
    )));

    let latency = Duration::from_millis(100);

    // SETUP has to follow the modes assertion straight away - a head unit walks away if we stop
    // making progress.
    let mut screen_guard = tx.setup_screen(latency).await?;

    // Hold only RECORD, which is the promise that video is coming.
    let first_frame = wait_for_first_keyframe(&mut video_rx, &keyframe_tx).await?;

    tx.record().await?;
    tracing::info!("RECORD sent, streaming video");

    let mut sent_keyframe = false;
    // Set while a keyframe request is outstanding, so a sustained stall asks once rather than
    // once per frame (which would hold the encoder at an all-IDR stream and make things worse).
    let mut requested_keyframe = false;
    let mut dropped_full: u64 = 0;
    let mut frames_sent: u64 = 0;
    let mut last_heartbeat = std::time::Instant::now();
    let mut frames_at_last_heartbeat: u64 = 0;
    const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

    // Steady media clock. The accessory honours our PTS (`respect_timestamps`), so presenting frames on
    // an evenly-spaced timeline (`frame_dur` apart) - rather than at jittery wall-clock arrival
    // time - is what makes playback smooth: the `latency` buffer soaks up encode/delivery jitter.
    // We re-anchor to real-time only if the clock drifts out of a safe window (a stall, or the
    // production rate wandering from `fps`), so a PTS never lands in the past (which the head unit
    // rejects) or runs unboundedly ahead.
    let fps = if max_fps == 0 { 30 } else { max_fps };
    let frame_dur = Duration::from_nanos(1_000_000_000 / fps as u64);
    const AHEAD_LIMIT: Duration = Duration::from_millis(200);
    let mut next_pts: Option<std::time::Instant> = None;

    // The held keyframe goes out first, so the stream opens on it.
    let mut pending = Some(first_frame);
    loop {
        let encoded = match pending.take() {
            Some(frame) => frame,
            None => match video_rx.recv().await {
                Some(frame) => frame,
                None => break,
            },
        };
        let data_len = encoded.data.len();
        let is_keyframe = encoded.config.is_some();

        // Backpressure and catplay both re-arm this mid-stream; the stream has to (re)open on a
        // keyframe, so skip deltas until one arrives.
        if !sent_keyframe && !is_keyframe {
            continue;
        }

        // The accessory has stopped keeping up (a WiFi stall, a busy head unit). `push_frame` would
        // wait for a slot, which just converts the backlog into latency, so drop this frame
        // instead - and because every later delta frame references the one we dropped, ask the
        // encoder for a fresh keyframe so the picture recovers now rather than at the next
        // natural IDR up to a second away.
        if screen_guard.is_full() {
            dropped_full += 1;
            if !requested_keyframe {
                tracing::warn!(
                    queued = screen_guard.queue_len(),
                    "Accessory's screen queue is full; dropping frame and forcing a keyframe"
                );
                let _ = keyframe_tx.try_send(());
                requested_keyframe = true;
            }
            sent_keyframe = false;
            continue;
        }

        // Advance the media clock by one frame; keep the smooth scheduled time as long as it
        // stays in the safe window (strictly in the future so it's never rejected as past, and
        // not more than AHEAD_LIMIT beyond the ideal so buffering can't grow unbounded), else
        // re-anchor to `now + latency`.
        let now = std::time::Instant::now();
        let ideal = now + latency;
        let pts_instant = match next_pts {
            Some(t) if t > now && t <= ideal + AHEAD_LIMIT => t,
            _ => ideal,
        };
        next_pts = Some(pts_instant + frame_dur);

        let frame = EncodedVideoFrame {
            pts: Pts(pts_instant),
            width,
            height,
            data: BytesMut::from(&encoded.data[..]),
            config: encoded.config.map(|avcc| AvccConfigExtended {
                hevc: codec.is_hevc(),
                avcc,
                video_latency: Duration::ZERO,
                width,
                height,
                respect_timestamps: true,
                view,
            }),
            nal_offsets: None,
            is_keyframe: Some(is_keyframe),
            // The encoder emits the wire form already; converting it again would read the length
            // prefixes as start codes.
            length_prefixed: true,
            header_buf: BytesMut::new(),
            chacha_tag_buf: BytesMut::new(),
        };

        if let Err((err, _)) = screen_guard.push_frame(frame).await {
            // A closed screen socket means the accessory tore the session down - bail so the
            // caller can restart the whole flow from a fresh Bluetooth connection.
            if matches!(err, ScreenTransmitError::Closed) {
                return Err(eyre!("Screen socket closed by accessory"));
            }
            if matches!(err, ScreenTransmitError::NeedsKeyframe) {
                // catplay refuses delta frames until the stream has opened on a keyframe (it
                // re-arms this whenever the screen socket reconnects). Pushing deltas at it
                // regardless just discards them until the encoder's next natural IDR, so ask
                // for one now and hold off until it arrives.
                if !requested_keyframe {
                    tracing::warn!("Screen stream needs a keyframe; requesting one");
                    let _ = keyframe_tx.try_send(());
                    requested_keyframe = true;
                }
                sent_keyframe = false;
                continue;
            }
            tracing::warn!("push_frame error: {err}");
        } else {
            frames_sent += 1;
            sent_keyframe = true;
            requested_keyframe = false;
        }

        let elapsed = last_heartbeat.elapsed();
        if elapsed >= HEARTBEAT_INTERVAL {
            let sent = frames_sent - frames_at_last_heartbeat;
            let rate = sent as f64 / elapsed.as_secs_f64();
            tracing::info!(
                "Streaming heartbeat: {rate:.1} fps ({sent} frames / {:.1}s), last frame {data_len} bytes, target {fps} fps, {dropped_full} dropped to backpressure",
                elapsed.as_secs_f64()
            );
            last_heartbeat = std::time::Instant::now();
            frames_at_last_heartbeat = frames_sent;
        }
    }

    tracing::info!("Video stream ended");
    Ok(())
}

/// How long to wait for the encoder's first keyframe before giving up on the session.
const FIRST_KEYFRAME_TIMEOUT: Duration = Duration::from_secs(10);

/// Block until the encoder has produced a keyframe.
///
/// RECORD starts the head unit's watchdog, so it must not be sent before there is a frame to send.
async fn wait_for_first_keyframe(
    video_rx: &mut mpsc::Receiver<EncodedFrame>,
    keyframe_tx: &mpsc::Sender<()>,
) -> Result<EncodedFrame> {
    // Anything already queued is stale. Dropping it can leave us mid-GOP, so ask for a fresh IDR.
    let mut dropped = 0;
    while video_rx.try_recv().is_ok() {
        dropped += 1;
    }
    if dropped > 0 {
        tracing::info!("Dropped {dropped} stale buffered frames; requesting a keyframe");
        let _ = keyframe_tx.try_send(());
    }

    // Warn while waiting: past a second it is the video source that has stalled, not the encoder,
    // and a silent wait here is near-impossible to diagnose after the fact.
    let started = std::time::Instant::now();
    let mut deltas = 0u32;
    let first = async {
        loop {
            let next = tokio::time::timeout(Duration::from_secs(1), video_rx.recv()).await;
            match next {
                Ok(Some(frame)) if frame.config.is_some() => {
                    tracing::info!(
                        "First keyframe after {:?} ({deltas} delta frames skipped)",
                        started.elapsed()
                    );
                    return Ok(frame);
                }
                Ok(Some(_)) => deltas += 1,
                Ok(None) => return Err(eyre!("encoder stopped before producing a keyframe")),
                Err(_) => tracing::warn!(
                    "Still waiting for the encoder's first keyframe after {:?} ({deltas} delta \
                     frames so far) - the accessory tears the session down ~2s from here",
                    started.elapsed()
                ),
            }
        }
    };

    match tokio::time::timeout(FIRST_KEYFRAME_TIMEOUT, first).await {
        Ok(frame) => frame,
        Err(_) => Err(eyre!(
            "no keyframe from the encoder within {:?}; not sending RECORD. The video source \
             produced nothing - check that the compositor is rendering (source=app) or that the \
             portal/PipeWire stream is live (source=desktop)",
            FIRST_KEYFRAME_TIMEOUT
        )),
    }
}

/// Answer the CarPlay control commands the accessory sends over the AirPlay event channel.
/// With no real iPhone to proxy to (we stream a desktop), the accessory just needs each command
/// acknowledged: ChangeModes gets a mode-state response, everything else an OK. Touch comes
/// here too, as `HidSendReport` touchscreen reports, which we decode and forward to the
/// compositor.
async fn handle_accessory_commands(
    tx: AirPlayTransmitterProxyRef,
    mut modes: ResourceController,
    touch_tx: mpsc::Sender<TouchEvent>,
    keyframe_tx: mpsc::Sender<()>,
    tunnel: crate::iap2_tunnel::Tunnel,
    handover: CancellationToken,
) {
    let mut touching = false;
    while let Some(mut cmd) = tx.pop_command().await {
        tracing::debug!("Accessory command: {:?}", cmd.command().get_type());
        match cmd.command() {
            Command::ChangeModes(change) => {
                let resp = modes.process_request_to_response(change);
                if cmd.respond_plist(HttpStatus::Ok, resp).is_err() {
                    cmd.respond_err();
                }
            }
            Command::ForceKeyFrame(_) => {
                // The accessory asks for this when it has lost sync - it has just joined the stream,
                // or dropped enough packets that its decoder can no longer follow the deltas.
                // Answering OK and doing nothing (which is what we used to do) leaves it
                // showing a frozen or torn picture until the encoder's next natural IDR.
                tracing::debug!("Accessory requested a keyframe");
                let _ = keyframe_tx.try_send(());
                cmd.respond_ok();
            }
            Command::HidSendReport(r) => {
                if let Some(event) = parse_touch_report(r.hid_report.as_ref(), &mut touching) {
                    let _ = touch_tx.try_send(event);
                }
                cmd.respond_ok();
            }
            Command::DisableBluetooth(_) => {
                // No unit has ever sent this - both drop the channel themselves - so it only marks
                // the handover, the same as reaching it any other way. A unit that sends it and
                // then *waits* for us to disconnect would hang here, and would be the reason to
                // start closing the link ourselves.
                tracing::info!("Accessory asked us to disable Bluetooth");
                cmd.respond_ok();
                handover.cancel();
            }
            Command::IApSendMessage(m) => {
                // iAP2 continues here once the accessory drops the Bluetooth link - see
                // `iap2_tunnel`. Answer first: catplay blocks its event server on our response.
                let data = m.data.as_ref().to_vec();
                cmd.respond_ok();
                tunnel.deliver(data);
                continue;
            }
            _ => cmd.respond_ok(),
        }
    }
    tracing::debug!("Accessory command handler ended");
}

/// Decode a CarPlay touchscreen HID report into a touch event. Observed format (12 bytes):
/// `[reportId=0, tip, Xlo, Xhi, Ylo, Yhi, const=1, 0, 0, 0, 0, 0]` - a single contact whose
/// `tip` switch (byte 1) is 1 while touching, and whose X/Y (little-endian u16) are in the
/// accessory's display-pixel space. Our compositor renders at exactly that resolution, so the
/// coordinates pass through unscaled. `touching` tracks the contact across reports so we can
/// emit Down/Motion/Up. Non-touch reports (e.g. 1-byte button reports) are ignored.
fn parse_touch_report(bytes: &[u8], touching: &mut bool) -> Option<TouchEvent> {
    if bytes.len() < 6 {
        return None;
    }
    let tip = bytes[1] != 0;
    let x = u16::from_le_bytes([bytes[2], bytes[3]]) as f64;
    let y = u16::from_le_bytes([bytes[4], bytes[5]]) as f64;
    let event_type = match (*touching, tip) {
        (false, true) => TouchEventType::Down,
        (true, true) => TouchEventType::Motion,
        (true, false) => TouchEventType::Up,
        (false, false) => return None,
    };
    *touching = tip;
    Some(TouchEvent {
        event_type,
        slot: 0,
        x,
        y,
    })
}

/// Abort a spawned task when the guard is dropped.
fn abort_on_drop(handle: tokio::task::JoinHandle<()>) -> impl Drop {
    struct Guard(tokio::task::JoinHandle<()>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    Guard(handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two units captured from a real iPhone: neither declares a codec, and only the
    /// implementation version separates the one that gets HEVC from the one that gets H264.
    #[test]
    fn hevc_follows_the_accessory_not_the_default() {
        assert!(
            expects_hevc(false, false, "475.8.2"),
            "CFMOTO takes HEVC from a phone"
        );
        assert!(
            !expects_hevc(false, false, "320.14"),
            "AUTO BOX gets H264 from a phone"
        );

        // A declaration settles it whatever the version says, and an unparseable version is not
        // evidence of anything.
        assert!(expects_hevc(true, false, "320.14"));
        assert!(expects_hevc(false, true, "320.14"));
        assert!(!expects_hevc(false, false, ""));
    }

    fn delta() -> EncodedFrame {
        EncodedFrame {
            data: vec![0u8; 4],
            config: None,
        }
    }

    fn keyframe() -> EncodedFrame {
        EncodedFrame {
            data: vec![1u8; 8],
            config: Some(AvccConfig {
                nal_size_len: 4,
                sps_pps: vec![0x67, 0x42, 0x68, 0xce],
            }),
        }
    }

    /// RECORD must not go out on a delta frame: the accessory's stream has to open on a keyframe, and
    /// the whole point of the gate is that we hold the announcement until we have one.
    #[tokio::test]
    async fn first_keyframe_skips_deltas() {
        let (tx, mut rx) = mpsc::channel(8);
        let (kf_tx, _kf_rx) = mpsc::channel(1);
        // Produced after the wait begins - a cold pipeline emitting into an already-drained
        // channel, which is the real sequence.
        let producer = tokio::spawn(async move {
            for _ in 0..3 {
                tx.send(delta()).await.unwrap();
            }
            tx.send(keyframe()).await.unwrap();
        });

        let frame = wait_for_first_keyframe(&mut rx, &kf_tx)
            .await
            .expect("keyframe");
        assert!(frame.config.is_some());
        assert_eq!(frame.data.len(), 8);
        producer.await.unwrap();
    }

    /// Frames already queued predate the session. They get dropped, and because dropping can
    /// land us mid-GOP we ask for a fresh IDR rather than waiting out the keyframe interval.
    #[tokio::test]
    async fn stale_frames_dropped_and_keyframe_requested() {
        let (tx, mut rx) = mpsc::channel(8);
        let (kf_tx, mut kf_rx) = mpsc::channel(1);
        // Queued before the wait starts, so `try_recv` sees them: a stale keyframe among them
        // must still be discarded rather than opening the stream on old content.
        tx.send(keyframe()).await.unwrap();
        tx.send(delta()).await.unwrap();

        let fresh = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            tx.send(keyframe()).await.unwrap();
        });

        let frame = wait_for_first_keyframe(&mut rx, &kf_tx)
            .await
            .expect("keyframe");
        assert!(frame.config.is_some());
        assert!(
            kf_rx.try_recv().is_ok(),
            "a keyframe should have been requested"
        );
        fresh.await.unwrap();
    }

    /// A dead encoder has to surface as an error rather than hanging: the caller aborts the
    /// session instead of leaving an AirPlay connection open that will never carry video.
    #[tokio::test]
    async fn encoder_closing_without_a_keyframe_errors() {
        let (tx, mut rx) = mpsc::channel(8);
        let (kf_tx, _kf_rx) = mpsc::channel(1);
        tx.send(delta()).await.unwrap();
        drop(tx);

        assert!(wait_for_first_keyframe(&mut rx, &kf_tx).await.is_err());
    }

    // Real reports captured from the head unit (`HID report (12 bytes)` log lines): the parser
    // must turn a stream of them into a well-formed Down -> Motion* -> Up gesture with the
    // right coordinates.
    #[test]
    fn touch_report_gesture() {
        let mut touching = false;

        // First contact -> Down at (0x018a, 0x00cd) = (394, 205).
        let down = parse_touch_report(
            &[0, 1, 0x8a, 0x01, 0xcd, 0x00, 1, 0, 0, 0, 0, 0],
            &mut touching,
        )
        .expect("down");
        assert!(matches!(down.event_type, TouchEventType::Down));
        assert_eq!((down.x, down.y), (394.0, 205.0));
        assert!(touching);

        // Still touching, moved -> Motion at (0x020f, 0x0114) = (527, 276).
        let mv = parse_touch_report(
            &[0, 1, 0x0f, 0x02, 0x14, 0x01, 1, 0, 0, 0, 0, 0],
            &mut touching,
        )
        .expect("motion");
        assert!(matches!(mv.event_type, TouchEventType::Motion));
        assert_eq!((mv.x, mv.y), (527.0, 276.0));

        // tip=0 -> Up (a real lift-off report).
        let up = parse_touch_report(
            &[0, 0, 0xe9, 0x02, 0xba, 0x01, 1, 0, 0, 0, 0, 0],
            &mut touching,
        )
        .expect("up");
        assert!(matches!(up.event_type, TouchEventType::Up));
        assert!(!touching);

        // No contact while not touching, and short (non-touch button) reports -> ignored.
        assert!(parse_touch_report(&[0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0], &mut touching).is_none());
        assert!(parse_touch_report(&[0x01], &mut touching).is_none());
    }
}
