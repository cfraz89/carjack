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
use libcarjack::{carplay, sdp::iap::iap_service_record};
use libcarjack::{
    carplay_session::{DeviceIdentity, WifiCredentials},
    uuid::{CARPLAY_DEVICE_UUID, IAP_ACCESSORY_UUID, IAP_DEVICE_UUID},
};
use tokio::sync::{Mutex, Semaphore, mpsc, oneshot};
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

use crate::wifi_connect::WifiBackend;

const IAP_CHANNEL: u16 = 1;

/// How long to wait for the configured head unit to show up in discovery before giving up on this
/// attempt (the caller retries).
const DISCOVERY_WAIT: Duration = Duration::from_secs(60);

/// Pause before re-arming the accept after a transient failure, so it can't busy-spin.
const ACCEPT_RETRY_DELAY: Duration = Duration::from_secs(1);

/// How many times `relink` pages before giving up, and the delay between attempts. A Page Timeout
/// is the unit's controller not answering; it frequently answers on a later try.
const PAGE_ATTEMPTS: usize = 3;
const PAGE_RETRY_DELAY: Duration = Duration::from_secs(2);

/// How long to wait for a unit to offer its iAP channel before cycling the link, and the ceiling
/// that wait backs off to. The unit opens the channel on its own schedule - at power-up, or when
/// the rider picks us on screen - so cycling eagerly tears down the link it was about to use, and
/// enough of that walks it out of page scan altogether.
const OFFER_WAIT: Duration = Duration::from_secs(20);
const OFFER_WAIT_MAX: Duration = Duration::from_secs(60);

/// BlueZ session, pairing agent, CarPlay advertisement and iAP profile registrations.
///
/// Must outlive every session - dropping it deregisters the profiles.
pub struct IapHost {
    adapter: Adapter,
    identity: DeviceIdentity,
    /// Held only to keep the registrations alive for the process lifetime.
    _session: Session,
    /// The iAP profile the head unit connects in to; a session takes exclusive use of its stream.
    iap_profile: Mutex<ProfileHandle>,
    /// The accessory's own iAP service, dialled outbound when it will not dial us.
    iap_client: Mutex<ProfileHandle>,
    _agent: bluer::agent::AgentHandle,
}

/// The one iAP profile - finding it in our SDP is what makes a head unit speak iAP rather than
/// Android Auto.
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

/// The accessory's iAP service, client side. Registered so we can dial the unit's own iAP server
/// rather than only wait to be dialled; BlueZ resolves its RFCOMM channel from the unit's SDP.
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

/// The CarPlay service, registered purely for its UUID: bluetoothd adds a registered profile's
/// UUID to the adapter's EIR, which a head unit inspects while bonding.
fn carplay_profile() -> rfcomm::Profile {
    rfcomm::Profile {
        uuid: CARPLAY_DEVICE_UUID,
        name: Some(String::from("CarPlay")),
        role: Some(Role::Server),
        //Need this until bluer fixes None sending no channel, when it should send 0 == auto to bluez
        channel: Some(0),
        require_authentication: Some(true),
        require_authorization: Some(false),
        ..Default::default()
    }
}

