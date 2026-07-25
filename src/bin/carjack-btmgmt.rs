//! `carjack-btmgmt` - a privileged one-shot sidecar for carjack.
//!
//! It applies the two BR/EDR presentation tweaks a head unit needs to treat us as an iPhone:
//!   1. add the CarPlay device UUID to the adapter's EIR (mgmt `ADD_UUID`), and
//!   2. set an iPhone-like Class of Device (mgmt `SET_DEV_CLASS`).
//! Both need a raw `AF_BLUETOOTH`/HCI control socket, which requires `CAP_NET_ADMIN`.
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
const MGMT_OP_SET_DEV_CLASS: u16 = 0x000E;

/// `SET_DEV_CLASS` takes `[major, minor]` (2 bytes), NOT the raw 3-byte CoD - the kernel derives
/// the service-class byte from the enabled UUIDs. `[0x02, 0x0c]` = Phone / Smartphone, i.e. the low
/// two bytes of a real iPhone's `0x7a020c`. NOTE: bluetoothd owns the adapter class (see
/// `/etc/bluetooth/main.conf` `[General] Class`) and will re-assert it, so this raw write is
/// best-effort / mostly redundant when bluetoothd is running.
const CARPLAY_DEV_CLASS: [u8; 2] = [0x02, 0x0c];

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

    if let Err(e) = send_mgmt_command(adapter_index, MGMT_OP_ADD_UUID, &payload) {
        eprintln!("carjack-btmgmt: ADD_UUID failed: {e}");
        return ExitCode::FAILURE;
    }

    // Present an iPhone-like Class of Device. Without this the adapter keeps BlueZ's default class
    // (Computer); a head unit then gives us HFP/A2DP but never starts CarPlay - it opens the iAP
    // RFCOMM channel and drops it without ever sending the iAP1->iAP2 switch (FF 55 02 00 EE 10).
    if let Err(e) = send_mgmt_command(adapter_index, MGMT_OP_SET_DEV_CLASS, &CARPLAY_DEV_CLASS) {
        eprintln!("carjack-btmgmt: SET_DEV_CLASS failed: {e}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
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
    let tv = libc::timeval {
        tv_sec: 5,
        tv_usec: 0,
    };
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
