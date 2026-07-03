use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::iap2::{config::LinkConfig, stream::Iap2};
use bluer::{
    Adapter,
    AdapterEvent::DeviceAdded,
    Device, DiscoveryFilter, Session, Uuid,
    adv::{Advertisement, Type},
    agent::Agent,
    rfcomm::{self, Role},
};
use eyre::{Result, eyre};
use libcarjack::{
    bluetooth::{CARPLAY_ADV_PAYLOAD, MANUFACTURER_APPLE},
    uuid::{
        CARPLAY_ACCESSORY_UUID, CARPLAY_DEVICE_UUID, IAP_ACCESSORY_UUID, IAP_DEVICE_UUID, IAP2_UUID,
    },
};
use tokio_stream::{StreamExt, StreamMap};

pub async fn wireless_carplay() -> Result<()> {
    let session = Session::new().await?;
    let headless_agent = Agent::default();
    let _agent = session.register_agent(headless_agent).await?;
    let adapter = session.default_adapter().await?;
    adapter.set_alias(String::from("carjack")).await?;
    adapter.set_powered(true).await?;
    let _advertisement_handle = publish_advertisement(&adapter).await?;

    // let mut iap2_handle = setup_iap_profile(
    //     &session,
    //     String::from("Wireless iAP v2"),
    //     IAP2_UUID,
    //     None,
    //     false,
    // )
    // .await?;
    let iap_device_handle = setup_iap_profile(
        &session,
        String::from("Wireless iAP"),
        IAP_DEVICE_UUID,
        None,
        false,
    )
    .await?;
    let iap_accessory_handle = setup_iap_profile(
        &session,
        String::from("Wireless iAP"),
        IAP_ACCESSORY_UUID,
        None,
        true,
    )
    .await?;
    //Need this on just to add the carplay uuid to eir data
    // let mut carplay_handle = setup_iap_profile(
    //     &session,
    //     String::from("Carplay"),
    //     CARPLAY_DEVICE_UUID,
    //     None,
    //     false,
    // )
    // .await?;
    let device = pair_available_accessory(&adapter).await?;
    if device.is_connected().await? {
        tracing::info!("Already connected, reconnecting");
        device.disconnect().await?;
    } else {
        tracing::info!("Connecting...");
        device.connect().await?;
    }

    tracing::debug!("Waiting for iAP2 connection requests");
    let mut connections = StreamMap::new();
    connections.insert("iap_device", iap_device_handle);
    connections.insert("iap_accessory", iap_accessory_handle);

    while let Some((k, rq)) = connections.next().await {
        tracing::debug!(?k, ?rq, "Got connection request");
        let (rx, tx) = rq.accept()?.into_split();
        tracing::debug!("Establishing iAP2 link");
        let mut iap2 = Iap2::start(rx, tx, LinkConfig::default()).await?;
        tracing::debug!("iAP2 link established");
        let _handle = tokio::spawn(async move {
            while let Some(packet) = iap2.next().await {
                tracing::debug!(?packet, "Received packet");
            }
            tracing::debug!("iAP lifecycle ended");
        });
    }
    tracing::debug!("No more iAP connections");

    Ok(())
}

async fn publish_advertisement(adapter: &Adapter) -> Result<bluer::adv::AdvertisementHandle> {
    let handle = adapter
        .advertise(Advertisement {
            advertisement_type: Type::Peripheral,
            service_uuids: BTreeSet::from([CARPLAY_DEVICE_UUID, IAP2_UUID]),
            manufacturer_data: BTreeMap::from([
                (MANUFACTURER_APPLE, CARPLAY_ADV_PAYLOAD.to_vec()),
                (MANUFACTURER_APPLE, vec![0x02, 0x00, 0x00, 0x00]),
            ]),
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
