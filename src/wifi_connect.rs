//! Joining the car's access point, over iwd.
//!
//! iwd has no notion of a connection profile on D-Bus: a network is either provisioned by a file
//! in iwd's state directory or it is not, and `Network.Connect()` is the whole of the API. So the
//! shape here is "write the provisioning file, scan, connect" rather than "build a profile object
//! and activate it". The file is what carries everything the car's AP needs that D-Bus cannot say
//! - the passphrase, that the SSID is hidden, that iwd must not autoconnect to it, and the station
//! address we already promised the accessory over iAP2.

use backon::{ConstantBuilder, Retryable};
use std::fmt::Write as _;
use std::io::Write as _;
use std::net::{IpAddr, Ipv4Addr};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use eyre::{Result, eyre};
use futures::StreamExt;
use iwd_automata::{
    DaemonProxy, DeviceProxy, NetworkProxy, ProxyExt, Ssid, StationProxy, StationState, discover,
};
use tokio::sync::OnceCell;

/// How long to give iwd to associate *and* configure IP before treating the attempt as failed and
/// retrying it. Association plus DHCP is a couple of seconds when it works at all, and the car
/// walks away from a session that stops making progress.
///
/// This bounds `Network.Connect()` itself, which with `EnableNetworkConfiguration=true` does not
/// return until iwd's DHCP client has finished - upstream documents up to ~30s per BSS.
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(30);

/// How long to wait for a scan to finish before looking at its results anyway. A scan that has not
/// settled is not fatal - the results from the previous one may already carry the car.
const SCAN_TIMEOUT: Duration = Duration::from_secs(15);

/// How long to wait after association for the kernel to carry both address families, and how often
/// to look. iwd reports neither over D-Bus, so this is read off the interface.
const ADDRESS_TIMEOUT: Duration = Duration::from_secs(20);
const ADDRESS_POLL: Duration = Duration::from_millis(200);

/// How long to wait for iwd to put a `Station` interface on a device we have just powered up, and
/// how often to look. The interface appears a moment after the netdev comes up, not with the
/// property write that asked for it.
const POWER_UP_TIMEOUT: Duration = Duration::from_secs(10);
const POWER_UP_POLL: Duration = Duration::from_millis(200);

/// Where iwd keeps provisioning files when `Daemon.GetInfo` does not say. Matches upstream's
/// `DAEMON_STORAGEDIR` default.
const DEFAULT_STATE_DIR: &str = "/var/lib/iwd";

static IWD: OnceCell<zbus::Connection> = OnceCell::const_new();

/// The system bus connection every iwd proxy is built on, made once.
///
/// Held for the life of the process because the proxies built on it cache properties by
/// subscribing to `PropertiesChanged`; a connection per call would resubscribe every time.
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
    #[allow(dead_code)]
    pub ip: IpAddr,
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

    // The same address we announced over iAP2 - the accessory waits for exactly that station.
    let announced = get_wifi_mac().await?;
    // Written before every attempt rather than reused: the car's passphrase changes per session,
    // and iwd rewrites the whole `[Security]` group with a derived `PreSharedKey` after a
    // successful connect, so a stale file would associate with last session's key.
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
            // An attempt iwd is still driving answers the next `Connect()` with InProgress, so the
            // retry would fail on our own leftovers rather than on the car.
            if let Err(e) = station.disconnect().await {
                tracing::debug!(?e, "Could not abort a failed connection attempt");
            }
            return Err(e);
        }

        // The whole point of pinning the station address is that the accessory sees the station it
        // was promised. If iwd did not apply it, the association is live but the accessory is
        // waiting for a station that will never appear - a failure that otherwise shows up much
        // later, as an AirPlay session that never starts.
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

        let ip = wait_for_addresses(&iface).await?;
        Ok(CarWifi {
            ssid: ssid.clone(),
            ip,
            iface,
        })
    })
    .retry(ConstantBuilder::new().with_delay(Duration::from_secs(2)))
    .notify(|e, _| tracing::warn!("Bringing up the car's WiFi failed ({e}); retrying"))
    .await
}

