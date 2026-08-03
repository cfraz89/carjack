use std::{sync::Arc, time::Duration};
use tap::TapFallible;
use thiserror::Error;

use backon::{ConstantBuilder, Retryable, RetryableWithContext};
use bluer::{
    Adapter,
    AdapterEvent::DeviceAdded,
    Address, Device, DiscoveryFilter, Session,
    agent::Agent,
    rfcomm::{self, ProfileHandle, ReqError, Role},
};
use eyre::{Result, eyre};
use iap2_proto::{error::Iap2Error, iap2::Iap2};
use libcarjack::carplay;
use libcarjack::{
    carplay_session::{DeviceIdentity, WifiCredentials},
    uuid::{CARPLAY_DEVICE_UUID, IAP_DEVICE_UUID},
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

/// How many times `relink` pages before giving up, and how long it waits between attempts. A Page
/// Timeout is the unit's controller not answering; it frequently answers on a later try.
const PAGE_ATTEMPTS: usize = 3;
const PAGE_RETRY_DELAY: Duration = Duration::from_secs(2);

/// How long to wait for a unit to offer its iAP channel before resetting bluetooth. When it works,
/// it should usually happen almost immediately, in my experience
const OFFER_WAIT: Duration = Duration::from_secs(5);

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

/// The CarPlay service. Registered purely for its UUID: bluetoothd adds a registered profile's
/// UUID to the adapter's EIR, which is what a head unit inspects while bonding. Server role so
/// bluetoothd actually publishes it; the channel is BlueZ's to pick.
fn carplay_profile() -> rfcomm::Profile {
    rfcomm::Profile {
        uuid: CARPLAY_DEVICE_UUID,
        name: Some(String::from("CarPlay")),
        role: Some(Role::Server),
        require_authentication: Some(true),
        require_authorization: Some(false),
        ..Default::default()
    }
}

/// Run the privileged sidecar that gives the adapter an iPhone-like Class of Device.
///
/// Best-effort, and deliberately not fatal: the capability is granted out of band (`just setcap`,
/// on the sidecar binary only - never on `carjack`, which a capability would /proc-lock to root and
/// break desktop mode). Without it carjack still runs, the unit just sees a plain computer.
async fn apply_device_class() {
    let Some(exe) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("carjack-hci-helper")))
    else {
        tracing::warn!("Could not locate carjack-hci-helper; skipping Class of Device setup");
        return;
    };
    match tokio::process::Command::new(&exe).output().await {
        Ok(out) if out.status.success() => tracing::info!("Applied iPhone Class of Device"),
        Ok(out) => tracing::warn!(
            status = ?out.status,
            stderr = %String::from_utf8_lossy(&out.stderr).trim(),
            "carjack-hci-helper failed; continuing without the Class of Device (try `just setcap`)"
        ),
        Err(e) => tracing::warn!(
            ?e,
            "Could not run carjack-hci-helper; continuing without it"
        ),
    }
}

/// What carjack calls itself in `RequestAccessoryWiFiConfiguration` - the adapter's Bluetooth
/// address, colon-separated and uppercase.
///
/// afd6605 changed this to a 24-character zero-padded form, on the reasoning that an iPhone sends
/// a UDID there rather than an address. The bike does not answer it: `0x4300` carrying
/// `0000000000004C796EDA294B` is acked and never replied to, while the same request carrying
/// `4C:79:6E:DA:29:4B` gets `0x4301` back. An iPhone's own 24-character identifier is answered
/// fine, so the format is not inherently wrong - ours specifically is not accepted, and the bike
/// is the target.
fn transport_identifier(addr: bluer::Address) -> String {
    // `CARJACK_WIFI_ID=padded` restores afd6605's form, for testing against a unit that wants it.
    if std::env::var("CARJACK_WIFI_ID").as_deref() == Ok("padded") {
        let hex: String = addr.iter().map(|b| format!("{b:02X}")).collect();
        return format!("{hex:0>24}");
    }
    colon_address(addr)
}

