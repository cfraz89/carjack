use std::net::SocketAddr;
use std::time::Duration;

use bytes::BytesMut;
use catplay_carplay::carplay_tx::{
    AirPlayTransmitter, AirPlayTransmitterBootstrap, AirPlayTransmitterProxy,
};
use catplay_carplay::modes::{
    EntityEnum, Resource, ResourceConstraint, ResourceController, ResourceID,
    ResourceTransferPriority,
};
use catplay_carplay::screen::tx::{ScreenTransmitError, ScreenTransmitSink};
use catplay_carplay::video::{
    AnnexBIter, AvccConfig, AvccConfigExtended, EncodedVideoFrame, NalType, Pts,
};
use catplay_hap::HomekitStorageFile;
use eyre::{Result, eyre};
use tokio::sync::mpsc;
use uuid::Uuid;

fn extract_avcc_config(data: &[u8]) -> Option<AvccConfig> {
    let mut sps = Vec::new();
    let mut pps = Vec::new();

    for chunk in AnnexBIter::annexb(data) {
        let chunk = chunk.ok()?;
        if chunk.data_size == 0 {
            continue;
        }
        let nal_start = chunk.prefix_start + chunk.prefix_len;
        let nal_type = NalType::from_byte(data[nal_start]);
        match nal_type {
            NalType::Sps => {
                sps.extend_from_slice(&data[chunk.prefix_start..nal_start + chunk.data_size])
            }
            NalType::Pps => {
                pps.extend_from_slice(&data[chunk.prefix_start..nal_start + chunk.data_size])
            }
            _ => {}
        }
    }

    if sps.is_empty() {
        return None;
    }
    let mut sps_pps = sps;
    sps_pps.extend_from_slice(&pps);

    Some(AvccConfig {
        nal_size_len: 4,
        sps_pps,
    })
}

pub async fn start_video_stream(
    peer_addr: SocketAddr,
    remote_homekit_id: Option<Uuid>,
    mut video_rx: mpsc::Receiver<Vec<u8>>,
) -> Result<()> {
    let homekit = HomekitStorageFile::memory();

    let bootstrap = AirPlayTransmitterBootstrap {
        homekit,
        peer_ip: peer_addr,
        controller_features: vec![],
        remote_homekit_id,
    };

    let tx = AirPlayTransmitterProxy::connect(bootstrap).await?;
    tracing::info!("AirPlay bootstrap complete!");

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

    let mut last_config: Option<AvccConfig> = None;
    let mut sent_keyframe = false;
    let mut frames_sent: u64 = 0;
    let mut last_heartbeat = std::time::Instant::now();
    const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

    while let Some(data) = video_rx.recv().await {
        let data_len = data.len();

        // data is now one complete Annex-B access unit (reassembled in encode.rs).
        let is_keyframe = AnnexBIter::annexb(&data).any(|chunk| {
            let Ok(chunk) = chunk else { return false };
            let nal_start = chunk.prefix_start + chunk.prefix_len;
            data.get(nal_start)
                .map(|b| matches!(NalType::from_byte(*b), NalType::IdrSlice | NalType::Sps))
                .unwrap_or(false)
        });

        // After draining we may land mid-GOP; the stream has to open on a keyframe.
        if !sent_keyframe && !is_keyframe {
            continue;
        }

        if is_keyframe {
            if let Some(avcc) = extract_avcc_config(&data) {
                if last_config.as_ref() != Some(&avcc) {
                    let config = AvccConfigExtended {
                        hevc: false,
                        avcc,
                        video_latency: Duration::ZERO,
                        width: 800,
                        height: 480,
                        respect_timestamps: true,
                    };
                    screen_guard.push_avcc_config(config.clone())?;
                    last_config = Some(config.avcc);
                }
            }
        }

        let frame = EncodedVideoFrame {
            // Present `latency` in the future, matching catplay's own transmitter usage.
            // A bare `now` lands in the past relative to the media clock anchor, producing
            // the negative/backwards timestamps the head unit rejects.
            pts: Pts(std::time::Instant::now() + latency),
            width: 800,
            height: 480,
            data: BytesMut::from(&data[..]),
            config: None,
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
