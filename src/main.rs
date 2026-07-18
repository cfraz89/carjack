use std::time::Duration;

use eyre::{Result, eyre};
use libcarjack::carplay_session::{TouchEvent, WifiCredentials};
use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use tracing_subscriber::filter::{EnvFilter, LevelFilter};

use crate::{
    airplay::start_video_stream,
    carplay_ctrl::wait_for_carplay_ctrl_probe,
    encode::encode_h264_to_channel,
    portal::start_remote_desktop,
    wifi_connect::{CarWifi, connect_to_car_wifi},
    wireless::wireless_carplay,
};

mod airplay;
mod carplay;
mod carplay_ctrl;
mod encode;
mod portal;
mod usb;
mod wifi_connect;
mod wireless;

/// Aborts a spawned task on drop. A dropped `JoinHandle` merely detaches the task, so we
/// need this to guarantee the CarPlay/Bluetooth task (and its BlueZ resources) is torn down
/// before a session restart begins a fresh Bluetooth connection.
struct TaskAbort(AbortHandle);

impl Drop for TaskAbort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

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

    // Each session runs the full flow (Bluetooth -> WiFi -> AirPlay). If it fails - most
    // importantly when the car tears down the screen socket - restart from a fresh Bluetooth
    // connection rather than exiting.
    loop {
        match run_session().await {
            Ok(()) => {
                tracing::info!("Session ended cleanly, exiting");
                return Ok(());
            }
            Err(e) => {
                tracing::warn!("Session failed, restarting from Bluetooth: {e}");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

async fn run_session() -> Result<()> {
    let (touch_tx, mut touch_rx) = mpsc::channel::<TouchEvent>(64);
    let (wifi_tx, mut wifi_rx) = mpsc::channel::<WifiCredentials>(4);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
    let (bt_mac_tx, bt_mac_rx) = tokio::sync::oneshot::channel::<String>();

    let (airplay_video_tx, airplay_video_rx) = mpsc::channel::<Vec<u8>>(64);

    tracing::info!("Starting CarPlay session");

    // Run the CarPlay/iAP2 Bluetooth session as its own task so the runtime keeps polling
    // it continuously. It must keep servicing the iAP2 link (keep-alives, ACKs) throughout
    // WiFi connect and AirPlay setup - if it only advanced when awaited in the loops below,
    // it would be starved for ~10s during screen-sharing setup and the car would reset the
    // Bluetooth link, tearing down CarPlay right as streaming starts.
    let mut carplay_handle = tokio::spawn(wireless_carplay(touch_tx, wifi_tx, ready_tx, bt_mac_tx));
    let _abort = TaskAbort(carplay_handle.abort_handle());

    let mut wifi_handle: Option<tokio::task::JoinHandle<Result<CarWifi>>> = None;
    let mut ready_rx = Some(ready_rx);

    // Phase 1: connect WiFi immediately when creds arrive; wait for the EA session.
    loop {
        tokio::select! {
            biased;
            result = &mut carplay_handle => {
                return match result {
                    Ok(Ok(())) => Err(eyre!("CarPlay session ended before it was ready")),
                    Ok(Err(e)) => Err(eyre!("CarPlay session error: {e}")),
                    Err(e) => Err(eyre!("CarPlay task panicked: {e}")),
                };
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

    tracing::info!("Car WiFi connected, waiting for car probe...");

    let bt_mac = bt_mac_rx
        .await
        .unwrap_or_else(|_| "00:00:00:00:00:00".to_string());

    // Advertise _carplay-ctrl._tcp via mDNS, wait for the car's GET /ctrl-int/1/connect,
    // and use its matching _airplay._tcp Bonjour record for the real peer address + HomeKit id.
    let (peer_addr, remote_homekit_id) = match wait_for_carplay_ctrl_probe(&car_wifi.iface, &bt_mac).await {
        Ok(result) => result,
        Err(e) => {
            tracing::warn!("CarPlay Ctrl probe failed, falling back to guessed gateway: {e}");
            (std::net::SocketAddr::new(car_wifi.ip, 7000), None)
        }
    };

    // Phase 3: start AirPlay video streaming over WiFi + remote desktop
    let airplay = start_video_stream(peer_addr, remote_homekit_id, airplay_video_rx);
    tokio::pin!(airplay);

    // CARPLAY_TEST_VIDEO skips the screen-share portal (which otherwise prompts for
    // permission on every run) and feeds the AirPlay pipeline a GStreamer test pattern.
    let rd_stream = if std::env::var("CARPLAY_TEST_VIDEO").is_ok() {
        tracing::info!("CARPLAY_TEST_VIDEO set: using test video source, skipping screen-share portal");
        None
    } else {
        Some(start_remote_desktop().await?)
    };
    let node_id = rd_stream.as_ref().map(|s| s.node_id()).unwrap_or(0);

    let encode = encode_h264_to_channel(node_id, airplay_video_tx);
    tokio::pin!(encode);

    // If the car later drops Bluetooth on its own, that's not fatal - keep streaming.
    let mut carplay_done = false;

    loop {
        tokio::select! {
            result = &mut carplay_handle, if !carplay_done => {
                carplay_done = true;
                match result {
                    Ok(Ok(())) => tracing::info!("CarPlay (Bluetooth) session ended"),
                    Ok(Err(e)) => tracing::warn!("CarPlay (Bluetooth) session ended: {e}"),
                    Err(e) => tracing::warn!("CarPlay task panicked: {e}"),
                }
            }
            result = &mut airplay => {
                // Propagate so the caller restarts the whole flow (e.g. car closed the
                // screen socket).
                result?;
                return Ok(());
            }
            result = &mut encode => {
                result?;
                return Ok(());
            }
            touch = touch_rx.recv(), if !carplay_done => {
                match touch {
                    Some(event) => {
                        if let Some(rd) = &rd_stream {
                            rd.send_touch(&event).await?;
                        }
                    }
                    None => carplay_done = true,
                }
            }
        }
    }
}
