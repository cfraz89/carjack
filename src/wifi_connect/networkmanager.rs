//! Joining the accessory's access point, over NetworkManager.

use std::str::FromStr;

use async_trait::async_trait;
use enclose::enclose;
use eyre::{Result, eyre};

use futures::StreamExt;
use nm_automata::{
    ActivationStateFlags, ActiveConnectionState, ActiveProxy, AssignedMacAddress,
    ConnectionProfile, ConnectionSetting, DeviceProxy, DeviceState, DeviceType, Ipv4Method,
    Ipv4Setting, Ipv6Method, Ipv6Setting, MacAddress, NetworkManagerProxy, NonEmpty, ProxyExt,
    SettingsConnectionProxy, SettingsProxy, Ssid, WirelessProxy, WirelessSecurityKeyMgmt,
    WirelessSecuritySetting, WirelessSetting,
};
use std::time::Duration;
use tap::TapFallible;
use tokio::sync::OnceCell;

use super::{AccessoryWifi, WifiBackend};

const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(30);

static NETWORK_MANAGER: OnceCell<NetworkManagerProxy<'static>> = OnceCell::const_new();

/// The NetworkManager proxy, built once. Its D-Bus connection is what the accessory's profile binds its
/// activation to, so it has to outlive every session.
async fn get_network_manager() -> &'static NetworkManagerProxy<'static> {
    NETWORK_MANAGER
        .get_or_init(async || {
            let connection = zbus::Connection::system()
                .await
                .expect("Unable to reach the system bus");
            NetworkManagerProxy::new(&connection)
                .await
                .expect("Unable to get networkmanager")
        })
        .await
}

pub(crate) struct NetworkManagerBackend;

#[async_trait]
impl WifiBackend for NetworkManagerBackend {
    async fn get_wifi_mac(&self) -> Result<String> {
        get_wifi_mac().await
    }

    async fn connect_to_accessory_wifi(
        &self,
        ssid: &str,
        passphrase: &str,
        accessory_mac: Option<&str>,
    ) -> Result<AccessoryWifi> {
        connect_to_accessory_wifi(ssid, passphrase, accessory_mac).await
    }
}

async fn connect_to_accessory_wifi(
    ssid: &str,
    passphrase: &str,
    accessory_mac: Option<&str>,
) -> Result<AccessoryWifi> {
    if let Some(accessory_wifi) = super::reuse_if_live(ssid, live_connection(ssid).await) {
        return Ok(accessory_wifi);
    }

    // The same address we announced over iAP2 - the accessory waits for exactly that station.
    let announced = get_wifi_mac().await?;
    let announced_mac = match MacAddress::from_str(&announced) {
        Ok(mac) => AssignedMacAddress::Address(mac),
        Err(e) => {
            tracing::warn!(%announced, ?e, "Announced MAC unparseable; letting NetworkManager pick");
            AssignedMacAddress::Preserve
        }
    };

    super::connect_with_known_settings(
        accessory_mac,
        enclose!((announced_mac, announced) |disable_he| { async move {
            let profile = accessory_profile(ssid, passphrase, announced_mac, disable_he);
            let nm = get_network_manager().await;
            let device = wifi_device().await?;
            let (conn, active) = match existing_profile(nm, &profile).await {
                Some(existing) => {
                    tracing::info!(path = ?existing.object_path(), "Using existing wifi profile for accessory");
                    let active = nm
                        .activate_connection(
                            &existing,
                            &device,
                            None,
                        )
                        .await?;
                    (existing, active)
                }
                None => {
                    let (conn, active) = nm
                        .add_and_activate_connection(&profile, &device, None)
                        .await?;
                    tracing::info!(path = ?conn.object_path(), "Connection created");
                    (conn, active)
                }
            };

            if let Err(e) = tokio::time::timeout(ACTIVATION_TIMEOUT, wait_for_activation(&active))
                .await
                .map_err(|_| eyre!("Timed out bringing up the accessory's WiFi connection"))
                .and_then(|r| r)
            {
                conn.delete()
                    .await
                    .tap_err(
                        |e| tracing::debug!(?e, "Could not delete the profile of a failed attempt"),
                    )
                    .ok();
                return Err(e);
            }
            // The whole point of pinning the profile's address is that the accessory sees the
            // station it was promised. If NetworkManager did not apply it, the association is
            // live but the accessory is waiting for a station that will never appear - a failure
            // that otherwise shows up much later, as an AirPlay session that never starts.
            match device.hw_address().await {
                Ok(actual) if actual.eq_ignore_ascii_case(&announced) => {
                    tracing::debug!(%actual, "Associated as the announced address")
                }
                Ok(actual) => tracing::warn!(
                    %actual,
                    %announced,
                    "Associated address is not the one announced over iAP2; the accessory is \
                     watching for a station that will not appear"
                ),
                Err(e) => tracing::debug!(?e, "Could not read back the associated address"),
            }
            accessory_wifi(&device, &active).await
        } }),
    )
    .await
}

