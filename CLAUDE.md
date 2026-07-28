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
  **`[bluetooth] device` is required** - the head unit's MAC or advertised name
  (`CARJACK.BLUETOOTH.DEVICE` / `--bt-device`). carjack connects to exactly that
  device and errors out at startup if it isn't set; there is no scan-and-guess
  fallback (see the note under the iAP2 section).
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
  `CARJACK_DESKTOP_CMD` overrides the test client and `CARJACK_TEST_FRAMES` caps
  the frames sampled - both read as bare env vars, *not* via config (so
  `CARJACK.APP.COMMAND` does **not** affect the test; verified 2026-07-25).
  It needs host GPU/DRM at runtime. Note the brightness assertion is vacuous in
  zero-copy mode: dmabuf frames aren't CPU-readable so the test hardcodes
  `max_brightness`, and a client that paints nothing still passes. To actually
  see what gets composited use `/run-carjack` (`.claude/skills/run-carjack/`),
  which forces the SHM path and writes a PNG.
- To *see* carjack's output without a car: `.claude/skills/run-carjack/shot.sh
  --out /tmp/carjack.png` - the `/run-carjack` skill. carjack has no window, so
  this is the only way to eyeball what the car would receive.

## CarPlay / iAP2 protocol notes

- WiFi connectivity is handled only via NetworkManager (`nmrs`) - there was a
  `nl80211.rs`/`iw` fallback path, it was removed; don't re-add it.
- The target head unit is **configured, never auto-detected** (`[bluetooth]
  device`, MAC or advertised name). `wireless::find_target_device` looks it up in
  BlueZ's known devices, else waits for it in a discovery aimed at that one
  device. The old scan-and-score selection was removed: BlueZ never clears a
  bonded device's cached RSSI/UUIDs, so a powered-off head unit looked exactly
  like a present one and won, leaving carjack paging a dead address (Page
  Timeout) while the real unit sat discovered and ignored.
- Two Bluetooth RFCOMM profiles are registered: `IAP_ACCESSORY_UUID`
  (`...deafdecacaff`, `Role::Client`) and `IAP_DEVICE_UUID` (`...deafdecacafe`,
  `Role::Server`, channel=1). The last byte is the whole distinction: **`cafe` is
  the device (phone) side, `caff` the accessory (car) side.** The bike advertises
  `caff` and searches us for `cafe`.
- **Both channel directions work, and they race. Neither is required.** iAP2 has
  been observed carrying a full session over each:
  - **Outbound** (carjack -> the bike's `caff` *accessory* service, RFCOMM ch 2):
    `cfmoto_12` t=60.059 `Sent SABM Channel=2` -> `Rcvd UA` -> the bike sends
    `FF 55` on that channel at t=60.392, and the session runs to AirPlay RECORD.
    Same in `cfmoto_11` (dlci `0x04`). This comes from `auto_connect: Some(true)`
    on the registered profile - `device.connect()` brings it up; there is no
    `connect_profile` call in the code and none is needed.
  - **Inbound** (the bike -> our `cafe` *device* service, RFCOMM ch 1):
    `cfmoto_14` t=00:09:09.818 `Rcvd SABM Channel=1` -> our `UA` -> `FF 55`.
    carjack had *also* just queried the bike's iAP record (t=66.568) but the
    inbound open beat it by 7ms.
  - **The bike has never rejected us on the accessory profile.** There is not a
    single `Rcvd DM` in any capture. Do not go looking for a profile-level refusal;
    it isn't there.
  - For contrast, in `iphone-cfmoto.pklg` the phone's outbound ch 2 (t=281.789,
    accepted with `UA`) carries **zero bytes**, and its session arrives inbound on
    ch 1 at t=419.341. So the iPhone's outbound channel being idle is a fact about
    that capture, **not** a rule - carjack's outbound channel demonstrably works.
- **The full `device.connect()` is what makes a head unit engage - never `connect_profile`.**
  Verified live on the AUTO BOX (2026-07-28, 4/4 runs): the unit only opens its own device-profile
  channel - the one iAP2 actually runs on - after a reconnect that brings up **all** profiles, HFP
  included. `connect_profile(&IAP_ACCESSORY_UUID)` is worse than useless here: it leaves a
  connection in progress, so a follow-up `connect()` fails `br-connection-busy` **every time** and
  HFP never comes up at all, and the unit stays silent forever. The first channel offered is always
  carjack's own accessory channel, and it never carries anything.
- **Start every session from a fresh link.** A link that is already up is stale - the unit settled
  on it before carjack was listening and it never carries the switch. `spawn_relink` cycles it (or
  simply pages, if there is none) at session start, gated so a retry cannot queue a second
  disconnect onto an in-flight connect (`br-connection-canceled`). The stale channel is still
  accepted, but dies by `ECONNABORTED` when the disconnect lands rather than waiting out
  `SWITCH_TIMEOUT`, so **nothing in startup is gated on a timeout**. Measured: handshake ~8s,
  `RECORD sent` ~13-16s.
- **One channel per attempt.** Concurrent handshakes across several channels were built and removed
  (2026-07-28): on the AUTO BOX the channels are always offered strictly sequentially - the dead
  accessory one, then the unit's live device one - so it was never exercised, and removing it was
  ~5s *faster*. Reinstate only if a unit is actually observed offering two inside one switch window
  (the bike may: `cfmoto_14` has two 1.7s apart, and there the live channel is the accessory one).
- **Profile-level cycling is not an option.** `disconnect_profile` on our own *device* (server)
  UUID is refused with `InvalidArguments` - only a remote's service can be disconnected - so
  "cycle both iAP profiles" cannot be expressed, and it left the unit ~7s slower to offer its
  channel. Cycle the device.
- **Blocking the accept loop loses the switch window.** The AUTO BOX offers `FF 55` **once a second
  for ~20s** and then goes quiet for good. In `bench_3` carjack committed to a *stale* channel left
  over from before it started, and the recovery path's `device.connect()` then blocked ~38s - so the
  unit opened a fresh channel, shouted the switch 21 times into an unread socket, and had given up
  long before carjack looked. Nothing in the accept path may block on device operations.
- **Reconnect off the critical path, and still bring up every profile.** BlueZ's `Connect()` does
  not return until it has tried *all* of them (~38s on the bench unit) - but we do want them all:
  `connect_profile` alone leaves HFP/A2DP/AVRCP down, and the head unit expects those. So the
  recovery path spawns `connect_profile(&IAP_ACCESSORY_UUID)` (fast, and the only one the handshake
  needs) followed by `device.connect()` for the rest, gated by `connect_attempt` so a retry storm
  cannot queue connects. `ConnectProfile`'s UUID must be a service the **remote** advertises, so the
  accessory side (`...caff`) - both units list it, neither lists `...cafe`.
- **Nothing in the accept path may take the `connect_attempt` permit.** It used to serialise accepts
  against connects; with channels raced, accepting *while* a reconnect is in flight is precisely the
  behaviour wanted, and holding it would reintroduce the stall the rewrite exists to remove.
- **Most of carjack's outbound pages never reach the bike at all.** Across
  `cfmoto_11`..`cfmoto_14`, **9 of 12** `Create Connection`s came back
  `Connect Complete status=0x04` - Page Timeout. That is baseband: the unit's
  controller never answered, so nothing above it (SDP, profiles, RFCOMM) was even
  reached. **Do not read a failure to connect as a profile or SDP problem** - that
  mistake has been made repeatedly. `connect_accessory` now logs "Outbound page
  succeeded" vs "Outbound page timed out" explicitly; count those lines first.
- **There is no pre-emptive disconnect at session start any more, in either
  direction.** Connected: dropping a healthy link is what pushes the bike out of
  page-scan and into paging us, and once we let go we often cannot get it back.
  Not connected: there is nothing to drop, and bluer documents that `disconnect()`
  "can be also used to cancel a preceding Connect call before a reply to it has
  been received" - so on an idle device it can only cancel a connection in flight,
  including one the *bike* is making. Every `Disconnect` in the `cfmoto_14` run
  returned `Ok(())`, so it was only ever doing the first. The retry loop still
  disconnects when an attempt genuinely fails, which is the case the original call
  was written for. When a page *does* land every layer above works: `cfmoto_11` had
  zero timeouts and ran a full iAP2 session over carjack's own outbound channel.
- **The ACL is not the trigger; the head unit deciding to start a session is.** In
  `cfmoto_14` session 2 *our* page created the ACL and the bike still ran its full
  browse-then-open sequence on it and sent `FF 55`. So do not reason about who
  paged - reason about whether the bike ran the SDP Service Search for
  `00000000-deca-fade-deca-deafdecacafe`. If it did, the switch follows within
  ~250ms; if it did not, the channel stays mute. **What makes it decide is still
  unknown** - the BLE CarPlay advertisement is the designed trigger and was active
  during both successes and failures, so it is necessary-at-best, not sufficient.
- **What actually opens the iAP channel today** is the *bike* browsing our SDP:
  it sends a Service Search for `00000000-deca-fade-deca-deafdecacafe`, gets our
  record back (count = 1), opens RFCOMM ch 1, and sends `FF 55`. Verified in
  `cfmoto_14` session 2 at 00:09:09.006 -> SABM 00:09:09.818 -> `FF 55`
  00:09:10.074 - **on a laptop-initiated ACL**, so this is not tied to who paged.
- **The iAP SDP record and the CarPlay advertisement must live for the *process*, not the session.**
  They used to be locals in `wireless_carplay`, which is spawned per session - so every time a
  session ended the handles dropped, BlueZ deregistered the profiles, and carjack stopped
  advertising any iAP service until the next session re-registered. The head unit probes SDP on its
  own schedule, and hitting that window is fatal: in `bench_4.pcapng` the AUTO BOX asked for
  `00000000-deca-fade-deca-deafdecacafe` at t=1.665 and t=4.945, got `Attribute Lists [count = 0]`
  both times, concluded carjack was not an iAP device and **fell back to Android Auto** - after
  which it speaks AA framing (`00 08 00 04 08 01` = length 8, message id 4, protobuf `08 01`),
  which surfaces as `UnexpectedBytes` out of `perform_switch`. Reconnecting does not change its
  mind. Compare `cfmoto_14`, where every such query was answered `count = 1` and the handshake ran.
  Everything of that kind now lives in `IapHost`, built once behind a `OnceCell`.
- **`UnexpectedBytes` from `perform_switch` starting `00 08 00 04` means Android Auto**, not a
  corrupt switch. Check what carjack answered to the unit's SDP query before looking anywhere else.
- **The inbound accept must be filtered by the configured address.** Those profiles are
  registered for the whole adapter, so *any* bonded head unit that pages us arrives at
  `attempt_iap2_link` - and accepting whoever knocks first means carjack runs a full CarPlay
  session against the wrong unit. Seen 2026-07-27 (`cfmoto_14`): configured for the CFMOTO, the
  bike went quiet, and 58s later the bench AUTO BOX paged us and got streamed to instead.
  `attempt_iap2_link` now compares `rq.device()` to the target and `reject()`s anything else. The
  accept path itself stays - the head unit is what opens the RFCOMM channel (next bullet).
- **Either side may bring the ACL up; the head unit opens the RFCOMM channel.** Do *not* assume the
  bike only speaks iAP2 on a connection it originated - `cfmoto_14` session 2 disproves it: our
  `Sent Create Connection` created the ACL (`Connect Complete status=0x00`), the bike then sent
  `Rcvd SABM Channel=1` on it and the switch 248ms later, and the whole session ran. So carjack
  keeps paging outbound concurrently with the accept loop. (The older `cfmoto_6` note that the unit
  "requires a car-originated ACL" is at best too strong.)
- **`Accessory connection r=Ok(())` does not mean our page made the link.** `connect_accessory`
  resolves Ok just as readily when the unit connected first and our in-flight `device.connect()`
  simply completed. In `cfmoto_14` session 2 our page returned `br-connection-page-timeout` twice
  and the eventual Ok landed **7ms after** an inbound connect request. Read HCI (`Sent Create
  Connection` vs `Rcvd Connect Request`, plus the `Connect Complete` status) to know who paged -
  the log alone cannot tell you.
- **Why an accepted iAP channel is sometimes silent is still unexplained.** Both a working
  (`cfmoto_14` session 2) and a dead (session 1) channel arrived as inbound RFCOMM requests from
  the configured unit. The dead one never carried `FF 55` at all. `ProfileKind` is logged on every
  accepted request to help narrow this; see the log entry.
- **A silent iAP channel is the wrong channel - drop it fast.** `SWITCH_TIMEOUT` is 3s with no
  retry (it was 12s × 3 ≈ 38s). Measured: the bike sends `FF 55` 268ms after the channel opens and
  the AUTO BOX repeats it every ~1s, so re-reading a mute channel cannot help; only getting a new
  one can, and failing fast is what lets the caller go get it.
- **No switch means no SYN either - don't try to skip the switch.** `perform_switch` reads all six
  bytes and requires them to be `FF 55 02 00 EE 10`. A two-byte magic dispatch was built
  (2026-07-28) so an accessory already in iAP2 could open with `FF 5A ...` and go straight to its
  SYN; **tested on the hardware and reverted - an accessory that does not send the switch never
  sends a SYN either**, so there is no such state to handle. If the six bytes don't arrive, the
  channel is dead: drop it and get another.
- `src/bin/carjack-hci-helper.rs` is a **privileged sidecar**: it sets the CarPlay EIR
  UUID (`ADD_UUID`) and an iPhone-like Class of Device (`SET_DEV_CLASS`,
  `CARPLAY_DEV_CLASS`) over a raw HCI mgmt socket. It takes no arguments and
  resolves the adapter itself via bluer's default adapter. **`cap_net_admin`
  lives on this binary, never on the main `carjack` one** - a
  capability-carrying process is /proc-locked to root under
  `fs.suid_dumpable=2`, which makes xdg-desktop-portal fail to introspect it
  ("Unable to open /proc/<pid>/root") and breaks desktop mode entirely.
  `just build`'s `_setcap` recipe applies it, skipping the `run0` prompt when
  `getcap` already shows the capability.
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
- Hardware quirk: the test head unit (whichever `[bluetooth] device` names) degrades into an
  "aggressive reconnect" state (iAP2 handshake won't complete /
  `ProfileUnavailable` / already-connected errors) after several rapid reconnect
  cycles during iteration. A power cycle clears it - don't spend time debugging
  carjack if you see this pattern, just reboot the head unit.
- **The link layer honours what the SYN negotiates** (`iap2-proto/src/link.rs`). The accessory's
  SYN parameters used to be parsed and discarded; `LinkParams` now carries them and `LinkState`
  enforces them. The CFMOTO unit sends `max_outstanding=1, max_length=4096, retx_timeout=1000ms,
  cumack_timeout=500ms, max_retx=15, max_cumack=0`. Consequences worth knowing:
  - **The send window is one packet.** `send_packet` *queues* rather than blocking when the
    window is full - the session dispatches replies from inside its own receive loop, so waiting
    for an ack there would deadlock the link outright. `maintain` releases queued packets as
    acks arrive.
  - **`max_cumack = 0` means "no limit", not "ack everything"**, so a received packet is not
    acknowledged immediately; the `cumack_timeout` timer gives our reply a chance to piggyback
    the ack instead, which is what keeps the handshake at one packet per exchange.
  - Acks retire in-flight packets by a window-bounded comparison, *not* a plain wrapping
    "is this seq <= ack". An accessory that has not heard from us sends `ack = 0`, which under a
    naive half-space test "covers" our initial seq `0x9D` and retires a packet it never received.
  - `receive_packet` absorbs the link layer: bare acks, duplicates (re-acked, not dispatched
    twice), EAK, and `rst` -> `Iap2Error::LinkReset`. The session loop only ever sees session
    data. The keepalive/retransmit/ack timers are driven by `wait_for_link_work()` (pure, safe to
    cancel in a `select!`) followed by `maintain()` (transmits - must **not** be inside one).
- **The head unit walks away ~2s after carjack stops making progress**, and it takes down the
  *entire* session - Bluetooth included, with an RFCOMM DISC and the AP dropping. Seen twice: 2.0s
  after RECORD in `cfmoto_12.pcapng` and 2.3s after `assert_modes` in `cfmoto_13.pcapng`, so it is
  not specifically a post-RECORD video watchdog. The failure looks nothing like "no video" - it
  looks like the car rebooting, because both radios go at once.
- **Never pause between `assert_modes` and `setup_screen`.** SETUP has to follow the modes
  assertion immediately (2ms in `cfmoto_12`, which is the only capture where the car went on to
  answer RECORD). Putting a wait there cost the session twice - `cfmoto_13` and `cfmoto_14` - and
  when the unit leaves it drops its AP *and* Bluetooth together, so whatever carjack tries to send
  next never even reaches the wire (the neighbour stops resolving; you see unanswered NS and
  DHCP Discovers, not a TCP error).
- **RECORD is a promise: never send it without a keyframe already in hand.** So the gate
  (`wait_for_first_keyframe`) sits **after `setup_screen`, before `record`** - and nowhere else.
  It logs a warning every second it waits.
- **The encode pipeline is not why anything is slow, but a cold GStreamer registry is.** Measured:
  `tests/encode_startup.rs` (compositor, **34-66ms** to first keyframe across a no-op reconfigure,
  a real resize and `max_fps=0`) and `tests/desktop.rs` (portal/PipeWire, **~660ms**). But the
  *first* `gst::init()` builds the plugin registry - **~4.3s** on the dev box - and in desktop mode
  that used to happen inside the first session: `cfmoto_14` session 1 took 4.9s to its first
  keyframe and lost the car, session 2 took 296ms. `run_desktop` now calls `encode::init_gstreamer`
  at startup (app mode already got it via `resolve_backend`). **Anything that can be warmed up must
  be warmed up before a session starts** - the unit's tolerance is ~2s.
- **Keyframes are requested by the car and must actually reach the encoder.** The car sends
  `ForceKeyFrame` over the AirPlay event channel when it loses sync, and catplay refuses delta
  frames (`ScreenTransmitError::NeedsKeyframe`) until the stream opens on one. Both used to be
  acknowledged and ignored, leaving a frozen/torn picture until the next natural IDR up to a
  second later. `run_video_stream` now forwards both (plus its own backpressure drops) down a
  `keyframe_tx` channel, and `encode.rs` turns each request into an **upstream force-key-unit
  event on the encoder's src pad** - portable across every backend, no per-row special-casing.
  Requests are deduplicated while one is outstanding, or a sustained stall would pin the encoder
  to an all-IDR stream and make the congestion worse.
- The currently-evolving debugging state lives in **`docs/`**, not here. Start at
  `docs/status.md` (where things stand, what is untested, what to do next);
  `docs/investigation-log.md` is the dated record of what each capture showed,
  `docs/captures.md` inventories the captures themselves, and `docs/tooling.md`
  covers the build/run harness. Update those rather than duplicating their
  content into this file - and promote anything that turns out to be a durable
  rule about the code back *into* this file.

## Video source (`source` config): own app vs desktop portal

`source = "app"` (default) streams carjack's own compositor running `[app]
command`; `source = "desktop"` streams the user's *existing* desktop via
xdg-desktop-portal (capture) plus libei (touch). `main.rs` branches on
it: `run_app` (compositor + app + display cache, below) or `run_desktop`.
Both share `establish_carplay`
(the BT -> WiFi -> AirPlay handshake) and the per-session select loop; they differ
only in the video source and where touch goes.

Desktop mode uses **xdg-desktop-portal** for capture (`src/portal.rs`, ashpd):
`start_remote_desktop` opens a ScreenCast + RemoteDesktop session once at startup
(consent dialog on first run; `PersistMode::Application` remembers it) and yields a
PipeWire node id. `encode::encode_pipewire_to_channel` scales/re-times it to the car
(unlike the internal path - the desktop isn't the car's resolution). `[graphics]
zerocopy` picks the encode path (`build_pipewire_encode`) **based on the GPU that
renders the desktop**, so the encoder runs on the same GPU the ScreenCast dmabuf lives
on (encoding an Intel/AMD desktop on NVIDIA, or vice-versa, would be a cross-GPU copy).

**Touch goes over libei, not the portal's D-Bus methods** (`src/libei.rs`, the `reis`
crate). The portal still opens the session; `RemoteDesktop.ConnectToEIS` (portal API
v2) hands back an EI socket and input travels over that. Why, measured on GNOME 50 /
mutter 50.1 with a 3456x2160 panel at scale 2 (2026-07-26): through `NotifyTouch*`
three coordinate spaces disagree - `pipewiresrc` delivers the **physical buffer**
(3456x2160), the stream's advertised `size` is **logical** (1728x1080), and Mutter's
`meta_screen_cast_monitor_stream_transform_position` **divides** whatever it's given by
the monitor scale (whenever DisplayConfig's `layout-mode` is `logical`, i.e. normally).
xdg-desktop-portal's `check_position` validates against the *advertised* size, so
logical coords get halved by Mutter and physical coords come back `"Invalid position"`:
on a scaled monitor **no** coordinate both validates and lands right. libei has no such
validation layer - an `ei_device` advertises **regions** in logical pixels and touch is
addressed within one - so `send_touch` scales car coords into the region and adds its
offset, with **no display-scale division** (adding one is what halved touch before).
The region is picked by matching the video stream's `mapping_id` (that is what the id
is for), which also makes multi-monitor correct. The `NotifyTouch*` path survives only
as a fallback for portals older than API v2, where it scales to the advertised size and
clamps just inside it (the check is exclusive, and CarPlay reports touch *at* the edge).

Two hard-won gotchas in that path, both verified live:
- **Ask for `DeviceType::Pointer`** in `SelectDevices` even though carjack only injects
  touch. Mutter's `add_viewport_devices` opens with
  `g_return_if_fail (eis_seat_has_capability (seat, EIS_DEVICE_CAP_POINTER_ABSOLUTE))`,
  so Keyboard|Touchscreen alone yields a keyboard and *nothing else* - libei touch just
  never becomes available (the assertion shows up in gnome-shell's journal).
- **Never emit an out-of-order touch.** Mutter's EIS `release_slot` is a `g_assert` on
  the slot being acquired, so an `up` for a slot that isn't down **aborts gnome-shell
  and takes the whole session down** (it did, during this work). A duplicate `down`
  leaks slots until `acquire_slot` fails, and a `motion` for an unknown id moves slot 0
  - the wrong finger. `SlotTracker` filters every event against the compositor's
  believed state, is cleared whenever a device pauses/is removed/disconnects (Mutter
  drops its slots then), and `EiTouch::release_all` (called from `run_desktop` when a
  car session ends) lifts anything still held so a car vanishing mid-gesture can't leave
  a finger stuck down.
`portal::display_scale()` is now only used to *detect* the advertised-logical /
delivered-physical mismatch and warn once on the D-Bus fallback. It reads the scale from
**`wp_fractional_scale_manager_v1::get_fractional_scale`** (`preferred_scale` is a
numerator over 120, so 1.5 arrives exactly), with `wl_output.scale` as a fallback for
compositors lacking the protocol - that one is an integer, so a 1.5 desktop reports 2.
Gotcha: the scale is *per surface* and Mutter only sends `preferred_scale` once the
surface has a **role**, so the probe surface gets an `xdg_toplevel` (no role = no event,
ever). It never gets a buffer, so it can't map and no window appears.
`portal::tests::portal_session_uses_libei` is the live check (`#[ignore]`, needs a human
for the consent dialog): it asserts touch landed on libei, logs the regions, and with
`CARJACK_TEST_INJECT=1` injects a tap and verifies the EI connection survived it.
Landing accuracy was confirmed separately by aiming the session's absolute pointer at
the car view's top-left corner and watching GNOME's hot corner fire (and *not* fire for
a centre aim).

