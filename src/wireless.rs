use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};
use tap::TapFallible;
use thiserror::Error;

use backon::{ConstantBuilder, RetryableWithContext};
use bluer::{
    Adapter,
    AdapterEvent::DeviceAdded,
    Address, Device, DiscoveryFilter, Session,
    adv::{Advertisement, Type},
    agent::Agent,
    rfcomm::{self, ProfileHandle, ReqError, Role},
};
use eyre::{Result, eyre};
use iap2_proto::{error::Iap2Error, iap2::Iap2};
use libcarjack::carplay;
use libcarjack::{
    bluetooth::{CARPLAY_ADV_PAYLOAD, MANUFACTURER_APPLE},
    carplay_session::{DeviceIdentity, WifiCredentials},
    uuid::{CARPLAY_DEVICE_UUID, IAP_DEVICE_UUID, IAP2_UUID},
};
use tokio::sync::{Mutex, Semaphore, mpsc, oneshot};
use tokio_stream::StreamExt;

use crate::wifi_connect::get_wifi_mac;

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
/// BlueZ session, pairing agent, CarPlay advertisement and iAP profile registrations.
///
/// Must outlive every session - dropping it deregisters the profiles.
pub struct IapHost {
    adapter: Adapter,
    identity: DeviceIdentity,
    /// Held only to keep the registrations alive for the process lifetime.
    _session: Session,
    /// The iAP profile the head unit connects in to; a session takes exclusive use of its stream.
    /// Registered at startup, not per target: a unit runs its SDP search for it while bonding.
    iap_profile: Mutex<ProfileHandle>,
    _agent: bluer::agent::AgentHandle,
    _advertisement: bluer::adv::AdvertisementHandle,
    // Held only to keep the registration alive; nothing consumes its request stream.
    // _iap_v2: Option<ProfileHandle>,
}

/// The one iAP profile. The head unit connects in to it, and finding this record in our SDP is
/// what makes it speak iAP rather than Android Auto.
fn device_profile() -> rfcomm::Profile {
    rfcomm::Profile {
        uuid: IAP_DEVICE_UUID,
        name: Some(String::from("Wireless iAP")),
        role: Some(Role::Server),
        channel: Some(IAP_CHANNEL),
        require_authentication: Some(true),
        require_authorization: Some(false),
        ..Default::default()
    }
}

/// The 24-character device identifier the accessory expects in `RequestAccessoryWiFiConfiguration`
/// (an iPhone sends its UDID there, not an address). Derived from the adapter address so it is
/// stable for a given machine.
fn transport_identifier(addr: bluer::Address) -> String {
    let hex: String = addr.iter().map(|b| format!("{b:02X}")).collect();
    format!("{hex:0>24}")
}

impl IapHost {
    /// Register everything once.
    pub async fn start() -> Result<Self> {
        let session = Session::new().await?;
        let agent = session.register_agent(Agent::default()).await?;
        let adapter = session.default_adapter().await?;
        adapter.set_alias(String::from("carjack")).await?;
        adapter.set_powered(true).await?;
        let identity = DeviceIdentity {
            name: String::from("carjack"),
            transport_identifier: transport_identifier(adapter.address().await?),
            wifi_mac: get_wifi_mac().await?,
        };
        let advertisement = publish_advertisement(&adapter).await?;

        // Some head units look for the v2 UUID. Shares `IAP_CHANNEL`, so BlueZ may refuse it.
        // let iap_v2 = match session
        //     .register_profile(rfcomm::Profile {
        //         uuid: IAP2_UUID,
        //         name: Some(String::from("Wireless iAP v2")),
        //         role: Some(Role::Server),
        //         channel: Some(IAP_CHANNEL),
        //         require_authentication: Some(true),
        //         require_authorization: Some(false),
        //         ..Default::default()
        //     })
        //     .await
        // {
        //     Ok(handle) => Some(handle),
        //     Err(e) => {
        //         tracing::debug!(?e, "iAP v2 profile not registered (shares a channel)");
        //         None
        //     }
        // };

        let iap_profile = Mutex::new(session.register_profile(device_profile()).await?);

        tracing::info!("Registered iAP profile and published CarPlay advertisement");

        Ok(Self {
            adapter,
            identity,
            _session: session,
            iap_profile,
            _agent: agent,
            _advertisement: advertisement,
            // _iap_v2: iap_v2,
        })
    }

}

/// Carried across retry attempts by `backon`: the host holding the profile we accept on, and the
/// device we relink so the unit offers a fresh channel.
struct LinkCtx {
    host: Arc<IapHost>,
    device: Device,
    connect_attempt: Arc<Semaphore>,
}

