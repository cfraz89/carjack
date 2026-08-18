//! Presents the adapter as an iPhone - Class of Device and the Apple manufacturer data bluetoothd
//! has no API for - re-applying for as long as it runs, since bluetoothd rewrites both on every
//! service change.
//!
//! Separate from `carjack` so the capability stays out of that process: under `fs.suid_dumpable=2`
//! it would break `xdg-desktop-portal`'s introspection.

use std::io::{Read, Write};
use std::process::ExitCode;
use std::time::Duration;

use bluer::Session;
use eyre::{Result, eyre};
// `sockopt_impl!` expands to unqualified `libc` and `setsockopt_impl!`, so both have to be nameable.
use nix::libc;
use nix::sys::socket::setsockopt;
use nix::{setsockopt_impl, sockopt_impl};
use socket2::{Domain, Protocol, SockAddr, SockAddrStorage, Socket, Type, socklen_t};

const AF_BLUETOOTH: i32 = 31;
const BTPROTO_HCI: i32 = 1;
const HCI_CHANNEL_RAW: u16 = 0;
const HCI_CHANNEL_CONTROL: u16 = 3;
const MGMT_INDEX_NONE: u16 = 0xffff;
const MGMT_OP_SET_DEV_CLASS: u16 = 0x000E;

const SOL_HCI: i32 = 0;
const HCI_FILTER: i32 = 2;
const HCI_COMMAND_PKT: u8 = 0x01;
const HCI_EVENT_PKT: u8 = 0x04;
const EVT_CMD_COMPLETE: u8 = 0x0e;
const OCF_READ_EIR: u16 = 0x0c51;
const OCF_WRITE_EIR: u16 = 0x0c52;
const OCF_WRITE_CLASS_OF_DEVICE: u16 = 0x0c24;
/// Zero-padded past the last AD structure.
const EIR_LEN: usize = 240;

/// mgmt takes `[major, minor]` only; the kernel derives the service byte from registered UUIDs.
const CARPLAY_DEV_CLASS: [u8; 2] = [0x02, 0x0c];

/// An iPhone's, little-endian. Written whole: the kernel's derived service byte gives us
/// Capturing where a phone has Networking.
const CARPLAY_CLASS_OF_DEVICE: [u8; 3] = [0x0c, 0x02, 0x7a];

/// `len, type=0xff, ...`. The company ID reads `00 4c`, not the spec's little-endian `4c 00`,
/// because that is what the phone emits.
const APPLE_MANUFACTURER_AD: [u8; 40] = {
    let mut ad = [0u8; 40];
    ad[0] = 0x27;
    ad[1] = 0xff;
    ad[2] = 0x00;
    ad[3] = 0x4c;
    ad[4] = 0x02;
    ad[5] = 0x24;
    ad[6] = 0x02;
    ad
};

