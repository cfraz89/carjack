//! BlueZ: the adapter, iAP profiles and pairing carjack registers, plus the paging/dialling/relink
//! moves that get a unit to offer its iAP channel. Orchestrated by [`super`].

use std::{sync::Arc, time::Duration};

use tap::TapFallible;

use backon::{ConstantBuilder, Retryable};
use bluer::AdapterEvent::DeviceAdded;
use bluer::agent::Agent;
use bluer::rfcomm::{self, ProfileHandle, ReqError, Role};
use bluer::{Adapter, Address, Device, DiscoveryFilter, Session};
use eyre::Result;
use tokio::sync::{Mutex, Semaphore};
use tokio_stream::StreamExt;

use libcarjack::uuid::{CARPLAY_DEVICE_UUID, IAP_ACCESSORY_UUID, IAP_DEVICE_UUID};
use libcarjack::{carplay_init::DeviceIdentity, sdp::iap::iap_service_record};

use carjack::config::{Hfp, Target};

use crate::wireless::hfp;

const IAP_CHANNEL: u16 = 1;

/// Discovery is never given up on - a unit that is powered after us has to be able to turn up
/// hours later - so a session that ends is simply started again. The gap is there to keep a
/// refusing adapter from spinning, and to leave the radio a window between inquiries in which the
/// unit can page us.
const DISCOVERY_RESTART_DELAY: Duration = Duration::from_secs(5);

/// A page timeout usually just means the controller missed; it often answers on a later try.
const PAGE_ATTEMPTS: usize = 3;
const PAGE_RETRY_DELAY: Duration = Duration::from_secs(2);

/// Adapter, agent and profile registrations. Must outlive every session - dropping it
/// deregisters the profiles.
pub struct IapHost {
    pub(crate) adapter: Adapter,
    pub(crate) identity: DeviceIdentity,
    _session: Session,
    /// The iAP profile the unit dials in on; a session takes exclusive use of its stream.
    pub(crate) iap_profile: Mutex<ProfileHandle>,
    /// The unit's own iAP server, dialed outbound when it won't dial us.
    pub(crate) iap_client: Mutex<ProfileHandle>,
    _agent: bluer::agent::AgentHandle,
}

/// Its presence in our SDP is what makes a unit speak iAP rather than Android Auto.
fn device_profile() -> rfcomm::Profile {
    rfcomm::Profile {
        uuid: IAP_DEVICE_UUID,
        name: Some(String::from("Wireless iAP")),
        role: Some(Role::Server),
        channel: Some(IAP_CHANNEL),
        require_authentication: Some(true),
        require_authorization: Some(false),
        service_record: Some(iap_service_record(IAP_CHANNEL as u8).to_xml()),
        ..Default::default()
    }
}

/// Lets us dial the unit's own iAP server rather than only wait to be dialled.
fn accessory_profile() -> rfcomm::Profile {
    rfcomm::Profile {
        uuid: IAP_ACCESSORY_UUID,
        name: Some(String::from("Wireless iAP client")),
        role: Some(Role::Client),
        require_authentication: Some(true),
        require_authorization: Some(false),
        ..Default::default()
    }
}

/// Only its UUID matters: bluetoothd advertises a registered profile's UUID in EIR, which units
/// check while bonding.
fn carplay_profile() -> rfcomm::Profile {
    advertised_profile(CARPLAY_DEVICE_UUID, "CarPlay")
}

fn advertised_profile(uuid: uuid::Uuid, name: &str) -> rfcomm::Profile {
    rfcomm::Profile {
        uuid,
        name: Some(String::from(name)),
        role: Some(Role::Server),
        //Need this until bluer fixes None sending no channel, when it should send 0 == auto to bluez
        channel: Some(0),
        require_authentication: Some(true),
        require_authorization: Some(false),
        ..Default::default()
    }
}