/// Find an existing profile that matches the one we want by ssid an assigned mac, and delete any others for the same SSID
async fn existing_profile(
    nm: &NetworkManagerProxy<'_>,
    profile: &ConnectionProfile,
) -> Option<SettingsConnectionProxy<'static>> {
    let settings = SettingsProxy::new(nm.inner().connection()).await.ok()?;
    let connections = settings.connections().await.ok()?;
    let mut keep: Option<SettingsConnectionProxy<'static>> = None;
    let profile_wireless = profile
        .wireless
        .as_ref()
        .expect("Existing profile wireless should not be none");
    let profile_ssid = profile_wireless
        .ssid
        .as_ref()
        .expect("Existing profile ssid should not be none");
    for conn in connections {
        if let Ok(ConnectionProfile {
            wireless:
                Some(WirelessSetting {
                    ssid: Some(ssid),
                    assigned_mac_address,
                    ..
                }),
            ..
        }) = conn.get_settings().await
            && &ssid == profile_ssid
        {
            if keep.is_none() && assigned_mac_address == profile_wireless.assigned_mac_address {
                keep = Some(conn);
            } else {
                let _ = conn
                    .delete()
                    .await
                    .tap_err(|e| tracing::warn!(?e, ?conn, "Error deleting connection"));
            }
        };
    }
    keep
}

/// The connection profile for the accessory's access point.
///
/// `disable_he` currently requires patched iwd, or wpa_supplicant with config_disable_he and patched NetworkManager.
fn accessory_profile(
    ssid: &str,
    passphrase: &str,
    announced_mac: AssignedMacAddress,
    disable_he: bool,
) -> ConnectionProfile {
    let wireless = WirelessSetting::default()
        .ssid(ssid)
        .hidden(true)
        .assigned_mac_address(announced_mac)
        .disable_he(disable_he);
    ConnectionProfile::new(ssid)
        .connection(ConnectionSetting::default().autoconnect(false))
        .wireless(wireless)
        .wireless_security(
            WirelessSecuritySetting::default()
                .key_mgmt(WirelessSecurityKeyMgmt::WpaPsk)
                .psk(passphrase),
        )
        .ipv4(Ipv4Setting::default().method(Ipv4Method::Auto))
        .ipv6(Ipv6Setting::default().method(Ipv6Method::LinkLocal))
}

async fn wait_for_activation(active: &ActiveProxy<'_>) -> Result<()> {
    let configured = ActivationStateFlags::IP4_READY | ActivationStateFlags::IP6_READY;
    let mut flag_changes = active.receive_state_flags_changed().await;
    let mut state_changes = active.receive_state_changed().await;
    loop {
        let state = active.state().await?;
        let flags = active.state_flags().await?;
        if state == ActiveConnectionState::Activated && flags.contains(configured) {
            return Ok(());
        }
        if matches!(
            state,
            ActiveConnectionState::Deactivating | ActiveConnectionState::Deactivated
        ) {
            return Err(eyre!(
                "The accessory's WiFi connection went down while activating"
            ));
        }
        tokio::select! {
            Some(_) = flag_changes.next() => {}
            Some(_) = state_changes.next() => {}
            else => return Err(eyre!("NetworkManager stopped reporting on the activation")),
        }
    }
}

