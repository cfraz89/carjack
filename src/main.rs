use eyre::{Result, eyre};
use libcarjack::carplay_session::{TouchEvent, WifiCredentials};
use tokio::sync::mpsc;
use tracing_subscriber::filter::{EnvFilter, LevelFilter};

use crate::{
    airplay::start_video_stream,
    encode::encode_h264_to_channel,
    portal::start_remote_desktop,
    wifi_connect::{CarWifi, connect_to_car_wifi},
    wireless::wireless_carplay,
};

mod airplay;
mod carplay;
mod encode;
mod portal;
mod usb;
mod wifi_connect;
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

    let (_video_tx, video_rx) = mpsc::channel::<Vec<u8>>(64);
    let (touch_tx, mut touch_rx) = mpsc::channel::<TouchEvent>(64);
    let (wifi_tx, mut wifi_rx) = mpsc::channel::<WifiCredentials>(4);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();

    let (airplay_video_tx, airplay_video_rx) = mpsc::channel::<Vec<u8>>(64);

    tracing::info!("Starting CarPlay session");

    let carplay = wireless_carplay(video_rx, touch_tx, wifi_tx, ready_tx);
    tokio::pin!(carplay);

    let mut wifi_handle: Option<tokio::task::JoinHandle<Result<CarWifi>>> = None;
    let mut ready_rx = Some(ready_rx);

    // Phase 1: process carplay session, connect WiFi immediately when creds arrive
    loop {
        tokio::select! {
            biased;
            result = &mut carplay => {
                if let Err(e) = result {
                    tracing::error!("CarPlay session error: {e}");
                }
                return Ok(());
            }
            Some(creds) = wifi_rx.recv() => {
                if wifi_handle.is_none() {
                    tracing::info!("Got WiFi creds, connecting");
                    let handle = tokio::spawn(async move {
                        connect_to_car_wifi(&creds.ssid, creds.passphrase.as_deref().unwrap_or("")).await
                    });
                    wifi_handle = Some(handle);
                }
            }
            _ = async { ready_rx.as_mut()?.await.ok() } => {
                tracing::info!("EA session ready");
                break;
            }
        }
    }

    // Phase 2: wait for WiFi connection to complete
    let car_wifi_result = if let Some(handle) = wifi_handle {
        handle.await.map_err(|e| eyre!("WiFi task panicked: {e}"))?
    } else if let Some(creds) = wifi_rx.recv().await {
        connect_to_car_wifi(&creds.ssid, creds.passphrase.as_deref().unwrap_or("")).await
    } else {
        return Err(eyre!("No WiFi credentials received from car"));
    };
    let car_wifi = car_wifi_result?;

    tracing::info!("Car WiFi connected, proceeding to video streaming");

    // Phase 3: start AirPlay video streaming over WiFi + remote desktop
    let airplay = start_video_stream(car_wifi.ip, airplay_video_rx);
    tokio::pin!(airplay);

    let rd_stream = start_remote_desktop().await?;
    let node_id = rd_stream.node_id();

    let encode = encode_h264_to_channel(node_id, airplay_video_tx);
    tokio::pin!(encode);

    loop {
        tokio::select! {
            result = &mut carplay => {
                if let Err(e) = result {
                    tracing::error!("CarPlay session error: {e}");
                }
                break;
            }
            result = &mut airplay => {
                if let Err(e) = result {
                    tracing::error!("AirPlay streaming error: {e}");
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
