# Where things stand

**Last updated: 2026-08-07.** This is the living document - if it disagrees with
[investigation-log.md](investigation-log.md), this one wins. Durable facts about how the code
works belong in `CLAUDE.md`, not here.

## The goal

A wireless CarPlay bridge that impersonates an iPhone to a car head unit: Bluetooth iAP2 to
authenticate and get handed the car's WiFi credentials, then CarPlay video over that WiFi via
AirPlay.

## The blocker on the bike (2026-08-04): the car deauths us 16 ms after the screen SETUP

**Primary suspect (2026-08-05): session ordering, not the `/setup` identity.** The iPhone capture
(`logs/cfmoto-iphone.pcapng`) reframes the old deviceID theory. The iPhone opens AirPlay with
`POST /pair-verify` **first** (stored pairing) and never sends an unauthenticated `/setup` - so it
never draws a 401, and its `/setup` is encrypted (its deviceID/macAddress are unreadable). carjack
led with an unencrypted `/setup`, drew a 401, then paired. The bike accepts the `/setup` either way
(200), so identity isn't checked at `/setup`; the autthentication-ordering divergence is the
plaintext-visible difference, and the bike appears to drop sessions that began with the 401. Also
settled by the capture: the bike does **zero CarPlay mDNS** - the phone connects straight out to the
receiver named over iAP2, so carjack skips the `_carplay-ctrl` advertisement entirely when iAP gives
direct details.

Implemented (fork `cfraz89/catplay@fix/airplay-device-identity`): `device_id` + `mac_address`
plumbed into `/setup` (defensible hygiene), plus a `pair_verify_first` flag - when a pairing is
stored, open with `/pair-verify` then `/setup` on the encrypted channel, matching the iPhone. carjack
sets it from "known remote" and skips mDNS on the direct-details path (still advertises for units
that name no receiver, e.g. the bench). Builds; `just test-lib` green. **Untested against the bike -
that is the next validation.**

**Experiment (2026-08-05, `logs/cfmoto.log` with `catplay_carplay=info`): the car answers every
AirPlay request through `assert_modes`, and goes silent exactly at the screen SETUP.** Pair-verify,
the encrypted `SETUP /setup`, `/info` (32 KB) and `modesChanged` all return 200 OK; the very next
request - `SETUP /` for the screen stream - is never answered at all (times out 11 s later into
`Request/channel timeout`). No `Screen setup response received`, no ACK, no RST. So the "did the
car answer the SETUP before deauthing" question is settled: **it does not answer it, in this run**;
it drops the session the moment the video screen session begins. Full exchange table in the
investigation log.

The sharpest evidence yet, from `cfmoto.log` + `cfmoto.pcapng` (17:05, the first run of the
`0x4301` fallback). Everything works until one exact moment:

```
16.620  Timeout waiting for CarPlay ctrl invite; connecting to [fe80::9083:…]:7000
16.909  AirPlay bootstrap complete!   Car display: 800x1280 @ 30 fps
16.922  Asserted screen modes (controller owns screen)      (385 -> 182 on the wire)
16.925  screen SETUP sent  (314 bytes)
16.9417 kernel: wlp0s20f3: deauthenticated from 92:83:8e:ef:a7:d3 (Reason: 2)
```

- **The direct connect works.** `/setup`, pair-setup, pair-verify, `/info` answered with a 13.8 KB
  plist, event socket up, modes exchange answered. 289 ms from TCP to display known. The `/info`
  timeout that once justified deleting this path was the dying link, nothing more.
