//! Joining the car's access point, over iwd.
//!
//! iwd has no connection-profile API: a network is provisioned by a file in its state directory,
//! and `Network.Connect()` is the whole of the API. The Agent API is no substitute - `AddressOverride`
//! has no D-Bus equivalent, and `Device.Address`/`KnownNetwork.Hidden` are read-only. So this
//! writes the file (via the privileged sidecar), scans, then connects.

use backon::{ConstantBuilder, Retryable};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use eyre::{Result, eyre};
use futures::{StreamExt, TryStreamExt};
use iwd_automata::{
    DaemonProxy, DeviceProxy, NetworkProxy, ProxyExt, Ssid, StationProxy, StationState, discover,
};
use netlink_packet_route::AddressFamily;
use netlink_packet_route::address::{
    AddressAttribute, AddressFlags, AddressHeaderFlags, AddressMessage, AddressScope,
};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::OnceCell;

/// How long to give iwd to associate and configure IP before retrying.
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(30);

/// How long to wait for a scan to settle before reading its results anyway.
const SCAN_TIMEOUT: Duration = Duration::from_secs(15);

/// How long to wait after association for the link-local IPv6 to settle, and how often to poll.
const ADDRESS_TIMEOUT: Duration = Duration::from_secs(20);
const ADDRESS_POLL: Duration = Duration::from_millis(200);

/// How long to wait for iwd to expose a Station after powering a device up, and how often to poll.
const POWER_UP_TIMEOUT: Duration = Duration::from_secs(10);
const POWER_UP_POLL: Duration = Duration::from_millis(200);

/// Where iwd keeps provisioning files if `Daemon.GetInfo` doesn't say.
const DEFAULT_STATE_DIR: &str = "/var/lib/iwd";

static IWD: OnceCell<zbus::Connection> = OnceCell::const_new();

/// The system bus connection every iwd proxy is built on. Held for the process's life, since
/// proxies cache properties via `PropertiesChanged` subscriptions.
async fn iwd() -> &'static zbus::Connection {
    IWD.get_or_init(async || {
        zbus::Connection::system()
            .await
            .expect("Unable to reach the system bus")
    })
    .await
}

pub struct CarWifi {
    #[allow(dead_code)]
    pub ssid: Ssid,
    pub iface: String,
}

pub(crate) async fn connect_to_car_wifi(ssid: &str, passphrase: &str) -> Result<CarWifi> {
    let ssid = Ssid::from(ssid);
    match live_connection(&ssid).await {
        Ok(Some(car_wifi)) => {
            tracing::info!("Already connected to {ssid}; reusing the live connection");
            return Ok(car_wifi);
        }
        Ok(None) => {}
        Err(e) => {
            tracing::warn!("Couldn't inspect the current connection ({e}); connecting anyway")
        }
    }

    // The address already announced to the accessory over iAP2.
    let announced = get_wifi_mac().await?;
    provision(&ssid, passphrase, &announced).await?;

    (|| async {
        let (station, device) = car_station().await?;
        let iface = device.name().await?;
        let network = car_network(&station, &ssid).await?;

        if let Err(e) = tokio::time::timeout(ACTIVATION_TIMEOUT, network.connect())
            .await
            .map_err(|_| eyre!("Timed out bringing up the car's WiFi connection"))
            .and_then(|r| r.map_err(Into::into))
        {
            // Abort so the next attempt doesn't fail `InProgress` on our own leftover connect.
            if let Err(e) = station.disconnect().await {
                tracing::debug!(?e, "Could not abort a failed connection attempt");
            }
            return Err(e);
        }

        // Confirm we associated as the address announced over iAP2 - the accessory waits for it.
        match current_mac(&iface) {
            Ok(actual) if actual.eq_ignore_ascii_case(&announced) => {
                tracing::debug!(%actual, "Associated as the announced address")
            }
            Ok(actual) => tracing::warn!(
                %actual,
                %announced,
                "Associated address is not the one announced over iAP2; the accessory is \
                 watching for a station that will not appear. iwd ignores \
                 [Settings].AddressOverride unless /etc/iwd/main.conf has \
                 [General] AddressRandomization=network"
            ),
            Err(e) => tracing::debug!(?e, "Could not read back the associated address"),
        }

        wait_for_addresses(&iface).await?;
        Ok(CarWifi {
            ssid: ssid.clone(),
            iface,
        })
    })
    .retry(ConstantBuilder::new().with_delay(Duration::from_secs(2)))
    .notify(|e, _| tracing::warn!("Bringing up the car's WiFi failed ({e}); retrying"))
    .await
}

