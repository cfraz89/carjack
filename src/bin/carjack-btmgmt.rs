//! `carjack-btmgmt` - a privileged one-shot sidecar for carjack.
//!
//! It does exactly one thing: add the CarPlay device UUID to a Bluetooth adapter's BR/EDR EIR
//! (mgmt `ADD_UUID`) so a head unit sees us as a CarPlay device. That requires opening a raw
//! `AF_BLUETOOTH`/HCI control socket, which requires `CAP_NET_ADMIN`.
//!
//! We keep that capability out of the main `carjack` process on purpose. Under `fs.suid_dumpable=2`
//! (the hardened default on Fedora and friends) a process that gains a capability at exec has its
//! `/proc/<pid>` reparented to root and made non-dumpable. In `source = "desktop"` mode that breaks
//! xdg-desktop-portal: it runs as the user and identifies its D-Bus caller by opening
//! `/proc/<pid>/root`, which it then can't do - ScreenCast/RemoteDesktop fails with
//! "Unable to open /proc/<pid>/root". So only this sidecar is `setcap cap_net_admin+ep`; carjack
//! execs it for this one operation and stays unprivileged (and portal-introspectable).
//!
//! Usage: `carjack-btmgmt <adapter-index>` (e.g. `0` for `hci0`). Exits 0 on success.

use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::ExitCode;

use libcarjack::uuid::CARPLAY_DEVICE_UUID;

const AF_BLUETOOTH: i32 = 31;
const BTPROTO_HCI: i32 = 1;
const HCI_CHANNEL_CONTROL: u16 = 3;
const MGMT_INDEX_NONE: u16 = 0xffff;
const MGMT_OP_ADD_UUID: u16 = 0x0010;

#[repr(C)]
struct SockAddrHci {
    family: u16,
    dev: u16,
    channel: u16,
}

fn main() -> ExitCode {
    let Some(index_arg) = std::env::args().nth(1) else {
        eprintln!("usage: carjack-btmgmt <adapter-index>");
        return ExitCode::from(2);
    };
    let adapter_index: u16 = match index_arg.parse() {
        Ok(i) => i,
        Err(e) => {
            eprintln!("carjack-btmgmt: invalid adapter index {index_arg:?}: {e}");
            return ExitCode::from(2);
        }
    };

    // Add the CarPlay device UUID to the BR/EDR EIR: payload is the 16-byte UUID (LE) + a
    // service-visibility flag byte.
    let mut payload = CARPLAY_DEVICE_UUID.to_bytes_le().to_vec();
    payload.push(0x00);

    match send_mgmt_command(adapter_index, MGMT_OP_ADD_UUID, &payload) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("carjack-btmgmt: ADD_UUID failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Open the HCI control socket, send one mgmt command, and wait (up to 5s) for the matching
/// CmdComplete. Blocking - this process does nothing else.
fn send_mgmt_command(adapter_index: u16, opcode: u16, payload: &[u8]) -> std::io::Result<()> {
    let fd = unsafe { libc::socket(AF_BLUETOOTH, libc::SOCK_RAW, BTPROTO_HCI) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: socket() just handed us a fresh owned fd.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    let raw = owned.as_raw_fd();

    let addr = SockAddrHci {
        family: AF_BLUETOOTH as u16,
        dev: MGMT_INDEX_NONE,
        channel: HCI_CHANNEL_CONTROL,
    };
    let rc = unsafe {
        libc::bind(
            raw,
            &addr as *const SockAddrHci as *const libc::sockaddr,
            mem::size_of::<SockAddrHci>() as u32,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }

    // Bound wait for the response.
    let tv = libc::timeval { tv_sec: 5, tv_usec: 0 };
    unsafe {
        libc::setsockopt(
            raw,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            &tv as *const libc::timeval as *const _,
            mem::size_of::<libc::timeval>() as u32,
        );
    }

    let mut packet = Vec::with_capacity(6 + payload.len());
    packet.extend_from_slice(&opcode.to_le_bytes());
    packet.extend_from_slice(&adapter_index.to_le_bytes());
    packet.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    packet.extend_from_slice(payload);

    let mut written = 0usize;
    while written < packet.len() {
        let n = unsafe {
            libc::write(
                raw,
                packet.as_ptr().add(written).cast(),
                packet.len() - written,
            )
        };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        written += n as usize;
    }

    let mut buf = [0u8; 1024];
    loop {
        let len = unsafe { libc::read(raw, buf.as_mut_ptr().cast(), buf.len()) };
        if len < 0 {
            // SO_RCVTIMEO surfaces the timeout as EAGAIN/EWOULDBLOCK.
            return Err(std::io::Error::last_os_error());
        }
        let len = len as usize;
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
