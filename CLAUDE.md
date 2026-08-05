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

- WiFi connectivity is handled only via NetworkManager (`nm-automata`, our own
  fully-generated bindings) - there was a `nl80211.rs`/`iw` fallback path, it was
  removed; don't re-add it. Every profile property is an `Option`: absent means
  "NetworkManager decides", so only set what actually matters.
- **The car's WiFi profile is created in memory, not on disk.** Its SSID and
  passphrase change per session, so a persistent profile per run accumulates
  without bound. `wifi_connect` calls `AddAndActivateConnection2` with
  `persist = "memory"` and `bind-activation = "dbus-client"`; NetworkManager then
  keeps it under `/run` and drops it when carjack's bus connection goes - which is
  why the `NetworkManagerProxy` is a process-lifetime `OnceCell`.
- **The car's profile sets `ipv6.method = "link-local"`.** The car answers no
  router solicitation, so `auto` can only ever time out on SLAAC; and its AirPlay
  address is link-local anyway. That setting is also what makes the activation's
  `IP6_READY` state flag mean "the link-local address has finished duplicate-address
  detection" - a link-local destination is unreachable from a *tentative* source
  (`connect()` fails `EADDRNOTAVAIL` before anything hits the wire), so
  `wait_for_activation` gates on that flag - and that gate is the whole of it. An
  rtnetlink DAD watcher used to re-check the flag by hand; it never once found
  anything left to wait for, so it and the `rtnetlink` dependency are gone.
- The target head unit is **configured, never auto-detected** (`[bluetooth]
  device`, MAC or advertised name). `wireless::find_target_device` looks it up in
  BlueZ's known devices, else waits for it in a discovery aimed at that one
  device. The old scan-and-score selection was removed: BlueZ never clears a
  bonded device's cached RSSI/UUIDs, so a powered-off head unit looked exactly
  like a present one and won, leaving carjack paging a dead address (Page
  Timeout) while the real unit sat discovered and ignored.
- **One Bluetooth RFCOMM profile is registered: `IAP_DEVICE_UUID`** (`...deafdecacafe`,
  `Role::Server`, channel=1), in `IapHost::start` - before any discovery or pairing, since a unit
  runs its SDP search while it bonds. The last byte is the whole distinction: **`cafe` is the
  device (phone) side, `caff` the accessory (car) side.** The bike advertises `caff` and searches
  us for `cafe`, so `cafe` is the record that matters. **Registering it is what makes a unit speak
  iAP at all**: with only the `IAP_ACCESSORY_UUID` client profile registered, the bench unit's SDP
  search finds no iAP service, decides we are an Android Auto sender, and answers `00 08 00 04 08
  01` on every attempt (`UnexpectedBytes` out of `perform_switch`).
- **The `IAP_ACCESSORY_UUID` client profile was removed - do not re-add it.** It never carried a
  switch in any run: bonded (its `auto_connect` off) the unit never even offered it, and unbonded
  it was offered first and was either aborted ~120us after accept or silent through the whole 3s
  timeout. A client-role profile publishes no SDP record, so dropping it cannot change how the unit
  sees us. Removing it also removed the bond-state plumbing that existed only to serve it - the
  accept is one stream again, and `was_paired` no longer reaches the link path.
- **Power the adapter up only after the profiles are registered, and never force it down.**
  `IapHost::start` registers the iAP and CarPlay profiles first and calls `set_powered(true)` last.
  A unit that connects while the adapter is live but our profiles are not offers its iAP channel to
  nobody, and that link then needs cycling - which the bike will not tolerate. On a box that boots
  with the unit (**the carjack box is powered by the bike**, so the race is otherwise unavoidable)
  set `[Policy] AutoEnable = false` in `/etc/bluetooth/main.conf`: the adapter then stays down until
  carjack powers it, closing the window entirely. **Do not force it down at startup** - tried, and
  it makes things worse: dropping a link the unit has already made means it never comes back
  (0/2 runs got any connection afterwards), the same way a disconnect without a page does.
  On the way out carjack powers the adapter **down** (`IapHost::power_down`, from both `run_app`
  and `run_desktop`), so between runs it is never reachable without carjack behind it. Best-effort:
  a second Ctrl-C or a SIGKILL - including `just test-run`'s timeout kill - exits first, and then
  `AutoEnable = false` is what keeps it down.
