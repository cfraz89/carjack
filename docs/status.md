# Where things stand

**Last updated: 2026-08-03.** This is the living document - if it disagrees with
[investigation-log.md](investigation-log.md), this one wins. Durable facts about how the code
works belong in `CLAUDE.md`, not here.

## The goal

A wireless CarPlay bridge that impersonates an iPhone to a car head unit: Bluetooth iAP2 to
authenticate and get handed the car's WiFi credentials, then CarPlay video over that WiFi via
AirPlay.

## The blocker on the bike (2026-07-31)

**The head unit deauthenticates us off its own AP 20-45 ms after it answers `assert_modes`, in
every capture ever taken, and takes the whole session with it.** Established with `iw event -t`
alongside `cfmoto_16`: `deauth <AP> -> <us> reason 2`, **`disconnected (by AP)`**. Not a local
fault - that run had `disable_11ac=1` and `power_scheme=1` active and behaved identically.

Everything downstream of the modes assertion is a red herring. SETUP, the screen socket, RECORD and
keyframe latency are simply what the runs that got further happened to be doing; `cfmoto_13` and
`cfmoto_14` never sent SETUP at all and died on the same 20-30 ms offset. The per-capture table is
in the log entry.

`cfmoto_16` is still the furthest the bike has got - fresh association on a new subnet (the first
run on the in-memory profile, so **that path works**), full AirPlay bootstrap, SETUP answered, both
sockets up - it just got 45 ms instead of 30.

**Current lead, and it is a strong one: the station we announce is not the station that
associates.** `0x4300` names the station that will join the AP, and `assert_modes` is the first
point the unit has any reason to check it - which is exactly where the deauth lands, with reason 2,
"previous authentication no longer valid". In `cfmoto_btmac_ea99` (2026-08-03) carjack announced
`FE:B2:9F:ED:EA:99` over iAP2 and then associated as `4c:79:6e:da:29:47`: the profile was still
pinned to `AssignedMacAddress::Permanent` while `get_wifi_mac` returned something else. The unit
was told to expect one station and saw another.

`617f28d` makes the two the same address - `announced_wifi_mac` derives a stable, locally
administered address, the car's profile pins to exactly it, and activation reads the device back
and warns if they ever differ. Verified on the bench (`Associated as the announced address`);
**not yet tested against the bike**, and that is the next run to do.

The identity fields in `0x4300` *have* now met the bike, and one of them mattered enormously - see
below. The bike answers `0x4300` for `FE:B2:9F:ED:EA:72` and for an invented `FE:B2:9F:ED:EA:99`,
and refuses to answer at all for the permanent `4c:79:6e:da:29:47`, which differs from the
adapter's Bluetooth address only in the last byte. An iPhone's globally administered address is
answered fine, so the administered bit is not the discriminator - it reads like a collision with
our own Bluetooth identity in the unit's device table. `afd6605` introduced that address and broke
`0x4300` outright: the unit acks the request and never sends `0x4301`.

If the announce/associate fix is not it, the next thing to vary is `assert_modes` itself - the
payload, or whether the main-screen resource is taken at all.

## Settled on 2026-08-03

- **The Bluetooth side is no longer a suspect.** A day of experiments eliminated every theory about
  the handshake, all against real units: SDP-registration ordering, which profile carries the
  channel, who opens it, who speaks first, adapter identity (CarPlay EIR UUID, iPhone Class of
  Device), mux ownership, and four different ways of resetting a link. The findings and the
  measurements are in `CLAUDE.md`; do not re-run them.
- **The dead first channel after a fresh bond was ours, and is fixed.** `set_trusted(false)` made
  BlueZ refuse the unit's inbound profile connections - HFP answered `DM`, AVDTP and HID refused
  with "security block" - and the unit will not drive iAP until its profile set is up. Trusted:
  **6/6 clean first bonds, against 1/17 untrusted**.
- **The `Connection reset by peer` on iAP2 after the WiFi handover is correct behaviour**, not a
  fault. CarPlay moves the session onto WiFi - frames tunnel inside AirPlay `iAPSendMessage`
  commands - and the phone asks the accessory to drop Bluetooth with `disableBluetooth`. The unit
  does the same to a real iPhone.
