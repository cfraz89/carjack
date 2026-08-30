//! The wireless CarPlay flow: bring the iAP2 link up over Bluetooth, run the CarPlay init sequence
//! on it, and hand the accessory's WiFi credentials + handover signal to the caller. The BlueZ
//! mechanics are in [`bluetooth`]; this is the glue.

mod bluetooth;
mod hfp;

pub use bluetooth::IapHost;

use std::{sync::Arc, time::Duration};

use backon::{ConstantBuilder, RetryableWithContext};
use bluer::{Address, Device, rfcomm::ReqError};
use eyre::Result;
use iap2_proto::{error::Iap2Error, iap2::Iap2};
use libcarjack::carplay_init::{CarplayInit, WifiCredentials};
use thiserror::Error;
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

use carjack::config::Target;

use crate::wireless::bluetooth::{dial_accessory, find_target_device, relink, request_is_target};

/// Handed to the caller: the channel a CarPlay session streams the accessory's credentials over,
/// and a token it cancels once on the accessory's access point - after which a Bluetooth drop is
/// the expected ending rather than a failure.
pub struct WifiOffhand {
    pub creds_tx: mpsc::Sender<WifiCredentials>,
    pub handover: CancellationToken,
}

const ACCEPT_RETRY_DELAY: Duration = Duration::from_secs(1);

/// How long to wait for the unit to offer its iAP channel before cycling the link, backing off to
/// a ceiling - cycling a channel about to be opened tears it down.
const OFFER_WAIT: Duration = Duration::from_secs(20);
const OFFER_WAIT_MAX: Duration = Duration::from_secs(60);

/// Retry context for [`attempt_iap2_link`].
struct LinkCtx {
    host: Arc<IapHost>,
    /// Resolved in the background; paging is the only thing that needs a `Device`.
    device: tokio::sync::watch::Receiver<Option<Device>>,
    target: Target,
    wanted: Option<Address>,
    connect_attempt: Arc<Semaphore>,
}

/// Why the iAP2 link attempt gave up; drives the retry predicate.
#[derive(Error, Debug)]
pub enum LinkAttemptError {
    /// BlueZ deregistered our profiles; retrying can't help.
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

/// Accept one iAP connection from the configured unit and bring the iAP2 link up on it.
async fn attempt_iap2_link(
    ctx: &mut LinkCtx,
) -> Result<Iap2<bluer::rfcomm::Stream>, LinkAttemptError> {
    let mut profile = ctx.host.iap_profile.lock().await;
    let mut client = ctx.host.iap_client.lock().await;

    loop {
        tracing::debug!("Waiting for iAP2 connection requests");
        // Re-check on every wait - a link can appear after this loop starts (e.g. BlueZ
        // auto-connecting a trusted device) and we'd never have cycled it.
        let mut wait = OFFER_WAIT;
        let mut round = 0u32;
        let rq = loop {
            tokio::select! {
                rq = profile.next() => break rq.ok_or(LinkAttemptError::ProfilesClosed)?,
                rq = client.next() => break rq.ok_or(LinkAttemptError::ProfilesClosed)?,
                _ = tokio::time::sleep(wait) => {
                    round += 1;
                    // Alternate between the one move that does not wait on the unit's schedule
                    // (dialling its own iAP service) and cycling the link. Dialling is off by
                    // default: the unit withdraws its iAP SDP record shortly after dialling once.
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
        // Profiles are registered adapter-wide, so any bonded unit can land here; match the
        // configured selector, not a resolved device - there may not be one yet.
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
    target: Target,
    remembered: Option<Address>,
    handoff: WifiOffhand,
    ready_tx: oneshot::Sender<()>,
    accessory_addr_tx: tokio::sync::watch::Sender<Option<String>>,
) -> Result<()> {
    let WifiOffhand { creds_tx, handover } = handoff;
    let adapter = &host.adapter;
    let identity = host.identity.clone();

    // Relink one at a time: a second one cancels the first's in-flight connect.
    let connect_attempt = Arc::new(tokio::sync::Semaphore::new(1));

    // Attempt to find target device while simultaneously waiting for iAP2 connection requests.
    //We can actually be be receiving connection requests before the device is resolved, so we need to resolve it in the background.
    let (device_tx, device_rx) = tokio::sync::watch::channel::<Option<Device>>(None);
    let _resolver = AbortOnDropHandle::new(tokio::spawn({
        let adapter = adapter.clone();
        let target = target.clone();
        let connect_attempt = connect_attempt.clone();
        async move {
            match find_target_device(&adapter, &target, remembered).await {
                Ok(device) => {
                    // Surface the MAC early so the caller can ready settings/cached display before
                    // WiFi or AirPlay report the real one.
                    let _ = accessory_addr_tx.send(Some(device.address().to_string()));
                    // Page only if there's no link; the accept loop cycles a healthy one itself.
                    if !device.is_connected().await.unwrap_or(false) {
                        relink(&device, &connect_attempt).await;
                    }
                    let _ = device_tx.send(Some(device));
                }
                Err(e) => tracing::warn!(?e, "Could not resolve the configured head unit"),
            }
        }
    }));

    let wanted: Option<Address> = target.address().and_then(|a| a.parse().ok());

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
    let result = CarplayInit::new(iap2, creds_tx, ready_tx, identity, handover)
        .run()
        .await;
    tracing::debug!("iAP lifecycle ended");
    result?;
    Ok(())
}
