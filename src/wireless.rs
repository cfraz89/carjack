use std::{sync::Arc, time::Duration};

use bluer::{
    Adapter,
    AdapterEvent::DeviceAdded,
    Address, Device, Session, Uuid,
    agent::Agent,
    gatt::local::Service,
    id::ServiceClass,
    rfcomm::{self, ConnectRequest, Role, SocketAddr},
};
use eyre::{Result, eyre};
use iap2_rs::{
    ConnectionConfig, FileTransferConfig, HidConfig, Iap2Config, Iap2Connection, LinkConfig,
    MockMfiProvider, NowPlayingConfig, PowerConfig, connect,
};
use tokio::time::sleep;
use tokio_stream::StreamExt;

const CARPLAY_DEVICE_UUID: Uuid = Uuid::from_u128(0x2D8D2466E14D451C88BC7301ABEA291A);
const CARPLAY_ACCESSORY_UUID: Uuid = Uuid::from_u128(0xEC884348CD4140A29727575D50BF1FD3);
const IAP2_DEVICE_UUID: Uuid = Uuid::from_u128(0x00000000decafadedecadeafdecacafe);
const IAP2_ACCESSORY_UUID: Uuid = Uuid::from_u128(0x00000000decafadedecadeafdecacaff);
// const IAP2_ACCESSORY_UUID: Uuid = Uuid::from_u128(0xffcacadeafdecadedefacade00000000);
pub async fn wireless_carplay() -> Result<()> {
    let session = Session::new().await?;
    let headless_agent = Agent::default();
    let _agent = session.register_agent(headless_agent).await?;
    let adapter = session.default_adapter().await?;
    adapter.set_alias(String::from("carjack")).await?;
    adapter.set_powered(true).await?;
    let mut iap_device_handle = setup_iap_profile(
        &session,
        String::from("Wireless iAP device"),
        IAP2_DEVICE_UUID,
        Role::Server,
        false,
    )
    .await?;
    let mut iap_accessory_handle = setup_iap_profile(
        &session,
        String::from("Wireless iAP accessory"),
        IAP2_ACCESSORY_UUID,
        Role::Client,
        true,
    )
    .await?;
    let device = discover_devices(&adapter).await?;
    tracing::info!("Waiting for iap connection...");
    tokio::spawn(async move {
        loop {
            let rq = tokio::select! {
                Some(rq) = iap_device_handle.next() => rq,
                Some(rq) = iap_accessory_handle.next() => rq
            };
            match async {
                let stream = rq.accept()?;
                connect_iap2(stream).await
            }
            .await
            {
                Ok(()) => tracing::info!("iAP2 connected"),
                Err(e) => tracing::error!(?e),
            }
        }
        // while let Some(rq) = iap_accessory_handle.next().await {
        //tracing::debug!(?rq, "Got connect request");
        //let stream = rq.accept()?;
        //     tracing::info!(?rq);
        //     let connection = connect_iap2(rq).await;
        //     tracing::info!(?connection);
        // }
        // let socket = rfcomm::Socket::new()?;
        // let stream = socket.connect(SocketAddr::new(device.address(), 1)).await?;
        // connect_iap2(stream).await?;
        // Result::<()>::Ok(())
    })
    .await?;
    Ok(())
}

async fn setup_iap_profile(
    session: &Session,
    name: String,
    uuid: Uuid,
    role: rfcomm::Role,
    auto_connect: bool,
) -> Result<rfcomm::ProfileHandle> {
    let profile_handle = session
        .register_profile(rfcomm::Profile {
            uuid,
            name: Some(name),
            auto_connect: Some(auto_connect),
            role: Some(role),
            require_authentication: Some(true),
            require_authorization: Some(true),
            ..Default::default()
        })
        .await?;

    Ok(profile_handle)
}

async fn discover_devices(adapter: &Adapter) -> Result<Device> {
    let mut device_events = adapter.discover_devices().await?;
    // pin_mut!(device_events);
    while let Some(DeviceAdded(addr)) = device_events.next().await {
        let device = adapter.device(addr)?;
        let alias = device.alias().await?;
        let rssi = device.rssi().await?;
        tracing::info!(alias, rssi, "Device added");
        // let carplay_device_name = env::var("CARPLAY_DEVICE_NAME")?;
        if rssi.is_some()
            && device
                .uuids()
                .await?
                .unwrap_or_default()
                .contains(&CARPLAY_ACCESSORY_UUID)
        {
            carplay_connect(&device).await?;
            return Ok(device);
        }
    }
    Err(eyre!("No more device events"))
}

async fn carplay_connect(device: &Device) -> Result<()> {
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
    tracing::info!("Device paired");
    query_all_device_properties(device).await?;
    if !device.is_connected().await? {
        device.connect().await?;
    }
    tracing::info!(connected = device.is_connected().await?);
    Ok(())
}

async fn query_all_device_properties(device: &Device) -> bluer::Result<()> {
    let props = device.all_properties().await?;
    for prop in props {
        println!("    {:?}", &prop);
    }
    Ok(())
}

async fn connect_iap2(stream: rfcomm::Stream) -> Result<()> {
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