/// Write the car's network into iwd's state directory - the only way to say what `Connect()`
/// cannot: the passphrase, that the SSID is hidden, no autoconnect, and our station address.
/// Rewritten before every attempt, since the car's passphrase changes per session.
async fn provision(ssid: &Ssid, passphrase: &str, announced_mac: &str) -> Result<()> {
    if !(8..=63).contains(&passphrase.len()) || passphrase.contains(['\n', '\r']) {
        return Err(eyre!(
            "The car offered a {}-character passphrase; WPA-PSK needs 8 to 63",
            passphrase.len()
        ));
    }
    let info = DaemonProxy::new(iwd().await).await?.get_info().await?;
    if info.network_configuration_enabled != Some(true) {
        return Err(eyre!(
            "iwd is not configuring IP addresses: set [General] EnableNetworkConfiguration=true \
             in /etc/iwd/main.conf and restart iwd. carjack needs the DHCP lease the car hands out"
        ));
    }
    let dir = PathBuf::from(info.state_directory.as_deref().unwrap_or(DEFAULT_STATE_DIR));
    let body = provisioning_file(passphrase, announced_mac);
    let name = network_file_name(ssid);
    write_profile(&name, &body).await.map_err(|e| {
        eyre!(
            "Could not provision {}: {e}. The sidecar needs cap_dac_override to write iwd's \
             state directory (root-owned, mode 0700) and cap_chown to leave the profile owned \
             by root, which is the only way iwd can read it; run `just setcap`",
            dir.join(&name).display()
        )
    })?;
    tracing::info!(path = %dir.join(&name).display(), "Provisioned the car's network for iwd");
    Ok(())
}

const SIDECAR: &str = "carjack-wireless-helper";