/// The current WiFi connection, if it is already on `ssid` and carries an address - i.e. there is
/// nothing left for `connect_to_accessory_wifi` to do. `None` means "not it, go and connect".
async fn live_connection(ssid: &str) -> Result<Option<AccessoryWifi>> {
    let device = wifi_device().await?;
    // Compared as SSIDs rather than as text: an access point names itself in bytes, and only the
    // bytes decide whether this is the same network.
    if device.state().await? != DeviceState::Activated
        || current_ssid(&device).await? != Some(Ssid::from(ssid))
    {
        return Ok(None);
    }
    // A half-configured connection means the activation is still in flight; let the normal path
    // drive it rather than handing one back.
    let Some(active) = device.active_connection().await? else {
        return Ok(None);
    };
    match accessory_wifi(&device, &active).await {
        Ok(accessory_wifi) => Ok(Some(accessory_wifi)),
        Err(e) => {
            tracing::debug!("On {ssid} already, but it isn't usable yet ({e})");
            Ok(None)
        }
    }
}

/// The SSID the device is currently associated with, if it is associated at all.
async fn current_ssid(device: &DeviceProxy<'_>) -> Result<Option<Ssid>> {
    let ssid = match device
        .cast::<WirelessProxy>()
        .await?
        .active_access_point()
        .await?
    {
        Some(access_point) => Some(access_point.ssid().await?),
        None => None,
    };
    Ok(ssid)
}

/// What the rest of carjack needs from an activated connection to the accessory.
async fn accessory_wifi(
    device: &DeviceProxy<'_>,
    active: &ActiveProxy<'_>,
) -> Result<AccessoryWifi> {
    // Absent until NetworkManager has finished the IPv4 stage, which is what `wait_for_activation`
    // gates on - so reaching here without one means the activation is still in flight.
    active.ip4_config().await?.ok_or(eyre!(
        "The accessory's connection has no IPv4 configuration yet"
    ))?;
    Ok(AccessoryWifi {
        iface: device.interface().await?,
    })
}

/// The WiFi device to run the accessory's connection on: whichever one is already up, else the first
/// the daemon lists.
async fn wifi_device() -> Result<DeviceProxy<'static>> {
    let mut wifi = Vec::new();
    for device in get_network_manager().await.get_devices().await? {
        if device.device_type().await? == DeviceType::Wifi {
            wifi.push(device);
        }
    }
    for device in &wifi {
        if device.state().await? == DeviceState::Activated {
            return Ok(device.clone());
        }
    }
    wifi.into_iter()
        .next()
        .ok_or(eyre!("No wifi devices on system"))
}

async fn get_wifi_mac() -> Result<String> {
    let device = wifi_device().await?;
    let wifi: WirelessProxy = device.cast().await?;
    // Derived from the permanent address rather than being it - see `super::announced_wifi_mac`.
    // The accessory's profile pins itself to the same value, so what we announce is what associates.
    let permanent = match wifi.perm_hw_address().await?.non_empty() {
        Some(permanent) => permanent,
        None => device.hw_address().await?,
    };
    Ok(super::announced_wifi_mac(&permanent))
}

#[cfg(test)]
mod tests {
    use nm_automata::{
        AssignedMacAddress, Ipv4Method, Ipv6Method, Ssid, WirelessBand, WirelessMode,
        WirelessSecurityKeyMgmt,
    };

