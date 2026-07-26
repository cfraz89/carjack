use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    time::Duration,
};
use thiserror::Error;

use backon::{ConstantBuilder, RetryableWithContext};
use bluer::{
    Adapter,
    AdapterEvent::DeviceAdded,
    Address, Device, DiscoveryFilter, Session,
    adv::{Advertisement, Type},
    agent::Agent,
    rfcomm::{self, ProfileHandle, Role},
};
use eyre::{Result, eyre};
use iap2_proto::{error::Iap2Error, iap2::Iap2};
use libcarjack::carplay;
use libcarjack::{
    bluetooth::{CARPLAY_ADV_PAYLOAD, MANUFACTURER_APPLE},
    carplay_session::{DeviceIdentity, WifiCredentials},
    uuid::{CARPLAY_DEVICE_UUID, IAP_ACCESSORY_UUID, IAP_DEVICE_UUID, IAP2_UUID},
};
use tap::{Tap, TapFallible};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::{StreamExt, adapters::Merge};
use tokio_util::task::AbortOnDropHandle;

const IAP_CHANNEL: u16 = 1;

/// How long to wait for the configured head unit to show up in discovery before giving up on this
/// session. The caller retries, so this only bounds a single attempt.
const DISCOVERY_WAIT: Duration = Duration::from_secs(60);

/// Pause before re-arming the accept after a transient failure, so a persistently failing
/// `ConnectRequest::accept` can't turn the wait loop into a busy spin.
const ACCEPT_RETRY_DELAY: Duration = Duration::from_secs(1);

// fn generate_iap2_sdp() -> String {
//     format!(
//         r#"<?xml version="1.0" encoding="UTF-8" ?>
// <record>
//     <attribute id="0x0001">
//         <sequence>
//             <uuid value="{}"/>
//         </sequence>
//     </attribute>
//     <attribute id="0x0004">
//         <sequence>
//             <sequence>
//                 <uuid value="0x0100"/>
//             </sequence>
//             <sequence>
//                 <uuid value="0x0003"/>
//                 <uint8 value="{}"/>
//             </sequence>
//         </sequence>
//     </attribute>
//     <attribute id="0x0009">
//         <sequence>
//             <sequence>
//                 <uuid value="0x1101"/>
//                 <uint16 value="0x0102"/>
//             </sequence>
//         </sequence>
//     </attribute>
//     <attribute id="0x0005">
//         <sequence>
//             <uuid value="0x1002"/>
//         </sequence>
//     </attribute>
//     <attribute id="0x0008">
//         <uint8 value="0xFF"/>
//     </attribute>
//     <attribute id="0x0100">
//         <text value="Wireless iAP"/>
//     </attribute>
// </record>"#,
//         IAP_ACCESSORY_UUID, IAP_CHANNEL
//     )
// }

fn bin_path() -> Option<OsString> {
    let current_exe = std::env::current_exe()
        .tap_err(|e| tracing::warn!("Unable to get path of current binary: {e}"))
        .ok()?;
    let path = std::env::var_os("PATH")?;
    let paths = std::env::split_paths(&path)
        .chain(std::iter::once(current_exe.parent()?.to_path_buf()))
        .tap(|i| tracing::trace!(paths = ?i, "paths"));
    std::env::join_paths(paths)
        .tap_err(|e| tracing::warn!("Unable to join paths: {e}"))
        .ok()
}

/// Exec the `carjack-hci-helper` sidecar (found next to our own binary) to apply the BR/EDR mgmt
/// tweaks (add the CarPlay EIR UUID and set an iPhone-like Class of Device). The sidecar carries
/// `cap_net_admin`; we don't. It resolves the adapter itself via bluer's default adapter.
async fn run_hci_helper() -> Result<()> {
    let status = tokio::process::Command::new("carjack-hci-helper")
        .env("PATH", bin_path().unwrap_or_default())
        .status()
        .await?;
    if !status.success() {
        return Err(eyre!("carjack-hci-helper exited with {status}"));
    }
    Ok(())
}

/// Carried across retry attempts by `backon`: the merged profile streams we accept inbound
/// connections from, and the single outbound page (dropped, and so cancelled, once any link lands).
struct LinkCtx {
    profiles: Merge<ProfileHandle, ProfileHandle>,
    dial: AbortOnDropHandle<()>,
    device: Device,
}