fn colon_address(addr: bluer::Address) -> String {
    addr.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

impl IapHost {
    pub async fn bluetooth_address(&self) -> Result<String> {
        Ok(colon_address(self.adapter.address().await?))
    }

    pub async fn start(hfp_identity: Hfp) -> Result<Self> {
        let session = Session::new().await?;
        let agent = session.register_agent(Agent::default()).await?;
        let adapter = session.default_adapter().await?;
        adapter.set_alias(String::from("carjack")).await?;
        // The unit dials us on its own schedule, so stay discoverable; zero is "no timeout", not "off".
        adapter.set_pairable(true).await?;
        adapter.set_discoverable_timeout(0).await?;
        adapter.set_discoverable(true).await?;
        let identity = DeviceIdentity {
            name: String::from("carjack"),
            bluetooth_address: colon_address(adapter.address().await?).to_lowercase(),
        };

        let iap_profile = Mutex::new(session.register_profile(device_profile()).await?);
        let iap_client = Mutex::new(session.register_profile(accessory_profile()).await?);
        // Rejecting connects so an unanswered request can't pin its fd. A collision means
        // bluetoothd or obexd already advertises the UUID, which is as good.
        for profile in [carplay_profile()] {
            let name = profile.name.clone().unwrap_or_default();
            let handle = session.register_profile(profile).await;
            let mut handle = match handle {
                Ok(handle) => handle,
                Err(e) => {
                    tracing::warn!(
                        ?e,
                        "Another process holds the {name} profile; leaving it to it"
                    );
                    continue;
                }
            };
            // The task owns the handle, so the registration lives as long as the process.
            tokio::spawn(async move {
                while let Some(rq) = handle.next().await {
                    tracing::debug!(from = %rq.device(), "Rejecting connection on the {name} profile");
                    rq.reject(ReqError::Rejected);
                }
            });
        }

        match session.register_profile(hfp::profile()).await {
            Ok(handle) => {
                tracing::info!(
                    manufacturer = %hfp_identity.manufacturer,
                    model = %hfp_identity.model,
                    "Serving the hands-free gateway"
                );
                hfp::serve(handle, hfp_identity)
            }
            Err(e) => tracing::warn!(
                ?e,
                "Could not register the hands-free gateway; another process holds it, so the unit's \
                 identity probe will go unanswered"
            ),
        }

        tracing::info!("Registered iAP and CarPlay profiles");

        Ok(Self {
            adapter,
            identity,
            _session: session,
            iap_profile,
            iap_client,
            _agent: agent,
        })
    }
}

fn page_timed_out(res: &Result<(), bluer::Error>) -> bool {
    res.as_ref()
        .is_err_and(|e| e.message.contains("page-timeout"))
}

/// Disconnect (or page if none) so the unit offers a fresh iAP channel. Teardown first: a
/// disconnect racing the accept loop aborts the very channel it just opened, and `Connect()`
/// blocks until every profile has been tried.
pub(crate) async fn relink(device: &Device, gate: &Arc<Semaphore>) {
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
        let res = (|| async { device.connect().await })
            .retry(
                ConstantBuilder::new()
                    .with_delay(PAGE_RETRY_DELAY)
                    .with_max_times(PAGE_ATTEMPTS),
            )
            .await;
        if page_timed_out(&res) {
            tracing::warn!(
                address = %device.address(),
                "Head unit did not answer {PAGE_ATTEMPTS} pages; it is off, asleep or out of range"
            );
        } else {
            tracing::debug!(?res, "Connection result")
        }
    });
}

/// Spawned: `ConnectProfile` blocks for the SDP lookup and connect; the stream wins on the client
/// profile.
pub(crate) fn dial_accessory(device: &Device) {
    let device = device.clone();
    tokio::spawn(async move {
        let res = device.connect_profile(&IAP_ACCESSORY_UUID).await;
        tracing::debug!(?res, "Outbound iAP dial result");
    });
}

pub(crate) async fn request_is_target(
    adapter: &Adapter,
    from: Address,
    target: &Target,
    wanted: Option<Address>,
) -> bool {
    match target {
        Target::Auto => match adapter.device(from) {
            // Nothing but a CarPlay unit dials our iAP profile, so SDP that has not resolved yet
            // is not a reason to refuse the request.
            Ok(device) => match device.uuids().await {
                Ok(Some(uuids)) => uuids.contains(&IAP_ACCESSORY_UUID),
                Ok(None) | Err(_) => true,
            },
            Err(_) => false,
        },
        Target::Selector(selector) => match wanted {
            Some(addr) => from == addr,
            None => match adapter.device(from) {
                Ok(device) => device_matches(&device, selector, None).await,
                Err(_) => false,
            },
        },
    }
}

/// Whether BlueZ has resolved this device as offering the iAP accessory service - the one thing
/// every CarPlay head unit hosts, and nothing else does.
async fn offers_iap(device: &Device) -> bool {
    device
        .uuids()
        .await
        .ok()
        .flatten()
        .is_some_and(|uuids| uuids.contains(&IAP_ACCESSORY_UUID))
}