/// Write the car's network into iwd's state directory.
///
/// Everything that is not `Connect()` lives here, because iwd exposes no way to say any of it over
/// D-Bus:
///
/// * **`AutoConnect=false`** - carjack associates deliberately, once, with an address it has just
///   announced over iAP2. An autoconnecting profile associates outside that sequence and the
///   accessory then sees a station it was never promised.
/// * **`Hidden=true`** - the car's AP does not beacon its SSID. A provisioned hidden network is
///   what makes iwd send directed probes for it, so it shows up as a `Network` object at all;
///   `ConnectHiddenNetwork` is not the route, it fails `AlreadyProvisioned` the moment this file
///   exists.
/// * **`AddressOverride`** - the accessory is told our station address over iAP2 before we
///   associate, so the address it is told has to be the one it then sees. **iwd ignores this
///   unless `/etc/iwd/main.conf` sets `[General] AddressRandomization=network`**; with that set and
///   no override it invents a per-SSID address of its own, which is just as wrong for us.
/// * **`[IPv6] Enabled=false`** - the car answers no router solicitation, so iwd's IPv6 stage could
///   only ever time out. The kernel still assigns a link-local (iwd never sets `disable_ipv6`,
///   whatever iwd.network(5) claims), which is the address AirPlay is actually reached on.
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
    let path = dir.join(network_file_name(ssid));
    write_atomically(&path, &body).map_err(|e| {
        eyre!(
            "Could not write {}: {e}. iwd's state directory is root-owned and mode 0700, so \
             carjack has to run as the same user as iwd to provision the car's network",
            path.display()
        )
    })?;
    tracing::info!(path = %path.display(), "Provisioned the car's network for iwd");
    Ok(())
}

/// The provisioning file's contents. See [`provision`] for why each key is here.
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

/// The file name iwd derives for a WPA-PSK network of this name.
///
/// iwd names a provisioning file after the SSID, but only while every byte is alphanumeric, `-`,
/// `_` or a space; anything else - and every SSID longer than 32 bytes - has to be written as
/// `=<lowercase hex>`. Getting this wrong is silent: iwd warns into its own log and treats the file
/// as absent, so the connect then fails for want of a passphrase.
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
        // Lower case is not cosmetic: iwd parses either case but then re-derives the lower-case
        // path to open the file, so an upper-case name is read and immediately lost.
        let _ = write!(name, "{byte:02x}");
    }
    name.push_str(".psk");
    name
}

/// Write the file the way iwd writes its own: a temporary beside it, then a rename.
///
/// iwd watches its state directory with inotify and re-reads on every event, so a plain in-place
/// write can be read half-finished. A rename is one event carrying the whole file. The temporary's
/// name starts with a dot so that iwd, which takes a file's security type from the text after the
/// *first* dot, cannot mistake it for a network.
fn write_atomically(path: &Path, body: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let temp = dir.join(format!(".carjack-{}.tmp", std::process::id()));
    let write = || -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            // 0600, not world-readable - the file holds the car's passphrase.
            .mode(0o600)
            .open(&temp)?;
        file.write_all(body.as_bytes())?;
        file.sync_all()
    };
    // iwd is root but its systemd unit drops `CAP_DAC_OVERRIDE` (CapabilityBoundingSet is only the
    // NET_* trio), so it reads files by ordinary permission bits - a chris-owned 0600 file is
    // unreadable to it while a world-readable 0644 works, which would leak the car's passphrase.
    // A POSIX ACL granting uid 0 read (`u:0:r`) keeps the file 0600 for everyone else and gives
    // iwd access. The ACL must be set *before* the atomic rename: iwd picks the file up from the
    // rename's inotify event, and if it lands ACL-less the load races the grant and the network
    // stays unprovisioned (measured: ACL-before-rename registers 6/6, ACL-after 0/6). carjack owns
    // the file, so it can `setfacl` without being root; iwd rewrites the file to root-owned 0600
    // after the first successful connect, after which it reads it as the owner.
    if let Err(e) = write()
        .and_then(|()| make_readable_by_iwd(&temp))
        .and_then(|()| std::fs::rename(&temp, path))
    {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    Ok(())
}