`config::desktop_render_driver()` finds it from the card with a connected display
(`/sys/class/drm/card*-<conn>/status`) and the `HW_BACKENDS` row whose `drivers`
match is tried first; the others stay as fallbacks (in table order), then software.
Each row's `desktop` field is its GPU chain, `desktop_memory` the memory feature it
hands the encoder:
- **VA-API / v4l2** (Intel/AMD/Qualcomm): `pipewiresrc ! vapostproc|v4l2convert !
  videorate ! caps(memory:DMABuf) ! vah264enc|v4l2h264enc` - the postproc imports
  the dmabuf and does convert+scale on the GPU, genuine zero-copy. **Verified** on
  Intel via `tests/desktop.rs`. `run_desktop` sets `GST_VA_ALL_DRIVERS=1` (the gst
  `va` plugin skips the iGPU otherwise on a hybrid box); needs the iGPU's VA driver
  (`intel-media-driver`) on the run host.
- **NVIDIA GL**: `pipewiresrc ! glupload ! glcolorconvert ! glcolorscale !
  videorate ! nvh264enc`. Convert/scale on the GPU, but the *final* GL->CUDA handoff
  currently downloads to system memory (interop fails with
  `INVALID_GRAPHICS_CONTEXT`; `cudaupload` hits the same) - so it's not fully
  zero-copy. True NVIDIA zero-copy would need a direct dmabuf->CUDA EGL import like
  `cuda-dmabuf` (only fits a same-GPU NVIDIA desktop; this gst build lacks a CUDA
  scaler anyway).
