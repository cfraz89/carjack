//! Standalone validation of the internal compositor without a car: bring it up, run a
//! fullscreen client that paints the screen white, and confirm the composited frames actually
//! contain the client's output (not just the black clear) - and that the H264 encode path
//! (including the zero-copy dmabuf pipeline) produces access units from them.
//!
//! Needs GPU/DRM (the render node) at runtime, so run it via `just test compositor` (which
//! builds first, then `cargo test compositor`). `CARJACK_DESKTOP_CMD` overrides the test client
//! to validate the real desktop app; `CARJACK_TEST_FRAMES` caps how many frames to sample.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use carjack::compositor::{CapturedFrame, Compositor};
use carjack::config::{Cli, Config};
use carjack::encode::{Bitrate, EncodeBackend, encode_h264_to_channel, resolve_backend};
use clap::Parser;
use eyre::{Result, eyre};
use libcarjack::airplay::EncodedFrame;
use tokio::sync::mpsc;
use tokio_util::task::AbortOnDropHandle;

/// Resolve the render node + encode backend + bitrate the same way `main` does (from config/env),
/// so the test exercises the real selection path. Returns (node, backend, bitrate).
fn resolve_gpu() -> Result<(String, EncodeBackend, Bitrate)> {
    // Empty CLI: config comes from defaults + CARJACK_* env vars (see the test's env).
    let cfg = Config::load(&Cli::parse_from(["carjack"]))?;
    let node = carjack::config::resolve_render_node(&cfg.graphics.drm_device)?;
    let backend = resolve_backend(cfg.graphics.zerocopy, &node, cfg.graphics.codec)?;
    Ok((node, backend, Bitrate::from_kbps(cfg.graphics.bitrate)))
}

