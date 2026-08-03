use backon::{ConstantBuilder, Retryable, RetryableWithContext};
use std::str::FromStr;

use eyre::{Result, eyre};
use futures::{StreamExt, TryStreamExt};
use nm_automata::{
    ActivationOptions, ActivationPersist, ActivationStateFlags,
    ActiveConnectionState, ActiveProxy, AssignedMacAddress, ConnectionProfile, ConnectionSetting,
    DeviceProxy,
    MacAddress,
    DeviceState, DeviceType, Ipv4Method, Ipv4Setting, Ipv6Method, Ipv6Setting, NetworkManagerProxy,
    NonEmpty, ProxyExt, Ssid, WirelessBand, WirelessMode, WirelessProxy, WirelessSecurityKeyMgmt,
    WirelessSecuritySetting, WirelessSetting,
};
use rtnetlink::Handle;
use rtnetlink::packet_route::address::{
    AddressAttribute, AddressFlags, AddressHeaderFlags, AddressMessage,
};
use std::net::{IpAddr, Ipv6Addr};
use std::time::Duration;
use tokio::sync::OnceCell;
use tokio_util::task::AbortOnDropHandle;

/// How long to give NetworkManager to bring the car's connection all the way up before treating
/// the attempt as failed and retrying it. Association plus DHCP is a couple of seconds when it
/// works at all, and the car walks away from a session that stops making progress.
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(30);

static NETWORK_MANAGER: OnceCell<NetworkManagerProxy<'static>> = OnceCell::const_new();

/// The NetworkManager proxy, built once. Its D-Bus connection is what the car's profile binds its
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

pub struct CarWifi {
    #[allow(dead_code)]
    pub ssid: Ssid,
    #[allow(dead_code)]
    pub ip: IpAddr,
    pub gateway: IpAddr,
    pub iface: String,
    pub iface_mac: String,
}

pub(crate) async fn connect_to_car_wifi(ssid: &str, passphrase: &str) -> Result<CarWifi> {
    match live_connection(ssid).await {
        Ok(Some(car_wifi)) => {
            tracing::info!("Already connected to {ssid}; reusing the live connection");
            return Ok(car_wifi);
        }
        Ok(None) => {}
        Err(e) => {
            tracing::warn!("Couldn't inspect the current connection ({e}); connecting anyway")
        }
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
    let profile = car_profile(ssid, passphrase, announced_mac);
    (|| async {
        let nm = get_network_manager().await;
        let device = wifi_device().await?;
        let (conn, active) = match reusable_profile(nm, ssid, &profile).await {
            Some(existing) => {
                tracing::info!(path = ?existing.object_path(), "Reusing the car's profile");
                let active = nm
                    .activate_connection(
                        &existing.object_path(),
                        &device.object_path(),
                        // No specific access point: let NetworkManager pick the BSS for the SSID.
                        &zbus::zvariant::ObjectPath::from_static_str_unchecked("/"),
                    )
                    .await?;
                (existing, active)
            }
            None => {
                let (conn, active, _) = nm
                    .add_and_activate_connection2(
                        &profile,
                        &device.object_path(),
                        &Default::default(),
                        // Kept on disk, one per accessory: the SSID, passphrase and pinned address
                        // are stable for a car, so rebuilding it every session only costs time -
                        // and a known profile is what a later "already on the car's WiFi, skip the
                        // Bluetooth handshake" path would key off. Duplicates are swept above.
                        &ActivationOptions::default().persist(ActivationPersist::Disk),
                    )
                    .await?;
                tracing::info!(path = ?conn.object_path(), "Connection created");
                (conn, active)
            }
        };

        // Tidy up after ourselves on failure. Each attempt otherwise leaves a live profile behind,
        // and several profiles for one SSID is how an association ends up happening outside the
        // announce-then-activate sequence.
        if let Err(e) = tokio::time::timeout(ACTIVATION_TIMEOUT, wait_for_activation(&active))
            .await
            .map_err(|_| eyre!("Timed out bringing up the car's WiFi connection"))
            .and_then(|r| r)
        {
            if let Err(e) = conn.delete().await {
                tracing::debug!(?e, "Could not delete the profile of a failed attempt");
            }
            return Err(e);
        }
        // The whole point of pinning the profile's address is that the accessory sees the station
        // it was promised. If NetworkManager did not apply it, the association is live but the
        // accessory is waiting for a station that will never appear - a failure that otherwise
        // shows up much later, as an AirPlay session that never starts.
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
        car_wifi(&device, &active).await
    })
    .retry(ConstantBuilder::new().with_delay(Duration::from_secs(2)))
    // Each attempt leaves another in-memory profile behind, so a silent retry looks like nothing
    // more than a duplicate "Connection created" line.
    .notify(|e, _| tracing::warn!("Bringing up the car's WiFi failed ({e}); retrying"))
    .await
}

/// Find a profile we can reuse for this AP, deleting every other one for the same SSID.
///
/// One profile per accessory, kept on disk: the SSID, passphrase and our pinned address are all
/// stable for a given car, so recreating it every session only costs time. Duplicates are the
/// thing to avoid, not persistence - one accrued per run when this was in-memory, 74 of them after
/// a day of testing, and any of them can bring the link up outside the announce-then-activate
/// sequence with a stale pinned address the accessory was never told about.
///
/// A profile only qualifies if its SSID, passphrase *and* pinned address all still match: a car
/// that rotates its passphrase, or a machine whose derived address changed, must not reuse a
/// profile that would associate wrongly.
async fn reusable_profile(
    nm: &NetworkManagerProxy<'_>,
    ssid: &str,
    profile: &ConnectionProfile,
) -> Option<nm_automata::proxies::settings_connection::ConnectionProxy<'static>> {
    let settings = nm_automata::SettingsProxy::new(nm.inner().connection())
        .await
        .ok()?;
    let connections = settings.connections().await.ok()?;
    let mut keep: Option<nm_automata::proxies::settings_connection::ConnectionProxy<'static>> =
        None;
    let mut removed = 0usize;
    for conn in connections {
        let Ok(existing) = conn.get_settings().await else {
            continue;
        };
        // Match on the SSID the profile actually joins, not its name: NetworkManager renames a
        // colliding profile to "<ssid> 1", "<ssid> 2" and so on, and those escape a name check
        // while still being able to associate.
        let joins_this_ap = existing
            .wireless
            .as_ref()
            .and_then(|w| w.ssid.as_ref())
            .is_some_and(|s| s.as_bytes() == ssid.as_bytes());
        if !joins_this_ap {
            continue;
        }
        let matches = keep.is_none() && same_ap(&existing, profile);
        if matches {
            keep = Some(conn);
        } else if conn.delete().await.is_ok() {
            removed += 1;
        }
    }
    if removed > 0 {
        tracing::info!(removed, %ssid, "Removed duplicate profiles for the car's AP");
    }
    keep
}