/// What carjack calls itself in `RequestAccessoryWiFiConfiguration` - the adapter's Bluetooth
/// address, colon-separated and uppercase. The bike answers only this form, not a zero-padded
/// 24-character one.
fn transport_identifier(addr: bluer::Address) -> String {
    // `CARJACK_WIFI_ID=padded` restores the zero-padded form, for testing against a unit that wants it.
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
    /// The adapter's Bluetooth address - a head unit keys the phone on it in both directions,
    /// including `_carplay-ctrl._tcp`'s `id`.
    pub async fn bluetooth_address(&self) -> Result<String> {
        Ok(colon_address(self.adapter.address().await?))
    }

    /// Register everything once.
    pub async fn start(wifi: &dyn WifiBackend) -> Result<Self> {
        let session = Session::new().await?;
        let agent = session.register_agent(Agent::default()).await?;
        let adapter = session.default_adapter().await?;
        adapter.set_alias(String::from("carjack")).await?;
        // The unit opens the iAP channel itself, so it has to be able to find us for as long as we
        // are waiting - a timeout of zero is "no timeout", not "off".
        adapter.set_pairable(true).await?;
        adapter.set_discoverable_timeout(0).await?;
        adapter.set_discoverable(true).await?;
        let identity = DeviceIdentity {
            name: String::from("carjack"),
            transport_identifier: transport_identifier(adapter.address().await?),
            wifi_mac: wifi.get_wifi_mac().await?,
        };

        let iap_profile = Mutex::new(session.register_profile(device_profile()).await?);
        let iap_client = Mutex::new(session.register_profile(accessory_profile()).await?);
        // Registered only so bluetoothd puts the CarPlay UUID in the adapter's EIR. The drain task
        // owns the handle for the process lifetime and rejects anything that arrives, so an
        // unanswered request can't pin its fd.
        let mut carplay_profile = session.register_profile(carplay_profile()).await?;
        tokio::spawn(async move {
            while let Some(rq) = carplay_profile.next().await {
                tracing::debug!(from = %rq.device(), "Rejecting connection on the CarPlay profile");
                rq.reject(ReqError::Rejected);
            }
        });

        // Class of Device is set before carjack even starts, by carjack-hci-helper.service
        // (src/bin/carjack-hci-helper.rs) running ahead of carjack.service.
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

/// Carried across retry attempts by `backon`: the host holding the profile we accept on, and the
/// device we relink so the unit offers a fresh channel.
struct LinkCtx {
    host: Arc<IapHost>,
    /// Resolved in the background: the unit can open its channel before discovery has produced a
    /// `Device`, and paging is the only thing that needs one.
    device: tokio::sync::watch::Receiver<Option<Device>>,
    target: String,
    wanted: Option<Address>,
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
/// been tried and so cannot be awaited there.
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

/// Dial the accessory's own iAP service. Spawned, because `ConnectProfile` blocks for the whole
/// SDP lookup and connect; the stream it wins arrives on the client profile.
fn dial_accessory(device: &Device) {
    let device = device.clone();
    tokio::spawn(async move {
        let res = device.connect_profile(&IAP_ACCESSORY_UUID).await;
        tracing::debug!(?res, "Outbound iAP dial result");
    });
}

/// Whether a connection that arrived on our profile came from the configured head unit.
async fn request_is_target(
    adapter: &Adapter,
    from: Address,
    target: &str,
    wanted: Option<Address>,
) -> bool {
    match wanted {
        Some(addr) => from == addr,
        None => match adapter.device(from) {
            Ok(device) => device_matches(&device, target, None).await,
            Err(_) => false,
        },
    }
}

/// Accept an iAP connection from the configured head unit and bring the iAP2 link up on it.
///
/// One channel per attempt; the caller's retry loop re-arms on failure.
async fn attempt_iap2_link(
    ctx: &mut LinkCtx,
) -> Result<Iap2<bluer::rfcomm::Stream>, LinkAttemptError> {
    let mut profile = ctx.host.iap_profile.lock().await;
    let mut client = ctx.host.iap_client.lock().await;

    loop {
        tracing::debug!("Waiting for iAP2 connection requests");
        // Re-checked on every wait, not just a startup sample - a link can appear after that (e.g.
        // BlueZ auto-connecting a trusted device) and we'd never have cycled it.
        let mut wait = OFFER_WAIT;
        let mut round = 0u32;
        let rq = loop {
            tokio::select! {
                rq = profile.next() => break rq.ok_or(LinkAttemptError::ProfilesClosed)?,
                rq = client.next() => break rq.ok_or(LinkAttemptError::ProfilesClosed)?,
                _ = tokio::time::sleep(wait) => {
                    round += 1;
                    // Alternate: dialling the unit's own iAP service is the one move that does not
                    // wait on its schedule, and cycling must not tear down the link it needs.
                    // Off by default - the unit withdrew its iAP SDP record seconds after the one
                    // dial that connected, and without that record no path works in either
                    // direction.
                    let device = ctx.device.borrow().clone();
                    let Some(device) = device else {
                        tracing::debug!("No iAP channel offered; head unit not resolved yet");
                        continue;
                    };
                    if round % 2 == 1 && std::env::var("CARJACK_DIAL_IAP").as_deref() == Ok("1") {
                        tracing::info!("No iAP channel offered; dialling the accessory");
                        dial_accessory(&device);
                    } else {
                        tracing::info!(?wait, "No iAP channel offered; cycling the link");
                        relink(&device, &ctx.connect_attempt).await;
                        wait = (wait * 2).min(OFFER_WAIT_MAX);
                    }
                }
            }
        };
        let from = rq.device();
        // Our profiles are registered adapter-wide, so any bonded head unit lands here. Matched
        // against the configured selector, not a resolved device - there may not be one yet.
        if !request_is_target(&ctx.host.adapter, from, &ctx.target, ctx.wanted).await {
            tracing::warn!(
                %from, target = %ctx.target,
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
    accessory_addr_tx: tokio::sync::watch::Sender<Option<String>>,
    handover: CancellationToken,
) -> Result<()> {
    let adapter = &host.adapter;
    let identity = host.identity.clone();

    // Gate on relinks: one at a time.
    let connect_attempt = Arc::new(tokio::sync::Semaphore::new(1));

    // Resolving the head unit must not gate accepting. It opens its channel seconds after bonding,
    // and discovery can still be running then - a request nobody reads is a session thrown away.
    let (device_tx, device_rx) = tokio::sync::watch::channel::<Option<Device>>(None);
    let _resolver = AbortOnDropHandle::new(tokio::spawn({
        let adapter = adapter.clone();
        let target = target.clone();
        let connect_attempt = connect_attempt.clone();
        async move {
            match find_target_device(&adapter, &target).await {
                Ok(device) => {
                    // Surface the accessory's MAC early - so the caller can look up its accessory
                    // settings before WiFi connects, and size the compositor / launch the app from
                    // its cached display before AirPlay reports the real one.
                    let _ = accessory_addr_tx.send(Some(device.address().to_string()));
                    // Page only if there is no link. A healthy one is left alone - tearing it down
                    // costs a page the unit may not answer, and the accept loop cycles it anyway.
                    if !device.is_connected().await.unwrap_or(false) {
                        relink(&device, &connect_attempt).await;
                    }
                    let _ = device_tx.send(Some(device));
                }
                Err(e) => tracing::warn!(?e, "Could not resolve the configured head unit"),
            }
        }
    }));

    let wanted: Option<Address> = target.parse().ok();

    let iap2 = ({
        |mut ctx: LinkCtx| async move {
            let res = attempt_iap2_link(&mut ctx).await;
            if let Err(LinkAttemptError::Iap2(e)) = &res {
                tracing::warn!(?e, "No channel carried the iAP2 handshake");
                let device = ctx.device.borrow().clone();
                if let Some(device) = device {
                    relink(&device, &ctx.connect_attempt).await;
                }
            }
            (ctx, res)
        }
    })
    // Retry the link unless our profiles start returning None.
    .retry(
        ConstantBuilder::new()
            .with_delay(ACCEPT_RETRY_DELAY)
            .without_max_times(),
    )
    .context(LinkCtx {
        host: host.clone(),
        device: device_rx,
        target: target.clone(),
        wanted,
        connect_attempt,
    })
    .when(|e| !matches!(e, LinkAttemptError::ProfilesClosed))
    .await
    .1?;

    tracing::debug!("iAP2 link established");
    carplay::run_carplay_session(iap2, wifi_tx, ready_tx, identity, handover).await?;
    tracing::debug!("iAP lifecycle ended");
    Ok(())
}

/// Resolve the configured accessory and get it ready to connect.
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

    // Otherwise wait for it to show up - discovery aimed at this one target, not a survey.
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
    let _ = device.set_trusted(true).await;
    Ok(device)
}

#[cfg(test)]
mod tests {
    use super::transport_identifier;

    /// The bike answers `0x4300` carrying this form and ignores the 24-character padded one.
    #[test]
    fn transport_identifier_is_the_colon_bluetooth_address() {
        let id = transport_identifier(bluer::Address::new([0x4c, 0x79, 0x6e, 0xda, 0x29, 0x4b]));
        assert_eq!(id, "4C:79:6E:DA:29:4B");
    }
}
