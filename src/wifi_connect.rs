use backon::{ConstantBuilder, Retryable, RetryableWithContext};
use enclose::enclose;
use eyre::{ErrReport, Result, eyre};
use futures::TryStreamExt;
use ipnet::IpNet;
use nmrs::NetworkManager;
use nmrs::builders::{WifiBand, WifiConnectionBuilder};
use rtnetlink::Handle;
use rtnetlink::packet_route::address::{
    AddressAttribute, AddressFlags, AddressHeaderFlags, AddressMessage,
};
use std::net::{IpAddr, Ipv6Addr};
use std::time::Duration;
use tokio::sync::OnceCell;
use tokio_util::task::AbortOnDropHandle;
use zbus::{proxy, zvariant::OwnedObjectPath};

static NETWORK_MANAGER: OnceCell<NetworkManager> = OnceCell::const_new();

pub(crate) async fn get_network_manager() -> &'static NetworkManager {
    NETWORK_MANAGER
        .get_or_init(async || {
            NetworkManager::new()
                .await
                .expect("Unable to get networkmanager")
        })
        .await
}

pub struct CarWifi {
    #[allow(dead_code)]
    pub ssid: String,
    #[allow(dead_code)]
    pub ip: IpAddr,
    pub gateway: IpAddr,
    pub iface: String,
    pub iface_mac: String,
}

pub(crate) async fn connect_to_car_wifi(ssid: &str, passphrase: &str) -> Result<CarWifi> {
    // Already sitting on the car's AP? Reuse the live connection rather than activating a new
    // profile over the top of it. Reassociating costs several seconds for no gain, and because a
    // fresh association regenerates the interface's link-local address it drops us straight back
    // into the duplicate-address-detection window that `wait_for_link_local` exists to cover -
    // in `cfmoto_11.pcapng` the host was already on the car's SSID and we tore it down anyway.
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

    let settings = WifiConnectionBuilder::new(ssid)
        .wpa_psk(passphrase)
        .band(WifiBand::A)
        .hidden(true)
        .ipv4_auto()
        .build();

    let car_wifi = enclose!(
        (settings)
            || async {
                let (conn_path, active_path) = get_network_manager()
                    .await
                    .add_and_activate_connection(settings, None, None)
                    .await?;
                tracing::info!("Connection created: {}, active: {}", conn_path, active_path);
                let car_wifi = get_car_wifi(active_path).await?;
                Ok::<_, ErrReport>(car_wifi)
            }
    )
    .retry(ConstantBuilder::new().with_delay(Duration::from_secs(2)))
    .await?;

    Ok(car_wifi)
}

/// The current WiFi connection, if it is already on `ssid` and carries an address - i.e. there is
/// nothing left for `connect_to_car_wifi` to do. `None` means "not it, go and connect".
async fn live_connection(ssid: &str) -> Result<Option<CarWifi>> {
    let Some(network) = get_network_manager().await.current_network().await? else {
        return Ok(None);
    };
    // No address yet means the activation is still in flight; let the normal path drive it rather
    // than handing back a half-configured connection.
    if network.ssid != ssid || network.ip4_address.is_none() {
        return Ok(None);
    }
    let active_path = active_connection_path(&network.device).await?;
    Ok(Some(get_car_wifi(active_path).await?))
}

/// The `ActiveConnection` object path for an interface. `ActiveWifiConnection` doesn't carry the
/// path and `get_car_wifi` needs one to read the gateway off IP4Config, so go via the device.
async fn active_connection_path(iface: &str) -> Result<OwnedObjectPath> {
    let nm = get_network_manager().await;
    let device_path = nm.get_device_by_interface(iface).await?;
    let active = DeviceProxy::new(nm.dbus_connection(), device_path)
        .await?
        .active_connection()
        .await?;
    if active.as_str() == "/" {
        return Err(eyre!("{iface} has no active connection"));
    }
    Ok(active)
}

