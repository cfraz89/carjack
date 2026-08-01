use backon::{ConstantBuilder, Retryable, RetryableWithContext};
use eyre::{Result, eyre};
use futures::{StreamExt, TryStreamExt};
use nm_automata::{
    ActivationStateFlags, ActiveConnectionState, ActiveProxy, AssignedMacAddress,
    ConnectionProfile, ConnectionSetting, DeviceProxy, DeviceState, DeviceType, Ipv4Method,
    Ipv4Setting, Ipv6Method, Ipv6Setting, NetworkManagerProxy, NmPath, NonEmpty, ProxyExt, Ssid,
    WirelessBand, WirelessMode, WirelessProxy, WirelessSecurityKeyMgmt, WirelessSecuritySetting,
    WirelessSetting,
};
use rtnetlink::Handle;
use rtnetlink::packet_route::address::{
    AddressAttribute, AddressFlags, AddressHeaderFlags, AddressMessage,
};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::time::Duration;
use tokio::sync::OnceCell;
use tokio_util::task::AbortOnDropHandle;
use zbus::zvariant::Value;

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

    let profile = car_profile(ssid, passphrase);
    (|| async {
        let nm = get_network_manager().await;
        let device = wifi_device().await?;
        // Per-session credentials: the profile should not outlive the process, and the SSID and
        // passphrase change every session, so a profile per run would accumulate on disk.
        let persist = Value::from("memory");
        let bind = Value::from("dbus-client");
        let options = HashMap::from([("persist", &persist), ("bind-activation", &bind)]);
        let (conn_path, active_path, _) = nm
            .add_and_activate_connection2(
                &profile,
                &device.object_path(),
                &Default::default(),
                options,
            )
            .await?;
        tracing::info!("Connection created: {conn_path}, active: {active_path}");

        let active = active_path.connect(nm.inner().connection()).await?;
        tokio::time::timeout(ACTIVATION_TIMEOUT, wait_for_activation(&active))
            .await
            .map_err(|_| eyre!("Timed out bringing up the car's WiFi connection"))??;
        car_wifi(&device, &active).await
    })
    .retry(ConstantBuilder::new().with_delay(Duration::from_secs(2)))
    // Each attempt leaves another in-memory profile behind, so a silent retry looks like nothing
    // more than a duplicate "Connection created" line.
    .notify(|e, _| tracing::warn!("Bringing up the car's WiFi failed ({e}); retrying"))
    .await
}

/// The connection profile for the car's access point.
fn car_profile(ssid: &str, passphrase: &str) -> ConnectionProfile {
    ConnectionProfile::new(ssid)
        .wireless(
            WirelessSetting::default()
                .ssid(ssid)
                .mode(WirelessMode::Infrastructure)
                .band(WirelessBand::A)
                .hidden(true)
                // The accessory is told our station address over iAP2 before we associate, so the address
                // it is told has to be the one it then sees. NetworkManager randomises per profile by
                // default, and this profile is new every session - pin it to the permanent address.
                .assigned_mac_address(AssignedMacAddress::Permanent),
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
    let wifi: WirelessProxy = device.cast().await?;
    let Some(access_point) = wifi.active_access_point().await? else {
        return Ok(None);
    };
    Ok(Some(access_point.ssid().await?))
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

pub(crate) async fn get_wifi_mac() -> Result<String> {
    let device = wifi_device().await?;
    let wifi: WirelessProxy = device.cast().await?;
    // The permanent address, not the current one: NetworkManager randomises per profile, and the
    // car's connection pins itself to `permanent` so that what we announce is what associates.
    let address = match wifi.perm_hw_address().await?.non_empty() {
        Some(permanent) => permanent,
        None => device.hw_address().await?,
    };
    Ok(address.to_lowercase())
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
            let profile = super::car_profile("carjack-profile-check", "hunter2hunter2");

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