- **Software**: `videorate ! videoscale ! videoconvert ! <software encoder>`.

`videorate` is memory-agnostic (`video/x-raw(ANY)`) so it re-times inside the GPU
chain. Note the `cuda` row's `gop-size=60`: when nvenc ends up fed *system memory*
(the GL path's final handoff downloads) it emits an IDR *every frame* at
`gop-size=30` specifically, so 60 is the one value that behaves on every path - which
is why one `encoder` string per row is enough. `tests/desktop.rs` drives GNOME's internal
`org.gnome.Mutter.ScreenCast` D-Bus API to exercise all this without a car or a
portal-consent dialog. `[app]`, `[display]`, and the display cache don't apply.
Needs a live desktop session with a portal backend (so: not the headless deployment
device), and a compositor with an EIS implementation for touch.

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
  `resolve_backend` picks a same-GPU hardware backend - a row of the single
  `HW_BACKENDS` table, probed in order (or forced via
  `CARPLAY_ENCODE_BACKEND=cuda|vaapi|v4l2`, which applies to the desktop path
  too). A row carries both video sources' chains (`import`/`postproc` for the
  compositor, `desktop`/`desktop_memory` for the portal) plus its tuned encoder,
  so **adding a platform = one row**:
  - **NVIDIA** (`cuda`, `import: Cuda`, no postproc): the `cuda-dmabuf` crate
    imports the block-linear dmabuf via EGL + `cuGraphicsEGLRegisterImage`,
    de-tiles it GPU->GPU into pitched `GstCudaMemory`, and pushes straight into
    `nvh264enc`. This exists because `glupload`/`vapostproc` **can't** import
    NVIDIA's tiled surface. It's the only backend that can fail *after* its
    elements probe OK (its CUDA context is bound to the render node), in which
    case probing falls through to the next row.
  - **Intel/AMD** (`vaapi`): `appsrc(DMABuf) ! vapostproc ! vah264enc`.
  - **Qualcomm/embedded** (`v4l2`):
    `appsrc(DMABuf) ! v4l2convert ! v4l2h264enc`. Untested off-device. Rate control
    goes through `extra-controls` (V4L2 controls) rather than encoder properties:
    `video_bitrate_mode=1` (CBR) + `video_bitrate` in bit/s. Keyframe interval is
    still left at the driver's default - a best guess to revisit on the device.
  - **Vulkan is deliberately NOT a row** (evaluated 2026-07-26, gst 1.28.5). It's
    the obvious vendor-neutral candidate, and the encoder does exist
    (`vulkanh264enc`, registered per-device as `vulkanh264device<N>enc` when a
    machine has several GPUs - so it can't even be named by a static table entry).
    But: (1) `vulkanupload` accepts only system memory / `memory:VulkanBuffer`, no
    `memory:DMABuf`, so feeding it the compositor's dmabuf needs a CPU roundtrip -
    strictly worse than the cuda/vaapi rows it would compete with; (2)
    `... ! vulkanupload ! vulkancolorconvert ! vulkanh264enc` **SIGSEGVs during
    preroll** on the NVIDIA dev box, both with the encoder's own device and with the
    loader pinned to a single ICD (`VK_LOADER_DRIVERS_SELECT`), so it's not a
    cross-device handoff. `vulkanupload ! vulkancolorconvert` to NV12 VulkanImage on
    its own works fine - the crash is the encoder. Revisit when `vulkanupload` grows
    dmabuf import; that's the change that would make a Vulkan row worth having (one
    row replacing vaapi+v4l2 on the embedded target).