/// Whether this device is the one to connect to.
///
/// `auto` additionally insists the unit is in range, using RSSI: bluer documents it as `None` for
/// a device that is known but not currently present, and a discovery stream opens by replaying
/// every device BlueZ has ever seen, so it is what separates the units here from that backlog. A
/// named unit is taken wherever it is found - out of range it just means the page retries.
async fn matches_target(device: &Device, target: &Target) -> bool {
    match target {
        Target::Auto => matches!(device.rssi().await, Ok(Some(_))) && offers_iap(device).await,
        Target::Selector(selector) => device_matches(device, selector, selector.parse().ok()).await,
    }
}

/// One discovery session's worth of scanning: the first device matching `target`, or `None` if
/// BlueZ ended the session before one turned up.
async fn scan_for_target(adapter: &Adapter, target: &Target) -> Result<Option<Address>> {
    // Inside the attempt, not once before them all: bluer refuses to set the filter while a
    // session is active, and a resolver starting while the last one's session is still tearing
    // down would otherwise take that as fatal and stop scanning for good.
    //
    // No UUID filter, deliberately. BlueZ only learns a BR/EDR device's UUIDs by browsing its SDP,
    // which it does on connect - during inquiry it has whatever the unit put in its EIR, often
    // nothing. Filtering on the iAP UUID therefore hides the devices whose UUIDs we are waiting to
    // see arrive. `matches_target` still decides; the filter would only have decided it sooner and
    // on less information.
    adapter
        .set_discovery_filter(DiscoveryFilter {
            discoverable: true,
            transport: bluer::DiscoveryTransport::BrEdr,
            rssi: Some(-100),
            ..DiscoveryFilter::default()
        })
        .await?;

    // Driven by the stream rather than polled beside it: bluer re-emits `DeviceAdded` for every
    // property change, so a unit whose name, RSSI or UUIDs land mid-scan is re-judged on the new
    // information. Reading it also matters in itself - the stream is a one-slot channel, so a scan
    // that only holds it wedges bluer's forwarding task after the first event or two.
    let mut events = adapter.discover_devices_with_changes().await?;

    while let Some(event) = events.next().await {
        let DeviceAdded(addr) = event else { continue };
        // One unreadable device is not a reason to tear down the session and start over.
        let Ok(device) = adapter.device(addr) else {
            continue;
        };
        if matches_target(&device, target).await {
            tracing::info!(?target, ?addr, "Discovered head unit");
            return Ok(Some(addr));
        }
    }
    Ok(None)
}

pub(crate) async fn find_target_device(adapter: &Adapter, target: &Target) -> Result<Device> {
    // A unit BlueZ already knows needs no inquiry, and connecting without one running is both
    // quicker and more reliable.
    match target {
        Target::Selector(selector) => {
            let wanted: Option<Address> = selector.parse().ok();
            for addr in adapter.device_addresses().await? {
                let device = adapter.device(addr)?;
                if device_matches(&device, selector, wanted).await {
                    tracing::info!(%selector, ?addr, "Found configured head unit in BlueZ's device list");
                    return prepare_device(device).await;
                }
            }
        }
        // Connected stands in for auto's RSSI screen: a connected device has no RSSI and
        // re-emits no DeviceAdded, so discovery never sees it.
        Target::Auto => {
            for addr in adapter.device_addresses().await? {
                let device = adapter.device(addr)?;
                if device.is_connected().await.unwrap_or(false) && offers_iap(&device).await {
                    tracing::info!(?addr, "Found a connected CarPlay unit in BlueZ's device list");
                    return prepare_device(device).await;
                }
            }
        }
    }

    tracing::info!(?target, "Looking for the head unit");

    let found = loop {
        match scan_for_target(adapter, target).await {
            Ok(Some(addr)) => break addr,
            // BlueZ ended the session, or would not start one. Neither says the unit is not
            // coming - it may not be powered yet - so scan again rather than give up on it.
            Ok(None) => tracing::debug!(
                ?target,
                "Discovery ended without the head unit; scanning again"
            ),
            Err(e) => tracing::warn!(?e, ?target, "Bluetooth discovery failed; scanning again"),
        }
        tokio::time::sleep(DISCOVERY_RESTART_DELAY).await;
    };

    prepare_device(adapter.device(found)?).await
}

async fn device_matches(device: &Device, target: &str, wanted: Option<Address>) -> bool {
    match wanted {
        Some(addr) => device.address() == addr,
        None => device.alias().await.map(|a| a == target).unwrap_or(false),
    }
}

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
mod tests {}