- **Only cycle a link that predates us; otherwise never page.** A link carjack just made by pairing,
  or one the unit brought up while we were already listening, carries the switch on its first
  channel - `prepare_device` samples `is_connected()` *before* pairing and reports it as
  `stale_link`, and `wireless_carplay` relinks only when it is true. A link that predates us is
  different: the unit offered its iAP channel while nobody was listening and will not offer again
  until it is cycled. Measured (trusted, fresh bond vs bonded reconnect): **no relink at all - 3/3
  clean fresh, 0/3 bonded; stale-link rule - 3/3 and 3/3**, with **zero `connect()`/`disconnect()`
  calls on a fresh bond**. That matters for the bike, which refuses inbound ACL connections and only
  reconnects from its dash: start carjack before the bike attaches and it is never paged.
  Half-measures do not work, all re-measured *with* the unit trusted: dropping a stale link without
  paging leaves it silent (2/3 runs got no offer); deregistering and re-registering the iAP profile
  instead of touching the ACL gets **no offer at all, 0/3**; and there is nothing to close at the
  fd level either, since on a stale link no channel is ever offered. When a cycle is needed it needs
  both halves. One relink at a time -
  a second disconnect landing on an in-flight connect cancels it.
- **Relink with `connect`, never `connect_profile`, and never await it inline.** `connect_profile`
  leaves a connection in progress so a following `connect()` fails busy and HFP never comes up,
  which leaves the unit silent; the whole profile set has to be up before it engages. And BlueZ does
  not return from `Connect()` until it has tried every profile, so awaiting it on the accept loop
  means the channel the unit opens meanwhile goes unread. Cycling only the iAP profiles is not an
  option either - `disconnect_profile` is refused for our own server UUID.
- **The relink's `disconnect` must finish before the accept loop runs; only its `connect` may
  overlap.** A `disconnect()` still in flight when the unit opens its channel tears the ACL down
  under the stream we just accepted, and `perform_switch` fails `ECONNABORTED` seconds in - looking
  exactly like the unit hanging up on us. `relink` therefore awaits the teardown and spawns only
  the page.
- **Trust the head unit (`set_trusted(true)`), or its first channel after a fresh bond is dead.**
  Untrusted, BlueZ refuses the unit's *inbound* profile connections - HFP is answered `DM`, AVDTP
  and HID get `Connection Response - Refused - security block` - and the unit will not drive iAP
  until its profile set is up. It opens the iAP channel anyway and then never sends the switch, so
  the handshake times out. **Measured: 6/6 clean first bonds trusted, against 1/17 untrusted** (and
  that one was chance). `prepare_device` used to `set_trusted(false)` deliberately, to keep the unit
  from connecting before we were listening; that is what cost us the handshake.
  This also explains the two things that never fitted. **Why the relink worked**: its outbound
  `connect()` brings the profile set up from our side, sidestepping the inbound refusals. **Why
  bonded reconnects were always clean**: the startup relink's `connect()` lands before the unit
  offers iAP, whereas on a fresh bond the pairing link already exists and the unit gets in first.
  Found by capturing our own side with `btmon` and diffing against `iphone-first-pair.pklg` - the
  refusals (`Sent DM Channel=13`, five times; `security block`, ten) are invisible from carjack's
  logs, which is why a day of black-box experiments went the wrong way. **Reach for a capture sooner.**
