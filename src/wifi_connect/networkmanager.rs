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
    SettingsConnectionProxy, SettingsProxy, Ssid, UserSetting, WirelessProxy,
    WirelessSecurityKeyMgmt, WirelessSecuritySetting, WirelessSetting,
};
use std::collections::HashMap;
use std::time::Duration;
use tap::TapFallible;
use tokio::sync::OnceCell;

use carjack::config::{Wifi, WifiRole};

use super::{AccessoryWifi, WifiBackend};

const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(30);

/// Namespace for [`accessory_uuid`]; fixed so the derived UUIDs can't collide with anything else.
const PROFILE_NAMESPACE: uuid::Uuid = uuid::uuid!("4c989952-3611-420d-8a26-91c9c035ad10");

/// `user.data` key holding the accessory's Bluetooth address, so [`superseded_by`] can tell
/// carjack's profiles from everyone else's, and one accessory's from another's.
const ACCESSORY_TAG: &str = "carjack.accessory";

static NETWORK_MANAGER: OnceCell<NetworkManagerProxy<'static>> = OnceCell::const_new();

/// The NetworkManager proxy, built once. Its D-Bus connection is what the accessory's profile binds its
/// activation to, so it has to outlive every session.
pub(crate) async fn get_network_manager() -> &'static NetworkManagerProxy<'static> {
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

pub(crate) struct NetworkManagerBackend {
    pub(crate) wifi: Wifi,
}

#[async_trait]
impl WifiBackend for NetworkManagerBackend {
    async fn get_wifi_mac(&self) -> Result<String> {
        get_wifi_mac(&self.wifi).await
    }

    async fn connect_to_accessory_wifi(
        &self,
        ssid: &str,
        passphrase: &str,
        accessory_mac: Option<&str>,
    ) -> Result<AccessoryWifi> {
        connect_to_accessory_wifi(&self.wifi, ssid, passphrase, accessory_mac).await
    }
}

async fn connect_to_accessory_wifi(
    wifi: &Wifi,
    ssid: &str,
    passphrase: &str,
    accessory_mac: Option<&str>,
) -> Result<AccessoryWifi> {
    if let Some(accessory_wifi) = super::reuse_if_live(ssid, live_connection(wifi, ssid).await) {
        return Ok(accessory_wifi);
    }

    // The same address we announced over iAP2 - the accessory waits for exactly that station.
    let announced = get_wifi_mac(wifi).await?;
    let announced_mac = match MacAddress::from_str(&announced) {
        Ok(mac) => AssignedMacAddress::Address(mac),
        Err(e) => {
            tracing::warn!(%announced, ?e, "Announced MAC unparseable; letting NetworkManager pick");
            AssignedMacAddress::Preserve
        }
    };

    // Resolved once, not per attempt: the profile is pinned to this interface, so which adapter
    // the accessory's connection lands on must not drift between retries.
    let device = wifi_device(wifi).await?;
    let iface = device.interface().await?;
    tracing::info!(%iface, "Joining the accessory on the CarPlay adapter");

    let owned_accessory_mac = accessory_mac.map(str::to_owned);
    super::connect_with_known_settings(
        accessory_mac,
        enclose!((announced_mac, announced, owned_accessory_mac, device, iface) |disable_he| { async move {
            let accessory_mac = owned_accessory_mac.as_deref();
            let profile = accessory_profile(ssid, passphrase, announced_mac, disable_he, accessory_mac, &iface);
            let nm = get_network_manager().await;
            let (conn, active) = match accessory_connection(nm, &profile).await {
                Some(existing) => {
                    tracing::info!(path = ?existing.object_path(), "Reusing this accessory's wifi profile");
                    // The unit may have renamed its network since; rewrite before activating.
                    existing.update(&profile).await?;
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

/// The stored profile carrying `uuid`, if NetworkManager has one.
pub(crate) async fn connection_by_uuid(
    nm: &NetworkManagerProxy<'_>,
    uuid: &str,
) -> Option<SettingsConnectionProxy<'static>> {
    let settings = SettingsProxy::new(nm.inner().connection()).await.ok()?;
    for conn in settings.connections().await.ok()? {
        let Ok(stored) = conn.get_settings().await else {
            continue;
        };
        if profile_uuid(&stored) == Some(uuid) {
            return Some(conn);
        }
    }
    None
}

/// This accessory's stored profile, deleting every stale one it passes.
///
/// Keyed by UUID, not SSID: a head unit is free to rename its network - some invent a fresh SSID
/// every boot - and each name must not become a profile of its own.
async fn accessory_connection(
    nm: &NetworkManagerProxy<'_>,
    profile: &ConnectionProfile,
) -> Option<SettingsConnectionProxy<'static>> {
    let settings = SettingsProxy::new(nm.inner().connection()).await.ok()?;
    let mut keep: Option<SettingsConnectionProxy<'static>> = None;
    for conn in settings.connections().await.ok()? {
        let Ok(stored) = conn.get_settings().await else {
            continue;
        };
        if profile_uuid(&stored).is_some() && profile_uuid(&stored) == profile_uuid(profile) {
            keep = Some(conn);
        } else if superseded_by(&stored, profile) {
            tracing::info!(path = ?conn.object_path(), "Deleting a stale profile for this accessory");
            let _ = conn
                .delete()
                .await
                .tap_err(|e| tracing::warn!(?e, ?conn, "Error deleting connection"));
        }
    }
    keep
}

/// Whether `want` replaces `stored`, so `stored` should be deleted.
///
/// Deleting one of carjack's profiles costs nothing - the accessory re-sends its SSID and key every
/// session - so the rules err towards collecting.
fn superseded_by(stored: &ConnectionProfile, want: &ConnectionProfile) -> bool {
    // A profile with no `wireless` setting is somebody else's.
    let (Some(stored_wireless), Some(want_wireless)) = (&stored.wireless, &want.wireless) else {
        return false;
    };
    if profile_uuid(stored).is_some() && profile_uuid(stored) == profile_uuid(want) {
        return false;
    }
    match (accessory_tag(stored), accessory_tag(want)) {
        (Some(stored_mac), Some(want_mac)) if stored_mac == want_mac => return true,
        // Tagged for another accessory - not ours to collect.
        (Some(_), _) => {}
        // Untagged, but pinned to the address only carjack invents. Which accessory it belonged to
        // is unrecoverable, and does not matter.
        (None, _) => {
            if matches!(
                want_wireless.assigned_mac_address,
                Some(AssignedMacAddress::Address(_))
            ) && stored_wireless.assigned_mac_address == want_wireless.assigned_mac_address
            {
                return true;
            }
        }
    }
    // Anything else on the SSID carjack is about to join would compete with it for the AP.
    stored_wireless.ssid.is_some() && stored_wireless.ssid == want_wireless.ssid
}

/// The accessory this profile was made for, as stamped in `user.data`.
fn accessory_tag(profile: &ConnectionProfile) -> Option<&str> {
    profile
        .user
        .as_ref()?
        .data
        .as_ref()?
        .get(ACCESSORY_TAG)
        .map(String::as_str)
}

fn profile_uuid(profile: &ConnectionProfile) -> Option<&str> {
    profile.connection.as_ref()?.uuid.as_deref()
}

/// The UUID this accessory's profile is keyed by. Derived from the Bluetooth address so it survives
/// the unit renaming its network; from the SSID when that address isn't known yet.
fn accessory_uuid(ssid: &str, accessory_mac: Option<&str>) -> uuid::Uuid {
    let name = match accessory_mac {
        Some(mac) => format!("bluetooth:{}", mac.to_ascii_lowercase()),
        None => format!("ssid:{ssid}"),
    };
    uuid::Uuid::new_v5(&PROFILE_NAMESPACE, name.as_bytes())
}

/// The connection profile for the accessory's access point.
///
/// `disable_he` requires wpa_supplicant with config_disable_he and a patched NetworkManager.
fn accessory_profile(
    ssid: &str,
    passphrase: &str,
    announced_mac: AssignedMacAddress,
    disable_he: bool,
    accessory_mac: Option<&str>,
    iface: &str,
) -> ConnectionProfile {
    let mut wireless = WirelessSetting::default()
        .ssid(ssid)
        .hidden(true)
        .assigned_mac_address(announced_mac);
    // Only when asked: stock NetworkManager has no disable-he, and one property it does not
    // know makes it drop the whole 802-11-wireless setting and report it missing.
    if disable_he {
        wireless = wireless.disable_he(true);
    }
    // `.connection()` replaces the whole setting, dropping the id `ConnectionProfile::new` put there.
    // Pinned to the CarPlay adapter so it can never activate on the uplink one.
    let connection = ConnectionSetting::default()
        .id(ssid)
        .uuid(accessory_uuid(ssid, accessory_mac).to_string())
        .interface_name(iface)
        .autoconnect(false);
    let mut profile = ConnectionProfile::new(ssid)
        .connection(connection)
        .wireless(wireless)
        .wireless_security(
            WirelessSecuritySetting::default()
                .key_mgmt(WirelessSecurityKeyMgmt::WpaPsk)
                .psk(passphrase),
        )
        // The head unit's DHCP hands out a default route to a network with no internet on it.
        // Taking it would blackhole everything the uplink adapter is carrying.
        .ipv4(
            Ipv4Setting::default()
                .method(Ipv4Method::Auto)
                .never_default(true),
        )
        .ipv6(
            Ipv6Setting::default()
                .method(Ipv6Method::LinkLocal)
                .never_default(true),
        );
    if let Some(accessory_mac) = accessory_mac {
        profile = profile.user(UserSetting::default().data(HashMap::from([(
            ACCESSORY_TAG.to_owned(),
            accessory_mac.to_owned(),
        )])));
    }
    profile
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
async fn live_connection(wifi: &Wifi, ssid: &str) -> Result<Option<AccessoryWifi>> {
    let device = wifi_device(wifi).await?;
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

/// Every WiFi device the daemon knows, ordered by interface name so the roles below are stable
/// across reboots.
async fn wifi_devices() -> Result<Vec<DeviceProxy<'static>>> {
    let mut wifi = Vec::new();
    for device in get_network_manager().await.get_devices().await? {
        if device.device_type().await? == DeviceType::Wifi {
            let iface = device.interface().await.unwrap_or_default();
            wifi.push((iface, device));
        }
    }
    wifi.sort_by(|(a, _), (b, _)| a.cmp(b));
    Ok(wifi.into_iter().map(|(_, device)| device).collect())
}

/// Whether an interface hangs off USB. Resolved, not the raw link: `device` points at a relative
/// path like `../../../3-1.2:1.0`, which names no bus at all.
fn is_usb(iface: &str) -> bool {
    std::fs::canonicalize(format!("/sys/class/net/{iface}/device"))
        .map(|p| p.to_string_lossy().contains("usb"))
        .unwrap_or(false)
}

/// Whether the adapter is one that can be unplugged, which is the USB port's own answer rather
/// than a guess from the bus. Walks up from the interface because drivers disagree about what
/// `device` points at - rtw89 names its USB interface, aic8800 the device above it - and stops at
/// the first level that answers, so a PCIe adapter (no `removable` anywhere up the chain) is
/// fixed.
fn is_removable(iface: &str) -> bool {
    let Ok(mut path) = std::fs::canonicalize(format!("/sys/class/net/{iface}/device")) else {
        return false;
    };
    while path.starts_with("/sys/devices") {
        if let Ok(state) = std::fs::read_to_string(path.join("removable")) {
            return state.trim() == "removable";
        }
        let Some(parent) = path.parent() else { break };
        path = parent.to_path_buf();
    }
    false
}

/// How much the adapter looks like something plugged into the board rather than part of it.
/// Lowest takes the CarPlay role.
fn plugged_in(iface: &str) -> u8 {
    2 * u8::from(is_removable(iface)) + u8::from(is_usb(iface))
}

/// Whether `selector` names this device, by interface name or by either of its MACs.
async fn device_is(device: &DeviceProxy<'_>, selector: &str) -> bool {
    if device
        .interface()
        .await
        .is_ok_and(|i| i.eq_ignore_ascii_case(selector))
    {
        return true;
    }
    let permanent = match device.cast::<WirelessProxy>().await {
        Ok(wifi) => wifi
            .perm_hw_address()
            .await
            .ok()
            .and_then(|a| a.non_empty()),
        Err(_) => None,
    };
    let current = device.hw_address().await.ok();
    [permanent, current]
        .into_iter()
        .flatten()
        .any(|mac| mac.eq_ignore_ascii_case(selector))
}

/// The adapter playing `role`.
///
/// Unconfigured, the CarPlay role goes to the adapter least like something plugged in - fixed
/// before removable, and off-bus before USB - and the uplink to the first that is left. A board
/// can carry its own radio on USB, as the Dragon Q6A does with its AIC8800, so the bus alone does
/// not tell the two apart. Picking "whichever is already up" instead would hand CarPlay
/// the uplink adapter the moment it associates with a home network - and, through
/// [`get_wifi_mac`], change the address carjack announces over iAP2 with it.
pub(crate) async fn role_device(
    role: WifiRole,
    selector: Option<&str>,
) -> Result<DeviceProxy<'static>> {
    let devices = wifi_devices().await?;
    if devices.is_empty() {
        return Err(eyre!("No wifi devices on system"));
    }

    if let Some(selector) = selector {
        for device in &devices {
            if device_is(device, selector).await {
                return Ok(device.clone());
            }
        }
        return Err(eyre!(
            "No wifi adapter matching '{selector}' for the {role:?} role"
        ));
    }

    let mut carplay = None;
    let mut best = u8::MAX;
    for device in &devices {
        let iface = device.interface().await.unwrap_or_default();
        let score = plugged_in(&iface);
        if score < best {
            best = score;
            carplay = Some(device.clone());
        }
    }
    let carplay = carplay.unwrap_or_else(|| devices[0].clone());

    match role {
        WifiRole::Carplay => Ok(carplay),
        WifiRole::Uplink => {
            let carplay_path = carplay.inner().path().to_owned();
            devices
                .into_iter()
                .find(|d| d.inner().path() != &carplay_path)
                .ok_or(eyre!(
                    "Only one wifi adapter, and CarPlay has it - the uplink needs a second one"
                ))
        }
    }
}

/// The adapter the accessory's connection runs on.
async fn wifi_device(wifi: &Wifi) -> Result<DeviceProxy<'static>> {
    role_device(WifiRole::Carplay, wifi.selector(WifiRole::Carplay)).await
}

async fn get_wifi_mac(wifi: &Wifi) -> Result<String> {
    let device = wifi_device(wifi).await?;
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
            let wifi = carjack::config::Wifi::default();
            let found = super::live_connection(&wifi, &ssid)
                .await
                .expect("querying the current connection failed")
                .expect("the SSID we are on should match");
            assert!(!found.iface.is_empty());

            assert!(
                super::live_connection(&wifi, "carjack-definitely-not-connected")
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
                Some("AA:BB:CC:DD:EE:FF"),
                "wlan0",
            );

            let stored = settings
                .add_connection_unsaved(&profile)
                .await
                .expect("NetworkManager rejected the accessory profile");
            let read_back = stored.get_settings().await.expect("couldn't read it back");
            stored.delete().await.expect("couldn't delete it");

            // One profile per accessory rests on NetworkManager storing the UUID carjack chose,
            // rather than minting one of its own.
            let connection = read_back
                .connection
                .as_ref()
                .expect("no connection setting");
            assert_eq!(connection.id.as_deref(), Some("carjack-profile-check"));
            assert_eq!(
                connection.uuid.clone(),
                Some(
                    super::accessory_uuid("carjack-profile-check", Some("AA:BB:CC:DD:EE:FF"))
                        .to_string()
                )
            );
            assert_eq!(super::accessory_tag(&read_back), Some("AA:BB:CC:DD:EE:FF"));

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
            None,
            "wlan0",
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
            None,
            "wlan0",
        );
        assert_eq!(
            profile.wireless.expect("no wireless setting").disable_he,
            Some(false)
        );
    }

    /// The accessory's link carries no internet, and on a two-adapter board it is not the only
    /// link. Both of these keep it from taking over the default route or the wrong radio.
    #[test]
    fn the_accessory_profile_is_pinned_and_never_the_default_route() {
        let profile = super::accessory_profile(
            "bench-1234",
            "hunter2hunter2",
            AssignedMacAddress::Permanent,
            false,
            Some("AA:BB:CC:DD:EE:FF"),
            "wlp2s0",
        );
        assert_eq!(
            profile
                .connection
                .as_ref()
                .expect("no connection setting")
                .interface_name
                .as_deref(),
            Some("wlp2s0")
        );
        assert_eq!(
            profile.ipv4.expect("no ipv4 setting").never_default,
            Some(true)
        );
        assert_eq!(
            profile.ipv6.expect("no ipv6 setting").never_default,
            Some(true)
        );
    }

    fn profile(ssid: &str, mac: Option<&str>) -> nm_automata::ConnectionProfile {
        super::accessory_profile(
            ssid,
            "hunter2hunter2",
            AssignedMacAddress::Address(announced()),
            false,
            mac,
            "wlan0",
        )
    }

    fn announced() -> nm_automata::MacAddress {
        nm_automata::MacAddress::new([0xFE, 0x01, 0x02, 0x03, 0x04, 0x05])
    }

    /// A unit that renames its network on every boot must still get one profile, not one per boot.
    #[test]
    fn the_uuid_follows_the_accessory_not_the_ssid() {
        let first = profile("bench-1234", Some("AA:BB:CC:DD:EE:FF"));
        let renamed = profile("bench-5678", Some("aa:bb:cc:dd:ee:ff"));
        let other_unit = profile("bench-1234", Some("11:22:33:44:55:66"));

        assert_eq!(super::profile_uuid(&first), super::profile_uuid(&renamed));
        assert_ne!(
            super::profile_uuid(&first),
            super::profile_uuid(&other_unit)
        );
        assert_eq!(super::accessory_tag(&first), Some("AA:BB:CC:DD:EE:FF"));
    }

    /// Without the accessory's address there is nothing better to key on than the SSID.
    #[test]
    fn an_unknown_accessory_falls_back_to_the_ssid() {
        assert_eq!(
            super::profile_uuid(&profile("bench-1234", None)),
            super::profile_uuid(&profile("bench-1234", None))
        );
        assert_ne!(
            super::profile_uuid(&profile("bench-1234", None)),
            super::profile_uuid(&profile("bench-5678", None))
        );
        assert_eq!(super::accessory_tag(&profile("bench-1234", None)), None);
    }

    #[test]
    fn the_accessorys_own_profile_is_never_collected() {
        // Stored under an older SSID, but the same UUID - the one to rewrite, not delete.
        let want = profile("bench-5678", Some("AA:BB:CC:DD:EE:FF"));
        assert!(!super::superseded_by(
            &profile("bench-1234", Some("AA:BB:CC:DD:EE:FF")),
            &want
        ));
    }

    #[test]
    fn other_accessories_are_left_alone_unless_they_hold_the_ssid() {
        let want = profile("bench-5678", Some("AA:BB:CC:DD:EE:FF"));
        assert!(!super::superseded_by(
            &profile("other-unit", Some("11:22:33:44:55:66")),
            &want
        ));
        // Two units advertising the same SSID: the stored one would compete for the AP.
        assert!(super::superseded_by(
            &profile("bench-5678", Some("11:22:33:44:55:66")),
            &want
        ));
    }

    /// A profile tagged for this accessory but keyed under some other UUID.
    #[test]
    fn a_rekeyed_profile_for_the_same_accessory_is_superseded() {
        let want = profile("bench-5678", Some("AA:BB:CC:DD:EE:FF"));
        let mut rekeyed = profile("bench-1234", Some("AA:BB:CC:DD:EE:FF"));
        rekeyed
            .connection
            .as_mut()
            .expect("no connection setting")
            .uuid = Some("2815492f-7e56-435e-b2e9-246bd7cdc664".to_owned());
        assert!(super::superseded_by(&rekeyed, &want));
    }

    /// An untagged profile is carjack's if it is pinned to the address only carjack invents.
    #[test]
    fn untagged_profiles_pinned_to_our_address_are_superseded() {
        let want = profile("bench-5678", Some("AA:BB:CC:DD:EE:FF"));
        assert!(super::superseded_by(&profile("bench-1234", None), &want));

        let someone_elses = super::accessory_profile(
            "home wifi",
            "hunter2hunter2",
            AssignedMacAddress::Permanent,
            false,
            None,
            "wlan0",
        );
        assert!(!super::superseded_by(&someone_elses, &want));
    }

    /// An ethernet or VPN profile is never carjack's.
    #[test]
    fn non_wireless_profiles_are_never_superseded() {
        let wired = nm_automata::ConnectionProfile::new("wired");
        let want = profile("bench-5678", Some("AA:BB:CC:DD:EE:FF"));
        assert!(!super::superseded_by(&wired, &want));
    }
}
