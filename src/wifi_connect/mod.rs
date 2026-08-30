//! WiFi backend abstraction. NetworkManager is the only implementation; the trait is the seam
//! [`crate::session`] holds, so it stays a trait.

pub(crate) mod networkmanager;
pub mod setup_ap;

use std::future::Future;
use std::time::Duration;

use async_trait::async_trait;
use backon::{ConstantBuilder, Retryable};
use eyre::Result;

use crate::accessory_settings::AccessoryStore;

/// What the rest of carjack needs from an activated connection to the accessory.
pub struct AccessoryWifi {
    pub iface: String,
}

/// A WiFi daemon carjack can drive to join the accessory's access point.
#[async_trait]
pub trait WifiBackend: Send + Sync {
    /// The WiFi address carjack announces to the accessory over iAP2, and associates with.
    async fn get_wifi_mac(&self) -> Result<String>;

    /// Fail fast if this backend can't do what `connect_to_accessory_wifi` will need later.
    async fn check_ready(&self) -> Result<()> {
        Ok(())
    }

    /// Join the accessory's access point (or reuse an existing connection already on it). Retries with
    /// whatever this accessory is already known to need - see [`crate::accessory_settings`].
    async fn connect_to_accessory_wifi(
        &self,
        ssid: &str,
        passphrase: &str,
        accessory_mac: Option<&str>,
    ) -> Result<AccessoryWifi>;
}

pub(crate) fn build(wifi: carjack::config::Wifi) -> Box<dyn WifiBackend> {
    Box::new(networkmanager::NetworkManagerBackend { wifi })
}

/// Log and unwrap the "are we already on this accessory's network" query, so
/// `connect_to_accessory_wifi` only has to decide whether to ask.
fn reuse_if_live(
    ssid: impl std::fmt::Display,
    checked: Result<Option<AccessoryWifi>>,
) -> Option<AccessoryWifi> {
    match checked {
        Ok(Some(accessory_wifi)) => {
            tracing::info!("Already connected to {ssid}; reusing the live connection");
            Some(accessory_wifi)
        }
        Ok(None) => None,
        Err(e) => {
            tracing::warn!("Couldn't inspect the current connection ({e}); connecting anyway");
            None
        }
    }
}

async fn connect_with_known_settings<F, Fut>(
    accessory_mac: Option<&str>,
    attempt: F,
) -> Result<AccessoryWifi>
where
    F: Fn(bool) -> Fut,
    Fut: Future<Output = Result<AccessoryWifi>>,
{
    let disable_he = accessory_mac.is_some_and(|mac| AccessoryStore::load().get(mac).disable_he);

    (|| attempt(disable_he))
        .retry(ConstantBuilder::new().with_delay(Duration::from_secs(2)))
        .notify(|e, _| tracing::warn!("Bringing up the accessory's WiFi failed ({e}); retrying"))
        .await
}

/// The WiFi address carjack announces to the accessory, and associates with.
///
/// **Not the permanent hardware address.** The bike answers `0x4300` for `FE:B2:9F:ED:EA:72` and
/// for an invented `FE:B2:9F:ED:EA:99`, and refuses to answer at all for this machine's permanent
/// `4c:79:6e:da:29:47` - which differs from the adapter's *Bluetooth* address only in the last
/// byte, so it most likely collides with us in the unit's device table. An iPhone's globally
/// administered address is answered fine, so it is the collision that matters, not the bit.
///
/// Derived from the permanent address so it is stable for a machine - a unit that remembers us by
/// this address keeps recognising us - and locally administered so it cannot collide with real
/// hardware. `CARJACK_WIFI_MAC` overrides it for testing.
fn announced_wifi_mac(permanent: &str) -> String {
    if let Ok(mac) = std::env::var("CARJACK_WIFI_MAC") {
        tracing::warn!(%mac, "Announcing an overridden WiFi MAC");
        return mac;
    }
    // FNV-1a over the permanent address: stable, no dependency, and good enough to scatter the
    // low bytes away from the hardware address.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in permanent.to_ascii_lowercase().as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let d = h.to_be_bytes();
    // `0xFE` is unicast and locally administered, and is the prefix both known-good addresses used.
    format!(
        "FE:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        d[0], d[1], d[2], d[3], d[4]
    )
}
