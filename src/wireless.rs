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
use iap2_proto::{config::LinkConfig, iap2::Iap2};
use libcarjack::{
    bluetooth::{CARPLAY_ADV_PAYLOAD, MANUFACTURER_APPLE},
    uuid::{
        CARPLAY_ACCESSORY_UUID, CARPLAY_DEVICE_UUID, IAP_ACCESSORY_UUID, IAP_DEVICE_UUID, IAP2_UUID,
    },
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

    let iap_accessory_handle = session
        .register_profile(rfcomm::Profile {
            uuid: IAP_ACCESSORY_UUID,
            name: Some(String::from("Wireless iAP")),
            auto_connect: Some(true),
            // channel: Some(rfcomm_channel),
            role: Some(Role::Client),
            ..Default::default()
        })
        .await?;
    let iap_device_handle = session
        .register_profile(rfcomm::Profile {
            uuid: IAP_DEVICE_UUID,
            name: Some(String::from("Wireless iAP")),
            //This is important, at least on my test device. Even though we dont connect through this profile,
            // the test device detects our machine as an android phone without the channel set to 1,
            // ie the channel its using on the accessory profile.
            role: Some(Role::Server),
            channel: Some(1),
            ..Default::default()
        })
        .await?;
    let device = pair_available_accessory(&adapter).await?;
    // let mut iap2_handle = session
    //     .register_profile(rfcomm::Profile {
    //         name: Some(String::from("Wireless iAP v2")),
    //         uuid: IAP2_UUID,
    //         channel: Some(rfcomm_channel),
    //         ..Default::default()
    //     })
    //     .await?;
    //Need this on just to add the carplay uuid to eir data
    // let carplay_handle = session
    //     .register_profile(rfcomm::Profile {
    //         name: Some(String::from("Carplay")),
    //         uuid: CARPLAY_DEVICE_UUID,
    //         ..Default::default()
    //     })
    //     .await?;
    if device.is_connected().await? {
        tracing::info!("Already connected");
        device.disconnect().await?;
    }
    //} else {
    tracing::info!("Connecting...");
    device.connect().await?;
    tokio::time::sleep(Duration::from_secs(1)).await;
    //};

    tracing::debug!("Waiting for iAP2 connection requests");

    let mut profiles = iap_accessory_handle.merge(iap_device_handle);
    while let Some(rq) = profiles.next().await {
        let stream = rq.accept()?;
        tracing::debug!("Establishing iAP2 link");
        let iap2 = Iap2::start(stream, LinkConfig::default()).await?;
        tracing::debug!("iAP2 link established");
        let mut packet_stream = iap2.packet_stream.lock().await;
        while let Some(result) = packet_stream.next().await {
            match result {
                Ok(packet) => tracing::debug!(?packet, "Received packet"),
                Err(e) => tracing::warn!(?e, "Error reading packet"),
            }
        }
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
    // pin_mut!(device_events);
    while let Some(DeviceAdded(addr)) = device_events.next().await {
        let device = adapter.device(addr)?;
        let alias = device.alias().await?;
        let rssi = device.rssi().await?;
        tracing::info!(alias, rssi, "Device added");
        if (rssi.is_some() || device.is_connected().await?)
        //If we cant match specific name, allow connection attempt, since we already matched carplay uuid in discovery filter
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

//If we have an env with CARPLAY_DEVICE_NAME, only match that, otherwise attempt anything with the carplay uuid
// async fn matches_carplay_device(device: &Device) -> Result<bool> {
//     match std::env::var("CARPLAY_DEVICE_NAME") {
//         Ok(carplay_device_name) => {
//             let alias = device.alias().await?;
//             let matches_carplay_alias = alias.eq_ignore_ascii_case(&carplay_device_name);
//             Ok(matches_carplay_alias)
//         }
//         Err(_) => {
//             let matches_carplay_uuid = device
//                 .uuids()
//                 .await?
//                 .unwrap_or_default()
//                 .contains(&CARPLAY_ACCESSORY_UUID);
//             Ok(matches_carplay_uuid)
//         }
//     }
// }

async fn matches_carplay_device(device: &Device) -> Result<bool> {
    let carplay_device_name = std::env::var("CARPLAY_DEVICE_NAME")?;
    return Ok(device.alias().await? == carplay_device_name);
}
