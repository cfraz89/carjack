//! Joining the accessory's access point, over iwd.
//!
//! iwd has no connection-profile API: a network is provisioned by a file in its state directory,
//! and `Network.Connect()` is the whole of the API. The Agent API is no substitute -
//! `AddressOverride` has no D-Bus equivalent, and `Device.Address`/`KnownNetwork.Hidden` are
//! read-only. So this writes the file directly (needs `CAP_DAC_OVERRIDE`/`CAP_CHOWN` - see
//! `write_profile`), scans, then connects.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use enclose::enclose;
use eyre::{Result, eyre};
use futures::{StreamExt, TryStreamExt};
use iwd_automata::{
    DaemonProxy, DeviceProxy, NetworkProxy, ProxyExt, Ssid, StationProxy, StationState, discover,
};
use netlink_packet_route::AddressFamily;
use netlink_packet_route::address::{
    AddressAttribute, AddressFlags, AddressHeaderFlags, AddressMessage, AddressScope,
};
use tokio::sync::OnceCell;

use super::{AccessoryWifi, WifiBackend};

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

pub(crate) struct IwdBackend;

#[async_trait]
impl WifiBackend for IwdBackend {
    async fn get_wifi_mac(&self) -> Result<String> {
        get_wifi_mac().await
    }

    async fn check_ready(&self) -> Result<()> {
        check_writable().await
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
    let ssid = Ssid::from(ssid);
    if let Some(accessory_wifi) = super::reuse_if_live(&ssid, live_connection(&ssid).await) {
        return Ok(accessory_wifi);
    }

    // The address already announced to the accessory over iAP2.
    let announced = get_wifi_mac().await?;

    super::connect_with_known_settings(
        accessory_mac,
        enclose!((ssid, announced) |disable_he| { async move {
            provision(&ssid, passphrase, &announced, disable_he).await?;

            let (station, device) = accessory_station().await?;
            let iface = device.name().await?;
            let network = accessory_network(&station, &ssid).await?;

            if let Err(e) = tokio::time::timeout(ACTIVATION_TIMEOUT, network.connect())
                .await
                .map_err(|_| eyre!("Timed out bringing up the accessory's WiFi connection"))
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
            Ok(AccessoryWifi { iface })
        } }),
    )
    .await
}

/// Write the accessory's network into iwd's state directory - the only way to say what `Connect()`
/// cannot: the passphrase, that the SSID is hidden, no autoconnect, and our station address.
/// Rewritten before every attempt, since the accessory's passphrase changes per session.
async fn provision(
    ssid: &Ssid,
    passphrase: &str,
    announced_mac: &str,
    disable_he: bool,
) -> Result<()> {
    if !(8..=63).contains(&passphrase.len()) || passphrase.contains(['\n', '\r']) {
        return Err(eyre!(
            "The accessory offered a {}-character passphrase; WPA-PSK needs 8 to 63",
            passphrase.len()
        ));
    }
    let info = DaemonProxy::new(iwd().await).await?.get_info().await?;
    if info.network_configuration_enabled != Some(true) {
        tracing::warn!(
            "iwd is not configuring IP addresses ([General] EnableNetworkConfiguration is not \
             true in /etc/iwd/main.conf); proceeding anyway to see if DHCP is actually needed"
        );
    }
    let dir = state_directory(&info)?;
    let body = provisioning_file(passphrase, announced_mac, disable_he);
    let name = network_file_name(ssid);
    let path = dir.join(&name);
    check_profile_name(&name)?;
    write_profile(&path, &body)
        .await
        .map_err(|_| cannot_write(&dir))?;
    tracing::info!(path = %path.display(), "Provisioned the accessory's network for iwd");
    Ok(())
}

fn state_directory(info: &iwd_automata::DaemonInfo) -> Result<PathBuf> {
    info.state_directory
        .as_deref()
        .map(PathBuf::from)
        .ok_or_else(|| {
            eyre!("iwd did not report a state directory (Daemon.GetInfo has no StateDirectory)")
        })
}

fn cannot_write(dir: &std::path::Path) -> eyre::Report {
    eyre!(
        "Cannot write {}; iwd backend requires to run carjack through the systemd service, or as root",
        dir.display()
    )
}

/// Confirm carjack can provision iwd's state directory before anything tries to join the accessory.
async fn check_writable() -> Result<()> {
    let info = DaemonProxy::new(iwd().await).await?.get_info().await?;
    let dir = state_directory(&info)?;
    let probe = dir.join(format!(".carjack-probe-{}", std::process::id()));
    write_profile(&probe, "")
        .await
        .map_err(|_| cannot_write(&dir))?;
    tokio::fs::remove_file(&probe).await.ok();
    Ok(())
}

/// Only what iwd names a WPA-PSK profile. `/`, `.` and `..` are outside both alphabets, so a name
/// that passes cannot leave the state directory - this is the security boundary of the write,
/// now that it happens with carjack's own `CAP_DAC_OVERRIDE`/`CAP_CHOWN` rather than a sidecar
/// validating an argument crossing a process boundary. `network_file_name` already only produces
/// names in this grammar; this catches it if that ever stops being true.
fn check_profile_name(name: &str) -> Result<()> {
    let stem = name
        .strip_suffix(".psk")
        .ok_or_else(|| eyre!("{name:?} is not a .psk profile"))?;
    if let Some(hex) = stem.strip_prefix('=') {
        if hex.is_empty()
            || hex.len() % 2 != 0
            || !hex
                .bytes()
                .all(|b| b.is_ascii_digit() || b"abcdef".contains(&b))
        {
            return Err(eyre!("{name:?} is not iwd's `=<lowercase hex>.psk` form"));
        }
        return Ok(());
    }
    if stem.is_empty()
        || stem.len() > 32
        || !stem
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b' '))
    {
        return Err(eyre!("{name:?} is not a literal iwd profile name"));
    }
    Ok(())
}

