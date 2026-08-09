//! Low-level per-thread capability primitives. See `wireless::spawn_privileged_thread` for why
//! these are used from a dedicated OS thread rather than inline in async code, and `main` for why
//! the drop has to happen before the tokio runtime is built.
//!
//! Holding any capability marks the process non-dumpable, which breaks `xdg-desktop-portal`'s
//! `/proc/<pid>/root` introspection (desktop-mode screen sharing needs that) - `mark_dumpable`
//! undoes that. It alone is not enough, though: `ptrace_access_check`'s capability-subset
//! comparison is a *separate* gate keyed on the permitted set, independent of dumpable, and only
//! `drop_all`'s `capset` clears that. Both verified empirically against a live kernel.
//!
//! Capabilities (and dumpable-relevant credential state) are per-thread in Linux - `capset(2)`
//! "operate\[s\] on the calling thread" - and a thread inherits its initial set from whichever
//! thread created it, at creation time. Calling these from the wrong thread silently does nothing
//! useful for the threads that actually matter.

use std::io;

const PR_SET_DUMPABLE: libc::c_int = 4;
const PR_CAP_AMBIENT: libc::c_int = 47;
const PR_CAP_AMBIENT_CLEAR_ALL: libc::c_int = 4;
const LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;

#[repr(C)]
struct CapHeader {
    version: u32,
    pid: libc::c_int,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct CapData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

/// Explicitly re-marks the process dumpable. This is process-wide (the `mm` is shared across
/// threads), so one call from any thread covers the whole process - unlike `drop_all`.
pub(crate) fn mark_dumpable() -> io::Result<()> {
    // SAFETY: PR_SET_DUMPABLE takes one plain integer argument, no pointers involved.
    if unsafe { libc::prctl(PR_SET_DUMPABLE, 1, 0, 0, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Empties the calling thread's effective/permitted/inheritable capability sets (via `capset(2)`)
/// and its ambient set. Bounding-set capabilities aren't otherwise reachable, so emptying permitted
/// is what makes them unreachable for the rest of this thread's life.
pub(crate) fn drop_all() -> io::Result<()> {
    // SAFETY: PR_CAP_AMBIENT_CLEAR_ALL takes no further arguments.
    if unsafe { libc::prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_CLEAR_ALL, 0, 0, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let header = CapHeader {
        version: LINUX_CAPABILITY_VERSION_3,
        pid: 0, // self
    };
    // Two 32-bit words: version 3 splits the 64 capability bits across a pair of cap_user_data_t.
    let data = [CapData::default(); 2];
    // SAFETY: `header` and `data` match capset(2)'s ABI for LINUX_CAPABILITY_VERSION_3.
    if unsafe { libc::syscall(libc::SYS_capset, &header, data.as_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
