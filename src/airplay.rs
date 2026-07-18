use std::net::SocketAddr;

use catplay_carplay::video::AvccConfig;
use eyre::Result;
use tokio::sync::mpsc;
use tokio_util::task::AbortOnDropHandle;
use uuid::Uuid;

pub use libcarjack::airplay::EncodedFrame;

pub async fn start_video_stream(
    peer_addr: SocketAddr,
    remote_homekit_id: Option<Uuid>,
    node_id: u32,
) -> Result<()> {
    let conn = libcarjack::airplay::connect_airplay(peer_addr, remote_homekit_id).await?;
    let (video_tx, video_rx) = mpsc::channel::<EncodedFrame>(64);
    let _encoder = AbortOnDropHandle::new(tokio::spawn(crate::encode::encode_h264_to_channel(
        node_id,
        conn.width,
        conn.height,
        video_tx,
    )));
    libcarjack::airplay::run_video_stream(conn, video_rx).await
}