#[tokio::test]
async fn compositor_frames_contain_client_content() -> Result<()> {
    // The test (and the compositor/encode paths it drives) log via tracing; without a subscriber
    // `cargo test -- --nocapture` shows nothing, including which encode backend was selected.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    let (node, backend, bitrate) = resolve_gpu()?;
    let comp = Compositor::start(
        node,
        backend.is_zero_copy(),
        carjack::encode::importable_modifiers(&backend),
        1280,
        720,
        30,
    )?;
    tracing::info!("compositor started, socket={:?}", comp.socket_name);

    // Use the configured desktop app if set (so this validates the real CARJACK_DESKTOP_CMD),
    // otherwise a foot that fills the screen white so a rendered frame is unmistakably non-black.
    let cmd = std::env::var("CARJACK_DESKTOP_CMD")
        .unwrap_or_else(|_| "foot -e sh -c 'printf \\033[47m\\033[2J; sleep 60'".to_string());
    tracing::info!("test client: {cmd}");
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .env("WAYLAND_DISPLAY", &comp.socket_name)
        .spawn()
        .map_err(|e| eyre!("spawn test client: {e}"))?;

    // Also run the H264 encoder on the compositor frames and count encoded access units, so
    // this validates the full compositor -> encode path (incl. the zero-copy dmabuf pipeline)
    // without needing the car.
    let (video_tx, mut video_rx) = mpsc::channel::<EncodedFrame>(64);
    // No car here to ask for keyframes, so the sender is simply dropped and the encoder's
    // request pump sees a closed channel and exits.
    let (_keyframe_tx, keyframe_rx) = mpsc::channel::<()>(1);
    let _encoder = AbortOnDropHandle::new(tokio::spawn(encode_h264_to_channel(
        comp.subscribe_frames(),
        video_tx,
        backend.clone(),
        bitrate,
        keyframe_rx,
    )));
    let encoded = Arc::new(AtomicU64::new(0));
    let encoded2 = encoded.clone();
    let _counter = AbortOnDropHandle::new(tokio::spawn(async move {
        while let Some(f) = video_rx.recv().await {
            let n = encoded2.fetch_add(1, Ordering::Relaxed) + 1;
            if n % 30 == 1 {
                tracing::info!(
                    "encoded AU #{n} ({} bytes, keyframe={})",
                    f.data.len(),
                    f.config.is_some()
                );
            }
        }
    }));

    let mut rx = comp.subscribe_frames();
    let mut checked = 0;
    let mut max_brightness = 0u64;
    // ~30fps, so frames ≈ seconds*30. GPU clients like Waydroid take a while to boot.
    let budget: u64 = std::env::var("CARJACK_TEST_FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(90);
    while checked < budget {
        if rx.changed().await.is_err() {
            break;
        }
        let Some(frame) = rx.borrow_and_update().clone() else {
            continue;
        };
        checked += 1;
        match &*frame {
            CapturedFrame::Shm {
                data,
                width,
                height,
                ..
            } => {
                // Sample colour to confirm the client actually rendered (not just black clear).
                // Sum only R/G/B per pixel - NOT the 4th (X) byte, which is opaque 255 padding
                // in BGRx and would make even a pure-black frame score non-zero.
                let sum: u64 = data
                    .chunks_exact(4)
                    .step_by(251)
                    .map(|px| px[0] as u64 + px[1] as u64 + px[2] as u64)
                    .sum();
                max_brightness = max_brightness.max(sum);
                if checked % 30 == 1 {
                    tracing::info!("frame #{checked}: SHM {width}x{height}, sample-sum={sum}");
                }
            }
            CapturedFrame::Dmabuf(_) => {
                // GPU memory - can't cheaply sample; the point is that dmabufs are flowing.
                max_brightness = 1_000_000;
                if checked % 30 == 1 {
                    tracing::info!("frame #{checked}: DMABUF forwarded (zero-copy)");
                }
            }
        }
    }
    let _ = child.kill();

    let encoded_total = encoded.load(Ordering::Relaxed);
    tracing::info!(
        "compositor OK: {checked} frames reached the stream, {encoded_total} encoded AUs"
    );
    assert!(
        encoded_total > 0,
        "encoder produced no output - the encode pipeline didn't work"
    );
    assert!(
        max_brightness > 10_000,
        "frames stayed black - client never rendered into the compositor"
    );
    Ok(())
}

/// The car's real display size/refresh arrive after startup, so the compositor starts at a
/// default and is reconfigured. Verify a live `reconfigure` actually switches the frame stream to
/// the new dimensions (no client needed - the compositor renders at the output size regardless).
#[tokio::test]
async fn reconfigure_switches_frame_size() -> Result<()> {
    let (node, backend, _bitrate) = resolve_gpu()?;
    let comp = Compositor::start(
        node,
        backend.is_zero_copy(),
        carjack::encode::importable_modifiers(&backend),
        1280,
        720,
        30,
    )?;
    let mut rx = comp.subscribe_frames();

    // First frame should be at the start size.
    let start = wait_for_frame_size(&mut rx).await?;
    assert_eq!(
        start,
        (1280, 720),
        "initial frame was not at the start size"
    );

    // Reconfigure to a different size + refresh; frames must switch to it.
    comp.reconfigure(800, 480, 60)?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "frames never switched to the reconfigured size"
        );
        if wait_for_frame_size(&mut rx).await? == (800, 480) {
            break;
        }
    }
    Ok(())
}

/// Await the next published frame and return its pixel dimensions (works for both the SHM
/// readback and the zero-copy dmabuf frame variants).
async fn wait_for_frame_size(
    rx: &mut tokio::sync::watch::Receiver<Option<Arc<CapturedFrame>>>,
) -> Result<(u32, u32)> {
    use smithay::backend::allocator::Buffer as _;
    rx.changed().await?;
    let frame = rx
        .borrow_and_update()
        .clone()
        .ok_or_else(|| eyre!("frame was None"))?;
    Ok(match &*frame {
        CapturedFrame::Shm { width, height, .. } => (*width, *height),
        CapturedFrame::Dmabuf(d) => (d.width(), d.height()),
    })
}
