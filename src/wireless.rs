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
use uuid::Uuid;

use libcarjack::carplay;

const IAP_CHANNEL: u16 = 19;


fn generate_iap2_sdp() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" ?>
<record>
    <attribute id="0x0001">
        <sequence>
            <uuid value="{}"/>
        </sequence>
    </attribute>
    <attribute id="0x0004">
        <sequence>
            <sequence>
                <uuid value="0x0100"/>
            </sequence>
            <sequence>
                <uuid value="0x0003"/>
                <uint8 value="{}"/>
            </sequence>
        </sequence>
    </attribute>
    <attribute id="0x0009">
        <sequence>
            <sequence>
                <uuid value="0x1101"/>
                <uint16 value="0x0102"/>
            </sequence>
        </sequence>
    </attribute>
    <attribute id="0x0005">
        <sequence>
            <uuid value="0x1002"/>
        </sequence>
    </attribute>
    <attribute id="0x0008">
        <uint8 value="0xFF"/>
    </attribute>
    <attribute id="0x0100">
        <text value="Wireless iAP"/>
    </attribute>
</record>"#,
        IAP_ACCESSORY_UUID, IAP_CHANNEL
    )
}


const CARPLAY_HCI_CLASS: [u8; 2] = [0x20, 0x04];

async fn apply_carplay_hci_hacks(adapter_index: u16) {
    // Adding the CarPlay UUID to the BR/EDR EIR needs a raw HCI mgmt socket, which needs
    // CAP_NET_ADMIN. We keep that capability OFF the main process (a capability-carrying process
    // is /proc-locked to root under fs.suid_dumpable=2, which stops xdg-desktop-portal from
    // introspecting us in desktop mode) and instead exec a tiny setcap'd helper binary that does
    // just this one thing and exits. Best-effort: warn and continue if it isn't there / fails.
    match run_btmgmt_helper(adapter_index).await {
        Ok(()) => tracing::debug!("Added CarPlay EIR UUID to BR/EDR adapter"),
        Err(e) => tracing::warn!(
            ?e,
            "Failed to add CarPlay EIR UUID (helper unavailable / needs cap_net_admin), continuing"
        ),
    }
}

/// Exec the `carjack-btmgmt` sidecar (found next to our own binary) to add the CarPlay EIR UUID.
/// The sidecar carries `cap_net_admin`; we don't.
async fn run_btmgmt_helper(adapter_index: u16) -> Result<()> {
    let exe = std::env::current_exe()?;
    let helper = exe
        .parent()
        .ok_or_else(|| eyre!("current exe has no parent dir"))?
        .join("carjack-btmgmt");
    let status = tokio::process::Command::new(&helper)
        .arg(adapter_index.to_string())
        .status()
        .await
        .map_err(|e| eyre!("failed to spawn {}: {e}", helper.display()))?;
    if !status.success() {
        return Err(eyre!("carjack-btmgmt exited with {status}"));
    }
    Ok(())

    // // Set Class of Device to match CarPlay accessory (catplay sends [0x20, 0x04], reversed)
    // let mut class = CARPLAY_HCI_CLASS;
    // class.reverse();
    // if let Err(e) = send_mgmt_command(adapter_index, MGMT_OP_SET_DEV_CLASS, &class).await {
    //     tracing::warn!(
    //         ?e,
    //         "Failed to set CarPlay device class (may need root), continuing"
    //     );
    // } else {
    //     tracing::debug!("Set CarPlay device class");
    // }
}

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

    // Apply HCI-level CarPlay hacks so BR/EDR inquiry shows the CarPlay UUID
    let adapter_name = adapter.name().to_string();
    let adapter_index = adapter_name
        .strip_prefix("hci")
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    tracing::trace!("Applying hci hacks");
    apply_carplay_hci_hacks(adapter_index).await;

    let sdp_record = generate_iap2_sdp();
    let iap_accessory_handle = session
        .register_profile(rfcomm::Profile {
            uuid: IAP_ACCESSORY_UUID,
            name: Some(String::from("Wireless iAP")),
            role: Some(Role::Client),
            auto_connect: Some(true),
            require_authentication: Some(true),
            ..Default::default()
        })
        .await?;
    let iap_device_handle = session
        .register_profile(rfcomm::Profile {
            uuid: IAP_DEVICE_UUID,
            name: Some(String::from("Wireless iAP")),
            role: Some(Role::Server),
            channel: Some(IAP_CHANNEL),
            service_record: Some(sdp_record),
            require_authentication: Some(true),
            require_authorization: Some(false),
            ..Default::default()
        })
        .await?;

    // Register Wireless iAP v2 UUID (some head units look for this)
    let _iap_v2_handle = session
        .register_profile(rfcomm::Profile {
            uuid: IAP2_UUID,
            name: Some(String::from("Wireless iAP v2")),
            role: Some(Role::Server),
            channel: Some(IAP_CHANNEL),
            require_authentication: Some(true),
            require_authorization: Some(false),
            ..Default::default()
        })
        .await;

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

    tracing::info!("Looking for compatible devices...");

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