- **Software**: the compositor reads the frame back to a `Vec` (one CPU copy)
  and `encode.rs` runs `appsrc(video/x-raw) ! videoconvert ! encoder`.
  `videoscale`/`videorate` were removed: the compositor already renders at the
  car's exact resolution and the feeder pushes a steady rate, so there's nothing
  to scale or re-time. `videoconvert` stays - most encoders (e.g. `x264enc`)
  won't accept the compositor's BGRx. The encoder is `DEFAULT_SW_ENCODER`
  (`x264enc speed-preset=ultrafast tune=zerolatency`, CBR, `key-int-max=30`),
  then `SW_ENCODER_FALLBACKS` (`openh264enc`), and finally - if the box has *no*
  CPU H264 encoder, which is the case on the NVIDIA dev machine - a `HW_BACKENDS`
  encoder fed system memory (each row's `encoder` is tuned to hold up there too, so
  it's used as-is). `zerocopy=false` means "no zero-copy import", not "don't
  touch the GPU", so that fallback is a feature, not a bug; watch the warning it
  logs if you expected a CPU encode.

Anti-stutter / low-latency (all in `encode.rs`). `CARPLAY_ENCODER` (a gst-launch
style `"name prop=val ..."` string) replaces the encoder on **every** path -
hardware rows included, not just software - so it can also break a hardware chain's
caps negotiation; that's the escape hatch's price:
- The encode feeder pushes on the compositor's frame production
  (`frame_rx.changed()`), **not** a second independent timer - two ~30fps clocks
  beat and periodically dup/drop a frame. The compositor render timer rearms to
  absolute deadlines (not `now + interval`) so per-frame render time doesn't drag
  the cadence.
- The CUDA de-tile buffers are pooled (`CudaPool` in `cuda-dmabuf`) so `import`
  reuses device memory instead of `cuMemAlloc`/`cuMemFree` per frame; buffers
  return to the pool via the GstCudaMemory free callback.
- Every backend's encoder (the `encoder` field of its `HW_BACKENDS` row, and
  `DEFAULT_SW_ENCODER`) is tuned the same way for real-time: ~1s keyframes
  (`gop-size`/`key-int-max=30`, not a fraction of a second - frequent IDRs are
  large and burst the car's WiFi); no reorder/lookahead delay (`bframes=0`, plus
  `tune=ultra-low-latency`/`rc-lookahead=0`/`zerolatency` on nvenc,
  `speed-preset=ultrafast tune=zerolatency` on x264); and CBR at `[graphics]
  bitrate` (steady, link-friendly) rather than the encoders' spiky defaults.
- **Bitrate is configurable** (`[graphics] bitrate` / `CARJACK.GRAPHICS.BITRATE` /
  `--bitrate`, in kbit/s, default **3000**). Each encoder description spells the
  unit it wants as a `{kbps}`/`{bps}` placeholder (openh264enc and the v4l2
  `extra-controls` take bit/s, everything else kbit/s) and `Bitrate::apply`
  substitutes it, so a new row just needs the right placeholder - a unit test
  fails if a row hardcodes a rate instead. `CARPLAY_ENCODER` replaces the whole
  description, so an override only gets the configured bitrate if it spells the
  placeholder itself.
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