async fn write_profile(name: &str, body: &str) -> Result<()> {
    let mut child = tokio::process::Command::new(SIDECAR)
        .arg("wifi-profile")
        .arg(name)
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| eyre!("could not run {SIDECAR} (is it installed via `just install`?): {e}"))?;
    // Dropped before the wait, or the helper blocks on a pipe we still hold.
    let mut stdin = child.stdin.take().expect("stdin was piped");
    stdin.write_all(body.as_bytes()).await?;
    drop(stdin);

    let out = child.wait_with_output().await?;
    if !out.status.success() {
        return Err(eyre!(
            "{SIDECAR} {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// The provisioning file's contents; see [`provision`] for what each key does.
fn provisioning_file(passphrase: &str, announced_mac: &str) -> String {
    format!(
        "[Security]\n\
         Passphrase={passphrase}\n\
         \n\
         [Settings]\n\
         AutoConnect=false\n\
         Hidden=true\n\
         AddressOverride={announced_mac}\n\
         \n\
         [IPv6]\n\
         Enabled=false\n"
    )
}

/// The file name iwd derives for a WPA-PSK network: the literal SSID if every byte is
/// alphanumeric/`-`/`_`/space and under 32 bytes, else `=<lowercase hex>`. Getting this wrong is
/// silent - iwd just treats the file as absent.
fn network_file_name(ssid: &Ssid) -> String {
    let ssid = ssid.as_str();
    let plain = !ssid.is_empty()
        && ssid.len() <= 32
        && ssid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b' '));
    if plain {
        return format!("{ssid}.psk");
    }
    let mut name = String::with_capacity(ssid.len() * 2 + 5);
    name.push('=');
    for byte in ssid.as_bytes() {
        // Lower case: iwd re-derives the lower-case path when it opens the file.
        let _ = write!(name, "{byte:02x}");
    }
    name.push_str(".psk");
    name
}

/// The car's network object, scanning if the last scan didn't find it. Must run after
/// provisioning - only then does iwd know to probe for a hidden SSID.
async fn car_network(station: &StationProxy<'_>, ssid: &Ssid) -> Result<NetworkProxy<'static>> {
    if let Some(network) = find_network(station, ssid).await? {
        return Ok(network);
    }
    scan(station).await?;
    find_network(station, ssid)
        .await?
        .ok_or_else(|| eyre!("iwd's scan did not find the car's network {ssid}"))
}

async fn find_network(
    station: &StationProxy<'_>,
    ssid: &Ssid,
) -> Result<Option<NetworkProxy<'static>>> {
    for (network, _strength) in station.get_ordered_networks().await? {
        if &network.name().await? == ssid {
            return Ok(Some(network));
        }
    }
    Ok(None)
}

/// Trigger a scan and wait for it to settle. Errors are swallowed - a scan already running answers
/// `Busy`, and stale results are still worth reading; the caller retries if the car isn't found.
async fn scan(station: &StationProxy<'_>) -> Result<()> {
    let mut changes = station.receive_scanning_changed().await;
    if let Err(e) = station.scan().await {
        tracing::debug!(?e, "Scan request refused; waiting on whatever is running");
    }
    let settled = tokio::time::timeout(SCAN_TIMEOUT, async {
        let mut started = station.scanning().await.unwrap_or(false);
        while let Some(change) = changes.next().await {
            match change.get().await {
                Ok(true) => started = true,
                Ok(false) if started => return,
                _ => {}
            }
        }
    })
    .await;
    if settled.is_err() {
        tracing::debug!("Scan did not report finished within {SCAN_TIMEOUT:?}; reading it anyway");
    }
    Ok(())
}

/// The current connection, if it's already on `ssid` and carries an address - i.e. nothing left
/// for `connect_to_car_wifi` to do. `None` means "not it, go and connect".
async fn live_connection(ssid: &Ssid) -> Result<Option<CarWifi>> {
    let (station, device) = car_station().await?;
    if station.state().await? != StationState::Connected {
        return Ok(None);
    }
    let Some(network) = station.connected_network().await? else {
        return Ok(None);
    };
    if &network.name().await? != ssid {
        return Ok(None);
    }
    let iface = device.name().await?;
    if link_local_is_ready(&iface).await? {
        Ok(Some(CarWifi {
            ssid: ssid.clone(),
            iface,
        }))
    } else {
        tracing::debug!("On {ssid} already, but its link-local IPv6 hasn't settled yet");
        Ok(None)
    }
}

/// The station carjack drives: whichever is already connected, else the first iwd lists.
///
/// A powered-down device carries no Station interface, so power-up goes through `Device` first,
/// then waits for the Station to appear.
async fn car_station() -> Result<(StationProxy<'static>, DeviceProxy<'static>)> {
    if let Some(found) = find_station().await? {
        return Ok(found);
    }
    power_up_devices().await?;
    let deadline = tokio::time::Instant::now() + POWER_UP_TIMEOUT;
    loop {
        if let Some(found) = find_station().await? {
            return Ok(found);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(eyre!(
                "iwd exposes no station on any WiFi device after {POWER_UP_TIMEOUT:?}. \
                 `iwctl device list` shows why: a device in ap or ad-hoc mode carries no Station \
                 interface, and carjack will not change another program's mode out from under it"
            ));
        }
        tokio::time::sleep(POWER_UP_POLL).await;
    }
}

/// The station to use, if iwd has one. `Device` lives on the same object path as `Station`, so
/// the cast is free and gives the interface name that `Station` doesn't carry.
async fn find_station() -> Result<Option<(StationProxy<'static>, DeviceProxy<'static>)>> {
    let stations = discover(iwd().await).await?.stations;
    let mut chosen = None;
    for station in &stations {
        if station.state().await? == StationState::Connected {
            chosen = Some(station.clone());
            break;
        }
    }
    let Some(station) = chosen.or_else(|| stations.first().cloned()) else {
        return Ok(None);
    };
    let device: DeviceProxy<'static> = station.cast().await?;
    Ok(Some((station, device)))
}

/// Bring every WiFi radio up, so that iwd starts exposing stations on them.
async fn power_up_devices() -> Result<()> {
    let devices = discover(iwd().await).await?.devices;
    if devices.is_empty() {
        return Err(eyre!("iwd has no WiFi devices"));
    }
    for device in &devices {
        if !device.powered().await? {
            let name = device.name().await?;
            tracing::info!(%name, "Powering up the WiFi device");
            device.set_powered(true).await?;
        }
    }
    Ok(())
}

/// Wait for the interface's link-local IPv6 to settle out of duplicate-address detection - that's
/// the address AirPlay is actually reached on. A destination is unreachable from a still-tentative
/// source, `connect()` failing `EADDRNOTAVAIL` before anything hits the wire.
async fn wait_for_addresses(iface: &str) -> Result<()> {
    let deadline = tokio::time::Instant::now() + ADDRESS_TIMEOUT;
    loop {
        if link_local_is_ready(iface).await? {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(eyre!(
                "{iface} has no settled link-local IPv6 after {ADDRESS_TIMEOUT:?}"
            ));
        }
        tokio::time::sleep(ADDRESS_POLL).await;
    }
}

/// The rtnetlink socket address queries run on, opened once and reused - iwd reports no address
/// state over D-Bus, so this reads the interface directly.
static RTNETLINK: OnceCell<rtnetlink::Handle> = OnceCell::const_new();

async fn rtnetlink_handle() -> Result<&'static rtnetlink::Handle> {
    RTNETLINK
        .get_or_try_init(|| async {
            let (connection, handle, _) =
                rtnetlink::new_connection().map_err(|e| eyre!("open rtnetlink socket: {e}"))?;
            tokio::spawn(connection);
            Ok::<_, eyre::Report>(handle)
        })
        .await
}

