//! `carjack-wireless-helper` - carjack's privileged one-shot sidecar. Sets an iPhone-like Class of
//! Device (mgmt `SET_DEV_CLASS`), which needs a raw `AF_BLUETOOTH`/HCI control socket and so
//! `CAP_NET_ADMIN`. The capability is granted out of band (`just setcap`); without it this exits
//! non-zero and carjack carries on unadorned.

use std::io::{Read, Write};
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
    match args.next().as_deref() {
        Some("device-class") => set_device_class().await,
        _ => bail!("usage: carjack-wireless-helper device-class"),
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
