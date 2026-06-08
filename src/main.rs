use eyre::Result;
use tracing::debug;
use tracing_subscriber::filter::{EnvFilter, LevelFilter};

use crate::{usb::register_gadget, wireless::wireless_carplay};

mod encode;
mod portal;
mod usb;
mod wireless;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::DEBUG.into())
                .from_env_lossy(),
        )
        .init();

    // let gadget = register_gadget()?;
    // debug!(?gadget);
    wireless_carplay().await?;

    let remote = portal::start_remote_desktop().await?;
    encode::encode_h264(remote.stream.pipe_wire_node_id()).await?;
    Ok(())
}