- **Everything below about the dead first channel was measured while we were refusing the unit's
  profiles, so treat it as conditional on that bug** rather than as current behaviour. Kept because
  the measurements are real and may matter again if the symptom returns. Ruled out as causes, each
  against a dead channel: SDP-registration ordering; which profile carries it; who opened it; who
  speaks first (sending the switch gets no echo - mute both ways); and adapter identity (CarPlay
  UUID in the EIR, iPhone Class of Device). Reset options, all on a fresh bond - **relink**: unit
  re-offers, channel live, ~11s; **nothing**: no re-offer, hangs indefinitely; **reject unread**:
  re-offers twice ~19s apart then quits; **deregister/re-register**: re-offers after ~57s, still
  dead; **own the mux then drop it** (outbound `connect_profile`, then `disconnect_profile`, which
  BlueZ *does* allow for a client UUID): re-offer after ~61s, still dead. The ~57s/~61s look like a
  timer in the unit rather than a reaction to us.
- **The iPhone's own reset is an L2CAP disconnect of the RFCOMM mux (PSM 0x0003), not an ACL cycle**
  - its ACL runs unbroken from t=94.881 to t=130.260 across the reset. We *can* collapse the mux
  (the kernel does it on idle - `Sent DISC Channel=0` two seconds after the last DLC closes, seen in
  our own btmon capture), but the unit owns the session when it creates it, and doing so never
  produced a live channel. Relevant if the bike proves awkward: it refuses inbound ACL connections
  and only reconnects when driven from its dash.
- **Skipping the startup relink does not avoid a relink**, it defers it to the failure path and
  costs ~4s (unbonded, discovery to link: 10.8s with, 14.6s without). A device channel that carries
  the switch has only ever appeared *after* a relink's disconnect - in the no-startup-relink run the
  pairing-established link sat live for 5s and produced nothing usable.
- **One channel per attempt.** Handshaking several concurrently was built and removed: units offer
  channels strictly sequentially, and after a fresh bond the first one offered is dead (above).
- **Outbound pages frequently return Page Timeout.** That is baseband - the unit's controller never
  answered - so nothing above it was reached. Do not read a failure to connect as a profile or SDP
  problem; `connect_accessory` logs page success/timeout explicitly.
- **Either side may bring the ACL up.** What matters is whether the unit then runs its SDP search
  for our iAP service and opens a channel - not who paged. If it finds no iAP service it decides we
  are an Android Auto sender and speaks AA framing instead, which surfaces as `UnexpectedBytes` from
  `perform_switch`.
- **The inbound accept must be filtered by the configured address.** The profile is
  registered for the whole adapter, so *any* bonded head unit that pages us arrives at
  `attempt_iap2_link` - and accepting whoever knocks first means carjack runs a full CarPlay
  session against the wrong unit. Seen 2026-07-27 (`cfmoto_14`): configured for the CFMOTO, the
  bike went quiet, and 58s later the bench AUTO BOX paged us and got streamed to instead.
  `attempt_iap2_link` now compares `rq.device()` to the target and `reject()`s anything else. The
  accept path itself stays - the head unit is what opens the RFCOMM channel (next bullet).
- **`Accessory connection r=Ok(())` does not mean our page made the link** - it resolves Ok just as
  readily when the unit connected first. Only HCI (`Sent Create Connection` vs `Rcvd Connect
  Request`, and the `Connect Complete` status) answers who paged.
- **A silent iAP channel is the wrong channel - drop it fast.** Re-reading a mute channel cannot
  help; only getting a different one can.
- **No switch means no SYN either - don't try to skip the switch.** An accessory that does not send
  `FF 55 02 00 EE 10` never sends a SYN either, so there is no already-switched state to handle.
- **The CarPlay UUID reaches the EIR by registering an RFCOMM profile for it**, not by a raw mgmt
  write: bluetoothd puts a registered profile's UUID in the adapter's EIR, which needs no privilege
  and - unlike `ADD_UUID` on a separate mgmt socket - is visible in `bluetoothctl show`, so it can
  be verified. `IapHost::start` registers `CARPLAY_DEVICE_UUID` purely for that; nothing handshakes
  on it and a drain task rejects anything that arrives (an unanswered request would pin its fd).
  A raw `ADD_UUID` never showed up in `bluetoothctl` at all, in either registration order.
