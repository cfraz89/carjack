use crate::carplay_session::{CarplaySession, DeviceIdentity, WifiCredentials};
use eyre::Result;
use iap2_proto::iap2::Iap2;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub async fn run_carplay_session(
    iap2: Iap2<impl iap2_proto::iap2::Stream>,
    wifi_tx: mpsc::Sender<WifiCredentials>,
    ready_tx: oneshot::Sender<()>,
    identity: DeviceIdentity,
    handover: CancellationToken,
) -> Result<()> {
    let mut session = CarplaySession::new(iap2, wifi_tx, ready_tx, identity, handover);
    session.run().await?;

    Ok(())
}
