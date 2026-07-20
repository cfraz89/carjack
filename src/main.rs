use std::time::Duration;

use eyre::{Result, eyre};
use libcarjack::carplay_session::{TouchEvent, WifiCredentials};
use tokio::sync::mpsc;
use tokio_util::task::AbortOnDropHandle;
use tracing_subscriber::filter::{EnvFilter, LevelFilter};

use libcarjack::carplay_ctrl::wait_for_carplay_ctrl_probe;

use carjack::{compositor::Compositor, encode};

use crate::{
    wifi_connect::{CarWifi, connect_to_car_wifi},
    wireless::wireless_carplay,
};

mod usb;
mod wifi_connect;
mod wireless;

/// Launch the desktop app(s) to stream, pointed at our internal compositor's Wayland socket.
/// `CARJACK_DESKTOP_CMD` is run via `sh -c` (e.g. a launcher or a single kiosk app); if unset,
/// nothing is launched and the compositor streams a black screen until a client connects.
fn spawn_desktop_apps(socket_name: &std::ffi::OsStr) {
    let Ok(cmd) = std::env::var("CARJACK_DESKTOP_CMD") else {
        tracing::warn!("CARJACK_DESKTOP_CMD not set - no desktop app launched, streaming black");
        return;
    };
    tracing::info!("Launching desktop app: {cmd}");
    match std::process::Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .env("WAYLAND_DISPLAY", socket_name)
        .spawn()
    {
        Ok(_) => {}
        Err(e) => tracing::warn!("failed to launch desktop app '{cmd}': {e}"),
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

    tracing::info!("Starting CarPlay session");

    // Run the CarPlay/iAP2 Bluetooth session as its own task so the runtime keeps polling
    // it continuously. It must keep servicing the iAP2 link (keep-alives, ACKs) throughout
    // WiFi connect and AirPlay setup - if it only advanced when awaited in the loops below,
    // it would be starved for ~10s during screen-sharing setup and the car would reset the
    // Bluetooth link, tearing down CarPlay right as streaming starts.
    // AbortOnDropHandle guarantees the task (and its BlueZ resources) is torn down when this
    // function returns - e.g. before a session restart - rather than detached like a plain
    // JoinHandle.
    let mut carplay_handle = AbortOnDropHandle::new(tokio::spawn(wireless_carplay(
        wifi_tx, ready_tx, bt_mac_tx,
    )));

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

    // Phase 3: connect AirPlay to learn the car's display size, then launch our internal
    // compositor sized to it, point the desktop app(s) at it, and stream its composited
    // frames. Learning the resolution first lets the compositor render at exactly the car's
    // size (no scaling) - and owning the compositor is what makes the desktop appear at all.
    let conn = libcarjack::airplay::connect_airplay(peer_addr, remote_homekit_id).await?;
    let compositor = Compositor::start(conn.width, conn.height)?;
    spawn_desktop_apps(&compositor.socket_name);
    let frame_rx = compositor.subscribe_frames();

    // Encode the compositor's frames to H264 on its own task (so it keeps buffering through
    // AirPlay setup) and stream them to the car. The encoder and the video stream are both
    // session-lifetime, so their arms live directly in the loop below alongside the Bluetooth
    // handle and touch - one place for the whole streaming session to end or error.
    let (video_tx, video_rx) = mpsc::channel::<libcarjack::airplay::EncodedFrame>(64);
    let mut encoder = AbortOnDropHandle::new(tokio::spawn(encode::encode_h264_to_channel(
        frame_rx,
        video_tx,
    )));
    let video = libcarjack::airplay::run_video_stream(conn, video_rx, touch_tx);
    tokio::pin!(video);

    // If the car later drops Bluetooth on its own, that's not fatal - keep streaming.
    let mut carplay_done = false;
    // touch_tx lives on the AirPlay/HID side (run_video_stream), not on carplay_handle, so it's
    // tracked separately - gating it on carplay_done meant a BT drop silently killed touch for
    // the rest of the session even though it kept working fine.
    let mut touch_done = false;

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
            // Race the encoder so a GStreamer failure surfaces as a real error rather than a
            // silently closed video channel (which the stream side only sees as a clean end).
            result = &mut encoder => {
                return match result {
                    Ok(Ok(())) => Err(eyre!("video encoder task ended unexpectedly")),
                    Ok(Err(e)) => Err(eyre!("video encoder failed: {e}")),
                    Err(e) => Err(eyre!("video encoder task panicked: {e}")),
                };
            }
            result = &mut video => {
                // Propagate so the caller restarts the whole flow (e.g. car closed the
                // screen socket).
                result?;
                return Ok(());
            }
            touch = touch_rx.recv(), if !touch_done => {
                match touch {
                    Some(event) => compositor.send_touch(&event)?,
                    None => touch_done = true,
                }
            }
        }
    }
}