    /// One runtime for every live test in this process.
    ///
    /// `get_network_manager` caches its D-Bus connection for the life of the process, but a
    /// `#[tokio::test]` gets a runtime of its own and drops it on the way out - taking the
    /// connection's driver task with it, so the next test to reuse the cached proxy blocks
    /// forever. carjack itself has one runtime for the process; these share one too.
    fn runtime() -> &'static tokio::runtime::Runtime {
        static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
        RUNTIME.get_or_init(|| {
            tokio::runtime::Runtime::new().expect("building a runtime for the live tests")
        })
    }

    /// Live check against NetworkManager: asking for the SSID we are already on must hand back
    /// the live connection so `connect_to_accessory_wifi` skips reactivating it, and any other SSID must
    /// not match. Read-only - it never activates or tears down anything.
    #[test]
    #[ignore = "needs a live NetworkManager WiFi connection; set CARJACK_TEST_SSID"]
    fn live_connection_matches_only_the_current_ssid() {
        runtime().block_on(async {
            let ssid = std::env::var("CARJACK_TEST_SSID").expect("set CARJACK_TEST_SSID");
            let found = super::live_connection(&ssid)
                .await
                .expect("querying the current connection failed")
                .expect("the SSID we are on should match");
            assert!(!found.iface.is_empty());

            assert!(
                super::live_connection("carjack-definitely-not-connected")
                    .await
                    .expect("querying an unrelated SSID failed")
                    .is_none()
            );
        });
    }

    /// Live check against NetworkManager: the profile carjack builds for the accessory has to be one
    /// the daemon accepts and stores as written - a mistyped property is refused outright, and a
    /// misnamed one is silently kept somewhere carjack's fields don't reach. Created unsaved so
    /// it never touches disk, read back, then deleted; never activated, and its SSID is one no
    /// access point advertises.
    #[test]
    #[ignore = "needs a live NetworkManager"]
    fn accessory_profile_is_accepted_by_networkmanager() {
        runtime().block_on(async {
            let nm = super::get_network_manager().await;
            let settings = nm_automata::SettingsProxy::new(nm.inner().connection())
                .await
                .expect("no settings interface");
            let profile = super::accessory_profile(
                "carjack-profile-check",
                "hunter2hunter2",
                AssignedMacAddress::Permanent,
                false,
            );

            let stored = settings
                .add_connection_unsaved(&profile)
                .await
                .expect("NetworkManager rejected the accessory profile");
            let read_back = stored.get_settings().await.expect("couldn't read it back");
            stored.delete().await.expect("couldn't delete it");

            let wireless = read_back.wireless.expect("no wireless setting");
            assert_eq!(wireless.ssid, Some(Ssid::from("carjack-profile-check")));
            assert_eq!(
                wireless.assigned_mac_address,
                Some(AssignedMacAddress::Permanent)
            );
            assert_eq!(wireless.band, Some(WirelessBand::A));
            assert_eq!(wireless.mode, Some(WirelessMode::Infrastructure));
            assert_eq!(wireless.hidden, Some(true));
            // The passphrase is a secret, so it does not come back - only that WPA-PSK was accepted.
            let security = read_back.wireless_security.expect("no security setting");
            assert_eq!(security.key_mgmt, Some(WirelessSecurityKeyMgmt::WpaPsk));
            assert_eq!(
                read_back.ipv4.and_then(|ipv4| ipv4.method),
                Some(Ipv4Method::Auto)
            );
            // The one that makes `IP6_READY` mean a settled link-local rather than "SLAAC gave up".
            assert_eq!(
                read_back.ipv6.and_then(|ipv6| ipv6.method),
                Some(Ipv6Method::LinkLocal)
            );
        });
    }

    #[test]
    fn accessory_profile_sets_disable_he() {
        let profile = super::accessory_profile(
            "carjack-profile-check",
            "hunter2hunter2",
            AssignedMacAddress::Permanent,
            true,
        );
        assert_eq!(
            profile.wireless.expect("no wireless setting").disable_he,
            Some(true)
        );

        let profile = super::accessory_profile(
            "carjack-profile-check",
            "hunter2hunter2",
            AssignedMacAddress::Permanent,
            false,
        );
        assert_eq!(
            profile.wireless.expect("no wireless setting").disable_he,
            Some(false)
        );
    }
}