async fn get_car_wifi(active_path: OwnedObjectPath) -> Result<CarWifi> {
    let network = get_network_manager()
        .await
        .current_network()
        .await?
        .ok_or(eyre!("No network connection"))?;
    let addr = network
        .ip4_address
        .ok_or(eyre!("No address on current connection"))?;
    let ipnet = addr.parse::<IpNet>().expect("Error parsing addr");
    let ip = ipnet.addr();

    let gateway = get_gateway(active_path).await?;

    Ok(CarWifi {
        ssid: network.ssid,
        ip,
        iface: network.device,
        iface_mac: get_wifi_mac().await?,
        gateway,
    })
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
/// lands squarely inside that window - which is exactly what killed the session in
/// `cfmoto_11.pcapng` (the car's AirPlay address from `0x4301` is link-local).
pub(crate) async fn wait_for_link_local(iface: &str) -> Result<Ipv6Addr> {
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

    tracing::info!("{iface} link-local {addr} is usable as a source address");
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
    let network = get_network_manager()
        .await
        .current_network()
        .await?
        .ok_or(eyre!("No network connection"))?;
    let device = get_network_manager()
        .await
        .wifi_device_by_interface(&network.device)
        .await?;
    Ok(device.hw_address)
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.Connection.Active",
    default_service = "org.freedesktop.NetworkManager"
)]
trait ActiveConnection {
    #[zbus(property)]
    fn ip4_config(&self) -> zbus::Result<OwnedObjectPath>;
}
#[proxy(
    interface = "org.freedesktop.NetworkManager.Device",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Device {
    #[zbus(property)]
    fn active_connection(&self) -> zbus::Result<OwnedObjectPath>;
}
#[proxy(
    interface = "org.freedesktop.NetworkManager.IP4Config",
    default_service = "org.freedesktop.NetworkManager"
)]
trait IP4Config {
    #[zbus(property)]
    fn gateway(&self) -> zbus::Result<String>;
}

async fn get_gateway(active_path: OwnedObjectPath) -> Result<IpAddr> {
    let conn = get_network_manager().await.dbus_connection();
    let active_connection_proxy = ActiveConnectionProxy::new(conn, active_path).await?;
    let ip4_config_path = active_connection_proxy.ip4_config().await?;
    tracing::debug!(%ip4_config_path);
    let ip4_config_proxy = IP4ConfigProxy::new(conn, ip4_config_path).await?;
    let gateway: IpAddr = ip4_config_proxy.gateway().await?.parse()?;
    tracing::debug!(?gateway);
    Ok(gateway)
}

#[cfg(test)]
mod tests {
    use super::{dad_finished, link_local_of};
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

    /// Live check against the running kernel: the netlink dump has to actually come back with
    /// this host's link-local and report it as settled. Names the interface via
    /// `CARJACK_TEST_IFACE` because there is no portable one to assume (`lo` has no link-local).
    #[tokio::test]
    #[ignore = "needs a real interface with an IPv6 link-local; set CARJACK_TEST_IFACE"]
    async fn link_local_is_found_on_a_real_interface() {
        let iface = std::env::var("CARJACK_TEST_IFACE").expect("set CARJACK_TEST_IFACE");
        let addr = super::wait_for_link_local(&iface)
            .await
            .expect("should find a settled link-local");
        assert!(addr.is_unicast_link_local(), "{addr} is not link-local");
    }

    /// Live check against NetworkManager: asking for the SSID we are already on must hand back
    /// the live connection so `connect_to_car_wifi` skips reactivating it, and any other SSID must
    /// not match. Read-only - it never activates or tears down anything.
    #[tokio::test]
    #[ignore = "needs a live NetworkManager WiFi connection; set CARJACK_TEST_SSID"]
    async fn live_connection_matches_only_the_current_ssid() {
        let ssid = std::env::var("CARJACK_TEST_SSID").expect("set CARJACK_TEST_SSID");
        let found = super::live_connection(&ssid)
            .await
            .expect("querying the current connection failed")
            .expect("the SSID we are on should match");
        assert_eq!(found.ssid, ssid);
        assert!(!found.iface.is_empty());

        assert!(
            super::live_connection("carjack-definitely-not-connected")
                .await
                .expect("querying an unrelated SSID failed")
                .is_none()
        );
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