- **Neither unit implements that tunnel.** The bench AUTO BOX and the bike both report
  `extended_features = [VocoderInfo]`. carjack has the transport (`libcarjack/src/iap2_tunnel.rs`)
  but only engages it once an accessory actually sends on the channel, and does not claim
  `ControllerFeature::IApChannel`. Until a unit turns up that does, our control channel genuinely
  ends at the handover - which costs app launch, Siri, calls and now-playing, and costs video and
  touch nothing.

Dead ends, recorded so they are not retried:

- **`swcrypto=1`** - iwlmvm refuses to load with it ("iwlmvm doesn't allow to disable HW crypto").
- **`disable_11ac=1` + `iwlmvm power_scheme=1`** - no effect; reverted. `iwlwifi: Unhandled alg:
  0x707` (RX status `SEC_UNKNOWN`) in `cfmoto_15` was a *symptom* of the association being pulled,
  not a cause.
- The leftover on-disk `CFMOTO-999700` NM profile (`autoconnect: yes`) is deleted - it was
  re-associating before carjack started, so carjack always took the "reuse the live connection"
  path. Gone now, and `cfmoto_16` associated fresh. Full detail in the
2026-07-31 entry of [investigation-log.md](investigation-log.md).

## What works

Verified against the CFMOTO bike unless noted.

- **Bluetooth → iAP2 handshake, end to end, in about a second.** iAP1 switch → link SYN →
  identification → the Apple authentication chain (`AA00`/`AA06`/`AA02`/`AA03`/`AA05`) →
  `4300`/`4301`, at which point the car hands over its WiFi credentials and the address of its
  AirPlay receiver. Confirmed clean in `cfmoto_11.pcapng`.
- **WiFi association and DHCP** on the car's AP, via NetworkManager.
- **AirPlay video and touch**, on the bench unit: the car's display is learned from `/info`, the
  compositor is reconfigured to it, and touch comes back over the AirPlay HID channel.
- **The internal compositor and both encode paths** (zero-copy CUDA on NVIDIA, software
  fallback). See `CLAUDE.md` for the backend table and what is verified where.

## Confirmed working on the bike as of `cfmoto_12` (2026-07-27)

The three changes that were pending retest all check out, and the AirPlay half now establishes
end to end against the bike:

1. **The AirPlay connect no longer races duplicate-address detection.** In `cfmoto_12` the
   neighbour solicitation is answered and the TCP SYN to port 7000 follows 70 µs later. This is
   the first capture with any TCP in it.
2. **We no longer reconnect to an AP we are already on.**
3. **The link layer honours what the SYN negotiates**, and the lock-step handshake
   (`max_outstanding = 1`) completes in ~1.1 s - no worse than `cfmoto_11`, so there is no need
   to relax `LinkParams::max_outstanding`.

Also settled: **`remote_homekit_id: None` is fine.** `pair-setup` and `pair-verify` both complete
against the bike without a Bonjour-supplied pairing id.

## What is done but not yet retested against the bike

**Six fixes and one refuted hypothesis, from `cfmoto_14` + its logs (2026-07-27/28). All
unit-tested, all building, none yet run against the car - that is the next thing to do.** Details
in the top two log entries.

**The Bluetooth connection problem is Page Timeout, not profiles.** 9 of carjack's 12 outbound
pages across `cfmoto_11`..`cfmoto_14` came back `Connect Complete status=0x04` - the bike's
controller never answered, so nothing above the baseband was reached. `cfmoto_11` is the control
case: zero timeouts, and iAP2 then ran over carjack's *own outbound* channel to the bike's
accessory service. When the page lands, every layer above it works. Two changes:

- The unconditional `device.disconnect()` at session start is **gone**. Connected: dropping a
  healthy link is what appears to push the bike out of page-scan. Not connected: bluer documents
  that `disconnect()` also cancels an in-flight `Connect`, so it could only ever cancel a
  connection the bike was making. The retry loop still disconnects on a real failure.