/// Whether an existing profile would associate exactly as the one we just built.
///
/// Compares the pinned address, not the passphrase: NetworkManager omits secrets from
/// `GetSettings`, so a stored profile's PSK always reads as absent and comparing it would reject
/// every profile - which is how the first version of this silently recreated the profile each run.
/// A car that rotates its passphrase is handled by activation failing, which deletes the profile
/// and lets the retry build a fresh one.
fn same_ap(existing: &ConnectionProfile, wanted: &ConnectionProfile) -> bool {
    let mac = |p: &ConnectionProfile| {
        p.wireless
            .as_ref()
            .and_then(|w| w.assigned_mac_address.clone())
    };
    mac(existing) == mac(wanted)
}

/// The connection profile for the car's access point.
fn car_profile(ssid: &str, passphrase: &str, announced_mac: AssignedMacAddress) -> ConnectionProfile {
    ConnectionProfile::new(ssid)
        // Never let NetworkManager bring this up on its own. carjack activates it deliberately,
        // once, with an address it has just announced over iAP2; an autoconnecting profile
        // associates outside that sequence - possibly a stale one from an earlier session, with a
        // different pinned address - and the accessory then sees a station it was never promised.
        // A leftover autoconnecting profile has caused this before (see docs/status.md).
        .connection(ConnectionSetting::default().autoconnect(false))
        .wireless(
            WirelessSetting::default()
                .ssid(ssid)
                .mode(WirelessMode::Infrastructure)
                .band(WirelessBand::A)
                .hidden(true)
                // The accessory is told our station address over iAP2 before we associate, so the address
                // it is told has to be the one it then sees. NetworkManager randomises per profile by
                // default, and this profile is new every session - pin it to the address we
                // announced in 0x4300, or the accessory waits for a station that never appears.
                .assigned_mac_address(announced_mac),
        )
        .wireless_security(
            WirelessSecuritySetting::default()
                .key_mgmt(WirelessSecurityKeyMgmt::WpaPsk)
                .psk(passphrase),
        )
        .ipv4(Ipv4Setting::default().method(Ipv4Method::Auto))
        // Link-local rather than `auto`: the car answers no router solicitation, so SLAAC would only
        // ever time out, and the AirPlay address it hands us over iAP2 is link-local anyway. It also
        // makes NetworkManager's `IP6_READY` mean exactly "the link-local address is usable".
        .ipv6(Ipv6Setting::default().method(Ipv6Method::LinkLocal))
}