/// Why an attempt to bring the iAP2 link up failed. Drives the `.when` retry predicate.
#[derive(Error, Debug)]
enum LinkAttemptError {
    /// The profile stream ended: BlueZ has taken our registrations away, so no further connection
    /// can ever arrive.
    #[error("iAP2 profiles stream closed")]
    ProfilesClosed,
    #[error("iAP2 error: {0:?}")]
    Iap2(#[from] Iap2Error),
    #[error("Bluer error: {0:?}")]
    Error(#[from] bluer::Error),
}

/// Wait for an inbound iAP2 profile connection, accept it, and bring the iAP2 link up on it.
async fn attempt_iap2_link(
    ctx: &mut LinkCtx,
) -> Result<Iap2<bluer::rfcomm::Stream>, LinkAttemptError> {
    tracing::debug!("Waiting for iAP2 connection requests");
    let rq = ctx
        .profiles
        .next()
        .await
        .ok_or(LinkAttemptError::ProfilesClosed)?;
    ctx.dial.abort();
    tracing::debug!("Got connect request");
    let stream = rq.accept()?;
    tracing::debug!("Establishing iAP2 link");
    let iap2 = Iap2::start(stream).await?;
    Ok(iap2)
}

pub async fn wireless_carplay(
    target: String,
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
    let identity = DeviceIdentity {
        name: String::from("carjack"),
        transport_identifier: adapter.address().await?.to_string(),
        wifi_mac: crate::wifi_connect::local_wifi_mac().unwrap_or_else(|| {
            tracing::warn!("No wireless interface found; reporting an empty WiFi MAC to the car");
            String::new()
        }),
    };
    // Reuse this adapter's address rather than opening a second concurrent bluer::Session
    // later just to look it up - that stepped on BlueZ's handling of the RFCOMM profile
    // this session already owns and reset the connection.
    let _ = bt_mac_tx.send(adapter.address().await?.to_string());
    let _advertisement_handle = publish_advertisement(&adapter).await?;

    // Apply HCI-level CarPlay hacks so BR/EDR inquiry shows the CarPlay UUID
    tracing::trace!("Applying hci hacks");
    //We need cap_net_admin to write to the hci socket directly.
    //However adding that cap to this process is probably a security liability, and breaks
    //xdg-desktop-portal. So we offload the work to a separate binary.
    let _ = run_hci_helper().await;

    // let sdp_record = generate_iap2_sdp();
    let iap_accessory_handle = session
        .register_profile(rfcomm::Profile {
            uuid: IAP_ACCESSORY_UUID,
            name: Some(String::from("Wireless iAP")),
            role: Some(Role::Client),
            auto_connect: Some(false),
            require_authentication: Some(true),
            require_authorization: Some(false),
            ..Default::default()
        })
        .await?;
    let iap_device_handle = session
        .register_profile(rfcomm::Profile {
            uuid: IAP_DEVICE_UUID,
            name: Some(String::from("Wireless iAP")),
            role: Some(Role::Server),
            channel: Some(IAP_CHANNEL),
            // service_record: Some(sdp_record),
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

    let device = find_target_device(&adapter, &target).await?;
    // Surface the car's MAC (known now, early) so the caller can look up its cached display and
    // size the compositor / launch the app before AirPlay reports the display.
    let _ = car_addr_tx.send(device.address().to_string());
    if device.is_connected().await? {
        tracing::info!("Already connected");
        device.disconnect().await?;
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    let profiles = iap_accessory_handle.merge(iap_device_handle);

    // Page the car exactly once, then leave the radio alone. Some head units only ever run iAP2
    // over an ACL link they originated (the CFMOTO bike: when our page wins, it opens RFCOMM ch 1
    // on our link and DISCs without ever sending `FF 55` - `cfmoto_6.pcapng`), while others won't
    // page us for a long time, if at all. A single attempt serves both: it still establishes the
    // first bond, but unlike the old unbounded retry it doesn't keep the controller paging and
    // starve our own page scan, so the car can reach us right after it fails.
    let dialled = device.clone();
    let accessory_profile_connect_task = AbortOnDropHandle::new(tokio::spawn(async move {
        match dialled.connect_profile(&IAP_ACCESSORY_UUID).await {
            Ok(()) => tracing::debug!("Outbound accessory profile connect accepted"),
            // Not fatal, and not worth retrying: the inbound path is still live, and the car may
            // simply not be pageable yet. Whichever side lands a connection first wins.
            Err(e) => tracing::info!(
                e = ?e,
                "Outbound accessory profile connect failed, waiting for the head unit to page us"
            ),
        }
    }));

    //Retry our link unless our profiles start to return None.
    let iap2: Iap2<rfcomm::Stream> = (|mut ctx: LinkCtx| async move {
        let res = attempt_iap2_link(&mut ctx).await;
        if let Err(LinkAttemptError::Iap2(e)) = &res {
            tracing::warn!(?e, "Got iap2 error bringing up link, disconnecting");
            let _ = &ctx.device.disconnect().await;
        }
        (ctx, res)
    })
    .retry(
        ConstantBuilder::new()
            .with_delay(ACCEPT_RETRY_DELAY)
            .without_max_times(),
    )
    .context(LinkCtx {
        profiles,
        dial: accessory_profile_connect_task,
        device,
    })
    .when(|e| !matches!(e, LinkAttemptError::ProfilesClosed))
    .await
    .1?;
    tracing::debug!("iAP2 link established");
    carplay::run_carplay_session(iap2, wifi_tx, ready_tx, identity).await?;
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

/// Resolve the configured accesory and get it ready to connect.
async fn find_target_device(adapter: &Adapter, target: &str) -> Result<Device> {
    let wanted: Option<Address> = target.parse().ok();

    // Whatever BlueZ already knows about (bonded from a previous run, or currently connected).
    for addr in adapter.device_addresses().await? {
        let device = adapter.device(addr)?;
        if device_matches(&device, target, wanted).await {
            tracing::info!(%target, ?addr, "Found configured head unit in BlueZ's device list");
            return prepare_device(device).await;
        }
    }

    // Otherwise wait for it to show up. This is discovery aimed at one known target, not a survey:
    // every device that isn't the configured one is ignored outright.
    adapter
        .set_discovery_filter(DiscoveryFilter {
            discoverable: true,
            transport: bluer::DiscoveryTransport::BrEdr,
            rssi: Some(-100),
            ..DiscoveryFilter::default()
        })
        .await?;
    tracing::info!(%target, "Waiting for the configured head unit to appear");

    let mut device_events = adapter.discover_devices_with_changes().await?;
    let found = tokio::time::timeout(DISCOVERY_WAIT, async {
        while let Some(event) = device_events.next().await {
            let DeviceAdded(addr) = event else { continue };
            let device = adapter.device(addr)?;
            if device_matches(&device, target, wanted).await {
                return Ok(device);
            }
        }
        Err(eyre!(
            "Bluetooth discovery ended without finding '{target}'"
        ))
    })
    .await
    .map_err(|_| eyre!("Timed out waiting for Bluetooth device '{target}'"))??;

    tracing::info!(%target, address = ?found.address(), "Discovered configured head unit");
    prepare_device(found).await
}

/// Whether this device is the configured target, by address if the selector parsed as a MAC,
/// otherwise by advertised name.
async fn device_matches(device: &Device, target: &str, wanted: Option<Address>) -> bool {
    match wanted {
        Some(addr) => device.address() == addr,
        None => device.alias().await.map(|a| a == target).unwrap_or(false),
    }
}

/// Pair (if needed) and trust the target, then hand it back.
async fn prepare_device(device: Device) -> Result<Device> {
    let alias = device.alias().await.unwrap_or_default();
    let rssi = device.rssi().await.ok().flatten();
    let class = device.class().await.ok().flatten();
    let was_bonded = device.is_paired().await.unwrap_or(false);
    tracing::info!(
        name = alias,
        address = ?device.address(),
        rssi,
        class = class.map(|c| format!("{c:#08x}")),
        was_bonded,
        "Preparing configured head unit"
    );

    if !was_bonded {
        // Not fatal: BlueZ often completes bonding as part of the profile connect that follows.
        if let Err(e) = device.pair().await {
            tracing::warn!(?e, "Pairing failed, continuing to the connection attempt");
        }
    }
    let _ = device.set_trusted(true).await;
    Ok(device)
}