- `src/bin/carjack-hci-helper.rs` is a **privileged sidecar, run by `IapHost::start`** after the
  profile registrations. It now does one thing: an iPhone-like Class of Device (`SET_DEV_CLASS`,
  `CARPLAY_DEV_CLASS`) over a raw HCI mgmt socket. Setting `[General] Class` in
  `/etc/bluetooth/main.conf` (this dev box has `0x7a020c`) does the same, but a deployment target's
  BlueZ config is not ours to control, so this is what makes the class portable - don't drop it as
  redundant. **`cap_net_admin` lives on this binary, never on the main `carjack` one** - a
  capability-carrying process is /proc-locked to root under `fs.suid_dumpable=2`, which makes
  xdg-desktop-portal fail to introspect it ("Unable to open /proc/<pid>/root") and breaks desktop
  mode entirely. `just build` does *not* setcap (it prompted on every build); `just setcap` does,
  and **a relink drops the capability again**. Missing capability is **not fatal**:
  `apply_device_class` warns and carries on.
- **No BLE advertisement.** `publish_advertisement` (CarPlay + iAP2 service UUIDs and Apple
  manufacturer data) was removed: head units reach us over BR/EDR, where an LE advertisement is not
  visible at all. Adapter identity that matters is the BR/EDR EIR, above.
- iAP2 magic bytes are `0xFF 0x5A`; the iAP1 switch-to-iAP2 command starts with
  `FF 55 02 00 EE 10`.
- AirPlay/CarPlay video runs over TCP port 7000 on the car.
- **Which side opens AirPlay depends on the unit, so carjack does both.** Once on the car's WiFi,
  `wait_for_carplay_ctrl_probe` advertises `_carplay-ctrl._tcp` and waits for the car's
  `GET /ctrl-int/1/connect`; if that times out and `0x4301` named a receiver, carjack connects
  there instead. A unit that browses Bonjour only invites a phone it has *discovered* (catplay's
  `cp_inviter.rs` loops doing nothing until one appears), so advertising is unconditional - the
  old "the bike doesn't invite us on mDNS anyway" note was circular, since carjack only advertised
  when `0x4301` omitted an address. **The record's `id` is the adapter's *Bluetooth* address**
  (`IapHost::bluetooth_address`), not the WiFi MAC - catplay documents the field as the phone's BT
  MAC, and it is how the car ties the station on its AP back to the device it authenticated over
  Bluetooth. **Measured on the bench AUTO BOX: advertising the Bluetooth address gets an invite in
  69 ms; the same unit advertising the WiFi MAC timed out at 15 s** - so on a unit that does invite,
  the `id` is the whole of it. **The bike does no Bonjour at all** - measured over 40 s of
  correctly-`id`ed advertising it neither browsed, advertised, nor invited, and its only IP traffic
  was DHCP and one ARP. A unit that names its receiver in `0x4301` is telling you to connect to it,
  so the wait before falling back is 3 s rather than 15. `0x4301` also carries what looks like the
  accessory's HomeKit pairing id (`WIFI_CONFIG_UUID`), which `WifiCredentials` parses and logs but
  does **not** pass to catplay - see the HomeKit note below.