- **The deauth is the car's.** `deauthenticated from …`, *not* `deauthenticating … by local
  choice`: mac80211 prints the first for a frame it **received**. The 21:02 run's four were all
  local-choice (rechecked), so these are two different failure modes and this is the one that
  matters.
- **16 ms is a decision, not a link failure.** Too fast for a rekey, an inactivity timer or a
  retry exhaustion. The car chose to drop the station immediately after the screen SETUP.
- **It also kills the mDNS-keepalive theory** from the 2026-08-03 entry: this run advertised and
  connected, and the association died anyway - on a timescale no keepalive argument can explain.

The car never answers the SETUP; carjack waits 11 s, gets `Request/channel timeout`, restarts, and
the retry ends in a Bluetooth page timeout.

This is the same shape as `cfmoto_16` (SETUP *answered* 314 -> 225, deauth 77 ms after the screen
data socket's handshake, before RECORD, no data ever sent) and `cfmoto_12`. So across three
captures the trigger sits in the same place: the screen stream. Whether the car answers the SETUP
first varies; that it drops the station within tens of milliseconds does not.

What the SETUP actually carries is small - `streamConnectionID` (a random u64), `type`,
`latencyMs` (carjack asks 100 ms), and the display `uuid` from `/info`. Nothing obviously
objectionable, and `cfmoto_16` shows the car accepting it. Two candidates worth separating:
the plist contents, and the car reconfiguring its AP when a session moves to streaming (an AP that
has torn down its association state answers exactly `PREV_AUTH_NOT_VALID`).

**Next evidence, cheap:** catplay already logs `Sending screen SETUP: {setup:?}` and
`Screen setup response received: {resp:?}` at info - `.env` was pinning `catplay_carplay=error`,
which is why no run has ever shown them. Now set to `info`. The next bike run therefore says
whether the car answered before deauthing, and with what.

## Superseded: the blocker as it looked on 2026-08-03 (a different failure mode)

**Not the car. The WiFi association dies on its own about 12 s in, while the car carries on
talking to us over Bluetooth the whole time.** The 21:02 run settles this:

- **The deauth is ours.** `wlp0s20f3: deauthenticating from 92:83:8e:ef:a7:d3 **by local choice**
  (Reason: 2=PREV_AUTH_NOT_VALID)`, and wpa_supplicant agrees: `CTRL-EVENT-DISCONNECTED …
  reason=2 **locally_generated=1**`. The earlier `iw event` reading of "disconnected (by AP)" for
  `cfmoto_16` was the same event read the wrong way round.
- **The AP is healthy and keeps taking us back.** Every retry gets `RX AssocResp … status=0
  aid=1`. An AP that had walked away or rebooted could not do that.
- **The car is healthy.** In the btsnoop, the bike is running SDP and PBAP (`Get final
  "telecom/pb.vcf"`) at 21:02:43 - half a minute *after* the WiFi started cycling, and long after
  carjack had given up. So "the car walks away and takes Bluetooth with it" does **not** apply
  here; only the WiFi path failed.
- **It is not an `assert_modes` reaction.** This run never got that far: `/info` was sent and
  never answered, so the deauth cannot be a response to anything we asserted. The
  ~20-45 ms-after-`assert_modes` correlation in the older captures was coincidence - the modes
  assertion is simply what a session that survives ~12 s is doing at the moment the link drops.

The shape to explain is now: associate → DHCP completes (`192.168.149.57`, car is `.201`) →
traffic flows for ~2 s → goes quiet → ~10 s later the local stack gives up and deauths → repeat
every ~11 s. What is missing is a WiFi capture spanning the association (with EAPOL and beacons)
or `wpa_supplicant -dd`; nothing in the journal says *why* the supplicant concluded the auth was
no longer valid. A group-key rekey the bike's AP never completes is the obvious thing to look for.

Two things did work for the first time and should not be re-litigated: the bike answered `0x4301`
with the **default** identity (no `CARJACK_WIFI_ID`/`CARJACK_WIFI_MAC` overrides), and
`Associated as the announced address actual=FE:E8:AC:31:AD:C9` confirmed announce == associate on
the bike. The announce/associate mismatch theory below is therefore **closed - it was not the
cause**, though the fix is still correct and worth keeping.

Everything downstream of the modes assertion is a red herring. SETUP, the screen socket, RECORD and
keyframe latency are simply what the runs that got further happened to be doing; `cfmoto_13` and
`cfmoto_14` never sent SETUP at all and died on the same 20-30 ms offset. The per-capture table is
in the log entry.

`cfmoto_16` is still the furthest the bike has got - fresh association on a new subnet (the first
run on the in-memory profile, so **that path works**), full AirPlay bootstrap, SETUP answered, both
sockets up - it just got 45 ms instead of 30.

**Closed lead (was: the station we announce is not the station that associates).** `0x4300` names
the station that will join the AP, and in `cfmoto_btmac_ea99` carjack announced
`FE:B2:9F:ED:EA:99` and then associated as `4c:79:6e:da:29:47` - the profile was pinned to
`AssignedMacAddress::Permanent` while `get_wifi_mac` returned something else. `617f28d` makes the
two agree (`announced_wifi_mac` derives a stable, locally administered address, the profile pins to
exactly it, and activation reads the device back and warns on a mismatch). The 21:02 run confirmed
on the bike that they now agree - and the link still died. Keep the fix; it is not the cause.

The identity fields in `0x4300` *have* now met the bike, and one of them mattered enormously - see
below. The bike answers `0x4300` for `FE:B2:9F:ED:EA:72` and for an invented `FE:B2:9F:ED:EA:99`,
and refuses to answer at all for the permanent `4c:79:6e:da:29:47`, which differs from the
adapter's Bluetooth address only in the last byte. An iPhone's globally administered address is
answered fine, so the administered bit is not the discriminator - it reads like a collision with
our own Bluetooth identity in the unit's device table. `afd6605` introduced that address and broke
`0x4300` outright: the unit acks the request and never sends `0x4301`.

**Current lead: carjack was connecting to AirPlay uninvited.** A head unit invites the phone -
it watches for a `_carplay-ctrl._tcp` Bonjour record and only then sends `GET /ctrl-int/1/connect`
(catplay's `cp_inviter.rs` loops doing nothing while no phone is advertising). carjack advertised
that record *only* when `0x4301` omitted an AirPlay address, and the bike always supplies one - so
on the bike it never advertised, was never discovered, was never invited, and connected on its own
to the address from `0x4301`. The bike accepts that TCP connection and never answers `/info`,
which is exactly where the 21:02 run died. The old code comment ("it doesn't seem to invite us on
mdns anyway") was circular - we never gave it anything to discover.

Two identity mismatches point the same way, and both are now understood to be Bluetooth-keyed:
the Bonjour record's `id` is documented by catplay as the phone's **Bluetooth** MAC (carjack sent
the WiFi interface MAC), and the iPhone's `0x4300` group 1 carries its **Bluetooth** MAC where
carjack sends a WiFi MAC. The car appears to correlate the station on its AP with the device it
authenticated over Bluetooth, by Bluetooth address.

Changed: carjack now always advertises `_carplay-ctrl._tcp` with the adapter's Bluetooth address
as `id`, waits for the invite, and connects only where the invite points. The direct-connect and
gateway-fallback paths are gone (`connect_airplay_endpoint` takes one address), along with
`CarWifi::gateway`/`iface_mac`.

**Confirmed on the bench AUTO BOX (2026-08-03 22:34, `/tmp/bench-invite.log`) - the `id` was the
whole problem.** The invite arrived **69 ms** after the advertisement:

```
12:34:23.708  Car WiFi connected, waiting for the car's CarPlay invite...
12:34:23.710  _carplay-ctrl._tcp advertised on wlp0s20f3:8082 (id=4C:79:6E:DA:29:4B)
12:34:23.779  Got CarPlay ctrl invite; AirPlay peer at 192.168.43.1:7000, pi=Some(90665097-…)
12:34:24.006  AirPlay bootstrap complete!  Car display: 1080x600 @ 30 fps
12:34:24.070  RECORD sent, streaming video     (first keyframe 4.05 ms)
              … 30.0 fps steady for 70 s, 0 dropped to backpressure, touch arriving …
