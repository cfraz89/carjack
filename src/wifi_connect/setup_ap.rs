//! The setup access point: an AP-mode NetworkManager profile on the uplink adapter, raised when
//! there is nothing to connect to so the config portal is reachable from a phone.
//!
//! NetworkManager's `ipv4.method=shared` does the work - address, DHCP, DNS and NAT - so all this
//! builds is the profile. What it cannot do from here is the DHCP option that advertises the
//! portal URL (RFC 8910): that belongs to the dnsmasq NetworkManager spawns for shared
//! connections, configured system-wide from `/etc/NetworkManager/dnsmasq-shared.d/` and shipped by
//! the package (see `dnsmasq-shared-carjack.conf`).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use eyre::{Result, eyre};
use nm_automata::{
    ActiveConnectionState, ActiveProxy, ConnectionProfile, ConnectionSetting, Ipv4Method,
    Ipv4Setting, Ipv6Method, Ipv6Setting, SettingsConnectionProxy, WirelessMode,
    WirelessSecurityKeyMgmt, WirelessSecuritySetting, WirelessSetting, zbus::zvariant::OwnedValue,
};

use carjack::config::{Wifi, WifiRole};

use super::networkmanager::{connection_by_uuid, get_network_manager, role_device};

/// Fixed rather than left to NetworkManager's `10.42.0.0/24` search: the DHCP option 114 URL in
/// the packaged dnsmasq drop-in has to name an address that is known before the AP comes up.
pub const AP_ADDRESS: &str = "10.42.0.1";
const AP_PREFIX: u32 = 24;

/// The UUID of the setup profile, so a leftover from a previous boot is rewritten rather than
/// piling up beside the new one.
const AP_UUID: &str = "0a3d5e5c-6b4e-5c2f-9d31-7e2a4c9f1b80";

/// A user unit cannot be ordered after NetworkManager, so an adapter may not be realised yet.
const UPLINK_SETTLE: Duration = Duration::from_secs(20);
const UPLINK_POLL: Duration = Duration::from_secs(1);

pub struct SetupAp {
    conn: SettingsConnectionProxy<'static>,
    ssid: String,
}

/// What the uplink adapter is doing, and so whether the setup AP is worth raising on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UplinkState {
    /// Free to host the setup AP.
    Idle,
    /// On a network already, which is carrying the internet uplink and is how carjack is
    /// reachable - taking it down to host a setup AP would be a downgrade.
    Busy,
    /// No adapter holds the uplink role, so there is nothing to raise an AP on. A board with one
    /// radio, which CarPlay has, or one whose second never turned up.
    Absent,
}

impl SetupAp {
    /// Waits up to [`UPLINK_SETTLE`] before settling on [`UplinkState::Absent`], logging why -
    /// the caller cannot tell one radio from a NetworkManager that never answered.
    pub async fn uplink_state(wifi: &Wifi) -> UplinkState {
        let deadline = Instant::now() + UPLINK_SETTLE;
        loop {
            let err = match role_device(WifiRole::Uplink, wifi.selector(WifiRole::Uplink)).await {
                Ok(device) => {
                    return if matches!(
                        device.state().await,
                        Ok(nm_automata::DeviceState::Activated)
                    ) {
                        UplinkState::Busy
                    } else {
                        UplinkState::Idle
                    };
                }
                Err(err) => err,
            };
            if Instant::now() >= deadline {
                tracing::info!(%err, "No uplink adapter after {UPLINK_SETTLE:?}");
                return UplinkState::Absent;
            }
            tracing::debug!(%err, "No uplink adapter yet; asking again");
            tokio::time::sleep(UPLINK_POLL).await;
        }
    }

    pub async fn raise(wifi: &Wifi, ssid: &str, passphrase: &str) -> Result<Self> {
        if passphrase.len() < 8 {
            return Err(eyre!("WPA2 needs a passphrase of at least 8 characters"));
        }
        let nm = get_network_manager().await;
        let device = role_device(WifiRole::Uplink, wifi.selector(WifiRole::Uplink)).await?;
        let iface = device.interface().await?;

        let profile = ap_profile(ssid, passphrase, &iface);
        // NetworkManager's connections outlive a reboot on /data, and AP_UUID is fixed, so
        // add_and_activate would fail with UuidExists on every boot after the first.
        let (conn, active) = match connection_by_uuid(nm, AP_UUID).await {
            Some(existing) => {
                existing.update(&profile).await?;
                let active = nm.activate_connection(&existing, &device, None).await?;
                (existing, active)
            }
            None => {
                nm.add_and_activate_connection(&profile, &device, None)
                    .await?
            }
        };
        wait_for_ap(&active).await?;

        tracing::info!(%ssid, %iface, address = AP_ADDRESS, "Setup access point is up");
        Ok(SetupAp {
            conn,
            ssid: ssid.to_owned(),
        })
    }

