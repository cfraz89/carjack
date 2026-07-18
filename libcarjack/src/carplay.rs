use eyre::Result;
use iap2_proto::iap2::Iap2;
use crate::carplay_session::{CarplaySession, TouchEvent, WifiCredentials};
use tokio::sync::{mpsc, oneshot};

pub async fn run_carplay_session(
    iap2: Iap2<impl iap2_proto::iap2::Stream>,
    touch_tx: mpsc::Sender<TouchEvent>,
    wifi_tx: mpsc::Sender<WifiCredentials>,
    ready_tx: oneshot::Sender<()>,
) -> Result<()> {
    let mut session = CarplaySession::new(iap2, touch_tx, wifi_tx, ready_tx);
    session.run().await?;

    Ok(())
}
