use eyre::Result;
use iap2_proto::{config::LinkConfig, iap2::Iap2};
use libcarjack::carplay_session::{CarplaySession, TouchEvent, WifiCredentials};
use tokio::sync::{mpsc, oneshot};

pub async fn run_carplay_session(
    stream: impl iap2_proto::iap2::Stream,
    video_rx: mpsc::Receiver<Vec<u8>>,
    touch_tx: mpsc::Sender<TouchEvent>,
    ready_tx: oneshot::Sender<()>,
) -> Result<()> {
    tracing::debug!("Establishing iAP2 link");

    let iap2 = Iap2::start(stream, LinkConfig::default()).await?;

    tracing::debug!("iAP2 link established");

    let (wifi_tx, mut wifi_rx) = mpsc::channel::<WifiCredentials>(4);

    tokio::spawn(async move {
        while let Some(creds) = wifi_rx.recv().await {
            tracing::info!(
                "Head unit WiFi: SSID={}, BSSID={:?}",
                creds.ssid,
                creds.bssid
            );
        }
    });

    let mut session = CarplaySession::new(iap2, video_rx, touch_tx, wifi_tx, ready_tx);
    session.run().await?;

    Ok(())
}