- **AirPlay pairing is HAP, it does run, and the identity persists.** The bike's session on the
  wire is `/setup` -> `POST /pair-setup` x3 (`X-Apple-HKP: 2`) -> `POST /pair-verify` x2, after
  which the control channel is encrypted. (`start_pair_task()` sits commented out in catplay's
  bootstrap, which is a **red herring** - `do_initial_setup` runs the pairing itself.) carjack keeps
  a persistent identity and pairing table in `~/.local/share/carjack/homekit.bin`
  (`init_homekit_storage`, called before either source path), so a car we have met takes
  `/pair-verify` instead of a full `/pair-setup`. `connect_airplay` passes a pairing id **only**
  when storage already knows it: unknown, all an id can do is make `/pair-setup` assert an identity
  match and fail the session over a guess, and a first contact needs to be free to succeed and be
  remembered. **`0x4301`'s `WIFI_CONFIG_UUID` is the accessory's HomeKit identity** - the bike's
  first session stored a pairing under exactly the UUID it had sent there. Not every unit does HAP:
  the bench AUTO BOX stored nothing across two full sessions to RECORD.
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
- **iAP2 does not end at the WiFi handoff - it moves onto WiFi, and losing the Bluetooth link is
  correct.** After AirPlay is up, iAP2 packets are tunnelled inside AirPlay `iAPSendMessage`
  commands on the control channel: catplay's receiver runs a whole iAP2 client whose remote is
  AirPlay (`AsyncClient::new(false, CsmRemote::airplay(), _sink)`), pushes outgoing packets with
  `Command::IApSendMessage`, and feeds incoming ones straight in with `iap2.read_frame_buf(&cmd.data)`
  (`carplay/catplay_carplay/src/carplay_rx/sink/receiver.rs`). `disableBluetooth` is
  `CommandDirection::FromDevice` - the *phone* asks the accessory to drop the BT link, and catplay's
  car side obeys by calling `BluezManager::disconnect_peer()`. So the `Connection reset by peer` on
  the iAP2 RFCOMM channel ~4-5s after the WiFi credentials is the handoff working, not a fault; the
  unit does it to a real iPhone too.
  **carjack never implements the tunnel**, so its control channel simply ends there - app launch,
  Siri, calls and now-playing are all unreachable after handoff. Video and touch are unaffected
  (touch goes over AirPlay by design). To regain control, handle `Command::IApSendMessage` where
  `libcarjack/src/airplay.rs` already dispatches `ChangeModes`/`ForceKeyFrame`/`HidSendReport`, and
  feed it into the existing `iap2-proto` session in place of the RFCOMM stream.
  Chased at length on the Bluetooth side before this was found; **none** of it was the cause -
  see the eliminated list above. When Bluetooth behaviour looks wrong after the handoff, read
  catplay first.
- **A head unit walks away a couple of seconds after carjack stops making progress**, taking the
  *entire* session with it - Bluetooth included, and its AP too. It is not specifically a post-RECORD
  video watchdog. The failure looks like the car rebooting, because both radios go at once.
- **Never pause between `assert_modes` and `setup_screen`.** SETUP has to follow the modes
  assertion immediately; a wait there loses the session.
- **RECORD is a promise: never send it without a keyframe already in hand.** So the gate
  (`wait_for_first_keyframe`) sits **after `setup_screen`, before `record`** - and nowhere else.
  It logs a warning every second it waits.
- **The encode pipeline is fast; a cold GStreamer registry is not.** The first `gst::init()` builds
  the plugin registry and takes seconds, so it must happen at startup, never inside a session
  (`encode::init_gstreamer`; app mode gets it via `resolve_backend`). `tests/encode_startup.rs` and
  `tests/desktop.rs` measure time to first keyframe on each path.
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
  `HW_BACKENDS` table (or forced via `CARPLAY_ENCODE_BACKEND=cuda|vaapi|v4l2`,
  which applies to the desktop path too). Rows whose `drivers` match the render
  node's actual kernel driver (`config::render_node_driver`) are probed first,
  the rest in table order behind them - as on the desktop path. **Table order
  alone is not enough**: point `drm_device` at the Intel node of a hybrid box and
  `cuda`'s elements all probe fine, then every frame's `eglCreateImageKHR` fails
  `BAD_MATCH` because the import is cross-GPU. A row carries both video sources' chains (`import`/`postproc` for the
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
