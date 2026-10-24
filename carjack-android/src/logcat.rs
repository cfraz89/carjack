//! `tracing` output to logcat, one record per line under the `carjack` tag.

use std::ffi::CString;
use std::io::{self, Write};

use tracing_subscriber::filter::{EnvFilter, LevelFilter};
use tracing_subscriber::prelude::*;

const ANDROID_LOG_INFO: i32 = 4;

#[cfg(target_os = "android")]
#[link(name = "log")]
unsafe extern "C" {
    fn __android_log_write(prio: i32, tag: *const std::ffi::c_char, text: *const std::ffi::c_char)
    -> i32;
}

#[cfg(not(target_os = "android"))]
unsafe fn __android_log_write(
    _prio: i32,
    _tag: *const std::ffi::c_char,
    text: *const std::ffi::c_char,
) -> i32 {
    // SAFETY: the caller passes a NUL-terminated string.
    eprintln!("{}", unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy());
    0
}

/// Buffers one formatted event and writes it to logcat on drop.
pub struct LogcatWriter(Vec<u8>);

impl Write for LogcatWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for LogcatWriter {
    fn drop(&mut self) {
        write_bytes(ANDROID_LOG_INFO, &self.0);
    }
}

/// One record under the `carjack` tag, at logcat priority `priority`.
pub fn write(priority: i32, text: &str) {
    write_bytes(priority, text.as_bytes());
}

fn write_bytes(priority: i32, text: &[u8]) {
    let text: Vec<u8> = text.iter().copied().filter(|&b| b != 0).collect();
    let Ok(text) = CString::new(text.trim_ascii_end()) else {
        return;
    };
    // SAFETY: both strings are NUL-terminated and outlive the call.
    unsafe { __android_log_write(priority, c"carjack".as_ptr(), text.as_ptr()) };
}

pub fn init() {
    let logcat = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .without_time()
        .with_writer(|| LogcatWriter(Vec::new()))
        .with_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::DEBUG.into())
                // catplay warns once a frame, dumps every RTSP exchange and event payload, and
                // falls back to plain ChaCha a few times a second: hours of that would rotate a
                // day's useful lines out of the log.
                .parse_lossy(
                    "debug,catplay_carplay=info,\
                     catplay_carplay::screen::tx::screen_tx_session=error,\
                     catplay_carplay::rtsp_session::rtsp_transmitter=error,\
                     catplay_carplay::events::events_server=error,\
                     catplay_hap::cipher=error",
                ),
        );
    let _ = tracing_subscriber::registry()
        .with(logcat)
        .with(crate::report::layer())
        .try_init();
}