/// Grant iwd's uid (0) read access to `path` through a POSIX ACL, without opening the file to any
/// other user.
///
/// Reading the ACL of a 0600 file synthesises owner/group/other with no named entries; adding a
/// named user entry for uid 0 (`"0"`, so it does not depend on a `/etc/passwd` lookup) gives iwd
/// access. exacl computes and inserts the kernel-required mask, so the result is exactly the
/// `u:0:r` ACL a hand-run `setfacl -m u:0:r` would leave, with `other` still getting nothing.
fn make_readable_by_iwd(path: &Path) -> std::io::Result<()> {
    use exacl::{AclEntry, AclEntryKind, Flag, Perm};
    let mut acl = exacl::getfacl(path, None)?;
    acl.push(AclEntry {
        kind: AclEntryKind::User,
        name: "0".to_owned(),
        perms: Perm::READ,
        flags: Flag::empty(),
        allow: true,
    });
    exacl::setfacl(&[path], &acl, None)?;
    Ok(())
}

/// The car's network object, scanning for it if the last scan did not find it.
///
/// A hidden network is only probed for while iwd knows it is hidden, which is what [`provision`]
/// has just told it - so this must run after provisioning, never before.
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

/// Trigger a scan and wait for it to settle.
///
/// Errors are swallowed on purpose: a scan already running answers `Busy`, and a scan that never
/// reports finished is still worth reading the results of. The caller's "not found" is the real
/// failure, and the retry loop above re-scans.
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

