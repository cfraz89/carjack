//! carjack driver: inject a synthetic edge swipe and see whether the client reacted.
//!
//! Answers one question without a head unit: does a touch that starts at the very bottom row of
//! the compositor reach the client and trigger Android's swipe-up-for-home? The accessory's HID
//! logical maximum is the pixel *count*, so the bike's bottom edge arrives as y == height, one
//! row past the surface - and an out-of-bounds DOWN latches no focus, dropping the whole gesture.
//! `--start-y` picks the row to start from so both sides of that boundary can be tried.
//!
//! Like `screenshot.rs` this touches ONLY the GPU/compositor - never Bluetooth, NetworkManager or
//! the accessory - so it is safe to run on the board while the bike is nowhere near.
//!
//!     cargo run --example touchtest -- --command 'waydroid show-full-ui' --start-y 1056

use std::io::Write;
use std::process::Command;
use std::time::Duration;

use carjack::compositor::{CapturedFrame, Compositor};
use carjack::config::{Cli, Config};
use clap::Parser;
use eyre::{Result, eyre};
use libcarjack::carplay_init::{TouchEvent, TouchEventType};

struct Args {
    out: String,
    width: u32,
    height: u32,
    fps: u32,
    command: Option<String>,
    start_y: Option<u32>,
    /// `false` swipes up from the bottom edge (home), `true` swipes down from the top edge
    /// (the notification shade) - the two gestures live on opposite edges.
    down: bool,
    settle_ms: u64,
    pre_swipe: Option<String>,
    pre_swipe_ms: u64,
    warmup_swipe: bool,
    tap: Option<(f64, f64)>,
}

fn parse_args() -> Result<Args> {
    let mut a = Args {
        out: "/tmp/carjack-touch".into(),
        width: 800,
        height: 1056,
        fps: 30,
        command: None,
        start_y: None,
        down: false,
        settle_ms: 2500,
        pre_swipe: None,
        pre_swipe_ms: 6000,
        warmup_swipe: false,
        tap: None,
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let next = |i: usize| -> Result<String> {
            argv.get(i + 1)
                .cloned()
                .ok_or_else(|| eyre!("{} needs a value", argv[i]))
        };
        match argv[i].as_str() {
            "--out" => a.out = next(i)?,
            "--width" => a.width = next(i)?.parse()?,
            "--height" => a.height = next(i)?.parse()?,
            "--fps" => a.fps = next(i)?.parse()?,
            "--command" => a.command = Some(next(i)?),
            "--start-y" => a.start_y = Some(next(i)?.parse()?),
            "--down" => {
                a.down = true;
                i += 1;
                continue;
            }
            "--settle-ms" => a.settle_ms = next(i)?.parse()?,
            "--warmup-swipe" => {
                a.warmup_swipe = true;
                i += 1;
                continue;
            }
            "--tap" => {
                let v = next(i)?;
                let (x, y) = v
                    .split_once(',')
                    .ok_or_else(|| eyre!("--tap wants X,Y"))?;
                a.tap = Some((x.trim().parse()?, y.trim().parse()?));
            }
            "--pre-swipe" => a.pre_swipe = Some(next(i)?),
            "--pre-swipe-ms" => a.pre_swipe_ms = next(i)?.parse()?,
            other => return Err(eyre!("unknown flag {other}")),
        }
        i += 2;
    }
    Ok(a)
}

/// Newest frame the compositor has published, as CPU-readable BGRx.
async fn grab(
    rx: &mut tokio::sync::watch::Receiver<Option<std::sync::Arc<CapturedFrame>>>,
) -> Result<(Vec<u8>, u32, u32)> {
    loop {
        rx.changed()
            .await
            .map_err(|_| eyre!("compositor stopped publishing frames"))?;
        let frame = rx.borrow_and_update().clone();
        let Some(frame) = frame else { continue };
        match &*frame {
            CapturedFrame::Shm {
                data,
                width,
                height,
                ..
            } => return Ok((data.clone(), *width, *height)),
            CapturedFrame::Dmabuf(_) => {
                return Err(eyre!("zero-copy frame has no CPU pixels; needs the SHM path"));
            }
        }
    }
}

