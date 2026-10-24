//! Validates the `source = "desktop"` encode path (`encode_pipewire_to_channel`) against *real*
//! desktop frames, without an accessory and without the xdg-desktop-portal consent dialog: it drives
//! GNOME's internal `org.gnome.Mutter.ScreenCast` D-Bus API directly to get a PipeWire node, then
//! runs the encoder and checks H264 access units come out.
//!
//! Skips (passes) when Mutter ScreenCast isn't on the session bus (headless / non-GNOME CI), so it
//! only actually exercises anything on a live GNOME-Wayland session. `MUTTER_CONNECTOR` overrides
//! the monitor (default `eDP-1`); `TEST_ZEROCOPY` (auto/true/false) picks the encode path.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use carjack::config::ZeroCopyMode;
use carjack::encode::{Bitrate, encode_pipewire_to_channel};
use eyre::{Result, eyre};
use futures::StreamExt;
use libcarjack::airplay::EncodedFrame;
use tokio::sync::mpsc;
use tokio_util::task::AbortOnDropHandle;
use zbus::zvariant::{OwnedObjectPath, Value};

const SC: &str = "org.gnome.Mutter.ScreenCast";

#[tokio::test]
async fn desktop_pipewire_encodes() -> Result<()> {
    let _ = tracing_subscriber::fmt().with_env_filter("info").try_init();
    // Match `run_desktop`: enable the gst `va` elements on multi-GPU boxes so the VA-API zero-copy
    // path is exercised. SAFETY: set before any gst init, no other thread reads it yet.
    if std::env::var_os("GST_VA_ALL_DRIVERS").is_none() {
        unsafe { std::env::set_var("GST_VA_ALL_DRIVERS", "1") };
    }
    let Ok(conn) = zbus::Connection::session().await else {
        eprintln!("no session bus - skipping");
        return Ok(());
    };
    let sc = zbus::Proxy::new(&conn, SC, "/org/gnome/Mutter/ScreenCast", SC).await?;
    // Presence check: reading Version fails if gnome-shell doesn't own the name.
    if sc.get_property::<i32>("Version").await.is_err() {
        eprintln!("org.gnome.Mutter.ScreenCast unavailable - skipping (need a live GNOME session)");
        return Ok(());
    }

    let connector = std::env::var("MUTTER_CONNECTOR").unwrap_or_else(|_| "eDP-1".to_string());
    let mode = match std::env::var("TEST_ZEROCOPY").as_deref() {
        Ok("false") | Ok("off") => ZeroCopyMode::Off,
        Ok("true") => ZeroCopyMode::Force,
        _ => ZeroCopyMode::Auto,
    };

    // CreateSession -> RecordMonitor -> (subscribe) -> Start -> PipeWireStreamAdded(node_id).
    let empty: HashMap<&str, Value> = HashMap::new();
    let session_path: OwnedObjectPath = sc.call("CreateSession", &(&empty,)).await?;
    let session =
        zbus::Proxy::new(&conn, SC, session_path.clone(), format!("{SC}.Session")).await?;

    let stream_path: OwnedObjectPath = session
        .call("RecordMonitor", &(connector.as_str(), &empty))
        .await?;
    let stream = zbus::Proxy::new(&conn, SC, stream_path, format!("{SC}.Stream")).await?;
    let mut added = stream.receive_signal("PipeWireStreamAdded").await?;

    let _: () = session.call("Start", &()).await?;

    let msg = tokio::time::timeout(Duration::from_secs(10), added.next())
        .await
        .map_err(|_| eyre!("timed out waiting for PipeWireStreamAdded"))?
        .ok_or_else(|| eyre!("PipeWireStreamAdded stream ended"))?;
    let node_id: u32 = msg.body().deserialize()?;
    eprintln!("Mutter ScreenCast: connector={connector}, node={node_id}, zerocopy={mode:?}");

    // Encode the desktop stream, scaled to an accessory-like 1080x600, and count access units.
    let (video_tx, mut video_rx) = mpsc::channel::<EncodedFrame>(64);
    let (dsize_tx, _dsize_rx) = tokio::sync::watch::channel::<Option<(u32, u32)>>(None);
    // No accessory here to ask for keyframes; the encoder's request pump just sees a closed channel.
    let (_keyframe_tx, keyframe_rx) = mpsc::channel::<()>(1);
    let _encoder = AbortOnDropHandle::new(tokio::spawn(async move {
        if let Err(e) = encode_pipewire_to_channel(
            node_id,
            1080,
            600,
            mode,
            carjack::encode::resolve_codec(carjack::config::CodecChoice::Auto, true),
            Bitrate::default(),
            video_tx,
            dsize_tx,
            keyframe_rx,
        )
        .await
        {
            eprintln!("encoder error: {e}");
        }
    }));
    let count = Arc::new(AtomicU64::new(0));
    let keyframes = Arc::new(AtomicU64::new(0));
    // Time to the *first keyframe*: `run_video_stream` holds RECORD until one exists, and the head
    // unit tears the session down ~2s after carjack stops making progress, so this is the number
    // that decides whether a desktop-mode session can ever start.
    let first_keyframe_ms = Arc::new(AtomicU64::new(0));
    let started = std::time::Instant::now();
    let (count2, kf2, kf_at) = (count.clone(), keyframes.clone(), first_keyframe_ms.clone());
    let _counter = AbortOnDropHandle::new(tokio::spawn(async move {
        while let Some(f) = video_rx.recv().await {
            let n = count2.fetch_add(1, Ordering::Relaxed) + 1;
            let is_kf = f.config.is_some();
            if is_kf {
                kf2.fetch_add(1, Ordering::Relaxed);
                let _ = kf_at.compare_exchange(
                    0,
                    started.elapsed().as_millis() as u64,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                );
            }
            if n % 30 == 1 {
                eprintln!("encoded AU #{n} ({} bytes, keyframe={is_kf})", f.data.len());
            }
        }
    }));

    tokio::time::sleep(Duration::from_secs(5)).await;
    let n = count.load(Ordering::Relaxed);
    let kf = keyframes.load(Ordering::Relaxed);
    let kf_ms = first_keyframe_ms.load(Ordering::Relaxed);
    eprintln!("desktop encode: {n} AUs in 5s ({kf} keyframes), first keyframe at {kf_ms}ms");

    let _: () = session.call("Stop", &()).await.unwrap_or(());
    // Keep the D-Bus proxies alive until here so the ScreenCast session doesn't close early.
    drop((sc, session, stream, conn));

    assert!(
        n > 0,
        "no H264 produced from the desktop PipeWire stream ({mode:?})"
    );
    // Regression guard for the nvenc gop-size=30 + system-memory all-IDR quirk: a mostly-static
    // desktop must produce inter (P) frames, not an IDR every frame.
    assert!(
        kf < n,
        "every frame was a keyframe ({kf}/{n}) - encoder isn't doing inter-prediction"
    );
    Ok(())
}
