---
name: run-carjack
description: Build, run, test, and screenshot carjack, the wireless CarPlay bridge. Use when asked to start or run carjack, build it, run its tests, capture/screenshot what it streams to the car, or verify the compositor and encode pipeline.
---

carjack streams a Wayland desktop to a car head unit over CarPlay. It has **no window** — its
entire visual output is the frame its internal Smithay compositor composites and hands to the H264
encoder. So "screenshot carjack" means "pull a frame off that stream", and the handle for that is
`.claude/skills/run-carjack/shot.sh`, which brings the compositor up on the real GPU, runs a
Wayland client inside it, and writes a PNG.

All paths below are relative to the repo root. **All commands here were run on a Fedora host
(`thunderhorse-mk2`) with a real GPU, BlueZ and iwd — not a container.** That matters:
see [Safety](#safety) before running anything that touches the car.

## Safety

carjack drives the **host's real iwd and BlueZ over D-Bus**. An unguarded run can
disconnect this machine's actual WiFi (it has happened) or leave stray processes behind.

- The screenshot driver and the tests below are safe: they touch **only** the GPU/compositor and
  never open Bluetooth, iwd, or a socket to the car.
- Anything that starts the full app (`just run`, `just test-run`) is **not** agent-safe — see
  [Live run](#live-run-needs-the-car-not-run-here).

## Prerequisites

Build deps live in a Fedora **toolbox** container; the binary then runs on the **host** (that is
where the DRM render node is). The container already exists here:

```bash
toolbox list -c
```

```
CONTAINER ID  CONTAINER NAME     CREATED       STATUS   IMAGE NAME
7f9bfcd919c7  fedora-toolbox-44  2 months ago  running  registry.fedoraproject.org/fedora-toolbox:44
```

If it is missing, create it and install the deps from `fedora-deps` inside it:

```bash
sudo dnf install gstreamer1-devel gstreamer1-plugins-base-devel pipewire-gstreamer dbus-devel \
  mesa-libgbm-devel libdrm-devel libxkbcommon-devel mesa-libEGL-devel mesa-libGLES-devel
```

`foot` (the default screenshot client) and ImageMagick (`magick`, for PPM→PNG) must be on the
**host**. Both were already present.

## Build

`.env` sets `TOOLBOX_BUILD=true` and `just` has `set dotenv-load`, so plain `just` does the right
thing — no need to export anything:

```bash
just build
```

```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.16s
```

## Run (agent path)

One command. Builds the driver in the toolbox, runs it on the host, writes a PNG:

```bash
.claude/skills/run-carjack/shot.sh --out /tmp/carjack.png --frames 45
```

```
render node: /dev/dri/renderD129
compositor up on "wayland-1"
client: foot -e sh -c 'printf "\033[48;5;27m\033[2J\033[H  carjack screenshot driver\n"; sleep 120'
captured 45 frames, best sample-sum=3323265 at 1280x720
wrote /tmp/carjack.png
```

Then **actually look at the PNG** — read it as an image. `sample-sum` is the black-detector: it
sums R/G/B over a sampled subset of pixels. Content scores in the millions; a black frame scores
`0` and the driver prints an explicit `WARNING`.

Takes ~20–30 s (it samples `--frames` frames at 30 fps, plus build time).

| flag | what it does |
|---|---|
| `--out PATH` | output PNG (default `/tmp/carjack-screenshot.png`) |
| `--width N` / `--height N` | compositor size (default 1280×720) — use the car's real size to test resize |
| `--fps N` | compositor render rate (default 30) |
| `--frames N` | how many frames to sample before picking the brightest (default 60) |
| `--command CMD` | run a different Wayland client instead of `foot` |
| `--no-client` | bare compositor, no client — expect a black frame |

Verified variants:

```bash
# The car-ish resolution — confirms the client resizes with the output.
.claude/skills/run-carjack/shot.sh --out /tmp/small.png --width 800 --height 480 --frames 40
```
```
captured 40 frames, best sample-sum=1385936 at 800x480
```

```bash
# Bare compositor: proves the black-detector fires rather than silently writing a blank PNG.
.claude/skills/run-carjack/shot.sh --out /tmp/bare.png --no-client --frames 15
```
```
captured 15 frames, best sample-sum=0 at 1280x720
WARNING: frame looks black - the client probably never painted. Check the client command runs under Wayland.
```

The driver is `.claude/skills/run-carjack/screenshot.rs`, registered as a cargo example
(`[[example]] name = "screenshot"` in `Cargo.toml`) so it builds with the workspace. To run it
directly, having already built:

```bash
./target/debug/examples/screenshot --out /tmp/carjack.png --frames 45
```

## Test

```bash
just test compositor
```

```
test compositor_frames_contain_client_content ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 3.25s
```

`just test <name>` is a **substring filter over test names**, not a file selector — `compositor`
matches only `compositor_frames_contain_client_content`. The other test in that file needs its own
invocation:

```bash
just test reconfigure
```

```
test reconfigure_switches_frame_size ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.28s
```

Config/unit tests (no GPU needed):

```bash
just test config
```

```
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; finished in 0.00s
```

## Live run (needs the car — NOT run here)

`just run` and `just test-run [timeout]` start the **whole** app: Bluetooth → WiFi → AirPlay to a
real head unit. **These were deliberately not executed in this session** — no head unit was
present, and they mutate host state. Two hard blockers an agent will hit:

- They run `sudo setcap` on the `carjack-wireless-helper` sidecar. With no cached sudo credential
  this **prompts for a password**, so it fails or hangs non-interactively. Run `sudo -v` first.
- `just run` begins with `waydroid session stop && …`; the `&&` means the run is skipped entirely
  if that command fails.

Prefer `just test-run 60` over `just run` — it caps the run, force-kills leftover carjack
processes, and reconnects whatever network iwd was on beforehand, however it ends.

## Gotchas

- **`just test compositor` cannot detect a black feed on this machine.** The brightness assertion
  is vacuous in zero-copy mode: `CapturedFrame::Dmabuf` pixels aren't CPU-readable, so the test
  hardcodes `max_brightness = 1_000_000` and asserts against that. Proven — a client that ran only
  `touch` and painted *nothing* still passed. `renderD129` here is **nvidia**, `zerocopy` defaults
  to `auto`, so frames are dmabufs and the check never bites. **The screenshot driver is the only
  way to verify content actually renders**; it forces the SHM path (`zero_copy: false`) precisely
  so there are pixels to look at.
- **Screenshots require the SHM path.** Starting the compositor with `zero_copy: true` yields
  `CapturedFrame::Dmabuf` with no CPU-readable pixels. The driver passes `false` and errors out
  loudly if it ever sees a dmabuf.
- **`CARJACK_DESKTOP_CMD`, not `CARJACK.APP.COMMAND`, overrides the compositor test's client.**
  CLAUDE.md claims the latter. It is wrong — verified by having each variant `touch` a marker file:
  `CARJACK.APP.COMMAND` never created it, `CARJACK_DESKTOP_CMD` did. The test reads the bare env
  var directly (`tests/compositor.rs:40`), bypassing the config system.
- **The compositor test's `tracing::info!` output is invisible.** It never installs a subscriber,
  so `--nocapture` and `RUST_LOG` show nothing. Debug it with side effects (a marker file) or use
  the screenshot driver instead.
- **`toolbox run` hangs forever without `< /dev/null`** — it allocates a podman `--tty`. Every
  invocation in the Justfile and in `shot.sh` redirects stdin for this reason.
- **`Unable to become drm master, assuming unprivileged mode`** on every start is normal and
  harmless — the compositor renders headless to the render node and never needs DRM master.
- **`foot` prints a wall of `compositor does not implement …` warnings** (primary selection, XDG
  activation, fractional scaling, server-side cursors, decoration manager). All expected: carjack's
  compositor is a deliberately minimal single-app kiosk. Filter with `grep -vE '^warn:'`.
- **Frame orientation is correct** — verified visually, not just numerically. CLAUDE.md lists this
  as unverified ("GL renders bottom-up… if the car shows an upside-down image"). A captured frame
  has the client's top line at the top of the PNG, so the `copy_framebuffer` readback is **not**
  flipped.
- **`waydroid session status` is not a valid subcommand** (only `start`/`stop`), so it can't be
  used to check whether a session is live.

## Troubleshooting

- **`rust-lld: error: unable to find library -lgstreamer-1.0`** (and `-lgstbase-1.0`, `-ldrm`,
  `-lglib-2.0`): you ran bare `cargo` on the host. The `-devel` libs are only in the toolbox — use
  `just build` / `just test <name>` / `shot.sh`, which re-exec into it.
- **`no head unit configured`**: `[bluetooth] device` is unset. Required since device
  auto-selection was removed. Set it in `~/.config/carjack/config.toml`, or pass
  `--bt-device <MAC|NAME>`, or set `CARJACK.BLUETOOTH.DEVICE` (`.env` already sets it).
- **Screenshot is black / `sample-sum=0` with a client**: the client failed to start under Wayland.
  The driver's client inherits only `WAYLAND_DISPLAY`; check the command runs standalone, and note
  GPU clients (Waydroid) need 20–30 s to boot — raise `--frames`.
- **`no magick/convert on PATH`**: the driver still wrote the raw frame as a `.ppm` next to the
  requested output and printed its path. Convert or view that instead.
