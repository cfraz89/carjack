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
/// attempt (the caller retries).
const DISCOVERY_WAIT: Duration = Duration::from_secs(60);

/// Pause before re-arming the accept after a transient failure, so it can't busy-spin.
const ACCEPT_RETRY_DELAY: Duration = Duration::from_secs(1);

/// How many times `relink` pages before giving up, and the delay between attempts. A Page Timeout
/// is the unit's controller not answering; it frequently answers on a later try.
const PAGE_ATTEMPTS: usize = 3;
const PAGE_RETRY_DELAY: Duration = Duration::from_secs(2);

/// How long to wait for a unit to offer its iAP channel before cycling the link.
const OFFER_WAIT: Duration = Duration::from_secs(5);

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
        require_authentication: Some(true),
        require_authorization: Some(false),
        ..Default::default()
    }
}

/// Run the privileged sidecar that gives the adapter an iPhone-like Class of Device.
///
/// Best-effort: the capability lives on the sidecar binary only (never `carjack`, which it would
/// /proc-lock to root and break desktop mode), and skipping it just leaves the unit seeing a plain
/// computer.
async fn apply_device_class() {
    let Some(exe) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("carjack-wireless-helper")))
    else {
        tracing::warn!("Could not locate carjack-wireless-helper; skipping Class of Device setup");
        return;
    };
    match tokio::process::Command::new(&exe)
        .arg("device-class")
        .output()
        .await
    {
        Ok(out) if out.status.success() => tracing::info!("Applied iPhone Class of Device"),
        Ok(out) => tracing::warn!(
            status = ?out.status,
            stderr = %String::from_utf8_lossy(&out.stderr).trim(),
            "carjack-wireless-helper failed; continuing without the Class of Device \
             (try `just setcap`)"
        ),
        Err(e) => tracing::warn!(
            ?e,
            "Could not run carjack-wireless-helper; continuing without it"
        ),
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

        let iap_profile = Mutex::new(session.register_profile(device_profile()).await?);
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

        // bluetoothd re-asserts the adapter class as its service set changes, so set ours once
        // that has settled.
        apply_device_class().await;

        tracing::info!("Registered iAP and CarPlay profiles");

        Ok(Self {
            adapter,
            identity,
            _session: session,
            iap_profile,
            _agent: agent,
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
        // Re-checked on every wait, not just a startup sample - a link can appear after that (e.g.
        // BlueZ auto-connecting a trusted device) and we'd never have cycled it.
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
    // Surface the car's MAC early so the caller can size the compositor / launch the app from its
    // cached display before AirPlay reports the real one.
    let _ = car_addr_tx.send(device.address().to_string());

    // Gate on relinks: one at a time.
    let connect_attempt = Arc::new(tokio::sync::Semaphore::new(1));

    // Cycles any stale link so the unit offers a fresh channel.
    relink(&device, &connect_attempt).await;

    let task_device = device.clone();

    let iap2 = ({
        |mut ctx: LinkCtx| async move {
            let res = attempt_iap2_link(&mut ctx).await;
            if let Err(LinkAttemptError::Iap2(e)) = &res {
                tracing::warn!(?e, "No channel carried the iAP2 handshake");
                relink(&ctx.device, &ctx.connect_attempt).await;
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
