use std::net::SocketAddr;
use std::time::Duration;

use bytes::BytesMut;
use catplay_carplay::carplay_tx::{
    AirPlayTransmitter, AirPlayTransmitterBootstrap, AirPlayTransmitterProxy,
    AirPlayTransmitterProxyRef,
};
use catplay_carplay::modes::{
    EntityEnum, Resource, ResourceConstraint, ResourceController, ResourceID,
    ResourceTransferPriority,
};
use catplay_carplay::msg::Command;
use catplay_carplay::rtsp_frame::HttpStatus;
use catplay_carplay::screen::tx::{ScreenTransmitError, ScreenTransmitSink};
use catplay_carplay::video::{AvccConfig, AvccConfigExtended, EncodedVideoFrame, Pts};
use catplay_hap::HomekitStorageFile;
use eyre::{Result, eyre};
use tokio::sync::mpsc;
use uuid::Uuid;

/// A single encoded H264 access unit with optional AVCC configuration
/// (present on keyframes, extracted by the encoder).
pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub config: Option<AvccConfig>,
}

pub struct AirPlayConnection {
    tx: AirPlayTransmitterProxyRef,
    pub width: u32,
    pub height: u32,
    pub max_fps: u32,
}

pub async fn connect_airplay(
    peer_addr: SocketAddr,
    remote_homekit_id: Option<Uuid>,
) -> Result<AirPlayConnection> {
    let homekit = HomekitStorageFile::memory();

    let bootstrap = AirPlayTransmitterBootstrap {
        homekit,
        peer_ip: peer_addr,
        controller_features: vec![],
        remote_homekit_id,
    };

    let tx = AirPlayTransmitterProxy::connect(bootstrap).await?;
    tracing::info!("AirPlay bootstrap complete!");

    let disp = tx
        .info_cached()
        .displays
        .first()
        .ok_or_else(|| eyre!("car declared no display in /info"))?;
    let (width, height) = (disp.width_pixels, disp.height_pixels);
    let max_fps = disp.max_fps.unwrap_or(0);
    let disp_uuid = disp.uuid.clone();
    tracing::info!("Car display: {width}x{height} @ {max_fps} fps, uuid={disp_uuid}");

    Ok(AirPlayConnection { tx, width, height, max_fps })
}

pub async fn run_video_stream(
    conn: AirPlayConnection,
    mut video_rx: mpsc::Receiver<EncodedFrame>,
) -> Result<()> {
    let AirPlayConnection { tx, width, height, .. } = conn;

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

    // Service the CarPlay control commands the car sends back over the AirPlay event
    // channel (HID reports, mode changes, key-frame requests, ...). catplay queues each one
    // and blocks its event server waiting for our response; if we never answer, the car
    // tears the whole session down within ~100ms of the first command (it sends a
    // HidSendReport right after RECORD). Runs for the session lifetime.
    let _cmd_guard = abort_on_drop(tokio::spawn(handle_car_commands(
        tx.clone(),
        modes,
    )));

    let latency = Duration::from_millis(100);
    let mut screen_guard = tx.setup_screen(latency).await?;

    tx.record().await?;
    tracing::info!("RECORD sent, streaming video");

    // The encoder has been running (and buffering) since before AirPlay setup completed,
    // so drop everything queued up to now - otherwise we'd open the stream with a burst of
    // seconds-old frames. Steady state then only ever pushes near-live frames.
    let mut dropped = 0;
    while video_rx.try_recv().is_ok() {
        dropped += 1;
    }
    if dropped > 0 {
        tracing::info!("Dropped {dropped} stale buffered frames before streaming");
    }

    let mut sent_keyframe = false;
    let mut frames_sent: u64 = 0;
    let mut last_heartbeat = std::time::Instant::now();
    const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

    while let Some(encoded) = video_rx.recv().await {
        let data_len = encoded.data.len();
        let is_keyframe = encoded.config.is_some();

        // After draining we may land mid-GOP; the stream has to open on a keyframe.
        if !sent_keyframe && !is_keyframe {
            continue;
        }

        let frame = EncodedVideoFrame {
            // Present `latency` in the future, matching catplay's own transmitter usage.
            // A bare `now` lands in the past relative to the media clock anchor, producing
            // the negative/backwards timestamps the head unit rejects.
            pts: Pts(std::time::Instant::now() + latency),
            width,
            height,
            data: BytesMut::from(&encoded.data[..]),
            config: encoded.config.map(|avcc| AvccConfigExtended {
                hevc: false,
                avcc,
                video_latency: Duration::ZERO,
                width,
                height,
                respect_timestamps: true,
            }),
            nal_offsets: None,
            is_keyframe: Some(is_keyframe),
            header_buf: BytesMut::new(),
            chacha_tag_buf: BytesMut::new(),
        };

        if let Err((err, _)) = screen_guard.push_frame(frame).await {
            // A closed screen socket means the car tore the session down - bail so the
            // caller can restart the whole flow from a fresh Bluetooth connection.
            if matches!(err, ScreenTransmitError::Closed) {
                return Err(eyre!("Screen socket closed by accessory"));
            }
            tracing::warn!("push_frame error: {err}");
        } else {
            frames_sent += 1;
            sent_keyframe = true;
        }

        if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
            tracing::info!(
                "Streaming heartbeat: {frames_sent} frames sent, last frame {data_len} bytes, keyframe={is_keyframe}"
            );
            last_heartbeat = std::time::Instant::now();
        }
    }

    tracing::info!("Video stream ended");
    Ok(())
}

/// Answer the CarPlay control commands the car sends over the AirPlay event channel.
/// With no real iPhone to proxy to (we stream a desktop), the car just needs each command
/// acknowledged: ChangeModes gets a mode-state response, everything else an OK.
async fn handle_car_commands(tx: AirPlayTransmitterProxyRef, mut modes: ResourceController) {
    while let Some(mut cmd) = tx.pop_command().await {
        tracing::debug!("Car command: {:?}", cmd.command().get_type());
        match cmd.command() {
            Command::ChangeModes(change) => {
                let resp = modes.process_request_to_response(change);
                if cmd.respond_plist(HttpStatus::Ok, resp).is_err() {
                    cmd.respond_err();
                }
            }
            _ => cmd.respond_ok(),
        }
    }
    tracing::debug!("Car command handler ended");
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
