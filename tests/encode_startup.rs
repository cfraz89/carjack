//! How long does the encode path take to produce its *first keyframe*?
//!
//! `run_video_stream` holds RECORD until a keyframe exists, so this is what the head unit's
//! watchdog is measured against.
//!
//! Deliberately runs with **no client connected**: an accessory session starts with the compositor
//! rendering black (or an app still booting), so this is the case that has to be fast.
//!
//! Needs GPU/DRM at runtime, like `tests/compositor.rs`. Run with
//! `TOOLBOX_RUN=true just test encode_startup -- --nocapture`.

use std::time::{Duration, Instant};

use carjack::compositor::Compositor;
use carjack::config::{Cli, Config};
use carjack::encode::{Bitrate, EncodeBackend, encode_h264_to_channel, resolve_backend};
use clap::Parser;
use eyre::{Result, eyre};
use libcarjack::airplay::EncodedFrame;
use tokio::sync::mpsc;
use tokio_util::task::AbortOnDropHandle;

fn resolve_gpu() -> Result<(String, EncodeBackend, Bitrate)> {
    let cfg = Config::load(&Cli::parse_from(["carjack"]))?;
    let node = carjack::config::resolve_render_node(&cfg.graphics.drm_device)?;
    let backend = resolve_backend(cfg.graphics.zerocopy, &node, cfg.graphics.codec)?;
    Ok((node, backend, Bitrate::from_kbps(cfg.graphics.bitrate)))
}

/// Time from encoder spawn to the first keyframe, driving the compositor exactly as `run_session`
/// does: start at `start_size`, then `reconfigure` to what the accessory reported, then subscribe and
/// encode. `accessory_fps` is passed through raw, as `run_session` passes `conn.max_fps`.
async fn time_to_first_keyframe(
    start_size: (u32, u32),
    accessory_size: (u32, u32),
    accessory_fps: u32,
) -> Result<(Duration, Duration, u32, String)> {
    let (node, backend, bitrate) = resolve_gpu()?;
    let comp = Compositor::start(
        node,
        backend.is_zero_copy(),
        carjack::encode::importable_modifiers(&backend),
        start_size.0,
        start_size.1,
        30,
    )?;

    let resized = comp.reconfigure(accessory_size.0, accessory_size.1, accessory_fps)?;
    println!(
        "  reconfigure({}x{} @ {accessory_fps}) -> resized={resized}",
        accessory_size.0, accessory_size.1
    );

    // Start the clock where `run_session` does: at the encoder spawn, with the compositor already
    // running and reconfigured. Everything after this is what the accessory waits through.
    let started = Instant::now();
    let (video_tx, mut video_rx) = mpsc::channel::<EncodedFrame>(4);
    let (_keyframe_tx, keyframe_rx) = mpsc::channel::<()>(1);
    let _encoder = AbortOnDropHandle::new(tokio::spawn(encode_h264_to_channel(
        comp.subscribe_frames(),
        video_tx,
        backend.clone(),
        bitrate,
        keyframe_rx,
    )));

    let mut first_frame: Option<Duration> = None;
    let mut frames = 0u32;
    let first_keyframe = loop {
        let next = tokio::time::timeout(Duration::from_secs(30), video_rx.recv()).await;
        match next {
            Err(_) => break None,
            Ok(None) => break None,
            Ok(Some(frame)) => {
                frames += 1;
                let at = started.elapsed();
                if first_frame.is_none() {
                    first_frame = Some(at);
                    tracing::info!(
                        "first access unit after {:?} ({} bytes)",
                        at,
                        frame.data.len()
                    );
                }
                if frame.config.is_some() {
                    break Some(at);
                }
            }
        }
    };

    let first_frame = first_frame.ok_or_else(|| eyre!("encoder produced no frames at all"))?;
    let first_keyframe =
        first_keyframe.ok_or_else(|| eyre!("encoder produced {frames} frames but no keyframe"))?;
    Ok((
        first_frame,
        first_keyframe,
        frames,
        backend.describe().to_string(),
    ))
}

#[tokio::test]
async fn first_keyframe_arrives_before_the_accessory_gives_up() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    // Three shapes of session, all of which have to beat the watchdog:
    //   - the display cache was right, so `reconfigure` is a no-op (the common repeat-connection
    //     case);
    //   - the cache was wrong or absent, so the compositor actually resizes;
    //   - the accessory reported no refresh rate at all (`max_fps` is optional in `/info`, and
    //     `run_session` forwards the raw 0).
    let cases: [(&str, (u32, u32), (u32, u32), u32); 4] = [
        ("cache hit, no-op reconfigure", (1280, 720), (1280, 720), 30),
        ("cache miss, real resize", (1280, 720), (800, 480), 30),
        ("accessory reported no fps", (1280, 720), (800, 480), 0),
        // A real head unit's geometry, whose width is not a round multiple: the allocator can
        // pick a different modifier for it than for the tidy sizes above, and the zero-copy
        // import has to cope with whatever it gets.
        ("head unit geometry", (1280, 720), (1080, 600), 30),
    ];

    let mut failures = Vec::new();
    for (name, start, accessory, fps) in cases {
        println!("case: {name}");
        match time_to_first_keyframe(start, accessory, fps).await {
            Ok((first_frame, first_keyframe, frames, backend)) => {
                // println rather than tracing: the toolbox runner doesn't forward RUST_LOG, and
                // these numbers are the entire point of the test.
                println!(
                    "  TIMING: backend={backend} first access unit at {first_frame:?}, first \
                     keyframe at {first_keyframe:?} ({frames} frames)"
                );
                // The head unit gives us about two seconds. Assert against a budget with headroom
                // rather than the wire deadline, so this fails here before it fails against an accessory.
                if first_keyframe >= Duration::from_millis(1500) {
                    failures.push(format!("{name}: first keyframe took {first_keyframe:?}"));
                }
            }
            Err(e) => failures.push(format!("{name}: {e}")),
        }
    }

    assert!(
        failures.is_empty(),
        "the accessory tears the session down ~2s after carjack stops making progress:\n  {}",
        failures.join("\n  ")
    );
    Ok(())
}