/// Write one iwd provisioning file - temp file then rename, so iwd's inotify watch never sees a
/// partial file. Write and `fchown` both land before the rename, since iwd loads on that event.
async fn write_profile(path: &std::path::Path, body: &str) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;

    let dir = path.parent().unwrap_or(std::path::Path::new("."));
    // Dot-prefixed so iwd, which reads a file's security type after the first dot, ignores it.
    let temp = dir.join(format!(".carjack-{}.tmp", std::process::id()));
    let result: std::io::Result<()> = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600) // Not world-readable - holds the accessory's passphrase.
            .open(&temp)
            .await?;
        file.write_all(body.as_bytes()).await?;
        file.sync_all().await?;
        // By descriptor rather than path, so it can't be redirected between the write and the
        // chown. No tokio wrapper exists for fchown - it's a fast metadata-only syscall, not
        // worth spawn_blocking on its own.
        std::os::unix::fs::fchown(&file, Some(0), Some(0))?;
        tokio::fs::rename(&temp, path).await
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
    }
    result
}

/// The provisioning file's contents; see [`provision`] for what each key does.
fn provisioning_file(passphrase: &str, announced_mac: &str, disable_he: bool) -> String {
    let disable_he = if disable_he { "DisableHE=true\n" } else { "" };
    format!(
        "[Security]\n\
         Passphrase={passphrase}\n\
         \n\
         [Settings]\n\
         AutoConnect=false\n\
         Hidden=true\n\
         AddressOverride={announced_mac}\n\
         {disable_he}\
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

/// The accessory's network object, scanning if the last scan didn't find it. Must run after
/// provisioning - only then does iwd know to probe for a hidden SSID.
async fn accessory_network(
    station: &StationProxy<'_>,
    ssid: &Ssid,
) -> Result<NetworkProxy<'static>> {
    if let Some(network) = find_network(station, ssid).await? {
        return Ok(network);
    }
    scan(station).await?;
    find_network(station, ssid)
        .await?
        .ok_or_else(|| eyre!("iwd's scan did not find the accessory's network {ssid}"))
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
/// `Busy`, and stale results are still worth reading; the caller retries if the accessory isn't found.
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
/// for `connect_to_accessory_wifi` to do. `None` means "not it, go and connect".
async fn live_connection(ssid: &Ssid) -> Result<Option<AccessoryWifi>> {
    let (station, device) = accessory_station().await?;
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
        Ok(Some(AccessoryWifi { iface }))
    } else {
        tracing::debug!("On {ssid} already, but its link-local IPv6 hasn't settled yet");
        Ok(None)
    }
}

/// The station carjack drives: whichever is already connected, else the first iwd lists.
///
/// A powered-down device carries no Station interface, so power-up goes through `Device` first,
/// then waits for the Station to appear.
async fn accessory_station() -> Result<(StationProxy<'static>, DeviceProxy<'static>)> {
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

async fn get_wifi_mac() -> Result<String> {
    let (_station, device) = accessory_station().await?;
    Ok(super::announced_wifi_mac(&permanent_mac(
        &device.name().await?,
    )?))
}

/// The radio's permanent hardware address, read from sysfs rather than `Device.Address` - iwd
/// reports the *current* netdev address there, which becomes our own override once connected.
///
/// Sysfs, not `nix`: this is `cfg80211`'s own attribute (`/sys/class/ieee80211/<phy>/macaddress`,
/// symlinked at `phy80211` under the netdev), not something a generic interface-address API
/// exposes - `nix` has no wrapper for it, nor for the `ETHTOOL_GPERMADDR` ioctl NetworkManager
/// uses for the same purpose on the D-Bus side (`WirelessProxy::perm_hw_address`).
fn permanent_mac(iface: &str) -> Result<String> {
    let mac = std::fs::read_to_string(format!("/sys/class/net/{iface}/phy80211/macaddress"))?
        .trim()
        .to_owned();
    if mac.is_empty() {
        return Err(eyre!("{iface}'s permanent address is empty"));
    }
    Ok(mac)
}

fn current_mac(iface: &str) -> Result<String> {
    mac_address::mac_address_by_name(iface)
        .map_err(|e| eyre!("mac_address_by_name {iface}: {e}"))?
        .ok_or_else(|| eyre!("no link-layer address for {iface}"))
        .map(|mac| mac.to_string())
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
        let file = super::provisioning_file("hunter2hunter2", "FE:B2:9F:ED:EA:72", false);
        assert!(file.contains("\nPassphrase=hunter2hunter2\n"));
        assert!(file.contains("\nAutoConnect=false\n"));
        assert!(file.contains("\nHidden=true\n"));
        assert!(file.contains("\nAddressOverride=FE:B2:9F:ED:EA:72\n"));
        assert!(!file.contains("DisableHE"));
        assert!(file.contains("[IPv6]\nEnabled=false\n"));
    }

    #[test]
    fn provisioning_file_adds_disable_he_only_when_asked() {
        let file = super::provisioning_file("hunter2hunter2", "FE:B2:9F:ED:EA:72", true);
        assert!(file.contains("\nDisableHE=true\n"));
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
    #[ignore = "needs a live iwd and CAP_DAC_OVERRIDE/CAP_CHOWN on this test binary"]
    fn iwd_reads_back_the_accessory_provisioning_file() {
        const SSID: &str = "carjack provision-check";
        RUNTIME.block_on(async {
            let ssid = Ssid::from(SSID);
            super::provision(&ssid, "hunter2hunter2", "FE:B2:9F:ED:EA:72", false)
                .await
                .expect("iwd would not take the accessory's provisioning file");

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
