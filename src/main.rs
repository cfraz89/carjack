use eyre::Result;
use libcarjack::carplay_session::TouchEvent;
use tokio::sync::mpsc;
use tracing_subscriber::filter::{EnvFilter, LevelFilter};

use crate::{encode::encode_h264_to_channel, portal::start_remote_desktop, wireless::wireless_carplay};

mod carplay;
mod encode;
mod portal;
mod usb;
mod wireless;

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

    let (video_tx, video_rx) = mpsc::channel::<Vec<u8>>(64);
    let (touch_tx, mut touch_rx) = mpsc::channel::<TouchEvent>(64);

    let (ready_tx, mut ready_rx) = tokio::sync::oneshot::channel::<()>();

    let carplay = wireless_carplay(video_rx, touch_tx, ready_tx);
    tokio::pin!(carplay);

    // Phase 1: wait until EA session is ready
    tokio::select! {
        result = &mut carplay => {
            if let Err(e) = result {
                tracing::error!("CarPlay session error: {e}");
            }
            return Ok(());
        }
        _ = &mut ready_rx => {
            tracing::info!("EA session ready, starting remote desktop and encoder");
        }
    }

    // Phase 2: EA session active, start remote desktop + encoder
    let rd_stream = start_remote_desktop().await?;
    let node_id = rd_stream.node_id();

    let encode = encode_h264_to_channel(node_id, video_tx);
    tokio::pin!(encode);

    loop {
        tokio::select! {
            result = &mut carplay => {
                if let Err(e) = result {
                    tracing::error!("CarPlay session error: {e}");
                }
                break;
            }
            result = &mut encode => {
                if let Err(e) = result {
                    tracing::error!("Encoding error: {e}");
                }
                break;
            }
            touch = touch_rx.recv() => {
                match touch {
                    Some(event) => {
                        rd_stream.send_touch(&event).await?;
                    }
                    None => break,
                }
            }
        }
    }
    Ok(())
}