/// Whether `iface` has a settled (non-tentative) link-local IPv6 address, queried over rtnetlink.
async fn link_local_is_ready(iface: &str) -> Result<bool> {
    let index =
        nix::net::if_::if_nametoindex(iface).map_err(|e| eyre!("if_nametoindex {iface}: {e}"))?;
    let handle = rtnetlink_handle().await?;
    let mut addresses = handle
        .address()
        .get()
        .set_link_index_filter(index)
        .execute();
    while let Some(msg) = addresses
        .try_next()
        .await
        .map_err(|e| eyre!("rtnetlink address dump for {iface}: {e}"))?
    {
        if is_settled_link_local(&msg) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether a netlink address is a non-tentative IPv6 link-local address. Falls back to the
/// header's flags if the kernel didn't send the extended `IFA_FLAGS` attribute.
fn is_settled_link_local(msg: &AddressMessage) -> bool {
    if msg.header.family != AddressFamily::Inet6 || msg.header.scope != AddressScope::Link {
        return false;
    }
    let tentative = msg
        .attributes
        .iter()
        .find_map(|a| match a {
            AddressAttribute::Flags(flags) => Some(flags.contains(AddressFlags::Tentative)),
            _ => None,
        })
        .unwrap_or_else(|| msg.header.flags.contains(AddressHeaderFlags::Tentative));
    !tentative
}

/// The WiFi address carjack announces and associates with - derived from the permanent hardware
/// address (stable per machine) but locally administered, since a unit that has us in its
/// Bluetooth device table refuses to answer for the real permanent address. `CARJACK_WIFI_MAC`
/// overrides it for testing.
fn announced_wifi_mac(permanent: &str) -> String {
    if let Ok(mac) = std::env::var("CARJACK_WIFI_MAC") {
        tracing::warn!(%mac, "Announcing an overridden WiFi MAC");
        return mac;
    }
    // FNV-1a over the permanent address, then force the locally-administered unicast prefix.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in permanent.to_ascii_lowercase().as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let d = h.to_be_bytes();
    format!(
        "FE:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        d[0], d[1], d[2], d[3], d[4]
    )
}

pub(crate) async fn get_wifi_mac() -> Result<String> {
    let (_station, device) = car_station().await?;
    Ok(announced_wifi_mac(&permanent_mac(&device.name().await?)?))
}

/// The radio's permanent hardware address, read from sysfs rather than `Device.Address` - iwd
/// reports the *current* netdev address there, which becomes our own override once connected.
fn permanent_mac(iface: &str) -> Result<String> {
    sysfs_mac(&format!("/sys/class/net/{iface}/phy80211/macaddress"))
}

/// The address the interface is using right now.
fn current_mac(iface: &str) -> Result<String> {
    sysfs_mac(&format!("/sys/class/net/{iface}/address"))
}

fn sysfs_mac(path: &str) -> Result<String> {
    let mac = std::fs::read_to_string(path)?.trim().to_owned();
    if mac.is_empty() {
        return Err(eyre!("{path} is empty"));
    }
    Ok(mac)
}

#[cfg(test)]
mod tests {
    use iwd_automata::Ssid;
    use std::time::Duration;

    /// One runtime for every live test in this process - `iwd`'s cached D-Bus connection outlives
    /// any single `#[tokio::test]` runtime.
    static RUNTIME: std::sync::LazyLock<tokio::runtime::Runtime> = std::sync::LazyLock::new(|| {
        tokio::runtime::Runtime::new().expect("building a runtime for the live tests")
    });

    /// iwd rejects, silently, any literal SSID carrying a character outside `[A-Za-z0-9-_ ]`.
    #[test]
    fn network_file_name_escapes_what_iwd_cannot_read_literally() {
        assert_eq!(
            super::network_file_name(&Ssid::from("carplay_wifi_6840")),
            "carplay_wifi_6840.psk"
        );
        assert_eq!(
            super::network_file_name(&Ssid::from("My Network-1")),
            "My Network-1.psk"
        );
        // A dot is not alphanumeric, and iwd splits the security suffix at the *first* one.
        assert_eq!(
            super::network_file_name(&Ssid::from("car.plaý")),
            "=6361722e706c61c3bd.psk"
        );
        let long = "a".repeat(33);
        assert_eq!(
            super::network_file_name(&Ssid::from(long.as_str())),
            format!("={}.psk", "61".repeat(33))
        );
    }

    /// Every key here is load-bearing and none of them can be said over D-Bus - see `provision`.
    #[test]
    fn provisioning_file_pins_the_address_and_holds_iwd_back() {
        let file = super::provisioning_file("hunter2hunter2", "FE:B2:9F:ED:EA:72");
        assert!(file.contains("\nPassphrase=hunter2hunter2\n"));
        assert!(file.contains("\nAutoConnect=false\n"));
        assert!(file.contains("\nHidden=true\n"));
        assert!(file.contains("\nAddressOverride=FE:B2:9F:ED:EA:72\n"));
        assert!(file.contains("[IPv6]\nEnabled=false\n"));
    }

    #[test]
    fn settled_link_local_requires_scope_link_and_no_tentative_flag() {
        use netlink_packet_route::AddressFamily;
        use netlink_packet_route::address::{
            AddressAttribute, AddressFlags, AddressHeader, AddressHeaderFlags, AddressMessage,
            AddressScope,
        };

        // AddressMessage is #[non_exhaustive], so it's built off Default rather than a literal.
        let msg = |family, scope, flags: AddressFlags| {
            let mut msg = AddressMessage::default();
            msg.header = AddressHeader {
                family,
                scope,
                ..Default::default()
            };
            msg.attributes = vec![AddressAttribute::Flags(flags)];
            msg
        };

        assert!(super::is_settled_link_local(&msg(
            AddressFamily::Inet6,
            AddressScope::Link,
            AddressFlags::Permanent
        )));
        assert!(!super::is_settled_link_local(&msg(
            AddressFamily::Inet6,
            AddressScope::Link,
            AddressFlags::Tentative
        )));
        // A routable (global-scope) address is not a link-local one, tentative or not.
        assert!(!super::is_settled_link_local(&msg(
            AddressFamily::Inet6,
            AddressScope::Universe,
            AddressFlags::Permanent
        )));
        // IPv4 is never the link-local address AirPlay is reached on.
        assert!(!super::is_settled_link_local(&msg(
            AddressFamily::Inet,
            AddressScope::Link,
            AddressFlags::Permanent
        )));
        // No IFA_FLAGS attribute: falls back to the header's (older, 8-bit) flags.
        let mut header_only = AddressMessage::default();
        header_only.header = AddressHeader {
            family: AddressFamily::Inet6,
            scope: AddressScope::Link,
            flags: AddressHeaderFlags::Tentative,
            ..Default::default()
        };
        assert!(!super::is_settled_link_local(&header_only));
    }

    /// Live check against iwd: needs `CARJACK_TEST_SSID` set to the SSID we're currently on.
    #[test]
    #[ignore = "needs a live iwd WiFi connection; set CARJACK_TEST_SSID"]
    fn live_connection_matches_only_the_current_ssid() {
        RUNTIME.block_on(async {
            let ssid = std::env::var("CARJACK_TEST_SSID").expect("set CARJACK_TEST_SSID");
            let found = super::live_connection(&Ssid::from(ssid.as_str()))
                .await
                .expect("querying the current connection failed")
                .expect("the SSID we are on should match");
            assert_eq!(found.ssid, Ssid::from(ssid.as_str()));
            assert!(!found.iface.is_empty());

            assert!(
                super::live_connection(&Ssid::from("carjack-definitely-not-connected"))
                    .await
                    .expect("querying an unrelated SSID failed")
                    .is_none()
            );
        });
    }

    /// Live check against iwd: the daemon must be configured the way carjack needs.
    #[test]
    #[ignore = "needs a live iwd"]
    fn iwd_is_configured_for_carjack() {
        RUNTIME.block_on(async {
            let info = iwd_automata::DaemonProxy::new(super::iwd().await)
                .await
                .expect("no Daemon interface")
                .get_info()
                .await
                .expect("GetInfo failed");
            assert_eq!(
                info.network_configuration_enabled,
                Some(true),
                "iwd must have [General] EnableNetworkConfiguration=true"
            );
            assert!(
                info.state_directory.is_some(),
                "iwd must name its state dir"
            );

            let mac = super::get_wifi_mac()
                .await
                .expect("no announceable address");
            assert!(mac.starts_with("FE:"), "{mac} is not locally administered");
        });
    }

    /// Live check against iwd: the provisioning file must be one the daemon reads back as written.
    #[test]
    #[ignore = "needs a live iwd and a capability-granted sidecar (`just setcap`)"]
    fn iwd_reads_back_the_car_provisioning_file() {
        const SSID: &str = "carjack provision-check";
        RUNTIME.block_on(async {
            let ssid = Ssid::from(SSID);
            super::provision(&ssid, "hunter2hunter2", "FE:B2:9F:ED:EA:72")
                .await
                .expect("iwd would not take the car's provisioning file");

            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            let known = loop {
                let networks = iwd_automata::discover(super::iwd().await)
                    .await
                    .expect("walking iwd's tree failed")
                    .known_networks;
                let mut found = None;
                for network in networks {
                    if network.name().await.as_deref() == Ok(SSID) {
                        found = Some(network);
                        break;
                    }
                }
                if let Some(found) = found {
                    break found;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "iwd never registered {SSID}; it warns into its own log and ignores a \
                     provisioning file it cannot read the name of"
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            };

            let hidden = known.hidden().await.expect("no Hidden property");
            let auto_connect = known.auto_connect().await.expect("no AutoConnect property");
            let kind = known.r#type().await.expect("no Type property");
            known.forget().await.expect("couldn't forget it");

            assert!(hidden, "iwd did not read [Settings] Hidden=true");
            assert!(
                !auto_connect,
                "iwd did not read [Settings] AutoConnect=false"
            );
            assert_eq!(kind, iwd_automata::KnownNetworkType::Psk);
        });
    }
}
