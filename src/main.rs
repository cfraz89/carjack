use eyre::Result;
use tracing_subscriber::filter::{EnvFilter, LevelFilter};

use crate::h264::encode_h264;

// use libcarjack::cast;
//
mod h264;
mod xdg;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::DEBUG.into())
                .from_env_lossy(),
        )
        .init();
    let remote = xdg::start_remote_desktop().await?;
    encode_h264(remote.stream.pipe_wire_node_id()).await?;
    Ok(())
}