- `connect_accessory` logs the page outcome explicitly ("Outbound page succeeded" / "Outbound page
  timed out - the head unit is not page-scanning"), so the timeout rate is visible from the console
  instead of needing HCI from a capture.

**Also settled:** the bike has never rejected carjack on the iAP accessory profile - no `Rcvd DM`
exists in any capture, and `cfmoto_12` shows `Sent SABM Channel=2` -> `Rcvd UA` -> `FF 55` on that
channel. Both channel directions carry iAP2; they race, and either can win.

**The Bluetooth side: one hypothesis refuted, three fixes kept.** I proposed that carjack was
losing a race with its own outbound page and ending up on a channel the bike would never drive.
**`cfmoto_14` session 2 disproves it** - our `Sent Create Connection` created that ACL
(`Connect Complete status=0x00`), the bike opened the RFCOMM channel on it and sent `FF 55` 248ms
later, and the session ran through to RECORD. The delay-before-paging change was reverted. What
stands:

- `SWITCH_TIMEOUT` is 3 s, single attempt (was 12 s x 3 = ~38 s). A mute channel will not start
  talking on a re-read; failing fast is what lets the caller go get another one.
- `perform_switch` still reads all six switch bytes. A 2-byte magic dispatch (to skip the switch
  when the accessory is already in iAP2) was built and then **reverted after testing on the
  hardware: no switch means no SYN either**, so the case does not exist.
- Every accepted request logs its `ProfileKind`, and requests from any device other than the
  configured one are rejected.

**Lesson worth keeping:** `Accessory connection r=Ok(())` does **not** mean our page made the link -
it resolves Ok just as readily when the unit connected first. Only HCI (`Sent Create Connection` vs
`Rcvd Connect Request`, plus the `Connect Complete` status) answers who paged.

And from the session/video side:

1. **GStreamer is initialised at startup in desktop mode** (`encode::init_gstreamer`), not lazily
   inside the first encoder task. The first `gst::init()` builds the plugin registry - **4.3 s** on
   the dev box - and that was landing inside a session the head unit only gives ~2 s. Session 1 of
   `cfmoto_14` took 4.92 s to its first keyframe; session 2, registry warm, took **296 ms**.
2. **The keyframe gate moved back after `setup_screen`.** Putting it before was my regression:
   nothing may sit between `assert_modes` and SETUP. `cfmoto_12` sent SETUP 2 ms after the modes
   assertion and the car answered RECORD; `cfmoto_13` and both `cfmoto_14` sessions delayed it and
   lost the unit within ~2 s.
3. **Inbound iAP2 connections are filtered by the configured address.** carjack was accepting a
   page from *any* bonded head unit - in `cfmoto_14` it streamed to the bench AUTO BOX 58 s after
   the bike's link failed.

**Measured, so it is no longer a guess:** `tests/encode_startup.rs` times the real `run_session`
sequence (compositor → `reconfigure` → encoder spawn) at **34-66 ms** to the first keyframe, across
a no-op reconfigure, a real resize and `max_fps=0`. `tests/desktop.rs` reports **~660 ms** for the
portal/PipeWire path. The encoder was never the bottleneck.

## Working end to end on the bench unit (2026-07-28)

**A full session now runs: iAP2 -> auth -> WiFi -> AirPlay -> sustained 30fps video, with touch
coming back.** Reproduced 4/4 consecutive `just test-run` cycles against the AUTO BOX, ~19s from
session start to `RECORD sent`, then 45s+ of `Streaming heartbeat: 30.0 fps` with zero frames
dropped to backpressure and no keyframe stalls.

Three things had to be true at once, and each was individually fatal:

1. **The iAP SDP record must live for the process, not the session** (`IapHost`). Registered per
   session, it blinked out between sessions; the unit probed in that window, saw no iAP service and
   fell back to **Android Auto** (`bench_4`).
2. **Start from a fresh link, and never commit to a stale one.** A pre-existing link is stale and
   never carries the switch; it is cycled at session start. carjack tries one channel per attempt -
   concurrent racing was built and removed as unexercised and slower.
3. **Recovery must call plain `device.connect()`.** Bringing up the whole profile set (HFP above
   all) is what makes the unit open its channel. `connect_profile` first leaves a connection in
   progress so the follow-up `connect()` fails `br-connection-busy` and HFP never comes up.

Startup no longer waits out a guaranteed failure. A link that predates the session is stale, so it
is cycled immediately instead of spending ~6s learning that. Measured over 6/6 runs: **iAP2
handshake complete at ~8s** (was ~14-17s), `RECORD sent` at **14-19s**, the remainder being
NetworkManager association (~4s) and Bonjour discovery of the AirPlay endpoint (~5s, bench-only -
the bike hands the address over on the iAP2 link). Nothing in the startup path is now gated on a
timeout.

**Ownership:** everything that makes carjack look like an iPhone - BlueZ session, agent, CarPlay
advertisement, iAP profile registrations - lives in an `IapHost` created once per run in
`run_app`/`run_desktop` and threaded through to `wireless_carplay`. It is an owned value, not a
global; dropping it deregisters the profiles, which is exactly the failure it exists to prevent.

## Fixed: nm-automata serialised a profile that dbus-broker rejected (2026-08-01)

For a while roughly half of all runs died before WiFi, unrecoverably - the bus dropped carjack's
connection, so every retry and every following session failed `Broken pipe` on a dead cached
proxy. The broker said why:

```
dbus-broker[1031]: Peer :1.917 is being disconnected as it sent a message with an invalid body.
```

**zvariant serialises a value twice** - once to size the buffer, then again to fill it - and the
length written into the message comes from the first pass. `ConnectionProfile::serialize` called
`to_map()`, which builds a **fresh `HashMap` every call**, so the two passes iterated in
different orders; dict entries are padded to their alignment, so a different order is a different
length. The message then declared one length and carried another. Same profile, back to back:
99 bytes or 102 bytes depending on the call, and `serialized_size` disagreeing with `to_bytes`.

The fix is in nm-automata (`settings::serialize_profile` / `serialize_setting`, and the two
templates in `codegen/src/render.rs` that emit the impls): serialise through a key-ordered view,
so the layout cannot depend on which map instance produced it. D-Bus dictionaries are unordered,
so nothing downstream can tell. Three regression tests in `nm-automata/tests/serialisation.rs`
pin it - the crucial one being that reserved size must equal bytes written.

Verified: the profile round-trip test went 15/15 (was ~50%), a raw send went 20/20, and the bench
unit ran clean with no retries.

Worth remembering, because both misled for a while: the same symptom was first blamed on leftover
`carplay_wifi_*` profiles (deleting them "fixed" it - that was a lucky process) and then on
`persist=volatile` (changed nothing, reverted). A ~50% failure that follows process boundaries is
a hash-order tell; chasing it as *state* wasted the most time.

## Housekeeping landed 2026-08-01 (unrun against a car)

Not aimed at the deauth; all three are things that were making iteration harder.

- **NetworkManager now goes through `nm-automata`**, our own bindings generated in full from
  NetworkManager's spec, replacing the hand-written `nmrs`. The car's profile is a typed
  `ConnectionProfile` rather than a settings map with two properties patched in by string key -
  and the values are typed too, not strings spelled by hand: `WirelessMode::Infrastructure`,
  `WirelessBand::A`, `WirelessSecurityKeyMgmt::WpaPsk`, `AssignedMacAddress::Permanent`,
  `Ipv4Method::Auto`, `Ipv6Method::LinkLocal`, and an `Ssid` (bytes, as an access point actually
  names itself) that the current-network comparison goes through instead of `String::from_utf8`.
  A property whose object is absent now reads as `None` rather than a proxy for `/`, so
  `current_ssid` no longer compares a path against a literal and `car_wifi` says plainly that the
  activation has no IPv4 configuration yet - which is what the stray `IP4Config ... path "/"`
  warning in the logs was.
  Three `#[ignore]` tests in `wifi_connect` check it against the running daemon: the read path
  (`live_connection`, the permanent MAC) and a round-trip of the car's profile through
  `AddConnectionUnsaved` → `GetSettings` → `Delete`. The round-trip one is what surfaced the
  serialisation bug above - run it a few times, not once, since that failure was intermittent.
  They share one tokio runtime rather than taking one each: `get_network_manager` caches its
  D-Bus connection for the process, and a `#[tokio::test]` dropping its own runtime kills that
  connection's driver task, so the next test to use it blocks forever.
- **The activation gate needs `IP4_READY` *and* `IP6_READY`, not just `Activated`.** Caught on
  the bench: NetworkManager reports a connection activated once one address family is done, so
  gating on the state alone handed back a connection whose `Ip4Config` was still `/` and the
  attempt failed into a silent retry (a second profile, no log line). backon now `notify`s, so a
  retry says why.
- **`ipv6.method` is now `link-local`, and the IPv6 gate is `IP6_READY`.** It was defaulting to
  `auto`, i.e. waiting on SLAAC - and the car answers no router solicitation, in `cfmoto_12` or
  `cfmoto_16` (only our own RSes appear, retried into silence). The rtnetlink DAD watcher is
  still called and now logs how long it waited *after* `IP6_READY`; if that stays at zero over a
  few sessions, the flag is sufficient and `wait_for_link_local` and the `rtnetlink` dependency
  can both go.
- **The HCI sidecar is no longer invoked**, and `just build` no longer setcaps it (`just setcap`
  does it by hand). Neither the CarPlay EIR UUID nor the iPhone-like class of device was ever
  shown to be required, and granting `cap_net_admin` on every build prompted through polkit,
  which broke non-interactive runs. `src/bin/carjack-hci-helper.rs` is kept, unused.

## Next move

0. **Count the two new page-outcome log lines.** "Outbound page succeeded" vs "Outbound page timed
   out" now answers in one glance what previously needed HCI from a capture. If timeouts have
   dropped now that a healthy link is no longer torn down at session start, that confirms carjack
   was putting the bike out of page-scan itself.
0. **Start the capture before carjack, and keep HCI in it.** The one thing still unexplained is why
   some accepted iAP channels never carry `FF 55` while others carry it in 248 ms. Session 1's
   channel opened before the `cfmoto_14` capture began, so it could not be checked. The Bluetooth
   link being already up before launch is the current suspicion - a capture that spans carjack's
   start would settle it, and `Sent Create Connection` vs `Rcvd Connect Request` is the field to
   read.
0. **Power-cycle the head unit, check `.env`, and keep the console output.** `just test-run` does
   not save a log, and the last two runs were diagnosed blind from packet captures alone - which
   is how the wrong conclusion got drawn from `cfmoto_12`. carjack logs the encode backend, the
   car's display, and (now) a warning every second the keyframe gate waits; that output answers
   in one line what a capture cannot answer at all. Note which `CARJACK.SOURCE` was used.
1. **Run against the bike and capture** (`just test-run`, with a BT+WiFi capture as in
   `cfmoto_12`). In order, confirm:
   - the iAP2 handshake completes at all - `cfmoto_13` had none;
   - AirPlay reaches RECORD (as in `cfmoto_12`);
   - **video bytes appear on the screen socket within milliseconds of RECORD** - that is the
     whole point of the fix, and the thing `cfmoto_12` lacked;
   - the car does *not* send RFCOMM DISC ~2 s later.
2. **Then** the video path proper: whether the car requests keyframes, whether backpressure
   fires, and whether the picture holds up. Frame orientation is the first thing to eyeball.
   While you are in the log, read the `link-local ... is usable as a source address, N after
   IP6_READY` line - that is the whole test of whether the DAD watcher can be deleted.
3. **Also retest the bench unit** - the RECORD gate is on the shared `run_video_stream` path, so
   it affects the AUTO BOX too.

## Open questions

- **Frame orientation is unverified.** GL renders bottom-up and the readback orientation has
  never been checked against content with a clear top/bottom. A one-line output-transform fix if
  the car shows an upside-down image.
- **Multitouch is correct by construction but unvalidated** end-to-end against a real car, and
  the touch coordinate range is assumed to match the display resolution.
- **HFP identity answers are still stubs** (`AT+CGMI`/`AT+CGMM` return `ERROR`). Not implicated
  in any failure so far - iAP2 gets well past it - but a real iPhone answers these.
- **Nothing is blocked on captures.** The iPhone↔head-unit reference recordings answered the
  questions that were open.

## Hardware quirks

- The test head unit degrades into an "aggressive reconnect" state after several rapid reconnect
  cycles - the iAP2 handshake stops completing, or you get `ProfileUnavailable` /
  already-connected errors. **Power-cycle the head unit**; don't debug carjack for this.
- On the primary dev machine, `iwlwifi` needs `disable_11ax=1` (via `/etc/modprobe.d/`) or
  association with the car's AP is flaky. Check that first on any new dev machine.
