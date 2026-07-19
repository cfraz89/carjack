use std::sync::Arc;

use eyre::{Result, eyre};
use libcarjack::carplay_session::TouchEvent;
use tokio::sync::{mpsc, watch};
use tokio_util::task::AbortOnDropHandle;

pub use libcarjack::airplay::{AirPlayConnection, EncodedFrame};

use crate::compositor::CapturedFrame;

/// Stream the compositor's `frame_rx` frames to the car over an already-connected AirPlay
/// session (`conn`), encoding to H264 sized to the car's display. Touch reports the car sends
/// back over the AirPlay event channel are decoded and forwarded to `touch_tx`.
pub async fn start_video_stream(
    conn: AirPlayConnection,
    frame_rx: watch::Receiver<Option<Arc<CapturedFrame>>>,
    touch_tx: mpsc::Sender<TouchEvent>,
) -> Result<()> {
    let (video_tx, video_rx) = mpsc::channel::<EncodedFrame>(64);
    let mut encoder = AbortOnDropHandle::new(tokio::spawn(crate::encode::encode_h264_to_channel(
        frame_rx,
        conn.width,
        conn.height,
        video_tx,
    )));

    // Race the encoder against the video stream rather than just awaiting the stream: if the
    // GStreamer pipeline errors, encode_h264_to_channel returns Err and drops video_tx, which
    // only shows up on the stream side as a silently closed channel ("Video stream ended", no
    // error) - racing surfaces the real reason instead of losing it.
    tokio::select! {
        result = &mut encoder => {
            match result {
                Ok(Ok(())) => Err(eyre!("video encoder task ended unexpectedly")),
                Ok(Err(e)) => Err(eyre!("video encoder failed: {e}")),
                Err(e) => Err(eyre!("video encoder task panicked: {e}")),
            }
        }
        result = libcarjack::airplay::run_video_stream(conn, video_rx, touch_tx) => result,
    }
}
