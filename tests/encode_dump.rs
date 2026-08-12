//! Write what the encoder would send the car to a file, so the picture the head unit decodes can
//! actually be looked at. The compositor's own screenshot driver forces the SHM path and so proves
//! nothing about the zero-copy dmabuf chain that a real run uses; this goes through whichever
//! backend `main` would pick.
//!
//! Ignored by default (it writes a file and takes ~10s):
//!
//! ```text
//! CARJACK_DUMP_PATH=/tmp/enc.h264 cargo test --test encode_dump -- --ignored --nocapture
//! ffmpeg -i /tmp/enc.h264 -vf fps=1 /tmp/enc-%03d.png
//! ```
//!
//! `CARJACK_DESKTOP_CMD` picks the client, `CARJACK_DUMP_FRAMES` how many access units to keep,
//! and `CARJACK_DUMP_WIDTH`/`_HEIGHT` the compositor size (defaults to the bike's 800x1280).

use std::io::Write;
use std::os::unix::process::CommandExt;

use carjack::compositor::Compositor;
use carjack::config::{Cli, Config};
use carjack::encode::{Bitrate, encode_h264_to_channel, resolve_backend};
use clap::Parser;
use eyre::{Result, eyre};
use libcarjack::airplay::EncodedFrame;
use tokio::sync::mpsc;
use tokio_util::task::AbortOnDropHandle;

fn env_or<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

#[tokio::test]
#[ignore = "writes a file; run explicitly with --ignored"]
async fn dump_encoded_stream() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    let cfg = Config::load(&Cli::parse_from(["carjack"]))?;
    let node = carjack::config::resolve_render_node(&cfg.graphics.drm_device)?;
    let backend = resolve_backend(cfg.graphics.zerocopy, &node, cfg.graphics.codec)?;
    let (width, height) = (
        env_or("CARJACK_DUMP_WIDTH", 800u32),
        env_or("CARJACK_DUMP_HEIGHT", 1280u32),
    );
    let path = std::env::var("CARJACK_DUMP_PATH").unwrap_or_else(|_| "/tmp/enc.h264".to_string());
    let frames: u64 = env_or("CARJACK_DUMP_FRAMES", 150);
    println!("backend={} {width}x{height} -> {path}", backend.describe());

    let comp = Compositor::start(
        node,
        backend.is_zero_copy(),
        carjack::encode::importable_modifiers(&backend),
        width,
        height,
        30,
    )?;

    let cmd = std::env::var("CARJACK_DESKTOP_CMD")
        .unwrap_or_else(|_| cfg.desktop_command().to_string());
    println!("client: {cmd}");
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("exec {cmd}"))
        .env("WAYLAND_DISPLAY", &comp.socket_name)
        .process_group(0)
        .spawn()
        .map_err(|e| eyre!("spawn client: {e}"))?;

    let (video_tx, mut video_rx) = mpsc::channel::<EncodedFrame>(64);
    let (_keyframe_tx, keyframe_rx) = mpsc::channel::<()>(1);
    let _encoder = AbortOnDropHandle::new(tokio::spawn(encode_h264_to_channel(
        comp.subscribe_frames(),
        video_tx,
        backend.clone(),
        Bitrate::from_kbps(cfg.graphics.bitrate),
        keyframe_rx,
    )));

    let mut file = std::fs::File::create(&path)?;
    let mut written = 0u64;
    let mut bytes = 0u64;
    let mut keyframes = 0u64;
    while written < frames {
        let Some(frame) = video_rx.recv().await else {
            break;
        };
        if frame.config.is_some() {
            keyframes += 1;
        }
        bytes += frame.data.len() as u64;
        file.write_all(&frame.data)?;
        written += 1;
    }
    file.flush()?;
    unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
    let _ = child.kill();
    let _ = child.wait();

    println!("wrote {written} AUs ({bytes} bytes, {keyframes} keyframes) to {path}");
    assert!(written > 0, "the encoder produced nothing");
    Ok(())
}
