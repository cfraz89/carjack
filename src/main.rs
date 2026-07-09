use eyre::Result;
use tracing_subscriber::filter::{EnvFilter, LevelFilter};

use crate::{portal::start_remote_desktop, usb::register_gadget, wireless::wireless_carplay};

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

    wireless_carplay().await?;
    Ok(())
}

async fn stream_desktop() -> Result<()> {
    let remote = portal::start_remote_desktop().await?;
    encode::encode_h264(remote.stream.pipe_wire_node_id()).await?;
    Ok(())
}