/// Wait until the activation has both address families configured.
///
/// **Both flags, not just `Activated`**: NetworkManager reports a connection activated once one
/// address family has succeeded, so waiting on the state alone hands back a connection whose
/// `Ip4Config` is still `/`. carjack uses each of them - the DHCP address and gateway to reach
/// AirPlay by route, the link-local as the source address that reaches it directly.
///
/// `IP6_READY` is what replaces watching duplicate-address detection by hand: with
/// `ipv6.method=link-local` NetworkManager's IPv6 stage is precisely "bring the link-local address
/// up", and it is not reported complete while that address is still tentative - which is what a
/// link-local destination cannot be reached without.
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
                "The car's WiFi connection went down while activating"
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
/// nothing left for `connect_to_car_wifi` to do. `None` means "not it, go and connect".
async fn live_connection(ssid: &str) -> Result<Option<CarWifi>> {
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
    match car_wifi(&device, &active).await {
        Ok(car_wifi) => Ok(Some(car_wifi)),
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

/// What the rest of carjack needs from an activated connection to the car.
async fn car_wifi(device: &DeviceProxy<'_>, active: &ActiveProxy<'_>) -> Result<CarWifi> {
    // Absent until NetworkManager has finished the IPv4 stage, which is what `wait_for_activation`
    // gates on - so reaching here without one means the activation is still in flight.
    let ip4 = active
        .ip4_config()
        .await?
        .ok_or(eyre!("The car's connection has no IPv4 configuration yet"))?;
    let ip: IpAddr = ip4
        .address_data()
        .await?
        .into_iter()
        .find_map(|address| address.address)
        .ok_or(eyre!("No address on the car's connection"))?
        .parse()?;
    // The bike is its own gateway, and that address is how AirPlay is reached when the
    // link-local route won't serve.
    let gateway: IpAddr = ip4
        .gateway()
        .await?
        .non_empty()
        .ok_or(eyre!("No gateway on the car's connection"))?
        .parse()?;

    Ok(CarWifi {
        ssid: current_ssid(device).await?.unwrap_or_default(),
        ip,
        iface: device.interface().await?,
        iface_mac: get_wifi_mac().await?,
        gateway,
    })
}

/// The WiFi device to run the car's connection on: whichever one is already up, else the first
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

/// Duplicate-address detection is a single solicitation with a 1s retransmit timer by default,
/// so a few seconds of polling is generous. Past that the address has either failed DAD or the
/// interface isn't doing IPv6 at all, and there is no point holding the car up any longer.
const DAD_POLL_DELAY: Duration = Duration::from_millis(250);
const DAD_POLL_ATTEMPTS: usize = 16;

/// Wait until `iface` has an IPv6 link-local address that has finished duplicate-address
/// detection, and return it.
///
/// A link-local destination can only be reached from a link-local *source* on the same link, and
/// the kernel will not select a **tentative** address as one: `connect()` fails immediately with
/// `EADDRNOTAVAIL` and nothing ever reaches the wire. NetworkManager reports a WiFi connection as
/// activated the moment IPv4 (DHCP) finishes, which is about a second before DAD completes on the
/// freshly generated link-local, so anything connecting straight after `connect_to_car_wifi`
/// lands squarely inside that window.
///
/// Kept alongside `wait_for_activation`'s `IP6_READY` gate to check it: if that flag really does
/// imply a settled link-local, this returns on its first pass and the elapsed time it logs stays
/// near zero. Delete it once that holds over a few sessions.
pub(crate) async fn wait_for_link_local(iface: &str) -> Result<Ipv6Addr> {
    let started = std::time::Instant::now();
    let index = nix::net::if_::if_nametoindex(iface)
        .map_err(|e| eyre!("Couldn't resolve interface index for {iface}: {e}"))?;

    let (connection, handle, _) = rtnetlink::new_connection()?;
    // The connection future drives the netlink socket - requests made on the handle only make
    // progress while it is polled - and closes the socket when dropped on return.
    let _connection = AbortOnDropHandle::new(tokio::spawn(connection));

    // The handle is threaded through the retry as context rather than cloned per attempt: each
    // attempt takes it and hands it back, so there is one handle over the one netlink socket.
    let (_handle, addr) = (async |handle: Handle| {
        let res = settled_link_local(&handle, index, iface).await;
        (handle, res)
    })
    .retry(
        ConstantBuilder::new()
            .with_delay(DAD_POLL_DELAY)
            .with_max_times(DAD_POLL_ATTEMPTS),
    )
    .context(handle)
    .await;
    let addr = addr?;

    tracing::info!(
        "{iface} link-local {addr} is usable as a source address, {:?} after IP6_READY",
        started.elapsed()
    );
    Ok(addr)
}

/// One pass over the interface's addresses, looking for a link-local that has settled.
async fn settled_link_local(handle: &Handle, index: u32, iface: &str) -> Result<Ipv6Addr> {
    let mut addrs = std::pin::pin!(
        handle
            .address()
            .get()
            .set_link_index_filter(index)
            .execute()
    );
    while let Some(msg) = addrs.try_next().await? {
        let Some(addr) = link_local_of(&msg) else {
            continue;
        };
        if dad_finished(&msg) {
            return Ok(addr);
        }
        tracing::debug!("{iface}: link-local {addr} is still tentative");
    }
    Err(eyre!("no link-local address on {iface} has finished DAD"))
}

/// The address this `RTM_NEWADDR` is about, if it is an IPv6 link-local one.
fn link_local_of(msg: &AddressMessage) -> Option<Ipv6Addr> {
    msg.attributes.iter().find_map(|attr| match attr {
        AddressAttribute::Address(IpAddr::V6(v6)) if v6.is_unicast_link_local() => Some(*v6),
        _ => None,
    })
}

/// Whether the kernel will let this address be used as a source: DAD has run and passed.
fn dad_finished(msg: &AddressMessage) -> bool {
    // IPv6 addresses carry the full 32-bit flag set in an `IFA_FLAGS` attribute; the header byte
    // only has room for the low eight bits, so prefer the attribute wherever it is present.
    let attr_flags = msg.attributes.iter().find_map(|attr| match attr {
        AddressAttribute::Flags(flags) => Some(*flags),
        _ => None,
    });
    match attr_flags {
        Some(flags) => !flags.intersects(AddressFlags::Tentative | AddressFlags::Dadfailed),
        None => !msg
            .header
            .flags
            .intersects(AddressHeaderFlags::Tentative | AddressHeaderFlags::Dadfailed),
    }
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

pub(crate) async fn get_wifi_mac() -> Result<String> {

    let device = wifi_device().await?;
    let wifi: WirelessProxy = device.cast().await?;
    // Derived from the permanent address rather than being it - see `announced_wifi_mac`. The
    // car's profile pins itself to the same value, so what we announce is what associates.
    let permanent = match wifi.perm_hw_address().await?.non_empty() {
        Some(permanent) => permanent,
        None => device.hw_address().await?,
    };
    Ok(announced_wifi_mac(&permanent))
}

#[cfg(test)]
mod tests {
    use super::{dad_finished, link_local_of};
    use nm_automata::{
        AssignedMacAddress, Ipv4Method, Ipv6Method, Ssid, WirelessBand, WirelessMode,
        WirelessSecurityKeyMgmt,
    };
    use rtnetlink::packet_route::address::{
        AddressAttribute, AddressFlags, AddressHeaderFlags, AddressMessage,
    };

    fn message(attributes: Vec<AddressAttribute>, header: AddressHeaderFlags) -> AddressMessage {
        let mut msg = AddressMessage::default();
        msg.header.flags = header;
        msg.attributes = attributes;
        msg
    }

    /// Only IPv6 link-locals can serve as the source for the car's `fe80::` AirPlay address, so
    /// the interface's IPv4 and global-scope addresses must not be mistaken for one.
    #[test]
    fn only_ipv6_link_locals_are_recognised() {
        for addr in ["192.168.199.152", "2001:db8::1"] {
            let msg = message(
                vec![AddressAttribute::Address(addr.parse().unwrap())],
                AddressHeaderFlags::empty(),
            );
            assert_eq!(link_local_of(&msg), None, "{addr} should not count");
        }
        let msg = message(
            vec![AddressAttribute::Address(
                "fe80::d819:2e6:a304:3de0".parse().unwrap(),
            )],
            AddressHeaderFlags::empty(),
        );
        assert_eq!(
            link_local_of(&msg),
            Some("fe80::d819:2e6:a304:3de0".parse().unwrap())
        );
    }

    /// An address still doing DAD, or one that failed it, cannot be used as a source - connecting
    /// from it fails with `EADDRNOTAVAIL` before anything reaches the wire.
    #[test]
    fn tentative_and_failed_addresses_are_not_usable() {
        let ok = message(
            vec![AddressAttribute::Flags(AddressFlags::Permanent)],
            AddressHeaderFlags::Permanent,
        );
        assert!(dad_finished(&ok));

        for flag in [AddressFlags::Tentative, AddressFlags::Dadfailed] {
            let msg = message(
                vec![AddressAttribute::Flags(AddressFlags::Permanent | flag)],
                AddressHeaderFlags::Permanent,
            );
            assert!(!dad_finished(&msg), "{flag:?} should block use");
        }
    }

    /// The kernel puts the full 32-bit flag set in `IFA_FLAGS` and leaves the 8-bit header copy
    /// behind, so the attribute has to win wherever both are present.
    #[test]
    fn ifa_flags_attribute_wins_over_the_header_byte() {
        let stale_header = message(
            vec![AddressAttribute::Flags(AddressFlags::Permanent)],
            AddressHeaderFlags::Tentative,
        );
        assert!(dad_finished(&stale_header));

        let stale_attribute = message(
            vec![AddressAttribute::Flags(AddressFlags::Tentative)],
            AddressHeaderFlags::Permanent,
        );
        assert!(!dad_finished(&stale_attribute));
    }

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

    /// Live check against the running kernel: the netlink dump has to actually come back with
    /// this host's link-local and report it as settled. Names the interface via
    /// `CARJACK_TEST_IFACE` because there is no portable one to assume (`lo` has no link-local).
    #[test]
    #[ignore = "needs a real interface with an IPv6 link-local; set CARJACK_TEST_IFACE"]
    fn link_local_is_found_on_a_real_interface() {
        runtime().block_on(async {
            let iface = std::env::var("CARJACK_TEST_IFACE").expect("set CARJACK_TEST_IFACE");
            let addr = super::wait_for_link_local(&iface)
                .await
                .expect("should find a settled link-local");
            assert!(addr.is_unicast_link_local(), "{addr} is not link-local");
        });
    }

    /// Live check against NetworkManager: asking for the SSID we are already on must hand back
    /// the live connection so `connect_to_car_wifi` skips reactivating it, and any other SSID must
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
            assert_eq!(found.ssid, Ssid::from(ssid.as_str()));
            assert!(!found.iface.is_empty());
            // The accessory is told this address over iAP2 and then has to see it associate, so a
            // lowercase colon-separated MAC is part of the contract, not just a formatting nicety.
            assert_eq!(found.iface_mac.len(), 17, "{}", found.iface_mac);
            assert_eq!(found.iface_mac, found.iface_mac.to_lowercase());

            assert!(
                super::live_connection("carjack-definitely-not-connected")
                    .await
                    .expect("querying an unrelated SSID failed")
                    .is_none()
            );
        });
    }

    /// Live check against NetworkManager: the profile carjack builds for the car has to be one
    /// the daemon accepts and stores as written - a mistyped property is refused outright, and a
    /// misnamed one is silently kept somewhere carjack's fields don't reach. Created unsaved so
    /// it never touches disk, read back, then deleted; never activated, and its SSID is one no
    /// access point advertises.
    #[test]
    #[ignore = "needs a live NetworkManager"]
    fn car_profile_is_accepted_by_networkmanager() {
        runtime().block_on(async {
            let nm = super::get_network_manager().await;
            let settings = nm_automata::SettingsProxy::new(nm.inner().connection())
                .await
                .expect("no settings interface");
            let profile = super::car_profile(
                "carjack-profile-check",
                "hunter2hunter2",
                AssignedMacAddress::Permanent,
            );

            let stored = settings
                .add_connection_unsaved(&profile)
                .await
                .expect("NetworkManager rejected the car profile");
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

    /// Without an `IFA_FLAGS` attribute there is nothing but the header byte to go on.
    #[test]
    fn the_header_byte_is_the_fallback() {
        assert!(dad_finished(&message(
            vec![],
            AddressHeaderFlags::Permanent
        )));
        assert!(!dad_finished(&message(
            vec![],
            AddressHeaderFlags::Tentative
        )));
    }
}
