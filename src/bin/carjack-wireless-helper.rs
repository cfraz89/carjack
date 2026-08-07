//! `carjack-wireless-helper` - carjack's privileged one-shot sidecar. Capabilities are granted out
//! of band (`just setcap`); without them a subcommand exits non-zero.
//!
//! - `device-class` sets an iPhone-like Class of Device. Needs `CAP_NET_ADMIN` for the raw
//!   `AF_BLUETOOTH`/HCI control socket. Best-effort: unset, the unit just sees a plain computer.
//! - `wifi-profile <name>` writes one iwd provisioning file, body on stdin. Needs
//!   `CAP_DAC_OVERRIDE` and `CAP_CHOWN` - see [`write_profile`].

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use eyre::{Result, WrapErr, bail, eyre};
use socket2::{Domain, Protocol, SockAddr, SockAddrStorage, Socket, Type, socklen_t};

const AF_BLUETOOTH: i32 = 31;
const BTPROTO_HCI: i32 = 1;
const HCI_CHANNEL_CONTROL: u16 = 3;
const MGMT_INDEX_NONE: u16 = 0xffff;
const MGMT_OP_SET_DEV_CLASS: u16 = 0x000E;

/// `SET_DEV_CLASS` takes `[major, minor]`, not the raw 3-byte CoD - the kernel derives the
/// service-class byte from the enabled UUIDs. Phone / Smartphone, from an iPhone's `0x7a020c`.
const CARPLAY_DEV_CLASS: [u8; 2] = [0x02, 0x0c];

#[repr(C)]
struct SockAddrHci {
    family: u16,
    dev: u16,
    channel: u16,
}

/// Where iwd keeps provisioning files if `Daemon.GetInfo` doesn't say.
const DEFAULT_STATE_DIR: &str = "/var/lib/iwd";

const MAX_PROFILE_BYTES: u64 = 64 * 1024;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("carjack-wireless-helper: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let subcommand = args.next().unwrap_or_default();
    match (subcommand.as_str(), args.next(), args.next()) {
        ("device-class", None, _) => set_device_class().await,
        ("wifi-profile", Some(name), None) => write_profile(&name).await,
        _ => bail!(
            "usage: carjack-wireless-helper device-class | wifi-profile <name> (body on stdin)"
        ),
    }
}

/// Left as BlueZ's default (Computer), a head unit gives us HFP/A2DP but never sends the
/// iAP1->iAP2 switch, so CarPlay never starts.
async fn set_device_class() -> Result<()> {
    let adapter_index = default_adapter_index().await?;
    send_mgmt_command(adapter_index, MGMT_OP_SET_DEV_CLASS, &CARPLAY_DEV_CLASS)
        .wrap_err("SET_DEV_CLASS failed")?;
    Ok(())
}

/// Write one iwd provisioning file, body on stdin, into iwd's state directory.
///
/// `CAP_DAC_OVERRIDE` gets past the directory (root-owned 0700, and systemd re-applies that mode
/// on every iwd start, zeroing any ACL mask with it); `CAP_CHOWN` makes the result root's, which
/// iwd needs since it runs uid 0 *without* `CAP_DAC_OVERRIDE`. See [`write_atomically`].
///
/// The name is an argument but the directory is not - it comes from `Daemon.GetInfo`, so this
/// can't be aimed elsewhere, which a `CAP_DAC_OVERRIDE` binary taking a full path could be.
async fn write_profile(name: &str) -> Result<()> {
    check_profile_name(name)?;
    let mut body = String::new();
    std::io::stdin()
        .take(MAX_PROFILE_BYTES)
        .read_to_string(&mut body)
        .wrap_err("reading the profile from stdin")?;
    if body.is_empty() {
        bail!("refusing to write an empty profile");
    }
    let path = state_directory().await.join(name);
    write_atomically(&path, &body).wrap_err_with(|| format!("writing {}", path.display()))
}

/// `GetInfo`'s `StateDirectory` is optional, hence the fallback.
async fn state_directory() -> PathBuf {
    let reported = async {
        let connection = zbus::Connection::system().await.ok()?;
        let info = iwd_automata::DaemonProxy::new(&connection)
            .await
            .ok()?
            .get_info()
            .await
            .ok()?;
        info.state_directory
    }
    .await;
    PathBuf::from(reported.unwrap_or_else(|| DEFAULT_STATE_DIR.to_owned()))
}

/// Only what iwd names a WPA-PSK profile. `/`, `.` and `..` are outside both alphabets, so a name
/// that passes cannot leave the state directory - this is the security boundary of the capability.
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
            bail!("{name:?} is not iwd's `=<lowercase hex>.psk` form");
        }
        return Ok(());
    }
    if stem.is_empty()
        || stem.len() > 32
        || !stem
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b' '))
    {
        bail!("{name:?} is not a literal iwd profile name");
    }
    Ok(())
}

/// Temp file then rename, the way iwd writes its own, so its inotify watch never sees a partial
/// file. The `fchown` is needed because a capability doesn't change our uid - without it the file
/// belongs to whoever ran carjack and iwd logs `Error loading`. Both land before the rename, since
/// iwd loads on the rename's inotify event.
fn write_atomically(path: &Path, body: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    // Dot-prefixed so iwd, which reads a file's security type after the first dot, ignores it.
    let temp = dir.join(format!(".carjack-{}.tmp", std::process::id()));
    let write = || -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600) // Not world-readable - holds the car's passphrase.
            .open(&temp)?;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
        give_to_root(&file)
    };
    if let Err(e) = write().and_then(|()| std::fs::rename(&temp, path)) {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    Ok(())
}

