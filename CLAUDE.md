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
- just run - builds and runs inside a nested sway session (see below). Requires
  `sway` on PATH - on the primary dev machine that means running it from
  inside the toolbox (`sway` is not installed on the host), e.g.
  `toolbox run just run`.
- just test-run [timeout] - **use this instead of `just run`/bare `cargo run`
  for any live/manual testing.** carjack drives the real host NetworkManager
  and BlueZ over D-Bus - including from inside toolbox, which shares the
  host's D-Bus session - so an unguarded run can disconnect the dev machine's
  actual WiFi or leave stray processes behind if killed mid-run (this has
  happened). `test-run` caps the run to `timeout` seconds (default 60), then
  force-kills any leftover carjack/sway processes and restores whatever
  NetworkManager connection was active before the run, no matter how it
  ended.

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

## Screen capture & input

`src/wayland_capture.rs` talks directly to the compositor via
`ext-image-copy-capture-v1` (capture) and `wlr-virtual-pointer-unstable-v1`
(touch, emulated as pointer clicks/drags - there's no standalone "virtual
touch" Wayland protocol). This needs no consent dialog, which matters on a
headless embedded device with nobody present to click "allow". It only works
on compositors that implement these protocols (wlroots-based ones - sway,
cage - and KWin); GNOME/Mutter implements neither by design, so `src/main.rs`
falls back to the xdg-desktop-portal path in `src/portal.rs` (which prompts
for permission each run) whenever the Wayland globals aren't present.
`xdg-desktop-portal-wlr` has never implemented the RemoteDesktop portal at
all, so the portal fallback's touch injection only actually works under
GNOME/KDE, not sway/cage - that's an upstream limitation, not something
fixable from carjack.

Capture prefers DMA-BUF (GBM-allocated, **Linear modifier only**) over SHM
when the compositor offers it - that avoids the compositor's own GPU readback
copy, which is the meaningful zero-copy win. Only Linear is requested even when
the compositor offers more: tested live, handing GBM the full modifier list
picked a tiled/compressed layout unreadable outside the GPU's own pipeline.

Do NOT try to hand the dmabuf fd straight to GStreamer as `memory:DMABuf`
`GstMemory` (with or without `format=DMA_DRM` caps) - it was tried and reverted.
`videoscale`/`videorate`/`videoconvert` in this build reject `memory:DMABuf`
buffers outright ("transform could not transform ... in anything we support" /
not-negotiated); a real fd-to-encoder zero-copy path needs DRM/VA-aware elements
(vapostproc/glupload/v4l2) the software pipeline doesn't have. So the captured
frame gets one CPU copy (mmap -> `Vec`) into a plain `CapturedFrame`; the
per-push side is still zero-copy (an `Arc` refcount bump, `encode::FrameData`).

**Critical (this cost a long debugging session):** `ext-image-copy-capture`
only produces a new frame when the output actually changes (damage). A static
desktop yields exactly ONE frame and then capture blocks. But the car tears the
session down if it gets no video for ~1s. So `encode.rs` runs a **repeater**: a
30fps timer that re-pushes the *latest* captured frame into the appsrc
regardless of whether a new one arrived. Never revert to a "push only when a new
frame arrives" feeder - it looks fine on an animated desktop and silently dies
on a static one. (This is also why the working `pipewiresrc` path "just worked":
pipewiresrc provides its own clock/timestamps and steady buffer flow.) The
appsrc also needs `do-timestamp=true` - buffers are pushed with no PTS, and
without it videorate stalls and nothing reaches the encoder.

Touch coordinates (`TouchEvent.x`/`.y`) are passed straight through as pixel
coordinates in the captured/output space, unscaled - the same assumption the
portal path already made. This hasn't been validated against a real car head
unit's actual touch coordinate range/scale.