/// The current WiFi connection, if it is already on `ssid` and carries an address - i.e. there is
/// nothing left for `connect_to_car_wifi` to do. `None` means "not it, go and connect".
async fn live_connection(ssid: &Ssid) -> Result<Option<CarWifi>> {
    let (station, device) = car_station().await?;
    // Compared as SSIDs rather than as text: an access point names itself in bytes, and only the
    // bytes decide whether this is the same network.
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
    // A half-configured connection means the association is still in flight; let the normal path
    // drive it rather than handing one back.
    match addresses(&iface) {
        Ok(Some(ip)) => Ok(Some(CarWifi {
            ssid: ssid.clone(),
            ip,
            iface,
        })),
        Ok(None) => {
            tracing::debug!("On {ssid} already, but it has no usable addresses yet");
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// The station carjack drives: whichever one is already connected, else the first iwd lists.
///
/// **A powered-down device is not a station.** iwd only puts the `Station` interface on a device
/// whose radio is up, so the power-up cannot be done through the station - it has to happen first,
/// against the `Device` interface, and then the station has to be waited for.
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

/// The station to use, if iwd has one at all.
///
/// The `Device` interface lives on the same object - iwd puts every interface a device implements
/// on one path - so the cast costs nothing and gives the interface name, which `Station` does not
/// carry.
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

/// Wait until the interface carries both an address carjack needs, then hand back the IPv4 one.
///
/// **Both families, not just the DHCP lease.** carjack uses each of them - the DHCP address to
/// reach AirPlay by route, the link-local as the source address that reaches it directly. iwd
/// reports neither over D-Bus (it has no equivalent of NetworkManager's `IP4_READY`/`IP6_READY`),
/// so this reads the interface itself.
///
/// The link-local half is not "does one exist" but "has it finished duplicate-address detection":
/// a link-local destination is unreachable from a *tentative* source, `connect()` failing
/// `EADDRNOTAVAIL` before anything hits the wire.
async fn wait_for_addresses(iface: &str) -> Result<IpAddr> {
    let deadline = tokio::time::Instant::now() + ADDRESS_TIMEOUT;
    loop {
        if let Some(ip) = addresses(iface)? {
            return Ok(ip);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(eyre!(
                "{iface} has no DHCP address and settled link-local after {ADDRESS_TIMEOUT:?}"
            ));
        }
        tokio::time::sleep(ADDRESS_POLL).await;
    }
}

/// The interface's routable IPv4 address, but only once its link-local IPv6 is out of DAD.
fn addresses(iface: &str) -> Result<Option<IpAddr>> {
    let Some(ip) = routable_ipv4(iface)? else {
        return Ok(None);
    };
    let if_inet6 = std::fs::read_to_string("/proc/net/if_inet6")?;
    Ok(link_local_is_ready(&if_inet6, iface).then_some(IpAddr::V4(ip)))
}

/// The interface's IPv4 address from the car's DHCP server.
///
/// 169.254/16 is skipped rather than accepted: it is what is left when DHCP failed, and handing it
/// back would report an association the car cannot be reached over.
fn routable_ipv4(iface: &str) -> Result<Option<Ipv4Addr>> {
    for ifaddr in nix::ifaddrs::getifaddrs()? {
        if ifaddr.interface_name != iface {
            continue;
        }
        let Some(ip) = ifaddr
            .address
            .as_ref()
            .and_then(|address| address.as_sockaddr_in())
            .map(|address| address.ip())
        else {
            continue;
        };
        if !ip.is_unspecified() && !ip.is_link_local() {
            return Ok(Some(ip));
        }
    }
    Ok(None)
}

/// Whether `iface` has a link-local IPv6 address that has finished duplicate-address detection,
/// read out of `/proc/net/if_inet6`.
///
/// Each line is `<32 hex address> <ifindex> <prefixlen> <scope> <flags> <device>`, all hex. Scope
/// `0x20` is link-local; `IFA_F_TENTATIVE` in the flags is DAD still running.
fn link_local_is_ready(if_inet6: &str, iface: &str) -> bool {
    const IFA_F_TENTATIVE: u32 = 0x40;
    const SCOPE_LINK: u32 = 0x20;
    if_inet6.lines().any(|line| {
        let mut fields = line.split_whitespace().skip(3);
        let (Some(scope), Some(flags), Some(device)) =
            (fields.next(), fields.next(), fields.next())
        else {
            return false;
        };
        device == iface
            && u32::from_str_radix(scope, 16) == Ok(SCOPE_LINK)
            && u32::from_str_radix(flags, 16).is_ok_and(|flags| flags & IFA_F_TENTATIVE == 0)
    })
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
    let (_station, device) = car_station().await?;
    // Derived from the permanent address rather than being it - see `announced_wifi_mac`. The
    // car's provisioning file pins itself to the same value, so what we announce is what
    // associates.
    Ok(announced_wifi_mac(&permanent_mac(&device.name().await?)?))
}

/// The radio's permanent hardware address.
///
/// **Read from sysfs, not from `Device.Address`.** iwd reports the *current* netdev address there,
/// which is our own `AddressOverride` once we have connected - and it does not put the permanent
/// one back on disconnect. Deriving the announced address from that would give a different answer
/// on a second run, so the accessory would be promised a station that never appears. The phy's
/// address is the one thing that cannot move.
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
    use exacl::{AclEntryKind, Perm};
    use iwd_automata::Ssid;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    /// One runtime for every live test in this process.
    ///
    /// `iwd` caches its D-Bus connection for the life of the process, but a `#[tokio::test]` gets a
    /// runtime of its own and drops it on the way out - taking the connection's driver task with
    /// it, so the next test to reuse a cached proxy blocks forever. carjack itself has one runtime
    /// for the process; these share one too.
    static RUNTIME: std::sync::LazyLock<tokio::runtime::Runtime> = std::sync::LazyLock::new(|| {
        tokio::runtime::Runtime::new().expect("building a runtime for the live tests")
    });

    /// The whole reason carjack sets an ACL rather than a 0644 chmod: iwd (uid 0, running without
    /// `CAP_DAC_OVERRIDE`) must be able to read the provisioning file while every other user still
    /// gets nothing. Guards against a future regression to world-readable.
    #[test]
    fn root_read_acl_keeps_the_file_closed_to_other() {
        let dir = std::env::temp_dir().join(format!("carjack-acl-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("carjack-profile.psk");
        std::fs::write(&path, "[Security]\nPassphrase=hunter2hunter2\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        super::make_readable_by_iwd(&path).unwrap();

        let acl = exacl::getfacl(&path, None).unwrap();
        // exacl stores the principal as a resolved name ("root" here); both spellings are uid 0,
        // which is what iwd runs as. A mask entry covering READ is implicit for a named user.
        let root_reads = acl.iter().any(|e| {
            e.kind == AclEntryKind::User
                && e.allow
                && matches!(e.name.as_str(), "0" | "root")
                && e.perms.contains(Perm::READ)
        });
        let other_reads = acl
            .iter()
            .any(|e| e.kind == AclEntryKind::Other && e.perms.contains(Perm::READ));
        // The "other" mode bit, not just the ACL, must stay off - a backup agent reorganising the
        // files, or iwd rewriting the file as root before the ACL is re-applied, could otherwise
        // reopen it.
        let other_bit = std::fs::metadata(&path).unwrap().permissions().mode() & 0o004 != 0;

        assert!(root_reads, "iwd (uid 0) must be granted read access");
        assert!(
            !other_reads,
            "ACL `other` must not read the car's passphrase"
        );
        assert!(!other_bit, "the file must not be world-readable");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    /// iwd reads a provisioning file's SSID back out of its name, and rejects - silently, into its
    /// own log - any literal name carrying a character outside `[A-Za-z0-9-_ ]`. A name we get
    /// wrong is a network iwd does not know it has a passphrase for.
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
        // Over 32 bytes: iwd refuses the literal form outright.
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
        // The car answers no router solicitation, so iwd's IPv6 stage could only time out.
        assert!(file.contains("[IPv6]\nEnabled=false\n"));
    }

    /// The gate that replaces NetworkManager's `IP6_READY`: a link-local still in duplicate-address
    /// detection cannot be used as a source address, and AirPlay on the car is link-local.
    #[test]
    fn link_local_is_ready_only_once_dad_has_finished() {
        // scope 20 = link-local, flags 80 = IFA_F_PERMANENT.
        let settled = "fe8000000000000056f5ac6d67819bf3 03 40 20 80     wlan0\n";
        // flags c0 = IFA_F_PERMANENT | IFA_F_TENTATIVE.
        let tentative = "fe8000000000000056f5ac6d67819bf3 03 40 20 c0     wlan0\n";
        // scope 00 = global: a routable address is not a link-local one.
        let global = "2401dc2028170000941b1a8217270993 03 40 00 80     wlan0\n";

        assert!(super::link_local_is_ready(settled, "wlan0"));
        assert!(!super::link_local_is_ready(tentative, "wlan0"));
        assert!(!super::link_local_is_ready(global, "wlan0"));
        // Another interface's settled link-local says nothing about ours.
        assert!(!super::link_local_is_ready(settled, "wlan1"));
        assert!(super::link_local_is_ready(
            &format!("{global}{tentative}{settled}"),
            "wlan0"
        ));
    }

    /// Live check against iwd: asking for the SSID we are already on must hand back the live
    /// connection so `connect_to_car_wifi` skips reconnecting, and any other SSID must not match.
    /// Read-only - it never provisions, associates or tears down anything.
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

    /// Live check against iwd: the daemon has to be configured the way carjack needs it, and the
    /// derived station address has to be readable, before any of the connect path can work.
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

    /// Live check against iwd: the provisioning file carjack writes has to be one the daemon
    /// picks up and reads as written. Everything about it is silent when wrong - a name iwd cannot
    /// parse, a key it does not know, a group in the wrong place all leave a network iwd simply
    /// does not have, and the failure only surfaces as a connect that asks for a passphrase.
    ///
    /// The SSID is one no access point advertises, and `AutoConnect=false` is asserted before
    /// anything else, so iwd never tries to associate with it. `Forget()` unlinks the file.
    #[test]
    #[ignore = "needs a live iwd whose state directory carjack can write"]
    fn iwd_reads_back_the_car_provisioning_file() {
        const SSID: &str = "carjack provision-check";
        RUNTIME.block_on(async {
            let ssid = Ssid::from(SSID);
            super::provision(&ssid, "hunter2hunter2", "FE:B2:9F:ED:EA:72")
                .await
                .expect("iwd would not take the car's provisioning file");

            // iwd notices the file through an inotify watch on its state directory, so the object
            // appears a moment after the rename rather than with it.
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

            // Without this iwd never probes for the car's SSID, so it is never a Network object.
            assert!(hidden, "iwd did not read [Settings] Hidden=true");
            // Without this iwd can associate outside the announce-then-activate sequence.
            assert!(
                !auto_connect,
                "iwd did not read [Settings] AutoConnect=false"
            );
            // The suffix carjack picked for the file is what decides this.
            assert_eq!(kind, iwd_automata::KnownNetworkType::Psk);
        });
    }
}
