//! carjack driver: capture what the car would actually see.
//!
//! carjack has no window. Its entire visual output is the frame its internal Smithay compositor
//! composites and hands to the H264 encoder - so "screenshot carjack" means "grab a frame off that
//! stream". This brings the compositor up on the real GPU, optionally runs a Wayland client inside
//! it, samples frames, and writes the brightest one to disk as PNG.
//!
//! Deliberately does NOT touch Bluetooth, NetworkManager or the car. It exercises the
//! compositor -> capture half of the pipeline only, which is the half you can verify without a
//! head unit (and without knocking this machine's WiFi over - see `just test-run`).
//!
//! Run it via the wrapper (handles the toolbox and env):
//!     .claude/skills/run-carjack/shot.sh --out /tmp/carjack.png
//!
//! Or directly:
//!     cargo run --example screenshot -- --out /tmp/carjack.png
//!
//! Flags: --out PATH, --width N, --height N, --fps N, --frames N, --command CMD, --no-client.

use std::io::Write;
use std::process::Command;

use carjack::compositor::{CapturedFrame, Compositor};
use carjack::config::{Cli, Config};
use clap::Parser;
use eyre::{Result, eyre};

struct Args {
    out: String,
    width: u32,
    height: u32,
    fps: u32,
    frames: u32,
    command: Option<String>,
    no_client: bool,
}

impl Args {
    fn parse() -> Result<Self> {
        // Hand-rolled rather than clap-derived: this binary takes a different shape of argument
        // than the main `Cli`, and pulling in a second derive just for six flags isn't worth it.
        let mut a = Args {
            out: "/tmp/carjack-screenshot.png".into(),
            width: 1280,
            height: 720,
            fps: 30,
            frames: 60,
            command: None,
            no_client: false,
        };
        let argv: Vec<String> = std::env::args().skip(1).collect();
        let mut i = 0;
        while i < argv.len() {
            let val = |i: &mut usize, name: &str| -> Result<String> {
                *i += 1;
                argv.get(*i)
                    .cloned()
                    .ok_or_else(|| eyre!("{name} needs a value"))
            };
            match argv[i].as_str() {
                "--out" => a.out = val(&mut i, "--out")?,
                "--width" => a.width = val(&mut i, "--width")?.parse()?,
                "--height" => a.height = val(&mut i, "--height")?.parse()?,
                "--fps" => a.fps = val(&mut i, "--fps")?.parse()?,
                "--frames" => a.frames = val(&mut i, "--frames")?.parse()?,
                "--command" => a.command = Some(val(&mut i, "--command")?),
                "--no-client" => a.no_client = true,
                "-h" | "--help" => {
                    println!(
                        "screenshot --out PATH [--width N] [--height N] [--fps N] \
                         [--frames N] [--command CMD] [--no-client]"
                    );
                    std::process::exit(0);
                }
                other => return Err(eyre!("unknown flag {other}")),
            }
            i += 1;
        }
        Ok(a)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,screenshot=info".into()),
        )
        .init();
    let args = Args::parse()?;

    // Resolve the render node exactly the way `main` does, so this picks the same GPU carjack
    // would use. Everything else about the encode backend is irrelevant here.
    let cfg = Config::load(&Cli::parse_from(["carjack", "--bt-device", "screenshot-unused"]))?;
    let node = carjack::config::resolve_render_node(&cfg.graphics.drm_device)?;
    println!("render node: {node}");

    // zero_copy = false is REQUIRED. In zero-copy mode the compositor forwards the GPU dmabuf
    // straight to the encoder and `CapturedFrame::Dmabuf` carries no CPU-readable pixels - there
    // is nothing to write to a PNG. Forcing the SHM readback path costs one copy per frame and is
    // what makes a screenshot possible at all.
    let comp = Compositor::start(node, false, args.width, args.height, args.fps)?;
    println!("compositor up on {:?}", comp.socket_name);

    let mut child = if args.no_client {
        None
    } else {
        // `foot` paints the terminal background over the whole surface, so a captured frame is
        // unmistakably non-black. Any Wayland client works via --command.
        let cmd = args.command.clone().unwrap_or_else(|| {
            "foot -e sh -c 'printf \"\\033[48;5;27m\\033[2J\\033[H  carjack screenshot driver\\n\"; sleep 120'".to_string()
        });
        println!("client: {cmd}");
        Some(
            Command::new("sh")
                .arg("-c")
                .arg(&cmd)
                .env("WAYLAND_DISPLAY", &comp.socket_name)
                .spawn()
                .map_err(|e| eyre!("spawn client: {e}"))?,
        )
    };

    // Keep the brightest frame seen. The client needs a moment to map and paint, so early frames
    // are legitimately the black clear - picking the max avoids racing the client's first paint.
    let mut rx = comp.subscribe_frames();
    let mut best: Option<(u64, Vec<u8>, u32, u32)> = None;
    let mut seen = 0u32;
    while seen < args.frames {
        if rx.changed().await.is_err() {
            break;
        }
        let Some(frame) = rx.borrow_and_update().clone() else {
            continue;
        };
        seen += 1;
        match &*frame {
            CapturedFrame::Shm {
                data,
                width,
                height,
                ..
            } => {
                // Sum R/G/B only. The 4th byte of BGRx is opaque 0xFF padding and would make even
                // a pure-black frame score high.
                let score: u64 = data
                    .chunks_exact(4)
                    .step_by(97)
                    .map(|p| p[0] as u64 + p[1] as u64 + p[2] as u64)
                    .sum();
                if best.as_ref().is_none_or(|(b, ..)| score > *b) {
                    best = Some((score, data.clone(), *width, *height));
                }
            }
            CapturedFrame::Dmabuf(_) => {
                return Err(eyre!(
                    "got a zero-copy dmabuf frame - screenshots need the SHM path; \
                     this build started the compositor in zero-copy mode"
                ));
            }
        }
    }
    if let Some(c) = child.as_mut() {
        let _ = c.kill();
        let _ = c.wait();
    }
    drop(comp);

    let (score, data, w, h) = best.ok_or_else(|| eyre!("no frames captured from the compositor"))?;
    println!("captured {seen} frames, best sample-sum={score} at {w}x{h}");
    if score < 10_000 {
        println!(
            "WARNING: frame looks black - the client probably never painted. \
             Check the client command runs under Wayland."
        );
    }

    write_png(&args.out, &data, w, h)?;
    println!("wrote {}", args.out);
    Ok(())
}

/// Write BGRx pixels out as a PNG.
///
/// Goes via a PPM and ImageMagick rather than adding a `png` crate: this driver is agent tooling
/// and not worth a new dependency in the workspace. If `magick`/`convert` is missing the PPM is
/// left in place and its path reported, which is still inspectable.
fn write_png(out: &str, bgrx: &[u8], w: u32, h: u32) -> Result<()> {
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for px in bgrx.chunks_exact(4) {
        rgb.extend_from_slice(&[px[2], px[1], px[0]]); // BGRx -> RGB
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
            if !st.success() {
                return Err(eyre!("{t} failed converting {ppm} -> {out}"));
            }
            let _ = std::fs::remove_file(&ppm);
            Ok(())
        }
        None => Err(eyre!(
            "no magick/convert on PATH; raw frame left at {ppm} (PPM, viewable in most tools)"
        )),
    }
}
