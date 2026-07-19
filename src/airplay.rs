use std::net::SocketAddr;

use eyre::{Result, eyre};
use tokio::sync::mpsc;
use tokio_util::task::AbortOnDropHandle;
use uuid::Uuid;

pub use libcarjack::airplay::EncodedFrame;

pub async fn start_video_stream(
    peer_addr: SocketAddr,
    remote_homekit_id: Option<Uuid>,
    video_source: crate::encode::VideoSource,
) -> Result<()> {
    let conn = libcarjack::airplay::connect_airplay(peer_addr, remote_homekit_id).await?;
    let (video_tx, video_rx) = mpsc::channel::<EncodedFrame>(64);
    let mut encoder = AbortOnDropHandle::new(tokio::spawn(crate::encode::encode_h264_to_channel(
        video_source,
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
        result = libcarjack::airplay::run_video_stream(conn, video_rx) => result,
    }
}