/// A Bluetooth address as a head unit expects to see it: colon-separated and uppercase.
fn colon_address(addr: bluer::Address) -> String {
    addr.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

impl IapHost {
    /// The adapter's Bluetooth address. A head unit keys the phone on it in both directions -
    /// `_carplay-ctrl._tcp`'s `id` is what ties the station on its AP back to the device it
    /// authenticated over Bluetooth.
    pub async fn bluetooth_address(&self) -> Result<String> {
        Ok(colon_address(self.adapter.address().await?))
    }

    /// Register everything once.
    pub async fn start() -> Result<Self> {
        let session = Session::new().await?;
        let agent = session.register_agent(Agent::default()).await?;
        let adapter = session.default_adapter().await?;
        adapter.set_alias(String::from("carjack")).await?;
        let identity = DeviceIdentity {
            name: String::from("carjack"),
            transport_identifier: transport_identifier(adapter.address().await?),
            wifi_mac: get_wifi_mac().await?,
        };
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
        // Registered only so bluetoothd puts the CarPlay UUID in the adapter's EIR. The drain task
        // owns the handle for the process lifetime (dropping it would deregister) and rejects
        // anything that arrives - an unanswered request would pin its fd, since bluer buffers one
        // per profile behind BlueZ's `NewConnection` call.
        let mut carplay_profile = session.register_profile(carplay_profile()).await?;
        tokio::spawn(async move {
            while let Some(rq) = carplay_profile.next().await {
                tracing::debug!(from = %rq.device(), "Rejecting connection on the CarPlay profile");
                rq.reject(ReqError::Rejected);
            }
        });

        // After the registrations: bluetoothd re-asserts the adapter class as its service set
        // changes, so set ours once that has settled. Best-effort - see `apply_device_class`.
        apply_device_class().await;

        tracing::info!("Registered iAP and CarPlay profiles");

        Ok(Self {
            adapter,
            identity,
            _session: session,
            iap_profile,
            _agent: agent,
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
        let res = (|| async { device.connect().await })
            .retry(
                ConstantBuilder::new()
                    .with_delay(PAGE_RETRY_DELAY)
                    .with_max_times(PAGE_ATTEMPTS),
            )
            .await;
        tracing::debug!(?res, "Connection result")
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
        // `stale_link` is sampled once, before pairing, so a link that appears *after* that -
        // the unit connecting on its own, or BlueZ auto-connecting a trusted device - is one we
        // never cycled and the unit will not offer a channel on. Re-check while waiting rather
        // than trusting the startup sample.
        let rq = loop {
            match tokio::time::timeout(OFFER_WAIT, profile.next()).await {
                Ok(rq) => break rq.ok_or(LinkAttemptError::ProfilesClosed)?,
                Err(_) => {
                    if ctx.device.is_connected().await.unwrap_or(false) {
                        tracing::info!("Linked but no iAP channel offered; cycling the link");
                        relink(&ctx.device, &ctx.connect_attempt).await;
                    }
                }
            }
        };
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

    //If the link is stale, we need to reconnect, as we've probably missed our window to accept the iap connection.
    //If it isn't, we need to connect if we haven't alreaady.
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

/// Resolve the we paired it this sessionconfigured accesory and get it ready to connect.
async fn find_target_device(adapter: &Adapter, target: &str) -> Result<Device> {
    let wanted: Option<Address> = target.parse().ok();

    // Whatever BlueZ already knows about (bonded from a previous run, or currently connected).
    for addr in adapter.device_addresses().await? {
        let device = adapter.device(addr)?;
        if device_matches(&device, target, wanted).await {
            tracing::info!(%target, ?addr, "Found configured head unit in BlueZ's device list");
            let device = prepare_device(device).await?;
            return Ok(device);
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
    let device = prepare_device(found).await?;
    Ok(device)
}

/// Whether this device is the configured target, by address if the selector parsed as a MAC,
/// otherwise by advertised name.
async fn device_matches(device: &Device, target: &str, wanted: Option<Address>) -> bool {
    match wanted {
        Some(addr) => device.address() == addr,
        None => device.alias().await.map(|a| a == target).unwrap_or(false),
    }
}

/// Pair (if needed) and trust the target. Reports whether a link *predated* us - see `relink`.
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
    let _ = device.set_trusted(true).await;
    Ok(device)
}

#[cfg(test)]
mod tests {
    use super::transport_identifier;

    /// The bike answers `0x4300` carrying this form and ignores the 24-character padded one that
    /// afd6605 introduced - see `transport_identifier`.
    #[test]
    fn transport_identifier_is_the_colon_bluetooth_address() {
        let id = transport_identifier(bluer::Address::new([0x4c, 0x79, 0x6e, 0xda, 0x29, 0x4b]));
        assert_eq!(id, "4C:79:6E:DA:29:4B");
    }
}