#[repr(C)]
struct SockAddrHci {
    family: u16,
    dev: u16,
    channel: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct HciFilter {
    type_mask: u32,
    event_mask: [u32; 2],
    opcode: u16,
}

sockopt_impl!(HciFilterOpt, SetOnly, SOL_HCI, HCI_FILTER, HciFilter);

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("carjack-hci-helper: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let session = Session::new().await?;
    let adapter = session.default_adapter().await?;
    let index = adapter
        .name()
        .strip_prefix("hci")
        .and_then(|index| index.parse().ok())
        .ok_or_else(|| {
            eyre!(
                "unexpected adapter name {:?}, expected `hci<N>`",
                adapter.name()
            )
        })?;
    send_mgmt_command(index, MGMT_OP_SET_DEV_CLASS, &CARPLAY_DEV_CLASS)?;
    apply_identity(&raw_hci_socket(index)?)?;
    eprintln!("carjack-hci-helper: applied iPhone identity, watching for overwrites");
    keep_identity_applied(index)
}

fn apply_identity(socket: &Socket) -> Result<()> {
    hci_command(socket, OCF_WRITE_CLASS_OF_DEVICE, &CARPLAY_CLASS_OF_DEVICE)?;
    add_manufacturer_data(socket)
}

fn keep_identity_applied(adapter_index: u16) -> Result<()> {
    let watcher = command_watch_socket(adapter_index)?;
    // Writes need their own socket: the watcher is filtered to commands, not events.
    let writer = raw_hci_socket(adapter_index)?;
    let mut buf = [0u8; 512];
    loop {
        let len = (&watcher).read(&mut buf)?;
        if len < 4 || buf[0] != HCI_COMMAND_PKT {
            continue;
        }
        let opcode = u16::from_le_bytes([buf[1], buf[2]]);
        let params = &buf[4..len];
        // Our own writes come back here; matching them is what stops a rewrite war with bluetoothd.
        let ours = match opcode {
            OCF_WRITE_CLASS_OF_DEVICE => params == CARPLAY_CLASS_OF_DEVICE,
            OCF_WRITE_EIR => ad_structures(params).any(|ad| ad == APPLE_MANUFACTURER_AD),
            _ => continue,
        };
        if ours {
            continue;
        }
        eprintln!("carjack-hci-helper: {opcode:#06x} overwritten, re-applying");
        apply_identity(&writer)?;
    }
}

fn add_manufacturer_data(socket: &Socket) -> Result<()> {
    let current = hci_command(socket, OCF_READ_EIR, &[])?;
    // FEC-required byte, then the field.
    if current.len() < 1 + EIR_LEN {
        return Err(eyre!(
            "Read_EIR returned {} bytes, expected {}",
            current.len(),
            1 + EIR_LEN
        ));
    }
    let fec = current[0];
    let mut eir = [0u8; EIR_LEN];
    eir.copy_from_slice(&current[1..1 + EIR_LEN]);

    let used = eir_used_len(&eir);
    if ad_structures(&eir[..used]).any(|ad| ad == APPLE_MANUFACTURER_AD) {
        return Ok(());
    }
    if used + APPLE_MANUFACTURER_AD.len() > EIR_LEN {
        return Err(eyre!(
            "no room for manufacturer data: {used} of {EIR_LEN} bytes already used"
        ));
    }
    eir[used..used + APPLE_MANUFACTURER_AD.len()].copy_from_slice(&APPLE_MANUFACTURER_AD);

    let mut params = Vec::with_capacity(1 + EIR_LEN);
    params.push(fec);
    params.extend_from_slice(&eir);
    hci_command(socket, OCF_WRITE_EIR, &params)?;
    Ok(())
}

/// Each AD structure with its length byte, so they can be compared whole.
fn ad_structures(eir: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        let end = at.checked_add(1)?.checked_add(*eir.get(at)? as usize)?;
        if eir[at] == 0 || end > eir.len() {
            return None;
        }
        let ad = &eir[at..end];
        at = end;
        Some(ad)
    })
}

fn eir_used_len(eir: &[u8]) -> usize {
    ad_structures(eir).map(<[u8]>::len).sum()
}

fn send_mgmt_command(adapter_index: u16, opcode: u16, payload: &[u8]) -> Result<()> {
    let socket = Socket::new(
        Domain::from(AF_BLUETOOTH),
        Type::RAW,
        Some(Protocol::from(BTPROTO_HCI)),
    )?;
    socket.bind(&hci_control_addr())?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;

    let mut packet = Vec::with_capacity(6 + payload.len());
    packet.extend_from_slice(&opcode.to_le_bytes());
    packet.extend_from_slice(&adapter_index.to_le_bytes());
    packet.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    packet.extend_from_slice(payload);
    // An untrusted socket is refused with a short write, not an error.
    let written = (&socket).write(&packet)?;
    if written != packet.len() {
        return Err(eyre!(
            "kernel accepted {written}/{} bytes of mgmt command {opcode:#06x} - the HCI control \
             socket is untrusted (missing cap_net_admin?)",
            packet.len()
        ));
    }

    let mut buf = [0u8; 1024];
    loop {
        let len = (&socket).read(&mut buf)?;
        if len < 6 {
            return Err(eyre!("mgmt read too short ({len} bytes)"));
        }
        let rsp_opcode = u16::from_le_bytes([buf[0], buf[1]]);
        let evt_len = u16::from_le_bytes([buf[4], buf[5]]) as usize;
        if len < 6 + evt_len {
            return Err(eyre!("mgmt payload shorter than its declared length"));
        }
        if rsp_opcode == 0x0001 && evt_len >= 3 {
            let completed_opcode = u16::from_le_bytes([buf[6], buf[7]]);
            let status = buf[8];
            if completed_opcode != opcode {
                continue;
            }
            if status != 0x00 {
                return Err(eyre!("MGMT command failed (status={status})"));
            }
            return Ok(());
        }
    }
}

