//! BlueZ: the adapter, iAP profiles and pairing carjack registers, plus the paging/dialling/relink
//! moves that get a unit to offer its iAP channel. Orchestrated by [`super`].

use std::{sync::Arc, time::Duration};

use tap::TapFallible;

use backon::{ConstantBuilder, Retryable};
use bluer::AdapterEvent::{DeviceAdded, DeviceRemoved};
use bluer::agent::Agent;
use bluer::rfcomm::{self, ProfileHandle, ReqError, Role};
use bluer::{Adapter, Address, Device, DiscoveryFilter, Session};
use eyre::{Result, eyre};
use tokio::sync::{Mutex, Semaphore};
use tokio_stream::StreamExt;

use libcarjack::uuid::{CARPLAY_DEVICE_UUID, IAP_ACCESSORY_UUID, IAP_DEVICE_UUID};
use libcarjack::{carplay_init::DeviceIdentity, sdp::iap::iap_service_record};

use carjack::config::{Hfp, Target};

use crate::wireless::hfp;

const IAP_CHANNEL: u16 = 1;

const DISCOVERY_WAIT: Duration = Duration::from_secs(60);

/// How long `auto` gathers inquiry results before ranking them, so the unit in this car wins on
/// signal strength rather than on which one BlueZ happened to report first.
const AUTO_SCAN_WINDOW: Duration = Duration::from_secs(6);

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

pub(crate) async fn find_target_device(
    adapter: &Adapter,
    target: &Target,
    remembered: Option<Address>,
) -> Result<Device> {
    match target {
        Target::Selector(selector) => find_selected_device(adapter, selector).await,
        Target::Auto => find_any_head_unit(adapter, remembered).await,
    }
}

async fn find_selected_device(adapter: &Adapter, target: &str) -> Result<Device> {
    let wanted: Option<Address> = target.parse().ok();

    // Prefer whatever BlueZ already knows (bonded, or currently connected).
    for addr in adapter.device_addresses().await? {
        let device = adapter.device(addr)?;
        if device_matches(&device, target, wanted).await {
            tracing::info!(%target, ?addr, "Found configured head unit in BlueZ's device list");
            let device = prepare_device(device).await?;
            return Ok(device);
        }
    }

    // Otherwise wait for it to show up in discovery aimed at this one target.
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

/// This device's signal strength, if BlueZ currently sees it as a head unit in range.
///
/// The liveness test is RSSI: bluer documents it as `None` for a device that is known but not
/// currently present, and `discover_devices_with_changes` opens by replaying every device BlueZ
/// has ever seen, so this is what separates the units in range from that backlog.
async fn head_unit_in_range(adapter: &Adapter, addr: Address) -> Option<i16> {
    let device = adapter.device(addr).ok()?;
    let rssi = device.rssi().await.ok()??;
    offers_iap(&device).await.then_some(rssi)
}

async fn find_any_head_unit(adapter: &Adapter, remembered: Option<Address>) -> Result<Device> {
    // No UUID filter, deliberately. BlueZ only learns a BR/EDR device's UUIDs by browsing its SDP,
    // which it does on connect - during inquiry it has whatever the unit put in its EIR, often
    // nothing. Filtering on the iAP UUID therefore hides the devices whose UUIDs we are waiting to
    // see arrive. `offers_iap` still decides; the filter would only have decided it sooner and on
    // less information.
    adapter
        .set_discovery_filter(DiscoveryFilter {
            discoverable: true,
            transport: bluer::DiscoveryTransport::BrEdr,
            rssi: Some(-100),
            ..DiscoveryFilter::default()
        })
        .await?;
    tracing::info!("Looking for any CarPlay head unit in range");

    // Driven by the stream rather than polled beside it: bluer re-emits `DeviceAdded` for every
    // property change, so a unit whose name, RSSI or UUIDs land mid-scan is re-judged on the new
    // information. Reading it also matters in itself - the stream is a one-slot channel, so a scan
    // that only holds it wedges bluer's forwarding task after the first event or two.
    let mut events = adapter.discover_devices_with_changes().await?;

    let rank_until = tokio::time::Instant::now() + AUTO_SCAN_WINDOW;
    let give_up_at = tokio::time::Instant::now() + DISCOVERY_WAIT;
    let mut best: Option<(Address, i16)> = None;

    let best = loop {
        tokio::select! {
            event = events.next() => {
                match event {
                    Some(DeviceAdded(addr)) => {
                        let Some(rssi) = head_unit_in_range(adapter, addr).await else {
                            continue;
                        };
                        // The unit from last time, back in range: no reason to spend the rest of
                        // the window ranking it against neighbours.
                        if remembered == Some(addr) {
                            tracing::info!(address = ?addr, "Found the head unit from the last session");
                            break addr;
                        }
                        if best.is_none_or(|(_, best_rssi)| rssi > best_rssi) {
                            tracing::debug!(address = ?addr, rssi, "New best CarPlay head unit");
                            best = Some((addr, rssi));
                        }
                    }
                    // Out of range again before we settled on it.
                    Some(DeviceRemoved(addr)) => best = best.filter(|(best_addr, _)| *best_addr != addr),
                    Some(_) => (),
                    None => return Err(eyre!("Bluetooth discovery ended before a head unit answered")),
                }
            }
            // Only once there is something to pick: an empty window is worth waiting out.
            () = tokio::time::sleep_until(rank_until), if best.is_some() => {
                let (addr, rssi) = best.expect("guarded on best.is_some()");
                tracing::info!(address = ?addr, rssi, "Picked the strongest CarPlay head unit in range");
                break addr;
            }
            () = tokio::time::sleep_until(give_up_at) => {
                return Err(eyre!("No CarPlay head unit came into range"));
            }
        }
    };

    prepare_device(adapter.device(best)?).await
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
