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
  compositor). Only the build is in the toolbox. Set `CARJACK_DESKTOP_CMD`
  (e.g. in `.env`) to a **host-installed** desktop app to stream into the
  compositor (quote values with spaces; leave unset to stream black).
- just test-run [timeout] - **use this instead of `just run`/bare `cargo run`
  for any live/manual testing.** carjack drives the real host NetworkManager
  and BlueZ over D-Bus - including from inside toolbox, which shares the
  host's D-Bus session - so an unguarded run can disconnect the dev machine's
  actual WiFi or leave stray processes behind if killed mid-run (this has
  happened). `test-run` caps the run to `timeout` seconds (default 60), then
  force-kills any leftover carjack processes and restores whatever
  NetworkManager connection was active before the run, no matter how it ended.
- To exercise the compositor standalone without a car: `CARJACK_TEST_COMPOSITOR=1`
  brings it up, runs a client, and checks frames contain client content.

## CarPlay / iAP2 protocol notes

- WiFi connectivity is handled only via NetworkManager (`nmrs`) - there was a
  `nl80211.rs`/`iw` fallback path, it was removed; don't re-add it.
- Two Bluetooth RFCOMM profiles are registered: `IAP_ACCESSORY_UUID`
  (`Role::Client`, `auto_connect=false`) and `IAP_DEVICE_UUID`
  (`Role::Server`, channel=1). `connect_profile(&IAP_ACCESSORY_UUID)` is what
  actually triggers the connection.
- iAP2 magic bytes are `0xFF 0x5A`; the iAP1 switch-to-iAP2 command starts
  with `FF 55 02 00 EE 10`.
- AirPlay/CarPlay video runs over TCP port 7000 on the car.
- On the primary dev machine's WiFi card, `iwlwifi` needed
  `disable_11ax=1` (via `/etc/modprobe.d/`) to avoid a HE/VHT rate mismatch
  when connecting to the car's AP - if WiFi association looks flaky on a new
  dev machine, check that first.
- `catplay_hap`/`catplay_carplay`/`catplay_util` are pinned to a fork
  (`github.com/cfraz89/catplay`, branch `fix/audio-output-formats-optional`)
  carrying an unmerged fix making `/info` plist fields optional
  (`audio_output_formats`, `limitedUI`, `oemIconVisible`, `rightHandDrive`) -
  real head units omit them and upstream's strict deserialize crashed on
  that. Revert to `catplay-labs/catplay` once that lands upstream.
- Hardware quirk: the test head unit (paired as "TESTUNIT-5678") degrades into
  an "aggressive reconnect" state (iAP2 handshake won't complete /
  `ProfileUnavailable` / already-connected errors) after several rapid
  reconnect cycles during iteration. A power cycle clears it - don't spend
  time debugging carjack if you see this pattern, just reboot the head unit.
- The detailed, currently-evolving debugging state (what's been tried, ruled
  out, current hypothesis for any open issue) lives in `summary.md`, not
  here - check that for where things stand, and update it rather than
  duplicating its content into this file.

## Internal compositor (`src/compositor.rs`)

carjack runs its **own** minimal headless Wayland compositor (Smithay 0.7,
GlesRenderer) rather than capturing from an external one. This is the core of
the screen/input architecture. The old approach (`ext-image-copy-capture-v1`
capture + `wlr-virtual-pointer` touch + xdg-desktop-portal fallback, in the now
deleted `wayland_capture.rs`/`portal.rs`) is gone - do not resurrect it.

Why own the compositor: (1) render at the car's *exact* resolution (learned
from AirPlay `/info` before the compositor starts) so no scaling is needed;
(2) choose the render GPU via the DRM render node (`CARJACK_RENDER_NODE`,
default `/dev/dri/renderD128`) - capture and encode GPU must match for
zero-copy; (3) get frames straight from the render loop; (4) inject touch
directly into a real `wl_touch` seat, which is **multitouch-capable** (the old
virtual-pointer path was single-pointer only - no gestures).

How it works: the compositor runs its calloop event loop on a dedicated thread
(GLES/EGL is thread-bound/`!Send`). On a **30fps timer** it composites connected
clients into a Linear dmabuf via `render_output`, reads it back to a
`CapturedFrame` (`Vec<u8>`, BGRx) with `ExportMem::copy_framebuffer`, and
publishes it on a `watch` channel that `encode.rs` consumes. Rendering on our
own timer (not on client damage) means a static desktop still emits frames -
the car tears the session down after ~1s without video, and this is what keeps
it fed (no separate repeater needed on the capture side, though the encode
feeder still re-pushes latest at 30fps). Clients are mapped fullscreen at the
output size (single-app kiosk). `main.rs` launches the app to stream via
`CARJACK_DESKTOP_CMD` (run through `sh -c`, `WAYLAND_DISPLAY` pointed at the
compositor's socket). Dropping the `Compositor` handle stops the loop and joins
the thread - important because the session flow restarts on failure.

The frame currently takes one CPU copy (GPU readback to `Vec`) because the
software GStreamer encode path can't consume dmabuf directly (see the encode
notes below). Full GPU zero-copy (forward the compositor's dmabuf fd straight to
a hardware encoder on the same GPU) is the remaining optimization, gated on the
capability-probed hardware backends in `encode.rs`.

### encode.rs gotchas (still apply)
- Do NOT hand a dmabuf to `videoscale`/`videorate`/`videoconvert` as
  `memory:DMABuf` (with or without `format=DMA_DRM`) - they reject it outright
  (not-negotiated). A real fd-to-encoder zero-copy path needs DRM/VA-aware
  elements (vapostproc/v4l2/glupload) - that's what `HW_BACKENDS` is for.
- The appsrc needs `do-timestamp=true`: buffers are pushed with no PTS and
  without it `videorate` stalls and nothing reaches the encoder.

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
