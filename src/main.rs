use eyre::Result;
use tracing_subscriber::filter::{EnvFilter, LevelFilter};

// use libcarjack::cast;
//
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
    Ok(remote)
}
