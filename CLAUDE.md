This project is carjack, which aims to implement ui streaming via carplay.
Wireless carplay is the first target, carplay over usb may come later.

It is split into a number of subprojects:

- iap2-proto - the core iap2 protocol implementation. Os agnostic.
- libcarjack - core carplay implementation, os agnostic.
- carjack - the root project. Implements linux version of carjack, which will
  stream a wayland desktop, using pipewire and gstreamer to convert the stream
  to h264 with hardware acceleration. It uses bluer to handle bluetooth, and
  networkmanager will be controlled using dbus via the zbus crate. so that the
  desktop may be streamed and controled via carplay.

In the future there may be a carjack-android, which will share the screen or
android auto via carplay.

To build and run the project, you will need to use the relevant just commands,
which wrap cargo in a toolbox runner, in order that dependencies are satisfied:

- just build - builds project
- just run - builds in the toolbox, then runs carjack on the **host** (it needs
  native host access to the GPU/DRM, NetworkManager and BlueZ, and runs its own
  internal Smithay compositor headless on the DRM render node - no external
  compositor). Only the build is in the toolbox. Configure a **host-installed**
  desktop app to stream via `[app] command` in the config file (or
  `CARJACK.APP.COMMAND` / `--command`); leave unset to stream black. Settings
  come from `~/.config/carjack/config.toml` (see `carjack.toml.example`),
  overridable by dotted `CARJACK.<config.path>` env vars (`.env` sets these; note
  the prefix separator is a dot, so `CARJACK.SOURCE`, `CARJACK.APP.COMMAND`) and
  launch flags (`carjack --help`); config module is `src/config.rs`.
- just test-run [timeout] - **use this instead of `just run`/bare `cargo run`
  for any live/manual testing.** carjack drives the real host NetworkManager and
  BlueZ over D-Bus - including from inside toolbox, which shares the host's
  D-Bus session - so an unguarded run can disconnect the dev machine's actual
  WiFi or leave stray processes behind if killed mid-run (this has happened).
  `test-run` caps the run to `timeout` seconds (default 60), then force-kills
  any leftover carjack processes and restores whatever NetworkManager connection
  was active before the run, no matter how it ended.
- To exercise the compositor standalone without a car: `just test compositor`
  (the `tests/compositor.rs` integration test) brings it up, runs a client, and
  checks the composited frames contain client content and drive the encode path.
  `CARJACK.APP.COMMAND` overrides the test client; `CARJACK_TEST_FRAMES` (read
  directly, not via config) caps the frames sampled. It needs host GPU/DRM at runtime.

## CarPlay / iAP2 protocol notes

- WiFi connectivity is handled only via NetworkManager (`nmrs`) - there was a
  `nl80211.rs`/`iw` fallback path, it was removed; don't re-add it.
- Two Bluetooth RFCOMM profiles are registered: `IAP_ACCESSORY_UUID`
  (`Role::Client`, `auto_connect=false`) and `IAP_DEVICE_UUID` (`Role::Server`,
  channel=1). `connect_profile(&IAP_ACCESSORY_UUID)` is what actually triggers
  the connection.
- iAP2 magic bytes are `0xFF 0x5A`; the iAP1 switch-to-iAP2 command starts with
  `FF 55 02 00 EE 10`.
- AirPlay/CarPlay video runs over TCP port 7000 on the car.
- On the primary dev machine's WiFi card, `iwlwifi` needed `disable_11ax=1` (via
  `/etc/modprobe.d/`) to avoid a HE/VHT rate mismatch when connecting to the
  car's AP - if WiFi association looks flaky on a new dev machine, check that
  first.
- `catplay_hap`/`catplay_carplay`/`catplay_util` are pinned to a fork
  (`github.com/cfraz89/catplay`, branch `fix/audio-output-formats-optional`)
  carrying an unmerged fix making `/info` plist fields optional
  (`audio_output_formats`, `limitedUI`, `oemIconVisible`, `rightHandDrive`) -
  real head units omit them and upstream's strict deserialize crashed on that.
  Revert to `catplay-labs/catplay` once that lands upstream.
- Hardware quirk: the test head unit (paired as "TESTUNIT-5678") degrades into an
  "aggressive reconnect" state (iAP2 handshake won't complete /
  `ProfileUnavailable` / already-connected errors) after several rapid reconnect
  cycles during iteration. A power cycle clears it - don't spend time debugging
  carjack if you see this pattern, just reboot the head unit.
