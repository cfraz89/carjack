use std::collections::{BTreeMap, BTreeSet};

use crate::iap2::stream::Iap2;
use bluer::{
    Adapter,
    AdapterEvent::DeviceAdded,
    Device, Session, Uuid,
    adv::{Advertisement, Type},
    agent::Agent,
    rfcomm::{self, Role},
};
use eyre::{Result, eyre};
use libcarjack::{
    bluetooth::{CARPLAY_ADV_PAYLOAD, MANUFACTURER_APPLE},
    uuid::{CARPLAY_ACCESSORY_UUID, CARPLAY_DEVICE_UUID, IAP_DEVICE_UUID, IAP2_UUID},
};
use tokio_stream::StreamExt;

pub async fn wireless_carplay() -> Result<()> {
    let session = Session::new().await?;
    let headless_agent = Agent::default();
    let _agent = session.register_agent(headless_agent).await?;
    let adapter = session.default_adapter().await?;
    adapter.set_alias(String::from("carjack")).await?;
    adapter.set_powered(true).await?;
    let _advertisement_handle = publish_advertisement(&adapter).await?;

    let mut iap2_handle = setup_iap_profile(
        &session,
        String::from("Wireless iAP v2"),
        IAP2_UUID,
        None,
        true,
    )
    .await?;
    let _iap_device_handle = setup_iap_profile(
        &session,
        String::from("Wireless iAP"),
        IAP_DEVICE_UUID,
        None,
        false,
    )
    .await?;
    //Need this on just to add the carplay uuid to eir data
    let _carplay_handle = setup_iap_profile(
        &session,
        String::from("Carplay"),
        CARPLAY_DEVICE_UUID,
        Some(Role::Server),
        false,
    )
    .await?;
    let device = pair_available_accessory(&adapter).await?;
    if device.is_connected().await? {
        tracing::info!("Already connected")
    } else {
        tracing::info!("Waiting for connection...");
        device.connect().await?;
    }

    while let Some(rq) = iap2_handle.next().await {
        tracing::debug!(?rq, "Got connection request");
        let stream = rq.accept()?;
        tracing::debug!("Establishing iAP2 link");
        let mut iap2 = Iap2::start(stream).await?;
        tracing::debug!("iAP2 link established");
        let _handle = tokio::spawn(async move {
            while let Some(packet) = iap2.next().await {
                tracing::debug!(?packet, "Received packet");
            }
            tracing::debug!("iAP lifecycle ended");
        });
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

async fn setup_iap_profile(
    session: &Session,
    name: String,
    uuid: Uuid,
    role: Option<rfcomm::Role>,
    auto_connect: bool,
) -> Result<rfcomm::ProfileHandle> {
    let profile_handle = session
        .register_profile(rfcomm::Profile {
            uuid,
            name: Some(name),
            auto_connect: Some(auto_connect),
            role,
            ..Default::default()
        })
        .await?;

    Ok(profile_handle)
}

async fn pair_available_accessory(adapter: &Adapter) -> Result<Device> {
    let mut device_events = adapter.discover_devices().await?;
    // pin_mut!(device_events);
    while let Some(DeviceAdded(addr)) = device_events.next().await {
        let device = adapter.device(addr)?;
        let alias = device.alias().await?;
        let rssi = device.rssi().await?;
        tracing::info!(alias, rssi, "Device added");
        if matches_carplay_device(&device).await.unwrap_or_default() {
            tracing::info!(
                name = device.alias().await?,
                address = ?device.address(),
                rssi = device.rssi().await?,
                "Attempting connection",
            );
            if !device.is_paired().await? {
                device.pair().await?;
            }
            device.set_trusted(true).await?;
            return Ok(device);
        }
    }
    Err(eyre!("No more device events"))
}

async fn matches_carplay_device(device: &Device) -> Result<bool> {
    match std::env::var("CARPLAY_DEVICE_NAME") {
        Ok(carplay_device_name) => {
            let alias = device.alias().await?;
            let matches_carplay_alias = alias.eq_ignore_ascii_case(&carplay_device_name);
            Ok(matches_carplay_alias)
        }
        Err(_) => {
            let matches_carplay_uuid = device
                .uuids()
                .await?
                .unwrap_or_default()
                .contains(&CARPLAY_ACCESSORY_UUID);
            Ok(matches_carplay_uuid)
        }
    }
}
