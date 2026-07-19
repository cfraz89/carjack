use eyre::Result;
use iap2_proto::iap2::Iap2;
use crate::carplay_session::{CarplaySession, WifiCredentials};
use tokio::sync::{mpsc, oneshot};

pub async fn run_carplay_session(
    iap2: Iap2<impl iap2_proto::iap2::Stream>,
    wifi_tx: mpsc::Sender<WifiCredentials>,
    ready_tx: oneshot::Sender<()>,
) -> Result<()> {
    let mut session = CarplaySession::new(iap2, wifi_tx, ready_tx);
    session.run().await?;

    Ok(())
}