- The detailed, currently-evolving debugging state (what's been tried, ruled
  out, current hypothesis for any open issue) lives in `summary.md`, not here -
  check that for where things stand, and update it rather than duplicating its
  content into this file.

## Video source (`source` config): own app vs desktop portal

`source = "app"` (default) streams carjack's own compositor running `[app]
command`; `source = "desktop"` streams the user's *existing* desktop via
xdg-desktop-portal. `main.rs` branches on
it: `run_app` (compositor + app + display cache, below) or `run_desktop` (portal).
Both share `establish_carplay`
(the BT -> WiFi -> AirPlay handshake) and the per-session select loop; they differ
only in the video source and where touch goes.

Desktop mode (`src/portal.rs`, ashpd): `start_remote_desktop` opens a portal
ScreenCast + RemoteDesktop session once at startup (consent dialog first run;
`PersistMode::Application` remembers it) and yields a PipeWire node id;
`encode::encode_pipewire_to_channel` scales/re-times it to the car (unlike the
internal path - the desktop isn't the car's resolution), and touch is injected
back via `RemoteDesktop::notify_touch_*`. `[graphics] zerocopy` picks the encode
path (`build_pipewire_encode`) **based on the GPU that renders the desktop** - so
the encoder runs on the same GPU the ScreenCast dmabuf lives on (encoding an
Intel/AMD desktop on NVIDIA, or vice-versa, would be a cross-GPU copy).
Touch is injected back via `RemoteDesktop::notify_touch_*`, but the car sends touch
in *its* (scaled-down) display pixels, while the portal wants the desktop *stream's*
pixels - so `RemoteDesktopStream::send_touch` scales car coords up by
`stream.size() / car_size` (else they land in the wrong spot).
`config::desktop_render_driver()` finds it from the card with a connected display
(`/sys/class/drm/card*-<conn>/status`). If it's NVIDIA -> prefer GL/NVENC; else ->
prefer the dmabuf postproc (VA-API/v4l2); fall through to the other if the preferred
elements are missing, then software. The paths:
- **VA-API / v4l2** (Intel/AMD/Qualcomm): `pipewiresrc ! vapostproc|v4l2convert !
  videorate ! caps(memory:DMABuf) ! vah264enc|v4l2h264enc`, via the shared
  `HW_BACKENDS` - the postproc imports the dmabuf and does convert+scale on the GPU,
  genuine zero-copy. **Verified** on Intel via `tests/desktop.rs`. `run_desktop`
  sets `GST_VA_ALL_DRIVERS=1` (the gst `va` plugin skips the iGPU otherwise on a
  hybrid box); needs the iGPU's VA driver (`intel-media-driver`) on the run host.
- **NVIDIA GL**: `pipewiresrc ! glupload ! glcolorconvert ! glcolorscale !
  videorate ! nvh264enc`. Convert/scale on the GPU, but the *final* GL->CUDA handoff
  currently downloads to system memory (interop fails with
  `INVALID_GRAPHICS_CONTEXT`; `cudaupload` hits the same) - so it's not fully
  zero-copy. True NVIDIA zero-copy would need a direct dmabuf->CUDA EGL import like
  `cuda-dmabuf` (only fits a same-GPU NVIDIA desktop; this gst build lacks a CUDA
  scaler anyway).
- **Software**: `videorate ! videoscale ! videoconvert`.

`videorate` is memory-agnostic (`video/x-raw(ANY)`) so it re-times inside the GPU
chain. When the encoder ends up on *system memory* (the GL/software paths feeding
`nvh264enc`), the path uses `DEFAULT_ENCODER_DESKTOP` (`gop-size=60`, not 30): nvenc
emits an IDR *every frame* with `gop-size=30` on system memory specifically (fine on
the internal CUDA path). `tests/desktop.rs` drives GNOME's internal
`org.gnome.Mutter.ScreenCast` D-Bus API to exercise all this without a car or a
portal-consent dialog. `[app]`, `[display]`, and the display cache don't apply.
Needs a desktop session with a portal backend (not the headless deployment device).

## Internal compositor (`src/compositor.rs`)

For `source = "app"`, carjack runs its **own** minimal headless Wayland
compositor (Smithay 0.7, GlesRenderer) rather than capturing from an external one.
This is the core of the internal screen/input architecture. (An older
always-on capture approach - `ext-image-copy-capture-v1` + `wlr-virtual-pointer`
in a deleted `wayland_capture.rs` - is gone; don't bring *that* back. The portal
path lives on only as the opt-in `source = "desktop"` mode above.)

Why own the compositor: (1) render at the car's _exact_ resolution + refresh
(learned from AirPlay `/info`) so no scaling is needed - the compositor starts
at the config default size (`[display]`) at launch and `Compositor::reconfigure`
resizes/re-times it once the car reports its display; (2) choose the render GPU
via `[graphics] drm_device` (resolved by `config::resolve_render_node` - `auto`
picks an encode-capable GPU, or a driver name/path/index) - capture and encode
GPU must match for zero-copy; (3) get frames straight from the render loop; (4)
inject touch directly into a real `wl_touch` seat, which is
**multitouch-capable** (the old virtual-pointer path was single-pointer only).

How it works: the compositor runs its calloop event loop on a dedicated thread
(GLES/EGL is thread-bound/`!Send`). On a **30fps timer** it composites connected
clients into a Linear dmabuf via `render_output`, reads it back to a
`CapturedFrame` (`Vec<u8>`, BGRx) with `ExportMem::copy_framebuffer`, and
publishes it on a `watch` channel that `encode.rs` consumes. Rendering on our
own timer (not on client damage) means a static desktop still emits frames - the
car tears the session down after ~1s without video, and this is what keeps it
fed (no separate repeater needed on the capture side, though the encode feeder
still re-pushes latest at 30fps; the timer period follows the car's refresh
after reconfigure). Clients are mapped fullscreen at the output size (single-app
kiosk); `reconfigure` re-sends each toplevel a fullscreen configure at the new
size, reallocates the render pool, and republishes one frame at the new size
synchronously so the encoder latches its caps onto the new dimensions. `main.rs`
starts the compositor and launches the app (`[app] command` / run through
`sh -c "exec ..."`, `WAYLAND_DISPLAY` pointed at the compositor's socket) _once
at launch_ - the compositor persists across session restarts (so a slow app like
Waydroid boots once), and each session reconfigures it rather than recreating
it. The `DesktopApp` owns the child (own process group, SIGKILLed on exit via
Drop); Ctrl-C (SIGINT) breaks the session loop and cleans up. Caveat: apps that
fix their resolution at boot (Android/Waydroid) **can't** live-resize.
`[app] resizable` handles this: `true` = launched at startup and left to resize
itself; `false` (default) = **not** launched until the car's resolution is known
(launched after the first `reconfigure`), and relaunched only on reconnects that
actually change the size. Setting `[display]` to the car's resolution avoids any
resize/relaunch.

Display cache (`src/display_cache.rs`, `~/.cache/carjack/displays.json`): each
session records the car's resolution/refresh keyed by the car's Bluetooth MAC
(surfaced early by `wireless_carplay` via `car_addr_tx`). At startup `main` sizes
the compositor from the *last* car's cached props and launches the app up front
(even a non-resizable one) - so Waydroid boots during BT/WiFi/AirPlay setup rather
than after. When AirPlay reports the real display, `reconfigure` is a no-op if the
cache was right (common single-car case); a wrong guess just resizes/relaunches as
usual and updates the cache.

Globals advertised: `wl_compositor`/`wl_subcompositor`, `wl_shm`, `xdg_wm_base`,
`wl_seat`, `wl_output`+`xdg_output`, `wl_data_device_manager`, and
**`zwp_linux_dmabuf_v1`** (formats from the GlesRenderer). The dmabuf global is
required for GPU-accelerated clients - Waydroid in particular renders Android on
the GPU and can't display at all without it (`DmabufHandler::dmabuf_imported`
validates by importing into the renderer). Note: Waydroid needs
`command = "waydroid show-full-ui"` (bare `waydroid` just prints help) with
`resizable = false`, and takes ~20-30s to boot Android (black until then; not a
teardown - the compositor keeps emitting frames throughout).

Backend is decided **once at launch** by `encode::resolve_backend` from
`[graphics]
zerocopy` (`auto` = try zero-copy then fall back to software with a
warning; `true` = require it, error out if unavailable; `false` = software) and
shared across sessions (`EncodeBackend`, holding the `Arc<CudaImporter>`). The
compositor is started in the matching mode (`is_zero_copy()`), so the frame
variant always matches the pipeline. Two families:

- **Zero-copy**: the compositor forwards its rendered dmabuf and
  `resolve_backend` picks a same-GPU hardware backend (auto-probed, or forced
  via `CARPLAY_ENCODE_BACKEND=cuda|vaapi|v4l2`):
  - **NVIDIA** (`cuda`): the `cuda-dmabuf` crate imports the block-linear dmabuf
    via EGL + `cuGraphicsEGLRegisterImage`, de-tiles it GPU->GPU into pitched
    `GstCudaMemory`, and pushes straight into `nvh264enc`. This exists because
    `glupload`/`vapostproc` **can't** import NVIDIA's tiled surface.
  - **Intel/AMD** (`vaapi`): `appsrc(DMABuf) ! vapostproc ! vah264enc`.
  - **Qualcomm/embedded** (`v4l2`):
    `appsrc(DMABuf) ! v4l2convert ! v4l2h264enc`. Untested off-device; keyframe
    control is a best guess to revisit. vaapi/v4l2 rows live in `HW_BACKENDS`;
    adding a platform = one row.
- **Software**: the compositor reads the frame back to a `Vec` (one CPU copy)
  and `encode.rs` runs `appsrc(video/x-raw) ! videoconvert ! encoder`.
  `videoscale`/`videorate` were removed: the compositor already renders at the
  car's exact resolution and the feeder pushes a steady rate, so there's nothing
  to scale or re-time. `videoconvert` stays - most encoders (e.g. `x264enc`)
  won't accept the compositor's BGRx.

Anti-stutter / low-latency (all in `encode.rs`, overridable via `CARPLAY_ENCODER`):
- The encode feeder pushes on the compositor's frame production
  (`frame_rx.changed()`), **not** a second independent timer - two ~30fps clocks
  beat and periodically dup/drop a frame. The compositor render timer rearms to
  absolute deadlines (not `now + interval`) so per-frame render time doesn't drag
  the cadence.
- The CUDA de-tile buffers are pooled (`CudaPool` in `cuda-dmabuf`) so `import`
  reuses device memory instead of `cuMemAlloc`/`cuMemFree` per frame; buffers
  return to the pool via the GstCudaMemory free callback.
- `DEFAULT_ENCODER` is tuned for real-time: `gop-size=30` (~1s keyframes, not a
  fraction of a second - frequent IDRs are large and burst the car's WiFi);
  `tune=ultra-low-latency` + `bframes=0` + `rc-lookahead=0` + `zerolatency` (no
  reorder/lookahead delay); `rc-mode=cbr bitrate=12000` (steady, link-friendly).
- A leaky `queue` (`leaky=downstream`, `max-size-buffers=3`) sits after the
  appsrc so a momentary encode/WiFi stall drops the oldest frame instead of
  growing latency.

### encode.rs gotchas (still apply)

- Do NOT hand a dmabuf to `videoscale`/`videorate`/`videoconvert` as
  `memory:DMABuf` (with or without `format=DMA_DRM`) - they reject it outright
  (not-negotiated). The zero-copy path uses DRM/VA-aware elements
  (vapostproc/v4l2) or the CUDA importer instead.
- The appsrc needs `do-timestamp=true`: buffers are pushed with no PTS and the
  encoder needs timestamps.
- CUDA gotcha: `gst_cuda_context_new` aborts unless libgstcuda's driver vtable
  is loaded first. The nvcodec plugin normally does that in its init, but we
  call the context ctor directly, so `cuda-dmabuf` calls
  `gst_cuda_load_library()` itself before creating the context.

### Unverified

- **Frame orientation**: GL renders bottom-up; `copy_framebuffer` readback
  orientation hasn't been checked against content with a clear top/bottom. If
  the car shows an upside-down image it's a one-line output-transform fix.
- **Touch**: multitouch is correct by construction (CarPlay per-finger slot ->
  `wl_touch` slot) but not yet validated end-to-end against a real client/car.
  Coordinates (`TouchEvent.x`/`.y`) are passed through as output-space pixels,
  unscaled - unverified against the car's actual touch range. `inject_touch`
  calls `frame()` per event; batching simultaneous points under one frame would
  be crisper if the car bursts multi-point updates.