/// By descriptor rather than path, so it can't be redirected between the write and the chown.
fn give_to_root(file: &std::fs::File) -> std::io::Result<()> {
    // SAFETY: `file` owns a valid descriptor for the duration of the call.
    if unsafe { libc::fchown(file.as_raw_fd(), 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Resolve the mgmt adapter index from BlueZ's default adapter, so we act on the same adapter the
/// main process does. BlueZ names adapters `hci<N>` and the mgmt index is that `N`.
async fn default_adapter_index() -> Result<u16> {
    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    let name = adapter.name();
    name.strip_prefix("hci")
        .and_then(|index| index.parse().ok())
        .ok_or_else(|| eyre!("unexpected adapter name {name:?}, expected `hci<N>`"))
}

/// Open the HCI control socket, send one mgmt command, and wait (up to 5s) for the matching
/// CmdComplete. Blocking - this process does nothing else.
fn send_mgmt_command(adapter_index: u16, opcode: u16, payload: &[u8]) -> std::io::Result<()> {
    let socket = Socket::new(
        Domain::from(AF_BLUETOOTH),
        Type::RAW,
        Some(Protocol::from(BTPROTO_HCI)),
    )?;
    socket.bind(&hci_control_addr())?;
    // Bound wait for the response; surfaces as EAGAIN/EWOULDBLOCK on expiry.
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;

    let mut packet = Vec::with_capacity(6 + payload.len());
    packet.extend_from_slice(&opcode.to_le_bytes());
    packet.extend_from_slice(&adapter_index.to_le_bytes());
    packet.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    packet.extend_from_slice(payload);
    // mgmt is message-oriented: the kernel takes the whole command or none of it. A short write
    // means it took none - which is how an *untrusted* socket is refused (the permission-denied
    // path returns 0 rather than an error), i.e. we're missing CAP_NET_ADMIN.
    let written = (&socket).write(&packet)?;
    if written != packet.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "kernel accepted {written}/{} bytes of mgmt command {opcode:#06x} - \
                 the HCI control socket is untrusted (missing cap_net_admin on this binary?)",
                packet.len()
            ),
        ));
    }

    let mut buf = [0u8; 1024];
    loop {
        let len = (&socket).read(&mut buf)?;
        if len == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "mgmt read EOF",
            ));
        }
        if len < 6 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "mgmt header too short",
            ));
        }
        let rsp_opcode = u16::from_le_bytes([buf[0], buf[1]]);
        let evt_len = u16::from_le_bytes([buf[4], buf[5]]) as usize;
        if len < 6 + evt_len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "mgmt payload too short",
            ));
        }
        // Wait for a CmdComplete (evt 0x0001) for our opcode.
        if rsp_opcode == 0x0001 && evt_len >= 3 {
            let completed_opcode = u16::from_le_bytes([buf[6], buf[7]]);
            let status = buf[8];
            if completed_opcode != opcode {
                continue;
            }
            if status != 0x00 {
                return Err(std::io::Error::other(format!(
                    "MGMT command failed (status={status})"
                )));
            }
            return Ok(());
        }
    }
}

/// `sockaddr_hci` bound to the mgmt control channel. The mgmt protocol addresses the adapter
/// per-command (the index in the packet header), so the socket itself binds `HCI_DEV_NONE`.
///
/// This is the one spot `socket2` can't cover: it has no typed constructor for a Bluetooth
/// sockaddr, so the storage has to be filled in by hand.
fn hci_control_addr() -> SockAddr {
    let mut storage = SockAddrStorage::zeroed();
    // SAFETY: `SockAddrHci` is a `repr(C)` `sockaddr_hci`, smaller than `sockaddr_storage`, and we
    // declare the matching length below - so the kernel reads exactly the bytes we wrote.
    unsafe {
        *storage.view_as::<SockAddrHci>() = SockAddrHci {
            family: AF_BLUETOOTH as u16,
            dev: MGMT_INDEX_NONE,
            channel: HCI_CHANNEL_CONTROL,
        };
        SockAddr::new(storage, size_of::<SockAddrHci>() as socklen_t)
    }
}

#[cfg(test)]
mod tests {
    /// The name is the only part of the destination a caller controls.
    #[test]
    fn check_profile_name_refuses_anything_that_leaves_the_state_directory() {
        for name in [
            "../../etc/shadow.psk",
            "..psk",
            "/etc/passwd.psk",
            "sub/dir.psk",
            ".hidden.psk",
            "car.plaý.psk", // a dot in the stem: iwd splits the suffix at the *first* one
            "=6361722E706C6179.psk", // upper-case hex is not what iwd re-derives
            "=636.psk",     // odd number of hex digits
            "=.psk",
            ".psk",
            "carplay_wifi_36A7",      // no suffix
            "carplay_wifi_36A7.open", // the wrong security type
            "carjack.tmp",
        ] {
            assert!(
                super::check_profile_name(name).is_err(),
                "{name:?} must be refused"
            );
        }
        assert!(super::check_profile_name(&format!("{}.psk", "a".repeat(33))).is_err());
    }

    /// And what `network_file_name` actually produces must survive the check.
    #[test]
    fn check_profile_name_accepts_what_iwd_names_a_psk_profile() {
        for name in [
            "carplay_wifi_36A7.psk",
            "My Network-1.psk",
            "=6361722e706c61c3bd.psk",
            &format!("{}.psk", "a".repeat(32)),
        ] {
            super::check_profile_name(name)
                .unwrap_or_else(|e| panic!("{name:?} must be accepted: {e}"));
        }
    }
}
