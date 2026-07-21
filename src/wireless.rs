use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use backon::{ConstantBuilder, Retryable};
use bluer::{
    Adapter,
    AdapterEvent::DeviceAdded,
    Device, DiscoveryFilter, Session,
    adv::{Advertisement, Type},
    agent::Agent,
    rfcomm::{self, Role},
};
use eyre::{Result, eyre};
use futures::FutureExt;
use iap2_proto::{config::LinkConfig, error::Iap2Error, iap2::Iap2};
use libcarjack::{
    bluetooth::{CARPLAY_ADV_PAYLOAD, MANUFACTURER_APPLE},
    carplay_session::WifiCredentials,
    uuid::{
        CARPLAY_ACCESSORY_UUID, CARPLAY_DEVICE_UUID, IAP_ACCESSORY_UUID, IAP_DEVICE_UUID, IAP2_UUID,
    },
};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::StreamExt;

use libcarjack::carplay;

pub async fn wireless_carplay(
    wifi_tx: mpsc::Sender<WifiCredentials>,
    ready_tx: oneshot::Sender<()>,
    bt_mac_tx: oneshot::Sender<String>,
    car_addr_tx: oneshot::Sender<String>,
) -> Result<()> {
    let session = Session::new().await?;
    let headless_agent = Agent::default();
    let _agent = session.register_agent(headless_agent).await?;
    let adapter = session.default_adapter().await?;
    adapter.set_alias(String::from("carjack")).await?;
    adapter.set_powered(true).await?;
    // Reuse this adapter's address rather than opening a second concurrent bluer::Session
    // later just to look it up - that stepped on BlueZ's handling of the RFCOMM profile
    // this session already owns and reset the connection.
    let _ = bt_mac_tx.send(adapter.address().await?.to_string());
    let _advertisement_handle = publish_advertisement(&adapter).await?;

    let iap_accessory_handle = session
        .register_profile(rfcomm::Profile {
            uuid: IAP_ACCESSORY_UUID,
            name: Some(String::from("Wireless iAP")),
            role: Some(Role::Client),
            //Make sure we don't auto connect, otherwise it will connect before we're ready to accept a connection
            auto_connect: Some(false),
            ..Default::default()
        })
        .await?;
    let iap_device_handle = session
        .register_profile(rfcomm::Profile {
            uuid: IAP_DEVICE_UUID,
            name: Some(String::from("Wireless iAP")),
            role: Some(Role::Server),
            channel: Some(1),
            ..Default::default()
        })
        .await?;

    let device = pair_available_accessory(&adapter).await?;
    // Surface the car's MAC (known now, early) so the caller can look up its cached display and
    // size the compositor / launch the app before AirPlay reports the display.
    let _ = car_addr_tx.send(device.address().to_string());
    if device.is_connected().await? {
        tracing::info!("Already connected");
        device.disconnect().await?;
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    tracing::debug!("Waiting for iAP2 connection requests");
    let mut profiles = iap_accessory_handle.merge(iap_device_handle);
    let acceptance = tokio::spawn(async move {
        match profiles.next().await {
            Some(rq) => {
                tracing::info!("Got connect request");
                let stream = rq.accept().map_err(|e| std::io::Error::from(e))?;
                Ok((stream, profiles))
            }
            None => Err(Iap2Error::ChannelClosed),
        }
    });

    (|| async {
        tokio::time::timeout(
            Duration::from_secs(3),
            device
                .connect_profile(&IAP_ACCESSORY_UUID)
                .map(|r| match r {
                    Ok(()) => Ok(()),
                    Err(e) if e.kind == bluer::ErrorKind::AlreadyConnected => {
                        tracing::info!("Accessory profile already connected, proceeding");
                        Ok(())
                    }
                    Err(e) => Err(eyre!(e)),
                }),
        )
        .await?
    })
    .retry(
        ConstantBuilder::new()
            .with_delay(Duration::from_millis(500))
            .with_max_times(40),
    )
    .await
    .unwrap_or_else(|e| {
        tracing::warn!("Connecting profile failed with {e}, proceeding anyway.");
    });

    let (stream, _profiles) = match tokio::time::timeout(Duration::from_secs(30), acceptance).await
    {
        Ok(joined) => joined??,
        Err(_) => {
            return Err(eyre!(
                "Timed out waiting for accessory Bluetooth connection"
            ));
        }
    };
    tracing::debug!("Establishing iAP2 link");

    let iap2 = Iap2::start(stream, LinkConfig::default()).await?;

    tracing::debug!("iAP2 link established");
    carplay::run_carplay_session(iap2, wifi_tx, ready_tx).await?;
    tracing::debug!("iAP lifecycle ended");
    Ok(())
}

async fn publish_advertisement(adapter: &Adapter) -> Result<bluer::adv::AdvertisementHandle> {
    let handle = adapter
        .advertise(Advertisement {
            advertisement_type: Type::Peripheral,
            service_uuids: BTreeSet::from([CARPLAY_DEVICE_UUID, IAP2_UUID]),
            manufacturer_data: BTreeMap::from([(MANUFACTURER_APPLE, CARPLAY_ADV_PAYLOAD.to_vec())]),
            ..Default::default()
        })
        .await?;
    Ok(handle)
}

// #[zbus::proxy(
//     interface = "org.bluez.Adapter1",
//     default_service = "org.bluez",
//     default_path = "/org/bluez/hci0"
// )]
// trait Bluez {
//     async fn stop_discovery(&self) -> zbus::Result<()>;
// }

async fn pair_available_accessory(adapter: &Adapter) -> Result<Device> {
    //This ensures we aren't already discovering when we start the process
    // let connection = zbus::Connection::system().await?;
    // let proxy = BluezProxy::new(&connection).await?;
    // proxy.stop_discovery().await?;
    adapter
        .set_discovery_filter(DiscoveryFilter {
            discoverable: true,
            transport: bluer::DiscoveryTransport::BrEdr,
            uuids: [CARPLAY_ACCESSORY_UUID].into(),
            rssi: Some(-100),
            ..DiscoveryFilter::default()
        })
        .await?;

    // An already-paired/connected accessory (e.g. it auto-reconnected from a prior session)
    // never fires a fresh DeviceAdded event, so check the devices BlueZ already knows first.
    for addr in adapter.device_addresses().await? {
        let device = adapter.device(addr)?;
        if is_candidate_accessory(&device).await && try_prepare_accessory(&device).await {
            return Ok(device);
        }
    }

    let mut device_events = adapter.discover_devices_with_changes().await?;
    while let Some(event) = device_events.next().await {
        let DeviceAdded(addr) = event else { continue };
        let device = adapter.device(addr)?;
        if is_candidate_accessory(&device).await && try_prepare_accessory(&device).await {
            return Ok(device);
        }
    }
    Err(eyre!("No more device events"))
}

/// Whether a device looks like our target CarPlay accessory (reachable and name-matching).
async fn is_candidate_accessory(device: &Device) -> bool {
    let reachable = device.rssi().await.ok().flatten().is_some()
        || device.is_connected().await.unwrap_or(false);
    reachable && matches_carplay_device(device).await.unwrap_or(true)
}

/// Log, pair (if needed) and trust the accessory. Returns whether it's ready to use.
async fn try_prepare_accessory(device: &Device) -> bool {
    let alias = device.alias().await.unwrap_or_default();
    let rssi = device.rssi().await.ok().flatten();
    tracing::info!(name = alias, address = ?device.address(), rssi, "Attempting connection");

    if !device.is_paired().await.unwrap_or(false) {
        if let Err(e) = device.pair().await {
            tracing::warn!(?e, "Pairing failed, trying to proceed anyway");
        }
    }
    let _ = device.set_trusted(true).await;
    true
}

async fn matches_carplay_device(device: &Device) -> Result<bool> {
    let carplay_device_name = std::env::var("CARPLAY_DEVICE_NAME")?;
    Ok(device.alias().await? == carplay_device_name)
}