fn raw_hci_socket(adapter_index: u16) -> Result<Socket> {
    let socket = Socket::new(
        Domain::from(AF_BLUETOOTH),
        Type::RAW,
        Some(Protocol::from(BTPROTO_HCI)),
    )?;

    // Must precede the bind: a fresh socket's filter is all zeroes, which drops everything.
    let filter = HciFilter {
        type_mask: 1 << HCI_EVENT_PKT,
        event_mask: [1 << EVT_CMD_COMPLETE, 0],
        opcode: 0,
    };
    setsockopt(&socket, HciFilterOpt, &filter)?;

    socket.bind(&hci_raw_addr(adapter_index))?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    Ok(socket)
}

/// Delivers outgoing commands rather than events, so bluetoothd's writes land here too.
fn command_watch_socket(adapter_index: u16) -> Result<Socket> {
    let socket = Socket::new(
        Domain::from(AF_BLUETOOTH),
        Type::RAW,
        Some(Protocol::from(BTPROTO_HCI)),
    )?;
    let filter = HciFilter {
        type_mask: 1 << HCI_COMMAND_PKT,
        event_mask: [0, 0],
        opcode: 0,
    };
    setsockopt(&socket, HciFilterOpt, &filter)?;
    socket.bind(&hci_raw_addr(adapter_index))?;
    Ok(socket)
}

fn hci_command(socket: &Socket, opcode: u16, params: &[u8]) -> Result<Vec<u8>> {
    let mut packet = Vec::with_capacity(4 + params.len());
    packet.push(HCI_COMMAND_PKT);
    packet.extend_from_slice(&opcode.to_le_bytes());
    packet.push(params.len() as u8);
    packet.extend_from_slice(params);
    let written = (&*socket).write(&packet)?;
    if written != packet.len() {
        return Err(eyre!(
            "kernel accepted {written}/{} bytes of HCI command {opcode:#06x} - the raw HCI socket \
             is untrusted (missing cap_net_raw?)",
            packet.len()
        ));
    }

    let mut buf = [0u8; 512];
    loop {
        let len = (&*socket).read(&mut buf)?;
        // [type, event, plen, ncmd, opcode_lo, opcode_hi, status, ...]
        if len < 7 || buf[0] != HCI_EVENT_PKT || buf[1] != EVT_CMD_COMPLETE {
            continue;
        }
        if u16::from_le_bytes([buf[4], buf[5]]) != opcode {
            continue;
        }
        if buf[6] != 0x00 {
            return Err(eyre!(
                "HCI command {opcode:#06x} failed (status={:#04x})",
                buf[6]
            ));
        }
        return Ok(buf[7..len].to_vec());
    }
}

fn hci_raw_addr(adapter_index: u16) -> SockAddr {
    let mut storage = SockAddrStorage::zeroed();
    // SAFETY: `repr(C)` `sockaddr_hci`, smaller than `sockaddr_storage`, with a matching length.
    unsafe {
        *storage.view_as::<SockAddrHci>() = SockAddrHci {
            family: AF_BLUETOOTH as u16,
            dev: adapter_index,
            channel: HCI_CHANNEL_RAW,
        };
        SockAddr::new(storage, size_of::<SockAddrHci>() as socklen_t)
    }
}

/// mgmt addresses the adapter per-command, so the socket itself binds `HCI_DEV_NONE`.
fn hci_control_addr() -> SockAddr {
    let mut storage = SockAddrStorage::zeroed();
    // SAFETY: as `hci_raw_addr`.
    unsafe {
        *storage.view_as::<SockAddrHci>() = SockAddrHci {
            family: AF_BLUETOOTH as u16,
            dev: MGMT_INDEX_NONE,
            channel: HCI_CHANNEL_CONTROL,
        };
        SockAddr::new(storage, size_of::<SockAddrHci>() as socklen_t)
    }
}
