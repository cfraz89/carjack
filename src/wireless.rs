use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use bluer::{
    Adapter,
    AdapterEvent::DeviceAdded,
    Device, DiscoveryFilter, Session,
    adv::{Advertisement, Type},
    agent::Agent,
    rfcomm::{self, Role},
};
use eyre::{Result, eyre};
use libcarjack::{
    bluetooth::{CARPLAY_ADV_PAYLOAD, MANUFACTURER_APPLE},
    carplay_session::TouchEvent,
    uuid::{
        CARPLAY_ACCESSORY_UUID, CARPLAY_DEVICE_UUID, IAP_ACCESSORY_UUID, IAP_DEVICE_UUID, IAP2_UUID,
    },
};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::StreamExt;

use crate::carplay;

pub async fn wireless_carplay(
    video_rx: mpsc::Receiver<Vec<u8>>,
    touch_tx: mpsc::Sender<TouchEvent>,
    ready_tx: oneshot::Sender<()>,
) -> Result<()> {
    let session = Session::new().await?;
    let headless_agent = Agent::default();
    let _agent = session.register_agent(headless_agent).await?;
    let adapter = session.default_adapter().await?;
    adapter.set_alias(String::from("carjack")).await?;
    adapter.set_powered(true).await?;
    let _advertisement_handle = publish_advertisement(&adapter).await?;

    let iap_accessory_handle = session
        .register_profile(rfcomm::Profile {
            uuid: IAP_ACCESSORY_UUID,
            name: Some(String::from("Wireless iAP")),
            auto_connect: Some(true),
            role: Some(Role::Client),
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
    if device.is_connected().await? {
        tracing::info!("Already connected");
        device.disconnect().await?;
    }
    tracing::info!("Connecting...");
    device.connect().await?;
    tokio::time::sleep(Duration::from_secs(1)).await;

    tracing::debug!("Waiting for iAP2 connection requests");

    let mut profiles = iap_accessory_handle.merge(iap_device_handle);
    if let Some(rq) = profiles.next().await {
        let stream = rq.accept()?;
        carplay::run_carplay_session(stream, video_rx, touch_tx, ready_tx).await?;
        tracing::debug!("iAP lifecycle ended");
    }
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

async fn pair_available_accessory(adapter: &Adapter) -> Result<Device> {
    adapter
        .set_discovery_filter(DiscoveryFilter {
            discoverable: true,
            transport: bluer::DiscoveryTransport::BrEdr,
            uuids: [CARPLAY_ACCESSORY_UUID].into(),
            rssi: Some(-100),
            ..DiscoveryFilter::default()
        })
        .await?;

    let mut device_events = adapter.discover_devices_with_changes().await?;
    while let Some(DeviceAdded(addr)) = device_events.next().await {
        let device = adapter.device(addr)?;
        if (device.rssi().await?.is_some() || device.is_connected().await?)
            && matches_carplay_device(&device).await.unwrap_or(true)
        {
            tracing::info!(
                name = device.alias().await?,
                address = ?device.address(),
                rssi = device.rssi().await?,
                "Attempting connection",
            );
            if !device.is_paired().await? {
                device.pair().await?;
            }
            device.set_trusted(false).await?;
            return Ok(device);
        }
    }
    Err(eyre!("No more device events"))
}

async fn matches_carplay_device(device: &Device) -> Result<bool> {
    let carplay_device_name = std::env::var("CARPLAY_DEVICE_NAME")?;
    Ok(device.alias().await? == carplay_device_name)
}
