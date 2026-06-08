use std::sync::Arc;

use bluer::{
    Adapter,
    AdapterEvent::DeviceAdded,
    Address, Device, Session, Uuid,
    agent::Agent,
    gatt,
    rfcomm::{self, ConnectRequest},
};
use eyre::Result;
use gst::glib::JoinHandle;
use iap2_rs::{
    ConnectionConfig, FileTransferConfig, HidConfig, Iap2Config, Iap2Connection, LinkConfig,
    MockMfiProvider, NowPlayingConfig, PowerConfig, connect,
};
use tokio_stream::StreamExt;

const CARPLAY_DEVICE_UUID: Uuid = Uuid::from_u128(0x2D8D2466E14D451C88BC7301ABEA291A);
const CARPLAY_ACCESSORY_UUID: Uuid = Uuid::from_u128(0xEC884348CD4140A29727575D50BF1FD3);
const IAP2_DEVICE_UUID: Uuid = Uuid::from_u128(0x00000000decafadedecadeafdecacafe);
const IAP2_ACCESSORY_UUID: Uuid = Uuid::from_u128(0x00000000decafadedecadeafdecacaff);

pub async fn wireless_carplay() -> Result<()> {
    let session = Session::new().await?;
    let headless_agent = Agent::default();
    let _agent = session.register_agent(headless_agent).await?;
    let adapter = session.default_adapter().await?;
    adapter.set_powered(true).await?;
    // let _ = setup_carplay_profile(&adapter).await?;
    let mut iap_client_handle =
        setup_iap_profile(&session, IAP2_DEVICE_UUID, CARPLAY_DEVICE_UUID, true).await?;
    let mut iap_accessory_handle =
        setup_iap_profile(&session, IAP2_ACCESSORY_UUID, CARPLAY_ACCESSORY_UUID, false).await?;
    discover_devices(&adapter).await?;
    let res = tokio::spawn(async move {
        loop {
            let rq = tokio::select! {
                Some(rq) = iap_client_handle.next() => rq,
                Some(rq) = iap_accessory_handle.next() => rq
            };
            tracing::info!(connection = ?connect_iap2(rq).await);
        }
    });
    res.await?;
    // tracing::info!("Advertising and monitoring");
    // sleep(Duration::from_hours(2)).await;
    Ok(())
}

// async fn setup_carplay_profile(adapter: &Adapter) -> Result<gatt::local::ProfileHandle> {
//     let profile_handle = adapter
//         .register_gatt_profile(bluer::gatt::local::Profile {
//             uuids: [CARPLAY_DEVICE_UUID].into(),
//             ..Default::default()
//         })
//         .await?;
//     Ok(profile_handle)
// }
// async fn advertise(adapter: &Adapter) -> Result<AdvertisementHandle> {
//     let advertisement_handle = adapter
//         .advertise(Advertisement {
//             service_uuids: BTreeSet::from([DEVICE_UUID]),
//             ..Default::default()
//         })
//         .await?;
//     Ok(advertisement_handle)
// }

async fn setup_iap_profile(
    session: &Session,
    uuid: Uuid,
    service: Uuid,
    auto_connect: bool,
) -> Result<rfcomm::ProfileHandle> {
    let profile_handle = session
        .register_profile(rfcomm::Profile {
            uuid,
            name: Some(String::from("iAP2 Wireless")),
            auto_connect: Some(auto_connect),
            require_authentication: Some(false),
            require_authorization: Some(false),
            service: Some(service),
            ..Default::default()
        })
        .await?;

    Ok(profile_handle)
}

async fn discover_devices(adapter: &Adapter) -> Result<()> {
    let mut device_events = adapter.discover_devices().await?;
    // pin_mut!(device_events);
    while let Some(DeviceAdded(addr)) = device_events.next().await {
        let device = adapter.device(addr)?;
        let alias = device.alias().await?;
        println!("Device added: {alias}");
        // tracing::debug!(service_data = ?device.uuids().await?.unwrap_or_default());
        // let carplay_device_name = env::var("CARPLAY_DEVICE_NAME")?;
        if device.rssi().await?.is_some()
            && device
                .uuids()
                .await?
                .unwrap_or_default()
                .contains(&CARPLAY_ACCESSORY_UUID)
        {
            carplay_connect(&device).await?;
        }
    }
    Ok(())
}

async fn carplay_connect(device: &Device) -> Result<()> {
    tracing::info!(
        name = device.alias().await?,
        address = ?device.address(),
        "Attempting connection",
    );
    device.set_trusted(true).await?;
    device.pair().await?;
    tracing::info!("Device paired");
    device.connect_profile(&CARPLAY_ACCESSORY_UUID).await?;
    Ok(())
}

async fn query_all_device_properties(adapter: &Adapter, addr: Address) -> bluer::Result<()> {
    let device = adapter.device(addr)?;
    let props = device.all_properties().await?;
    for prop in props {
        println!("    {:?}", &prop);
    }
    Ok(())
}

async fn connect_iap2(rq: ConnectRequest) -> Result<()> {
    let stream = rq.accept()?;
    let config = Iap2Config {
        identification: iap2_rs::DeviceIdentification {
            name: String::from("Carjack"),
            model_identifier: String::from("cj-dragonq-6a"),
            manufacturer: String::from("Carjack"),
            serial_number: String::new(),
            firmware_version: String::from("0.01"),
            hardware_version: String::from("0.01"),
            ea_protocol_name: String::from("com.carjack.myproto"),
            ..Default::default()
        },
        mfi_provider: Arc::new(MockMfiProvider::empty()),
        enable_now_playing: true,
        enable_hid: true,
        link_config: LinkConfig::default(),
        now_playing_config: NowPlayingConfig::default(),
        hid_config: HidConfig::default(),
        file_transfer_config: FileTransferConfig::default(),
        connection_config: ConnectionConfig::default(),
        power_config: PowerConfig::default(),
    };
    let mut conn = connect(stream, config).await?;
    while let Some(event) = conn.events.recv().await {
        tracing::info!(?event, "Received event!");
    }
    Ok(())
}