/// Fraction of sampled pixels that changed by more than a small threshold.
fn diff_fraction(a: &[u8], b: &[u8]) -> f64 {
    let (mut changed, mut total) = (0u64, 0u64);
    for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)).step_by(13) {
        total += 1;
        let d = (pa[0] as i32 - pb[0] as i32).abs()
            + (pa[1] as i32 - pb[1] as i32).abs()
            + (pa[2] as i32 - pb[2] as i32).abs();
        if d > 24 {
            changed += 1;
        }
    }
    if total == 0 { 0.0 } else { changed as f64 / total as f64 }
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    let args = parse_args()?;

    let cfg = Config::load(&Cli::parse_from(["carjack", "--bt-device", "touchtest-unused"]))?;
    let node = carjack::config::resolve_render_node(&cfg.graphics.drm_device)?;
    println!("render node: {node}");

    // SHM, not zero-copy: the before/after comparison needs CPU-readable pixels.
    let comp = Compositor::start(
        node,
        carjack::encode::FrameFormat::Shm,
        Vec::new(),
        args.width,
        args.height,
        args.fps,
    )?;
    println!("compositor up {}x{} on {:?}", args.width, args.height, comp.socket_name);

    let mut child = match &args.command {
        Some(cmd) => {
            println!("client: {cmd}");
            Some(
                Command::new("sh")
                    .arg("-c")
                    .arg(cmd)
                    .env("WAYLAND_DISPLAY", &comp.socket_name)
                    .spawn()
                    .map_err(|e| eyre!("spawn client: {e}"))?,
            )
        }
        None => None,
    };

    let mut rx = comp.subscribe_frames();
    println!("letting the client paint for {} ms", args.settle_ms);
    tokio::time::sleep(Duration::from_millis(args.settle_ms)).await;
    if args.warmup_swipe {
        let y0 = args.start_y.unwrap_or(args.height - 1);
        println!("warm-up swipe from ({}, {y0}) to clear the keyguard", args.width / 2);
        swipe(&comp, (args.width / 2) as f64, y0, args.height, false).await?;
        tokio::time::sleep(Duration::from_millis(3000)).await;
    }

    if let Some((tx, ty)) = args.tap {
        println!("tapping ({tx}, {ty}) to open an app");
        comp.send_touch(&TouchEvent { event_type: TouchEventType::Down, slot: 0, x: tx, y: ty })?;
        tokio::time::sleep(Duration::from_millis(60)).await;
        comp.send_touch(&TouchEvent { event_type: TouchEventType::Up, slot: 0, x: tx, y: ty })?;
        tokio::time::sleep(Duration::from_millis(args.pre_swipe_ms)).await;
    }

    if let Some(cmd) = &args.pre_swipe {
        println!("pre-swipe: {cmd}");
        let st = Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .env("WAYLAND_DISPLAY", &comp.socket_name)
            .status()
            .map_err(|e| eyre!("pre-swipe command: {e}"))?;
        println!("pre-swipe exited {st}; settling {} ms", args.pre_swipe_ms);
        tokio::time::sleep(Duration::from_millis(args.pre_swipe_ms)).await;
    }

    let (before, w, h) = grab(&mut rx).await?;
    write_png(&format!("{}-before.png", args.out), &before, w, h)?;

    // Start on the bottom row unless told otherwise; `height` itself is the out-of-bounds case.
    let start_y = args
        .start_y
        .unwrap_or(if args.down { 0 } else { args.height - 1 });
    let x = (args.width / 2) as f64;
    let dir = if args.down { "down" } else { "up" };
    println!("swiping {dir} from ({x}, {start_y}) on a {}x{} surface", args.width, args.height);

    swipe(&comp, x, start_y, args.height, args.down).await?;

    println!("waiting for the client to react");
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let (after, w2, h2) = grab(&mut rx).await?;
    write_png(&format!("{}-after.png", args.out), &after, w2, h2)?;

    if let Some(c) = child.as_mut() {
        let _ = c.kill();
        let _ = c.wait();
    }
    drop(comp);

    let frac = diff_fraction(&before, &after);
    println!("changed pixels: {:.1}%", frac * 100.0);
    println!(
        "{}",
        if frac > 0.02 {
            "SCREEN CHANGED - the swipe reached the client"
        } else {
            "SCREEN UNCHANGED - the swipe did not register"
        }
    );
    Ok(())
}

/// One vertical swipe: DOWN at `start_y`, ~200 ms of smooth travel, then UP. The travel has to
/// be gradual - a single jump to the destination reads as a tap, not a fling. `down` swipes
/// towards the bottom (the shade pull) instead of up (home).
async fn swipe(comp: &Compositor, x: f64, start_y: u32, height: u32, down: bool) -> Result<()> {
    comp.send_touch(&TouchEvent {
        event_type: TouchEventType::Down,
        slot: 0,
        x,
        y: start_y as f64,
    })?;
    let steps = 12;
    let reach = if down {
        (height - 1).saturating_sub(start_y) as f64
    } else {
        start_y as f64
    };
    let travel = (height as f64 * 0.45).min(reach);
    let sign = if down { 1.0 } else { -1.0 };
    for s in 1..=steps {
        let y = start_y as f64 + sign * travel * (s as f64 / steps as f64);
        comp.send_touch(&TouchEvent { event_type: TouchEventType::Motion, slot: 0, x, y })?;
        tokio::time::sleep(Duration::from_millis(16)).await;
    }
    comp.send_touch(&TouchEvent {
        event_type: TouchEventType::Up,
        slot: 0,
        x,
        y: start_y as f64 + sign * travel,
    })?;
    Ok(())
}

/// Write BGRx pixels out as a PNG, falling back to leaving a PPM if ImageMagick is absent.
fn write_png(out: &str, bgrx: &[u8], w: u32, h: u32) -> Result<()> {
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for px in bgrx.chunks_exact(4) {
        rgb.extend_from_slice(&[px[2], px[1], px[0]]);
    }
    let ppm = format!("{out}.ppm");
    let mut f = std::fs::File::create(&ppm)?;
    write!(f, "P6\n{w} {h}\n255\n")?;
    f.write_all(&rgb)?;
    f.flush()?;
    let tool = ["magick", "convert"]
        .into_iter()
        .find(|t| Command::new(t).arg("-version").output().is_ok());
    match tool {
        Some(t) => {
            let st = Command::new(t).arg(&ppm).arg(out).status()?;
            if st.success() {
                let _ = std::fs::remove_file(&ppm);
                println!("wrote {out}");
            }
            Ok(())
        }
        None => {
            println!("no magick/convert; raw frame left at {ppm}");
            Ok(())
        }
    }
}