/// Why an attempt to bring the iAP2 link up failed. Drives the `.when` retry predicate.
#[derive(Error, Debug)]
pub enum LinkAttemptError {
    /// The profile stream ended: BlueZ has taken our registrations away, so no further connection
    /// can ever arrive.
    #[error("iAP2 profiles stream closed")]
    ProfilesClosed,
    #[error("iAP2 error: {0:?}")]
    Iap2(#[from] Iap2Error),
    #[error("Bluer error: {0:?}")]
    BluerError(#[from] bluer::Error),
    #[error("Semaphore acquire error: {0:?}")]
    AcquireError(#[from] tokio::sync::AcquireError),
    #[error("Semaphore try acquire error: {0:?}")]
    TrueAcquireError(#[from] tokio::sync::TryAcquireError),
}

/// Cycle the link - or page, if there is none - so the unit offers a fresh iAP channel.
///
/// The teardown happens before this returns, the page after: a `disconnect` racing the accept loop
/// aborts the very channel the unit just opened, while `Connect()` blocks until every profile has
/// been tried and so cannot be awaited there. `connect_profile` is not a substitute for either; it
/// leaves a connection in progress.
async fn relink(device: &Device, gate: &Arc<Semaphore>) {
    // One at a time: a second disconnect landing on an in-flight connect cancels it.
    let Ok(permit) = gate.clone().try_acquire_owned() else {
        tracing::debug!("Relink already in flight; skipping this one");
        return;
    };
    if device.is_connected().await.unwrap_or(false) {
        let dis = device.disconnect().await;
        tracing::debug!(?dis, "Disconnected stale link");
    }
    let device = device.clone();
    tokio::spawn(async move {
        let _permit = permit;
        let res = device.connect().await;
        tracing::debug!(?res, "Connected");
    });
}

/// Accept an iAP connection from the configured head unit and bring the iAP2 link up on it.
///
/// One channel per attempt; the caller's retry loop re-arms on failure.
async fn attempt_iap2_link(
    ctx: &mut LinkCtx,
) -> Result<Iap2<bluer::rfcomm::Stream>, LinkAttemptError> {
    let target = ctx.device.address();
    let mut profile = ctx.host.iap_profile.lock().await;

    loop {
        tracing::debug!("Waiting for iAP2 connection requests");
        let rq = profile
            .next()
            .await
            .ok_or(LinkAttemptError::ProfilesClosed)?;
        let from = rq.device();
        // Our profiles are registered adapter-wide, so any bonded head unit lands here.
        if from != target {
            tracing::warn!(
                %from, %target,
                "Rejecting iAP2 connection from a device that is not the configured head unit"
            );
            rq.reject(ReqError::Rejected);
            continue;
        }

        tracing::debug!(%from, "Got connect request");
        let stream = rq.accept()?;
        tracing::debug!("Establishing iAP2 link");
        return Ok(Iap2::start(stream).await?);
    }
}

pub async fn wireless_carplay(
    host: Arc<IapHost>,
    target: String,
    wifi_tx: mpsc::Sender<WifiCredentials>,
    ready_tx: oneshot::Sender<()>,
    car_addr_tx: oneshot::Sender<String>,
) -> Result<()> {
    let adapter = &host.adapter;
    let identity = host.identity.clone();

    let device = find_target_device(adapter, &target).await?;
    // Surface the car's MAC (known now, early) so the caller can look up its cached display and
    // size the compositor / launch the app before AirPlay reports the display.
    let _ = car_addr_tx.send(device.address().to_string());

    // Gate on relinks: one at a time.
    let connect_attempt = Arc::new(tokio::sync::Semaphore::new(1));

    tracing::info!("Establishing fresh connection with head unit");
    relink(&device, &connect_attempt).await;

    let task_device = device.clone();

    let iap2 = ({
        |mut ctx: LinkCtx| async move {
            let res = attempt_iap2_link(&mut ctx).await;
            if let Err(LinkAttemptError::Iap2(e)) = &res {
                tracing::warn!(?e, "No channel carried the iAP2 handshake");
                // ctx.device.disconnect().await?;
                relink(&ctx.device, &ctx.connect_attempt).await;
            }
            (ctx, res)
        }
    })
    //Retry our link unless our profiles start to return None.
    .retry(
        ConstantBuilder::new()
            .with_delay(ACCEPT_RETRY_DELAY)
            .without_max_times(),
    )
    .context(LinkCtx {
        host: host.clone(),
        device: task_device,
        connect_attempt,
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

/// Resolve the we paired it this sessionconfigured accesory and get it ready to connect.
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

/// Pair (if needed) and trust the target.
async fn prepare_device(device: Device) -> Result<Device> {
    let alias = device.alias().await.unwrap_or_default();
    let rssi = device.rssi().await.ok().flatten();
    let class = device.class().await.ok().flatten();
    let was_paired = device.is_paired().await.unwrap_or(false);
    tracing::info!(
        name = alias,
        address = ?device.address(),
        rssi,
        class = class.map(|c| format!("{c:#08x}")),
        was_paired,
        "Preparing configured head unit"
    );

    if !was_paired {
        // Not fatal: BlueZ often completes pairing as part of the profile connect that follows.
        let _ = device.pair().await.tap_err(|e| {
            tracing::warn!(?e, "Pairing failed, continuing to the connection attempt")
        });
    }
    //We try keeping trust off so it only connects when we're ready to listen
    let _ = device.set_trusted(false).await;
    Ok(device)
}

#[cfg(test)]
mod tests {
    use super::transport_identifier;

    /// The accessory expects the iPhone's 24-character identifier shape here, not an address -
    /// a 17-character MAC string in this field is what the bike was being sent.
    #[test]
    fn transport_identifier_is_24_hex_chars() {
        let id = transport_identifier(bluer::Address::new([0x4c, 0x79, 0x6e, 0xda, 0x29, 0x4b]));
        assert_eq!(id, "0000000000004C796EDA294B");
        assert_eq!(id.len(), 24);
    }
}