```

The same unit on 2026-07-17 advertised the **WiFi** MAC and timed out after 15 s, reaching RECORD
only through the fallback that is now deleted. Same unit, same advertisement, different `id`:
69 ms versus never. That is direct evidence for the Bluetooth-MAC correlation, independent of the
bike. It is also strictly better than the old path - the invite carries the car's HomeKit pairing
id (`pi`), which the gateway fallback never had, so pair-verify now has its fast path.

**Tested on the bike (2026-08-03 22:53, `cfmoto.pcapng` + `/tmp/carjack.btsnoop`) - it does not
invite, and it does no Bonjour at all.** carjack advertised `_carplay-ctrl._tcp` with
`id=4C:79:6E:DA:29:4B` (the Bluetooth address) once a second from t=17.0 to the end of the run,
across two full iAP2 sessions, and was never invited. In ~70 s the bike sent exactly **three**
IP packets: a DHCP offer, a DHCP ack, and one ARP for us at t=22.07 (answered immediately, and
nothing followed it). No mDNS query, no mDNS announcement, no TCP in either direction. So the
bike neither browses for a phone nor publishes `_airplay._tcp` - the `id` fix is real but has
nothing to act on here.

What it does instead is repeat `0x4301` every ~3 s indefinitely (six times per session, until
carjack's invite wait expired and restarted the session), handing over the same 158 bytes each
time: SSID, passphrase, channel 157, `fe80::9083:8eff:feef:a7d3`, port 7000, and its HomeKit
pairing id `9a011f99-…`. That message is byte-identical to the one the iPhone gets in
`iphone-cfmoto.pklg`, and there the phone's iAP2 traffic *ends* 3 s after the second copy - the
handoff. The bike is saying "connect to me", and only carjack was waiting.

**Two things this run also settles.** First, **the WiFi association is stable**: associated at
22:53:41 and still up when the capture ended 56 s later, with no deauth cycling at all - the
~11 s deauth loop needs re-explaining, and continuous mDNS chatter keeping mac80211's
connection-loss detection quiet is the obvious candidate. Second, the `/info` timeout that made
the direct connect look broken came from a run whose WiFi was dying every 11 s, so it was never
evidence against connecting out.

Changed: the invite is still preferred and still advertised unconditionally, but when `0x4301`
named a receiver the wait is 3 s and carjack then connects there. **Untested on the bike.**

## HomeKit (corrected 2026-08-04): it runs, and `0x4301`'s UUID is the accessory identity

An earlier entry here claimed catplay never runs the pairing flow, on the strength of
`start_pair_task()` being commented out in its bootstrap and of two bench sessions that stored
nothing. **Both were misleading.** `do_initial_setup` runs the pairing itself, and the bike's
session shows it plainly on the wire: `/setup`, `POST /pair-setup` x3 (`X-Apple-HKP: 2`),
`POST /pair-verify` x2, and everything after that encrypted. The bench AUTO BOX simply does not do
HAP - which is why nothing was stored there.

The persistence work therefore paid off on its first bike run, and settled the open question at the
same time. `homekit.bin` grew from 102 to 151 bytes and now holds a pairing under
`9a011f99-597e-424d-b490-806fd7c07800` - byte-for-byte the UUID the bike sent in `0x4301`. So that
field **is** the accessory's HomeKit identity, and the next bike session will find it in storage,
pass it, and take `/pair-verify`.

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

## Changed: WiFi goes through iwd, not NetworkManager (2026-08-07)

`src/wifi_connect.rs` is now written against `iwd-automata` and the `nm-automata` dependency is
gone. Nothing else in carjack touched NetworkManager, so the port is that one module, the
`test-run` cleanup in the `Justfile`, and the notes in `CLAUDE.md`.

The interesting part is that iwd has **no connection-profile API on D-Bus at all** - a network is
provisioned by a file in iwd's state directory or it is not, and `Network.Connect()` is the whole
of the client side. So the NetworkManager `ConnectionProfile` became a `<ssid>.psk` file carrying
the passphrase, `AutoConnect=false`, `Hidden=true` and `AddressOverride`, written temp-then-rename
before every attempt. Three things that were free under NetworkManager are not:

- **The pinned station address needs host configuration.** iwd ignores `[Settings]
  AddressOverride` unless `/etc/iwd/main.conf` has `[General] AddressRandomization=network`, and
  says nothing when it does. carjack compares the associated address to the announced one after
  connecting and warns; that check was already there for NetworkManager and has become the only
  detector.
- **DHCP is off by default.** `[General] EnableNetworkConfiguration=true` or iwd does no IP
  configuration whatsoever. `provision` reads `Daemon.GetInfo().NetworkConfigurationEnabled` and
  refuses to go on without it, rather than failing later as a connection with no address.
- **There is no `IP4_READY`/`IP6_READY`.** `wait_for_addresses` reads the interface instead: a
  routable IPv4 out of `getifaddrs`, and a link-local out of `/proc/net/if_inet6` whose flags no
  longer carry `IFA_F_TENTATIVE`. That is the same gate the NetworkManager flags gave, restored by
  hand - and it matters for the same reason, that a link-local destination cannot be reached from
  a tentative source.

One latent bug the port closed: the announced address is now derived from the phy's address in
`/sys/class/net/<iface>/phy80211/macaddress`. iwd's `Device.Address` is the *current* netdev
address - our own override once connected - and iwd never restores the permanent one on
disconnect, so deriving from it would have announced a different address on every second run.

Two real defects surfaced and fixed while standing this up against a live iwd on the host:

- **`iwd-automata`'s `discover` never set a D-Bus destination** (`src/ext.rs`,
  `ObjectPathExt::proxy`). It only trips once iwd exposes an object of that kind, so the
  device-less container CI never hit it - on real hardware every `discover()` failed with
  "destination was not specified". Fixed by defaulting the destination to `net.connman.iwd`.
- **iwd's systemd unit drops `CAP_DAC_OVERRIDE`, so it reads provisioning files by ordinary
  permissions, not as root** - a chris-owned 0600 file stayed unprovisioned (0/6) while a
  world-readable 0644 worked (6/6). Fixing it with 0644 would leak the car's passphrase, so
  `wifi_connect` now writes 0600 and sets a root-read POSIX ACL via the `exacl` crate (`u:0:r`):
  0600 for everyone else, readable by iwd, no root needed (measured: registers). The ACL has to
  land before the atomic rename; set after, iwd scratches its head (0/6).

Verified against a live host iwd: `iwd_is_configured_for_carjack`, `live_connection_...` and
`iwd_reads_back_the_car_provisioning_file` (writes a throwaway profile, confirms iwd registered
it with `Hidden=true`/`AutoConnect=false`/type `psk`, then `Forget()`s it) all pass. **Not yet
run against a car.**

## What works

Verified against the CFMOTO bike unless noted.

- **Bluetooth → iAP2 handshake, end to end, in about a second.** iAP1 switch → link SYN →
  identification → the Apple authentication chain (`AA00`/`AA06`/`AA02`/`AA03`/`AA05`) →
  `4300`/`4301`, at which point the car hands over its WiFi credentials and the address of its
  AirPlay receiver. Confirmed clean in `cfmoto_11.pcapng`.
- **WiFi association and DHCP** on the car's AP - via NetworkManager up to 2026-08-07, via iwd
  since; the iwd path has not yet met a car (see below).
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
  `cfmoto_16` (only our own RSes appear, retried into silence). **The flag is sufficient and the
  DAD watcher is now deleted**: over every session that logged it, `wait_for_link_local` returned
  on its first pass 660-810 µs after `IP6_READY` - i.e. it never waited for anything.
  `wait_for_link_local`, its tests and the `rtnetlink` dependency are gone.
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