    /// Take the AP down and delete its profile, so the uplink adapter is free to be an uplink.
    pub async fn lower(self) {
        if let Err(e) = self.conn.delete().await {
            tracing::warn!(?e, ssid = %self.ssid, "Could not delete the setup AP profile");
        } else {
            tracing::info!(ssid = %self.ssid, "Setup access point is down");
        }
    }
}

fn ap_profile(ssid: &str, passphrase: &str, iface: &str) -> ConnectionProfile {
    let mut address = HashMap::new();
    address.insert(
        "address".to_owned(),
        OwnedValue::from(nm_automata::zbus::zvariant::Str::from(AP_ADDRESS)),
    );
    address.insert("prefix".to_owned(), OwnedValue::from(AP_PREFIX));

    ConnectionProfile::new(ssid)
        .connection(
            ConnectionSetting::default()
                .id(ssid)
                .uuid(AP_UUID)
                .interface_name(iface)
                .autoconnect(false),
        )
        .wireless(
            WirelessSetting::default()
                .ssid(ssid)
                .mode(WirelessMode::Ap)
                .hidden(false),
        )
        .wireless_security(
            WirelessSecuritySetting::default()
                .key_mgmt(WirelessSecurityKeyMgmt::WpaPsk)
                .psk(passphrase),
        )
        // `shared` is what starts dnsmasq and the NAT rules; the pinned address is what the
        // packaged option-114 URL points at.
        .ipv4(
            Ipv4Setting::default()
                .method(Ipv4Method::Shared)
                .address_data(vec![address])
                .never_default(true),
        )
        .ipv6(Ipv6Setting::default().method(Ipv6Method::LinkLocal))
}

async fn wait_for_ap(active: &ActiveProxy<'_>) -> Result<()> {
    use futures::StreamExt;
    let mut states = active.receive_state_changed().await;
    loop {
        match active.state().await? {
            ActiveConnectionState::Activated => return Ok(()),
            ActiveConnectionState::Deactivating | ActiveConnectionState::Deactivated => {
                return Err(eyre!(
                    "The setup access point went down while coming up - does this adapter support \
                     AP mode? (`iw list` must show AP under supported interface modes)"
                ));
            }
            _ => {}
        }
        if states.next().await.is_none() {
            return Err(eyre!("NetworkManager stopped reporting on the setup AP"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ap_profile_is_an_ap_that_shares_a_fixed_address() {
        let profile = ap_profile("carjack-setup", "hunter2hunter2", "wlan1");
        let wireless = profile.wireless.expect("no wireless setting");
        assert_eq!(wireless.mode, Some(WirelessMode::Ap));
        assert_eq!(
            wireless.ssid,
            Some(nm_automata::Ssid::from("carjack-setup"))
        );

        let ipv4 = profile.ipv4.expect("no ipv4 setting");
        assert_eq!(ipv4.method, Some(Ipv4Method::Shared));
        let address = ipv4.address_data.expect("no address");
        assert_eq!(address.len(), 1);
        assert_eq!(
            address[0]
                .get("address")
                .and_then(|v| String::try_from(v.clone()).ok()),
            Some(AP_ADDRESS.to_owned())
        );
        assert_eq!(
            address[0]
                .get("prefix")
                .and_then(|v| u32::try_from(v.clone()).ok()),
            Some(AP_PREFIX)
        );

        // Pinned to the uplink adapter, so raising the AP can never take the CarPlay radio down.
        assert_eq!(
            profile
                .connection
                .expect("no connection setting")
                .interface_name
                .as_deref(),
            Some("wlan1")
        );
    }

    #[tokio::test]
    async fn a_short_passphrase_is_refused_before_touching_networkmanager() {
        let err = match SetupAp::raise(&Wifi::default(), "carjack-setup", "short").await {
            Err(e) => e,
            Ok(_) => panic!("a 5-character passphrase is not a valid WPA2 key"),
        };
        assert!(err.to_string().contains("at least 8"));
    }
}
