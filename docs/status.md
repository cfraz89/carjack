# Where things stand

**Last updated: 2026-08-12.** This is the living document - if it disagrees with
[investigation-log.md](investigation-log.md), this one wins. Durable facts about how the code
works belong in `CLAUDE.md`, not here.

## Who starts the iAP channel, and why it looked random (2026-08-13, night)

The unit hosts the iAP accessory service (`...decacaff`); we host the device side (`...decacafe`)
and can only wait. So the channel opens on the unit's schedule - but "its schedule" is not
arbitrary, and 17 successful bike runs split cleanly in two:

| how the channel arrived | runs | timing |
| --- | --- | --- |
| within a second of **our** page completing | 9 | 0.00 - 0.86 s after `Connection result` |
| with no page of ours anywhere near it | 8 | the unit initiating by itself |

So paging *is* a trigger, and when it works it works immediately. Nothing waits 20 s and then
succeeds. That makes a run easy to read: if the channel has not arrived a second after the ACL is
up, that page was refused, and waiting longer is not going to change it.

**The three initiation paths had been switched off** in the working tree - `PAGE_RETRY_DELAY` 2 s ->
10000 s, `OFFER_WAIT` 5 s -> 50000 s, and the startup `relink` commented out. A baseline run
confirmed the effect: carjack found the unit, printed `Waiting for iAP2 connection requests`, and
sat idle for 45 s. Only a unit that initiated on its own could ever connect, which is exactly the
randomness that prompted this.

**Cycling harder is the wrong lever.** Restored at 5 s, the unit accepted every page
(`res=Ok(())`, ~1 s each) and offered nothing; after five rapid cycles `connect()` began hanging
outright - the unit had stopped answering pages, which is the "aggressive reconnect" degradation in
the hardware-quirks list, and we were causing it. At 20 s backing off to 60 s, ten cycles over nine
minutes drew no degradation and one recovered page timeout.

What the run could not do is matter, because **the unit sat in its Phone settings page the whole
time** (`Now discoverable as "CFMOTO-G2Y70E"`, `My Devices > carjack` ticked). Photographed at
21:19 and again at 21:34, unchanged across every run. A unit parked in its own Bluetooth settings
does not start CarPlay on link-up; the rider backing out of that screen, pressing the device, or
power-cycling the bike is what restores the path that works in under a second.

Changed in `wireless.rs`: `OFFER_WAIT` 20 s backing off to 60 s; the startup page fires only when
the link is *down*, so a healthy one is never torn down; the timeout branch now also pages a link
that has gone away, where before it only cycled one that was up and so waited forever after a drop;
and the adapter is left discoverable and pairable while waiting, which it was not.

Untested: a successful initiation under the new timing. Nine minutes of runs never got one,
because of the screen the unit was on.

### What HCI says the unit is actually doing (`logs/bike-hci.btsnoop`)

`btmon -w` over three page cycles, read back with `btmon -r` (no privileges needed for the read).
Two things settle:

**The unit never asks us about iAP or CarPlay.** Every Service Search Request it sent us was for a
classic profile - Handsfree AG, Audio Source, AVRCP Target, Phonebook Access, OBEX Push, Headset -
and it is running HFP AT commands at us (`AT+CLIP=1`). Not one query for `...decacafe` or
`2d8d2466-...`. Our records are fine: when it browses us we do answer with both, five times each.
So this is not our SDP, and no amount of paging will provoke a unit that is not looking.

**The unit hosts its iAP service on RFCOMM channel 2**, record handle `0x00010009`, and it will
take a connection there. Registering the accessory UUID client-side and calling `ConnectProfile`
(`dial_accessory`) got a channel **43 ms after the dial** on the first attempt - the fastest any
channel has ever arrived. It then reset it 20 ms into the handshake, while iAP2 was waiting for the
accessory's SYN:

```
12:09:41.020  dialling the accessory
12:09:41.063  Got connect request from=B6:5D:B5:04:8E:FC
12:09:41.063  Waiting for SYN from accessory
12:09:41.083  Error performing syn: Connection reset by peer (104)
```

So the reverse direction is reachable and the unit answers on it instantly - it just will not carry
iAP2 while it is in plain-phone mode. Whether it completes once the unit is in CarPlay mode is the
open question, and the one worth testing next. Later dials in the same run return
`br-connection-not-supported`, which needs explaining before the path can be relied on.

`dial_accessory` alternates with the relink on each timeout, but is **off unless
`CARJACK_DIAL_IAP=1`** - see below.

### The unit withdraws its iAP record, and that is what "random" was

The same SDP query, seventeen minutes apart, from the same capture:

| | t=1040 (22:09:41) | t=2067 (22:26:48) |
| --- | --- | --- |
| query | `SSA Request` for `...decacaff` | identical |
| response | `Attribute bytes: 68` - handle `0x00010009`, RFCOMM channel 2 | **`Attribute bytes: 2`**, empty |
| outcome | dial connected in 43 ms | `record_cb() No SDP records found` -> `br-connection-not-supported` |

The record was published at 21:53 and at 22:09:41, and **gone by 22:10:02** - within about twenty
seconds of the one dial that connected and had its handshake reset. `bluetoothctl info` still lists
the UUID, but that is BlueZ's cached list from when the record was live; the unit itself now answers
the query empty.

With no record there is no iAP path in **either** direction - nothing to dial, and nothing that
would make the unit dial us. That is a better explanation of "it connects randomly" than anything
before it: the record is only up sometimes, and a session can only start while it is. Both triggers
that are known to work - power-cycling the bike, pressing the device on screen - are things that
would republish it.

Moving the bike to its home screen changed nothing, which rules the screen out as the mechanism on
its own; the record being absent is upstream of it.

**The dial is gated off by default** (`CARJACK_DIAL_IAP=1` to enable), but the suspicion that it
*caused* the withdrawal is now **wrong** - see below.

## A restart buys exactly one session (2026-08-13, night)

Restarting the bike republishes the record, and the whole chain then completes:

```
13:18:01  Preparing configured head unit
13:18:21  cycle 1 -> Connection result Ok    <- refused, no channel
13:19:02  cycle 2 -> Connection result Ok  (13:19:46.530)
13:19:46.758  Got connect request           <- 228 ms later
13:19:47.286  iAP2 link established
13:19:51.905  AirPlay bootstrap complete!
          Streaming heartbeat: 1.0 fps, 0 dropped to backpressure
```

Bluetooth -> iAP2 -> WiFi -> AirPlay -> video, first success under the new timing, and the <1 s rule
held exactly. Note it still took two pages: the first was refused even on a freshly restarted unit.

**The next run failed.** Three cycles, every page accepted, no channel - and a dial probe
immediately after returned `br-connection-not-supported`, so the record had already gone again.
That withdrawal happened with the dial **disabled** for the whole preceding run, which exonerates
it: the record goes away when a *session ends*, not when we dial.

So the pattern is one restart, one session. Every previously "random" success fits it.

**Candidate cause, unproven:** carjack handled `SIGINT` only, and `just test-run` kills it with
`timeout(1)`, which sends `SIGTERM` - whose default disposition killed us outright, leaving the
accessory holding a session we never ended. Every timed run tonight died that way. `main.rs` now
treats SIGTERM like Ctrl-C (`shutdown_requested`, and the watchdog arms both), which is right
regardless of whether it is what the unit is reacting to.

To test: restart the bike, run once and let it end **gracefully**, then run again without touching
the bike. If the second run connects, the abrupt kill was the cause.

## Why the unit shows no CarPlay icon for us: the record has no `0x1101` (2026-08-14)

`logs/cfmoto-to-iphone.pklg`, an iPhone-side PacketLogger capture of CarPlay being started from the
bike repeatedly without a restart. `btmon -r` reads it directly. The bike's probe is a fixed
sequence, repeated once per initiation - 26 service searches across the capture, 9 for the iAP UUID
and 16 for Serial Port:

```
tid 5358  ->  00000000-deca-fade-deca-deafdecacafe
tid 5359  ->  Serial Port (0x1101)
tid 5360  ->  Serial Port (0x1101)
tid 5361  ->  00000000-deca-fade-deca-deafdecacafe
...
```

The iPhone answers **all** of them from one record, because its iAP record declares SPP in its
profile descriptor list:

| attribute | iPhone's iAP record |
| --- | --- |
| `0x0001` Service Class ID List | `...decacafe` |
| `0x0004` Protocol Descriptor List | L2CAP + RFCOMM channel 1 |
| `0x0005` Browse Group List | Public Browse Root |
| `0x0006` Language Base | en / fr / de / ja |
| `0x0008` Service Availability | `0xff` |
| **`0x0009` Profile Descriptor List** | **Serial Port (0x1101), 0x0102** |
| `0x0100` Service Name | `Wireless iAP` |

**Our record carries none of that.** "Serial Port" appears 32 times in the iPhone capture and
**zero** times in ours - BlueZ's default record for an external profile is the service class, the
RFCOMM channel and little else. So a search for `0x1101` finds nothing on us however correct our
service class UUID is, and the unit never concludes we do CarPlay. No icon, no probe, no session.

This also explains the shape of what the unit does to us: it browses, reads attribute `0x0001` off
each record, and stops. It never sends us the `0x1101` search it sends an iPhone, and never the
targeted `...decacafe` search either.

**The icon is a cached verdict, not a live probe** - the unit shows it for the iPhone even with the
phone out of range. So the sequence above is what CarPlay *initiation* looks like once a device is
already classified; the classification itself happened earlier and stuck. Two consequences:

- fixing the record is necessary but may not be sufficient - the unit is still holding "carjack does
  not do CarPlay" from whenever it last decided, and has to be made to re-evaluate;
- the decision point is most likely the bond, when a unit browses a new device in full. carjack's
  profiles only exist while it runs, so any pairing done with carjack stopped taught the unit the
  wrong thing.

Not the device identity: we already present as Apple in the Device ID record - `Vendor: Apple, Inc.
(76)`, product `0x761d`, version `26.5.0`.

**The profile API cannot express this record**, so the manual XML is not laziness. BlueZ's
`get_generic_record()` fills attribute `0x0001` from `svc_str` (`Service` *or* the profile UUID -
it replaces, never both) and attribute `0x0009` from `uuid_str` (the profile UUID), emitted only
when `Version` is set. So `Version = 0x0102` yields a descriptor carrying our 128-bit UUID and a
`0x1101` search still misses, while `Service = 0x1101` puts SPP in the service class list and the
unit's `...decacafe` search misses instead. The iPhone's record needs both at once. Registering a
second, standalone SPP profile does not help either: the unit's `0x1101` search must resolve to the
iAP record itself - its answer to `tid 5360` is a record whose service class is the 128-bit UUID.

`device_profile()` now supplies `service_record` by hand - the iPhone's record, field for field.
The function carjack used to have for this (`generate_iap2_sdp`, commented out and then deleted in
`0bfcf85`) built the same shape, `0x1101`/`0x0102` included; deleting it is when this was lost.

### Confirmed on the wire: the record works, and we then failed to answer

A re-bond with the new record produced the furthest the bike has ever got. It walked our iAP record
attribute by attribute and got everything an iPhone gives it:

```
0x0001 -> 00000000-deca-fade-deca-deafdecacafe
0x0004 -> L2CAP + RFCOMM
0x0009 -> Serial Port (0x1101), 0x0102
0x0100 -> "Wireless iAP"
```

10 ms later it opened PSM 3, then RFCOMM **dlci 0x02 - channel 1, our iAP channel** - and began
sending iAP2 SYN:

```
BIKE->us  ff 55 02 00 ee 10    x50, once per second
us->BIKE  (nothing)
```

**Fifty SYNs, zero replies.** So the SDP record is settled: the unit classifies us, opens the
channel, and speaks. The failure moved to our side.

The cause was ordering. `wireless_carplay` awaited `find_target_device` *before* anything read the
profile stream, and with the bond just removed that falls into discovery for up to `DISCOVERY_WAIT`
(60 s). BlueZ answered the SABM - the profile is registered from `IapHost::start`, so the channel
opens - but nothing was polling for the `ConnectRequest`, so it was neither accepted nor rejected.
The unit talked into a socket no one was reading, for the whole discovery window, on every retry.

Resolution now runs in a background task feeding a `watch<Option<Device>>`: the accept loop arms
immediately and filters incoming requests against the configured *selector* rather than a resolved
address, while paging and the MAC hand-off wait on the resolved device. Confirmed by log order -
`Waiting for iAP2 connection requests` at 12.227, `Waiting for the configured head unit` at 12.232.

The bond is currently removed, so the next test is: start carjack, then pair from the unit.

### The unit asks nothing else, and it decides 240 ms after bonding

The same capture contains the bond, so the whole classification is visible:

```
95.672 - 96.040   phone browses the UNIT (all attributes, PnP, SDP server class)
96.144 - 99.613   pairing: IO capability, user confirmation, link key, auth complete
99.850            UNIT -> phone: search 00000000-deca-fade-deca-deafdecacafe
99.904 / 99.929   UNIT -> phone: search Serial Port (0x1101), twice
100.162           UNIT -> phone: search A/V Remote Control Target (0x110c)
102.094 ...       the same three-query probe again, per initiation
123.123           iAP2 SYN (ff 55 02 00 ee 10) - the session finally starts
```

**Those three UUIDs are the only things the unit ever asks the phone for**, across the entire
capture. It never browses the phone's full service list, never reads its PnP/Device ID record,
sends no Apple HFP extension (no `AT+XAPL`, no `AT+IPHONEACCEV` - only stock HF commands), and no
vendor-specific exchange. Nothing the phone volunteers is involved either: the first probe lands
**240 ms after the link key**, and iAP2 does not start for another 23 seconds, so no in-session
identification can be the source.

So the memorised fact is built from SDP alone, at bond time, and the only query of the three we
cannot answer is `0x1101`. That also fixes when the fix has to be in place: the record must be
registered **before the unit bonds**, because that is the moment it decides.

## Answering `ForceKeyFrame` properly (2026-08-13)

A request from the accessory now moves three things instead of one:

- the encoder forces an IDR, as before;
- the **config frame goes out with it** (and the `0x10` opening marker, which follows the config) -
  a receiver that asks for a keyframe has lost its decoder, and the configuration is what starts
  one. A keyframe nobody asked for still carries neither;
- the **compositor publishes a frame** even if nothing changed, so the forced keyframe has a buffer
  to ride out on within a tick instead of waiting up to `IDLE_REFRESH`.

Which keyframes carry a config is now the sender's decision (`885c374` moves the last policy out of
catplay), because the accessory's request is only visible here. `tests/wire_format.rs` pins it: 45
frames including natural IDRs carry no config, then a request produces one that does.

Untested against a unit that actually asks - the bench never has, in nine runs. The bike asked 407
times in one run, so that is where it will show.

## Damage-driven frames, and one config frame again (2026-08-13)

Both changes follow from the iPhone capture below.

**One config frame per stream** (`80d131e`), reverting the per-keyframe config of 2026-08-12. A
phone sends exactly one, and one `0x10` marker with it; the reasoning for repeating them - that the
bike could not recognise the keyframe it asked for - now looks wrong, because the bike wanted HEVC
and was getting it. If a unit ever needs a config to recover, the place to put it back is a
keyframe that answers a `ForceKeyFrame`, not every keyframe.

**The compositor publishes on damage** rather than on its 30 Hz timer. Any client commit marks the
frame dirty; the timer renders and publishes only when it is, with a one-second floor so a still
screen still feeds the accessory's decoder and a keyframe request has a buffer to ride out on.
Clients get their frame callbacks on every tick either way, so nothing that wants to animate is
left waiting. Measured on the bench, app path:

```
compositor: 30.0 fps published of 30.0 polled     <- waydroid animating
compositor:  1.0 fps published of 30.0 polled     <- screen still
Streaming heartbeat: 1.0 fps (5 frames / 5.2s), 0 dropped to backpressure
```

An iPhone is damage-driven too - 264 frames in 17.4 s with gaps up to 4 s. The desktop path
(`pipewiresrc`) is untouched.

## What an iPhone sends the bench: H264, and a bare `avcC` (2026-08-13)

`logs/iphone-bench.pcapng`, phone -> AUTO BOX, 17 s of screen socket. The phone sends this unit
**H264**, and its config frame is a different shape from the one it sends the bike:

| | bike (CFMOTO) | bench (AUTO BOX) |
| --- | --- | --- |
| codec | HEVC | **H264** |
| config body | 247 b `hvc1` sample entry | **41 b bare `avcC`** |
| flags | `0x011e` (incl. `UseFormatDescription`) | **`0x0116`** (no `UseFormatDescription`) |

`01 64 00 1f ff e1 0016 <SPS> 01 0004 <PPS> 02 00 00 00` - High profile, level 3.1, 4-byte
lengths, one SPS and one PPS. That is the shape catplay already writes for H264, so our config
frame matches the phone's but for four trailing bytes (`02 00 00 00`) it appends after the PPS and
we do not; the bench decodes ours without them.

**The phone picks per unit, and neither unit declares a codec.** Their `/info` `features` words are
byte-identical (416679199360), `displays[].features` is 10 on both, and neither carries `hevcInfo`.
The only meaningful difference between them is `sourceVersion`: **475.8.2 gets HEVC, 320.14 gets
H264**. `auto` now reads that too (`expects_hevc`, threshold interpolated at 400), so both units
get what a phone gives them. This is a two-sample rule: panel size (800x1280 vs 1080x600) fits the
same evidence, and if a third unit contradicts it the threshold is one constant to change.

Two more things the capture settles:

- **One config frame and one `0x10` marker in the whole session** - 282 frames. Our config frame per
  keyframe is therefore not what a phone does, though the capture cannot say how many keyframes the
  phone sent (the bodies are encrypted), and the bench is happy with ours either way.
- Its keep-alives carry the same stats plist we now send (`rttAvg: 21`, `encoderCurrentFPS: 30`,
  `txCapacityAvg: 16193600`), so a 320.14 unit wants those too.
- The phone is damage-driven: 264 frames in 17.4 s (~15 fps), 257 b to 135 kB, where we send a flat
  30.

## The bench unit shows a picture again: HEVC was the regression (2026-08-13)

With a webcam on the bench unit (see `.claude/skills/webcam`) the loop finally closes - a log
cannot tell a decoded stream from an ignored one, a photograph can.

| | picture? |
| --- | --- |
| idle, no session | the unit's own home menu |
| `codec = hevc` (the default since 2026-08-12) | black |
| `codec = h264` | **video** |
| `codec = auto` after the fix (picks H264) | **video** |

So the black bench screen was the HEVC default, not any of the wire-format work: every fix from
the last two days - the AUD strip, the `hvcC` profile/level, a config frame per keyframe, the
opening marker, `hvc1` from the parser - was in the stream that now displays.

`auto` now sends HEVC only to an accessory that declares it (`hevcInfo` or
`SUPPORTS_SCREEN_MULTI_CODEC`), and the declaration is settled after `/info` rather than at
startup, so the backend is re-stamped with the codec once the accessory has spoken. The bench
declares neither and decodes H264 perfectly; the earlier reasoning - that an iPhone sends HEVC to
units that declare nothing, so the declaration can confirm but never veto - was exactly backwards
for at least one real unit. `codec = "hevc"` still forces it, which is the setting to try on the
bike, whose iPhone capture does show HEVC.

Two things this run of experiments also settled:

- **The bench never displayed anything through the HEVC period.** Its 1822-frame run with zero
  keyframe requests was a black screen the whole time. Silence from a head unit means nothing.
- **The unit does not always switch to the CarPlay surface.** Three runs out of five showed video;
  the other two sat on the unit's own home menu with identical logs and a healthy 30 fps stream. A
  blank screen is worth photographing twice before believing it.

## The encoder now produces the wire form directly (2026-08-13)

The screen stream carries length-prefixed NALs and a decoder configuration record, which is exactly
what gstreamer's `avc`/`hvc1` stream formats mean - so the parser is asked for them instead of for
byte-stream, and its `codec_data` is the record. That deletes `normalize_annexb_4byte` (a shim for
`AnnexBIter::annexb` requiring 4-byte start codes), the Annex-B scan in `split_access_unit`, and
catplay's Annex-B -> HVCC conversion per frame. Keyframes come from the parser's `DELTA_UNIT` flag
rather than from looking for parameter sets.

What the parser does **not** do is take the parameter sets out of the sample: they arrive in
`codec_data` *and* in front of the slice. So `coded_slices` still keeps only the coded slices, for
the reason below.

catplay gained `EncodedVideoFrame::length_prefixed` (`fc15449`) so a sender whose encoder already
emits the wire form skips the conversion.

Verified, in order of how much they prove:

- The parameter sets in gstreamer's `codec_data` are **byte-identical** to the ones the old path
  scraped out of the bitstream, so the config frame is unchanged - and the sample entry it produces
  still matches the iPhone's field for field, `ffff` dataRefIndex through `0x200` quality to the
  `hvcC` PTL. `tests/wire_format.rs` builds both frames from real encoder output and asserts it,
  including that the sample opens on an IRAP and carries nothing but slices.
- A **full 75 s run against the bench unit**: 1822 frames at 30 fps, 61 config frames (one per
  keyframe), zero keyframe requests, zero drops, zero errors. It accepted everything.
- H264 still encodes and decodes (`avc`, High, yuv420p), though no unit has been given one since.

The bench cannot be seen from here, so this still does not prove a picture - and no capture was
taken, because `dumpcap` needs a privilege this session does not have.

## Progress: the bike now asks for a keyframe on every frame (2026-08-12, late evening)

`logs/cfmogo-12-08-2053.log` + capture. Still black, but the behaviour changed for the first time
in the whole investigation: **407 `ForceKeyFrame` requests, one per frame at 30/s**, where every
previous run produced exactly one. The unit is now looking at our frames and rejecting each one,
rather than ignoring the stream.

The capture says what it is rejecting. Frame by frame, off the wire:

```
#0 op=1 (config)  sp=001e01  226 b
#1 op=0 sp=100000  24139 b   <- the opening keyframe, marked, config in front of it
#2..#5 op=0 sp=000000  71, 142, 240, 572 b     deltas
#6 op=0 sp=000000  21528 b   <- the keyframe it asked for: unmarked, no config
```

...and the flood starts right after #6. The unit asked for a keyframe, we sent one, and on the wire
it was **indistinguishable from a delta frame**: no config frame, no `0x10` marker. Only the first
keyframe of a session ever carried either, because catplay deduplicated the config against the last
one it sent - and the config is what the marker follows.

Fixed in the fork (`1bbb452`): the config frame is no longer deduplicated, so it accompanies every
keyframe, and because the session re-arms the opening marker on each config, every keyframe now
carries `0x10` as well. The sample entry itself is already byte-identical to the phone's apart from
the parameter-set lengths and one constraint-flag byte our encoder sets differently (`90` vs `b0`,
`non_packed_constraint`).

The remaining reading of that capture, if this is still black: the phone's single marked frame was
also its only keyframe in 86 frames, so "`0x10` marks a keyframe" and "`0x10` marks the first frame"
fit the reference equally well. We now do both.

## Every frame we ever sent opened with an access unit delimiter (2026-08-12, late)

The `hvcC` fix below changed nothing on the bike (`logs/cfmoto-12-08-1830.log`), so the question
became what a receiver looks at to decide a frame is worth decoding. catplay's own receiver - the
one real iPhones talk to - reads **the first NAL of the sample** and asks whether it is an IRAP:

```rust
let nal_byte = self.data.get(nal_size_len).copied().unwrap_or(0);
let is_keyframe = HevcNalType::from_byte(nal_byte).is_irap();
```

Dumping what our encoder actually produces (`tests/encode_dump.rs`, 60 access units) settles it:

```
AUD, VPS, SPS, PPS, IDR_W_RADL,   AUD, TRAIL_R,   AUD, TRAIL_R,   ...
```

**Every access unit starts with an access unit delimiter**, so by that rule no frame we have ever
sent - in any codec, to any unit - has read as a keyframe. A head unit waiting for one before it
starts its decoder waits forever, which is precisely the failure: it ACKs the SETUP, asks for a
keyframe once, gets another "delta", and shows black while feeding back happily. Our own dumps
always decoded because ffmpeg is not looking for permission to start.

The parameter sets sat in the same sample, which an `hvc1`/`avc1` entry also forbids - they belong
in the sample entry, which is where we already send them.

Fixed in `split_access_unit` (src/encode.rs): the sample keeps its coded slices and nothing else,
so a keyframe now opens on `IDR_W_RADL` and a delta on `TRAIL_R`. `encode_dump` puts the parameter
sets back in front of each keyframe so the dumped file still decodes; it comes back Main / yuv420p
/ 800x1280 as before.

**Untested against either unit.** It is the first explanation that covers the whole history - both
codecs, both units, every run - rather than the bike alone.

## The `hvcC` described a stream at no level (2026-08-12, evening)

Two more bike runs, both black (`logs/cfmoto-12-08-1942.*` with a capture, `cfmoto-12-08-1945.log`
at trace). Nothing in either log is wrong: HEVC picked, VBR, view rect sent, first keyframe 88 ms
in, the unit acknowledges the screen SETUP, asks for one keyframe, then feeds back happily for the
rest of the run. It even moves iAP2 onto the AirPlay control channel two seconds after the WiFi
handover and completes the link there (detect, SYN, SYN+ACK, ACK).

Comparing our screen stream against the iPhone's, frame header by frame header, is what found it.
The decoder configuration record we send is not the one the phone sends:

| | iPhone | ours |
| --- | --- | --- |
| `general_profile_compatibility_flags` | `60000000` | `00000000` |
| `general_constraint_indicator_flags` | `b00000000000` | `000000000000` |
| `general_level_idc` | `78` (level 4.0) | `00` |
| `numTemporalLayers` | 1 | 0 |
| config frame timestamp (`params[0]`) | the first frame's PTS | 0 |
| first video frame `small_param[0]` | `0x10`, on that frame alone | 0 |

catplay wrote a fixed `hvcC` skeleton and never looked at the SPS, so **every HEVC stream we have
ever sent claimed no profile and no level** - which is a decoder that has no reason to start, on a
receiver that configures itself from this record rather than from the parameter sets. Our own dumps
never showed it because ffmpeg reads the in-band VPS/SPS/PPS and ignores the broken record. The
`avcC` path took profile and level straight from the SPS all along, so this is HEVC-only.

Fixed in the fork (`5001cec`): the twelve profile/tier/level bytes and the sub-layer count now come
from the SPS, the config frame carries its frame's timestamp, and the sample entry's `dataRefIndex`
and quality fields match the phone's. A round-trip test feeds the phone's own record through our
writer and demands the bytes back unchanged.

Keep-alives now match too (`68cadb9`). The phone sends `KeepAliveWithBody` carrying a binary plist
of its own stream stats - `rttAvg`, `txUsageAvg`, `encoderCurrentFPS`, `sentFramesAvg`,
`queuedFramesAvg`, `txCapacityAvg`, `lossAvg` - with the body length repeated in `params[14]`,
where we sent an empty `KeepAlive`. We send the same frame with the five we can measure honestly;
`rttAvg` and `txCapacityAvg` are omitted rather than sent as a flattering zero, since nothing here
measures round-trip time or link capacity.

**Untested against either unit.** Still-known deltas, neither explained: the phone puts the video
view one pixel above the panel's declared origin (223 vs 224), and its PTS domain is device uptime
where ours starts near zero. And on the tunnelled iAP2 link both sides go quiet - the accessory
sent `StartIdentification` within 80 ms over Bluetooth, but over the tunnel nobody speaks, because
that is the one thing we do not drive there.

## Bench regression after the HEVC switch (2026-08-12): no regression on the wire

`logs/bench-hevc-regression.log`, AUTO BOX, 100 s to the `test-run` timeout. The whole flow held
with HEVC in place of H264: iAP2 identification (`AUTO BOX`), EA session, WiFi, **Bonjour discovery**
(the bench path the bike never uses), AirPlay bootstrap, 1080x600 @ 30 fps, first keyframe 7 ms
after the encoder started, then 86 keyframes and a steady 30.0 fps with **zero dropped frames and no
teardown**. The bench declares no HEVC support (`hevc_declared=false`, like every unit) and takes it
anyway. VBR shows too - 2-3 KB frames where CBR sent 12500 every time. Sending no view rect on a
unit with no `displayPanels` is correctly a no-op.

**What this does not prove is that the bench displays a picture** - exactly the trap the bike set.
Nothing in a log distinguishes a decoded stream from an ignored one; only eyes on the unit do.

### The Bluetooth teardown: do what the phone does, and stop calling it an error

Three attempts at closing the link ourselves, and the answer was not to. An iPhone does **not**
disconnect at the handover: its iAP2 traffic ends at `4300`/`4301`, but it keeps acknowledging and
lets the accessory drop the link. So carjack does the same, and the `os error 104` that has been in
every log since the beginning is simply reported as what it is.

A `handover` token (fired when the accessory's WiFi associates, or on `disableBluetooth`) marks the
point after which losing the link is the expected ending. Past it, a link error ends the session
`Ok`, at info, and no goodbye reset is sent - a phone would not have sent one and the socket is gone
anyway. Before it, everything behaves exactly as before.

Bench, `logs/bench-swallow-104.log`: `Handover complete` at `52.962`, the accessory drops the
channel 215 ms later, `Accessory closed the iAP2 link after the handover (IO Error: Connection reset
by peer)` at **info**, `CarPlay (Bluetooth) session ended` with no error suffix, streaming
unaffected. No `WARN carjack:` in the run at all.

Recorded so the earlier attempts are not repeated: triggering on the **iAP2 tunnel** only ever fires
on the bike, since the AUTO BOX never opens one; triggering on **AirPlay connecting** is too late,
because the AUTO BOX resets the channel 254-444us after that, before `cancel()` even runs on the
main task.

### There is no codec negotiation to inspect, and nothing inbound is being dropped

There is no SDP in CarPlay: the session is `GET /info`, `SETUP /setup`, `SETUP /` (screen),
`RECORD /`, `POST /command`, `POST /feedback`. catplay's `Announce` is an unused RTSP enum entry and
`force_announce` is Bonjour, not SDP. The screen SETUP response carries `data_port` and
`stream_type` and nothing else. `/info`'s plist is the only capability channel; carjack reads it
(`hevcInfo`, `features` bit 42 `SUPPORTS_SCREEN_MULTI_CODEC`, `displays`, `displayPanels`) and logs
`Accessory codec declaration` - both units answer nothing at all for codecs.

That the accessory might be *sending* something we quietly drop was checked separately, since "we
never logged one" is not the same claim. All four inbound paths:

- **Event channel** - every request is logged before dispatch (non-`/command` is logged *then*
  404'd). Only `POST /command`, across three runs and both units.
- **Control connection** - we are the client; an inbound request is `ProtocolViolationGeneric`, a
  hard error. Zero in any run.
- **Screen socket** - same strictness, and the bike capture has zero bytes accessory->us on it.
- **carplay-ctrl** (bench only; the bike never invites) - logs every frame, but at `debug`, which
  `catplay_carplay=info` hides. Re-run at debug (`logs/bench-ctrl-debug.log`): exactly one frame,
  `GET /ctrl-int/1/connect`, and no `Rejected invalid CarPlay-Ctrl request`.

At the network level on the bike: **zero accessory-initiated TCP connections**, zero RSTs from us, zero
ICMPv6 unreachables - so nothing was sent to a port we were not listening on either.

## The run that ruled out the source, and the config-frame diff (2026-08-12, later)

`logs/cfmoto-12-08-0456.*`: HEVC on the wire, view rect declared, VBR working - and still black.
Two things it settled and one lead it produced.

**Waydroid was rendering.** VBR made this answerable for the first time: 1259 video frames, 39 of
them **over 20 KB** and one at **187 KB**, against a median of 67 B. Filler would have flattened all
of that to 12500. So a real picture was encoded and sent; the black screen is not a black source.

**The accessory is now engaging with the stream.** It sent a `ForceKeyFrame` 21 ms after RECORD - the
first time it has ever asked us for anything on the video path - and forwarded 33 touch reports, so
it is in CarPlay mode with our stream as its screen.

**What is still different from the iPhone is the config frame**, byte for byte:

```
                    iPhone              carjack (before)
small_param         00 1e 01            00 0e 00
compressorname      04 "HEVC"           06 "'1cva'"      <- the AVC name, in an hvc1 entry
after hvcC          colr (nclc 1/13/1)  absent
first video frame   small_param[0]=0x10 0x00
```

The flags line is the substantive one. `ScreenFlag` was a `u8` over `small_param[1]`, with
`// const NoDisplaySleep = 1 << 8;` commented out in catplay - a bit that cannot fit a `u8`. Read
as a **16-bit little-endian field across `small_param[1]` and `[2]`**, the iPhone's `1e 01` is
`0x011e`: the flags we already sent, plus bit 4 and **`NoDisplaySleep`**. A receiver that was never
told to keep its panel awake is a plausible black screen with every other layer healthy.

Fixed in the fork (`821c412`, branch `feat/hevc-and-view-rect`): flags are 16-bit and carry
`NoDisplaySleep` and bit 4; an `hvc1` entry names `HEVC`; the `colr` atom (BT.709 / sRGB) is
appended. A test pins the result against the captured reference bytes. The first-frame `0x10`
marker is **not** done - it needs per-session state in the screen transmitter.

**Untested on the bike.** If it is still black, the remaining deltas are that marker byte, the
`dataRefIndex`/quality fields in the sample entry (`ffff`/`0x200` vs `00ff`/`0`), and the
one-pixel view origin difference.

## Built: HEVC, VBR and the view rect (2026-08-12)

All three of the differences from the iPhone below are now closed, verified locally through the
real encode chain (`tests/encode_dump.rs`, decoded with ffmpeg) but **not yet against the bike**.

- **HEVC.** `[graphics] codec` takes `auto` (the default), `hevc` or `h264`; `auto` means HEVC.
  Nothing to negotiate against - the bike advertises neither `hevcInfo` nor
  `SUPPORTS_SCREEN_MULTI_CODEC` (its feature bits are 7, 9, 18, 26, 32, 37, 38) and takes HEVC from
  a phone anyway - so the declaration is logged as confirmation and never as a veto, and `auto`
  falls back to H264 only when no HEVC encoder is installed. `vah265enc`/`nvh265enc`/`v4l2h265enc`
  are in the backend table, and `extract_codec_config` now collects VPS/SPS/PPS (NAL 32/33/34)
  as well as H264's SPS/PPS.
- **One trap worth knowing:** left to negotiate freely `vah265enc` takes `vapostproc`'s YUY2 and
  encodes **4:2:2** (`main-422-10`, which decodes as Range Extensions) - no accessory decodes that. The
  fix is the new `encoder_input` column pinning NV12 *with its memory feature named*; constraining
  the encoder's output profile instead looks equivalent and isn't, because it pushes the upstream
  negotiation onto DMABuf and dies with "DMABuf caps negotiated without the mandatory support of
  VideoMeta". Output is now HEVC Main / yuv420p / 800x1280, matching the phone.
- **VBR, both codecs.** `rate-control=vbr` (and `video_bitrate_mode=0` for v4l2, `pass=qual` for
  x264enc), so `bitrate` is a ceiling rather than a floor of filler. Measured over the same 600
  frames of Waydroid: **7.13 MB H264/CBR -> 1.70 MB H264/VBR -> 1.36 MB HEVC/VBR.**
- **The view rect.** `params[3..6]` of the config frame now carry origin and size from `/info`'s
  `displayPanels` (fork branch `feat/hevc-and-view-rect`, which also parses `displayPanels` at
  all). For the bike that is origin `(0, 224)`, size `800x1056`. The iPhone sends `223`, one less
  than the panel declares, for reasons unknown - the declared value is used.

## The iPhone reference settles it: we send the wrong codec (2026-08-12)

`logs/cfmoto-iphone.pcapng` - a working iPhone session against this same bike - was re-parsed frame
by frame (screen frame headers are plaintext). It differs from what carjack sends in four ways, and
the first is almost certainly the black screen:

- **The iPhone streams HEVC, not H264.** Its `VideoConfig` is a `hvc1` sample entry (compressorname
  "HEVC", coded 800x1280) with `UseFormatDescription` set - `small_param[1] = 0x1e` against our
  `0x06`. carjack sends H264 through `vah264enc` and hardcodes `hevc: false`. The bike takes it,
  ACKs it, asks for nothing, and displays nothing. catplay already has the send path
  (`hevc: true` -> `hvcc_config_serialize` -> `hvc1`/`hvcC` atom); what is missing on our side is an
  HEVC encoder and NAL parsing for VPS/SPS/PPS (32/33/34) in `extract_avcc_config`.
- **It declares the video's view rect.** `params[3..6]` carry origin `(0, 223)` and size
  `(800, 1056)` - matching `/info`'s `displayPanels.initialVideoStreams.originYPixels = 224` under a
  1280-tall panel. carjack sets only `params[1]` (the 800x1280 display size); catplay's decode path
  has the view params commented out.
- **It sends frames only when the screen changes.** 77 video frames in 6.6 s (~11 fps average),
  arriving in bursts with idle gaps of 0.5 s, 1.0 s, 1.0 s and **4.0 s**, filled by
  `KeepAliveWithBody`. Sizes run 376 B to 63 KB (median 11 KB) - VBR, no filler - averaging
  1.66 Mbit/s while active and nothing while idle. carjack renders on a fixed 30 fps timer by
  design and pads every frame to the CBR target, so it transmits a flat 3.0 Mbit/s forever.
- **Its frame PTS omits the NTP epoch offset**, which its time sync packets carry: `t3` read
  `2209933588.87` and the PTS on frames 0.5 s later read `944789.37`, exactly one epoch apart. See
  the correction below.

## Corrected: the frame PTS was never wrong (2026-08-12)

An earlier entry here called the 70-year gap between catplay's time sync domain and its frame PTS a
bug, "fixed" it in the fork on `fix/media-clock-ntp-domain`, and predicted the bike would then show
a picture. **Both halves were wrong.** The iPhone capture above shows a real sender doing exactly
what catplay already did - sync packets in NTP, PTS in bare monotonic - and the bike stayed black
after the change, which should have been the first clue.

Reverted: the change is gone from every branch the build uses. The only thing kept is a doc comment
and a test on `NTP_UNIX_EPOCH_OFFSET_NS` (fork commit `283adaf`, on `fix/airplay-device-identity`
and inherited by `feat/hevc-and-view-rect`) pinning the split, so it does not get "fixed" a second
time. The `fix/media-clock-ntp-domain` branch is left on the fork, unused.

## Also settled on 2026-08-12

- **The encode path is correct.** `tests/encode_dump.rs` writes what the encoder would send to a
  file, through the backend `main` actually picks (the screenshot driver forces SHM and proves
  nothing about the dmabuf chain). Decoded at the bike's 800x1280 through `vapostproc ! vah264enc`:
  a correct, upright picture. Framing and orientation are ruled out - the *codec choice* is not.
- **Frame size means nothing.** `rate-control=cbr` pads every frame to exactly `bitrate/fps` =
  12500 bytes, so an empty black compositor still costs ~10 KB a frame, and every "healthy 30 fps
  at 12500 bytes" heartbeat is equally consistent with a black screen. `CARPLAY_ENCODER` overrides
  the tuning without a rebuild, so `rate-control=vbr` is a one-env-var experiment - and the iPhone
  numbers above say VBR is also what the real thing does.
- **A stale waydroid session is still a live trap.** The `0136` run's stream is indistinguishable
  from black (2207 frames, not one over the cap, opening with the exact 281-byte IDR + 45-byte
  P-frame signature of a uniform black frame), waydroid printed none of its startup lines into that
  log, and it was found afterwards attached to a dead socket. Check `waydroid status` before a run
  and grep the log for `Android with user 0 is ready` before reading anything into the protocol.
- **`disableBluetooth` never arrives on this unit** - not over AirPlay, iAP2 or the tunnel, in any
  capture. The accessory signals the handover by bringing the tunnelled iAP2 link up and resetting RFCOMM
  76 ms later, so the graceful shutdown fires on the tunnel link as well as on the command.

## Reverted: back to NetworkManager, not iwd (2026-08-09)

`main` goes through `nm-automata` again; the `iwd` work (`wifi_connect.rs` against
`iwd-automata`, the `carjack-wireless-helper wifi-profile` sidecar path, the whole
patched-iwd build pipeline) moved to the `iwd-backend` branch rather than being carried
forward or deleted.

The bench unit's HE-downgrade disconnect (see "Changed: WiFi goes through iwd" below) is real
and driver-agnostic, but the fix does not need iwd specifically - wpa_supplicant has had
per-network `disable_ht`/`disable_vht`/`disable_he`/`disable_eht` config keys for a while, and
`nm-disable-modes.patch` (this repo) adds the missing piece, four `NMSettingWireless`
properties (`disable-ht`/`-vht`/`-he`/`-eht`) that forward them to wpa_supplicant the same way
`hidden` or `ap-isolation` do. Builds and compiles clean against a real NetworkManager
checkout; not yet deployed.

`wifi_connect.rs`'s retry-with-DisableHE mechanism carries over: `accessory_profile` sets
`disable-he` through `WirelessSetting::other` (the generated binding's catch-all map for
properties it doesn't have a typed field for) rather than a typed setter, since picking that
up for real needs `nm-automata` regenerated against a patched NetworkManager - not done yet,
so this is unverified against a live daemon. `other` is real plumbing, not a hack: it's what
`nm-automata` round-trips unknown settings through by design.

A wireless CarPlay bridge that impersonates an iPhone to an accessory head unit: Bluetooth iAP2 to
authenticate and get handed the accessory's WiFi credentials, then CarPlay video over that WiFi via
AirPlay.

## The blocker on the bike (2026-08-04): the accessory deauths us 16 ms after the screen SETUP

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

**Experiment (2026-08-05, `logs/cfmoto.log` with `catplay_carplay=info`): the accessory answers every
AirPlay request through `assert_modes`, and goes silent exactly at the screen SETUP.** Pair-verify,
the encrypted `SETUP /setup`, `/info` (32 KB) and `modesChanged` all return 200 OK; the very next
request - `SETUP /` for the screen stream - is never answered at all (times out 11 s later into
`Request/channel timeout`). No `Screen setup response received`, no ACK, no RST. So the "did the
accessory answer the SETUP before deauthing" question is settled: **it does not answer it, in this run**;
it drops the session the moment the video screen session begins. Full exchange table in the
investigation log.

The sharpest evidence yet, from `cfmoto.log` + `cfmoto.pcapng` (17:05, the first run of the
`0x4301` fallback). Everything works until one exact moment:

```
16.620  Timeout waiting for CarPlay ctrl invite; connecting to [fe80::9083:…]:7000
16.909  AirPlay bootstrap complete!   Accessory display: 800x1280 @ 30 fps
16.922  Asserted screen modes (controller owns screen)      (385 -> 182 on the wire)
16.925  screen SETUP sent  (314 bytes)
16.9417 kernel: wlp0s20f3: deauthenticated from 92:83:8e:ef:a7:d3 (Reason: 2)
```

- **The direct connect works.** `/setup`, pair-setup, pair-verify, `/info` answered with a 13.8 KB
  plist, event socket up, modes exchange answered. 289 ms from TCP to display known. The `/info`
  timeout that once justified deleting this path was the dying link, nothing more.
- **The deauth is the accessory's.** `deauthenticated from …`, *not* `deauthenticating … by local
  choice`: mac80211 prints the first for a frame it **received**. The 21:02 run's four were all
  local-choice (rechecked), so these are two different failure modes and this is the one that
  matters.
- **16 ms is a decision, not a link failure.** Too fast for a rekey, an inactivity timer or a
  retry exhaustion. The accessory chose to drop the station immediately after the screen SETUP.
- **It also kills the mDNS-keepalive theory** from the 2026-08-03 entry: this run advertised and
  connected, and the association died anyway - on a timescale no keepalive argument can explain.

The accessory never answers the SETUP; carjack waits 11 s, gets `Request/channel timeout`, restarts, and
the retry ends in a Bluetooth page timeout.

This is the same shape as `cfmoto_16` (SETUP *answered* 314 -> 225, deauth 77 ms after the screen
data socket's handshake, before RECORD, no data ever sent) and `cfmoto_12`. So across three
captures the trigger sits in the same place: the screen stream. Whether the accessory answers the SETUP
first varies; that it drops the station within tens of milliseconds does not.

What the SETUP actually carries is small - `streamConnectionID` (a random u64), `type`,
`latencyMs` (carjack asks 100 ms), and the display `uuid` from `/info`. Nothing obviously
objectionable, and `cfmoto_16` shows the accessory accepting it. Two candidates worth separating:
the plist contents, and the accessory reconfiguring its AP when a session moves to streaming (an AP that
has torn down its association state answers exactly `PREV_AUTH_NOT_VALID`).

**Next evidence, cheap:** catplay already logs `Sending screen SETUP: {setup:?}` and
`Screen setup response received: {resp:?}` at info - `.env` was pinning `catplay_carplay=error`,
which is why no run has ever shown them. Now set to `info`. The next bike run therefore says
whether the accessory answered before deauthing, and with what.

## Superseded: the blocker as it looked on 2026-08-03 (a different failure mode)

**Not the accessory. The WiFi association dies on its own about 12 s in, while the accessory carries on
talking to us over Bluetooth the whole time.** The 21:02 run settles this:

- **The deauth is ours.** `wlp0s20f3: deauthenticating from 92:83:8e:ef:a7:d3 **by local choice**
  (Reason: 2=PREV_AUTH_NOT_VALID)`, and wpa_supplicant agrees: `CTRL-EVENT-DISCONNECTED …
  reason=2 **locally_generated=1**`. The earlier `iw event` reading of "disconnected (by AP)" for
  `cfmoto_16` was the same event read the wrong way round.
- **The AP is healthy and keeps taking us back.** Every retry gets `RX AssocResp … status=0
  aid=1`. An AP that had walked away or rebooted could not do that.
- **The accessory is healthy.** In the btsnoop, the bike is running SDP and PBAP (`Get final
  "telecom/pb.vcf"`) at 21:02:43 - half a minute *after* the WiFi started cycling, and long after
  carjack had given up. So "the accessory walks away and takes Bluetooth with it" does **not** apply
  here; only the WiFi path failed.
- **It is not an `assert_modes` reaction.** This run never got that far: `/info` was sent and
  never answered, so the deauth cannot be a response to anything we asserted. The
  ~20-45 ms-after-`assert_modes` correlation in the older captures was coincidence - the modes
  assertion is simply what a session that survives ~12 s is doing at the moment the link drops.

The shape to explain is now: associate → DHCP completes (`192.168.149.57`, accessory is `.201`) →
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
carjack sends a WiFi MAC. The accessory appears to correlate the station on its AP with the device it
authenticated over Bluetooth, by Bluetooth address.

Changed: carjack now always advertises `_carplay-ctrl._tcp` with the adapter's Bluetooth address
as `id`, waits for the invite, and connects only where the invite points. The direct-connect and
gateway-fallback paths are gone (`connect_airplay_endpoint` takes one address), along with
`AccessoryWifi::gateway`/`iface_mac`.

**Confirmed on the bench AUTO BOX (2026-08-03 22:34, `/tmp/bench-invite.log`) - the `id` was the
whole problem.** The invite arrived **69 ms** after the advertisement:

```
12:34:23.708  Accessory WiFi connected, waiting for the accessory's CarPlay invite...
12:34:23.710  _carplay-ctrl._tcp advertised on wlp0s20f3:8082 (id=4C:79:6E:DA:29:4B)
12:34:23.779  Got CarPlay ctrl invite; AirPlay peer at 192.168.43.1:7000, pi=Some(90665097-…)
12:34:24.006  AirPlay bootstrap complete!  Accessory display: 1080x600 @ 30 fps
12:34:24.070  RECORD sent, streaming video     (first keyframe 4.05 ms)
              … 30.0 fps steady for 70 s, 0 dropped to backpressure, touch arriving …
```

The same unit on 2026-07-17 advertised the **WiFi** MAC and timed out after 15 s, reaching RECORD
only through the fallback that is now deleted. Same unit, same advertisement, different `id`:
69 ms versus never. That is direct evidence for the Bluetooth-MAC correlation, independent of the
bike. It is also strictly better than the old path - the invite carries the accessory's HomeKit pairing
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
  world-readable 0644 worked (6/6). Fixing it with 0644 would leak the accessory's passphrase.
- **Superseded (2026-08-07): the POSIX ACL answer to that does not survive an iwd restart.**
  `wifi_connect` used to write 0600 and add a root-read ACL (`u:0:r`) via the `exacl` crate. That
  holds until the *directory* is the problem: `iwd.service` sets `StateDirectory=iwd` with
  `StateDirectoryMode=0700`, and systemd re-applies that mode on every start - which recomputes
  any POSIX ACL mask from the new group bits, i.e. zeroes it. Seen as
  `user:chris:rwx #effective:---` alongside `mask::---` on `/var/lib/iwd`, so carjack could not
  even create its temp file there ("Permission denied (os error 13)").
  **Now:** the write goes through `carjack-wireless-helper wifi-profile <name>` (body on stdin),
  which carries `cap_dac_override+ep`. That bypasses the directory mode and the mask both, and
  leaves a root-owned 0600 file iwd reads with no ACL at all - so `exacl` is gone. The sidecar
  takes the file *name* only, deriving the directory from iwd's own `Daemon.GetInfo` and checking
  the name against iwd's profile grammar, so a `CAP_DAC_OVERRIDE` binary is not an
  arbitrary-file-write primitive for any local user.
- **Superseded again (2026-08-24): the capability is gone; the grant is a group.** Carrying
  `CAP_DAC_OVERRIDE` to reach one directory is a write to every file on the system, and under
  `DynamicUser=` it was not even sufficient - that implies `ProtectSystem=strict`, so `/var` is
  mounted read-only for the service and the write is refused before DAC is ever consulted.
  **Now:** `iwd-carjack-state.conf` (shipped by the deb into `/usr/lib/systemd/system/iwd.service.d/`)
  gives `iwd.service` `Group=iwd-provision` and `StateDirectoryMode=0770`, so systemd hands
  `/var/lib/iwd` to that group on every iwd start; `carjack.service` joins it with
  `SupplementaryGroups=`, adds `ReadWritePaths=/var/lib/iwd` for the mount, and now runs with an
  empty `CapabilityBoundingSet=`. `write_profile` writes 0640 and `fchown`s only the *group*, taken
  from the directory itself - setting a group the process is already in needs no privilege, which
  is what makes the bullet above (iwd reading by ordinary permissions) work without root ownership.
  The group has to exist before iwd starts or the unit fails, so it is created in three places: the
  deb's `debian/postinst`, `just install-carjack-service`, and the image build's
  `config-carjack.conf`.
- **The HCI sidecar absorbed it** rather than a second binary being added:
  `src/bin/carjack-hci-helper.rs` is now `src/bin/carjack-wireless-helper.rs`, with the old
  behaviour under a `device-class` subcommand, and holds both capabilities
  (`cap_net_admin,cap_dac_override+ep`). One thing to grant, one to deploy.

Verified against a live host iwd: `iwd_is_configured_for_carjack`, `live_connection_...` and
`iwd_reads_back_the_accessory_provisioning_file` (writes a throwaway profile, confirms iwd registered
it with `Hidden=true`/`AutoConnect=false`/type `psk`, then `Forget()`s it) all pass. Then
confirmed against the bench unit - see below. Note the live iwd tests must run **on the host**,
not under `toolbox run`: inside the container iwd's D-Bus answers `AccessDenied` even though
`wheel` is present in both, so `just test-bin` will always fail them.

`/etc/sudoers.d/carjack-setcap` matches its arguments literally, so it has to name the binary and
the exact capability list. Every comma must be backslash-escaped or sudoers reads it as the
`Cmnd_List` separator and rejects the next capability as "expected a fully-qualified path name";
double quotes do not help. `visudo -c -f <file>` catches it before installing.

## Video works end to end on the bench unit (2026-08-07)

**3249 frames at 30.0 fps over 108s, zero session failures**, stopped only by `just test-run`'s
timeout: Bluetooth → iAP2 → WiFi provisioning → AirPlay → zero-copy VA-API H264. First keyframe
10.4 ms after the screen SETUP, 0 delta frames skipped. The iAP2 `Connection reset by peer` at
the WiFi handover happened as always and the session **carried on through it** - the first run
where that held.

Three separate bugs were in the way, none of them WiFi:

- **A dmabuf backend could be selected for a GPU it cannot import from.** `matches_driver` was
  only a *sort key* in `try_zero_copy`, never a filter, so on a hybrid machine `vaapi` was chosen
  for an NVIDIA render node - its elements exist because of the *other* GPU, and `available()`
  only checks that elements exist. `Import::Cuda` reports failure up front and falls through;
  `Import::DmaBuf` has no such check, so the pipeline built and then died mid-session on
  `caps ... not accepted`, taking the CarPlay session with it. Now `can_import_from()` filters it
  out, and `zerocopy=true` correctly errors instead of picking something doomed.
- **`auto` preferred the one GPU this build cannot encode on.** `driver_rank` ranks nvidia first
  "by hardware-encode capability", but the nvidia row is `#[cfg(feature = "cuda")]` - on a
  non-cuda build there is no NVIDIA encode path at all, so `auto` guaranteed software encode
  while an Intel GPU able to do full zero-copy sat idle. `pick_auto` now sorts on
  `driver_has_zero_copy_backend` first and keeps `driver_rank` as the tiebreak. Non-cuda builds
  land on renderD128/i915 + vaapi; `cuda=true` still lands on renderD129/nvidia + nvh264enc.
- **The compositor allocated a modifier the encoder cannot import.** It handed GBM all six
  renderable Argb8888 modifiers and GBM picked the GPU's favourite, Tile4
  (`0x0100000000000008`); `vapostproc` lists `AR24` with `I915_FORMAT_MOD_Y_TILED`
  (`0x0100000000000002`) and nothing else. Negotiation therefore failed only once frames flowed
  (`not-negotiated (-4)`, surfacing as appsrc "Internal data stream error"), never at link time.
  `encode::importable_modifiers()` now reads the postproc's sink pad and the compositor
  intersects before allocating: "6 renderable ... 1 of them importable by the encoder".

Also fixed, because it made all of the above harder to read: the session `select!` loops (app and
desktop paths) were **unbiased**, and the encoder ending makes both its arm and `video`'s ready at
once - so identical faults reported either the real GStreamer error or "encoder stopped before
producing a keyframe", at random. Both loops are now `biased;`.

`tests/compositor.rs` drives compositor → encode with **no accessory** and reproduces the whole encode
path in ~3s; it is the right first stop for anything in this area. `GST_DEBUG=*:2` alone hid the
cause - the caps detail needs `GST_DEBUG=*:2,GST_CAPS:5`.

## What works

Verified against the CFMOTO bike unless noted.

- **Bluetooth → iAP2 handshake, end to end, in about a second.** iAP1 switch → link SYN →
  identification → the Apple authentication chain (`AA00`/`AA06`/`AA02`/`AA03`/`AA05`) →
  `4300`/`4301`, at which point the accessory hands over its WiFi credentials and the address of its
  AirPlay receiver. Confirmed clean in `cfmoto_11.pcapng`.
- **WiFi association and DHCP** on the accessory's AP - via NetworkManager up to 2026-08-07, via iwd
  since; the iwd path has not yet met an accessory (see below).
- **AirPlay video and touch**, on the bench unit: the accessory's display is learned from `/info`, the
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
unit-tested, all building, none yet run against the accessory - that is the next thing to do.** Details
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
   assertion and the accessory answered RECORD; `cfmoto_13` and both `cfmoto_14` sessions delayed it and
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

## Housekeeping landed 2026-08-01 (unrun against an accessory)

Not aimed at the deauth; all three are things that were making iteration harder.

- **NetworkManager now goes through `nm-automata`**, our own bindings generated in full from
  NetworkManager's spec, replacing the hand-written `nmrs`. The accessory's profile is a typed
  `ConnectionProfile` rather than a settings map with two properties patched in by string key -
  and the values are typed too, not strings spelled by hand: `WirelessMode::Infrastructure`,
  `WirelessBand::A`, `WirelessSecurityKeyMgmt::WpaPsk`, `AssignedMacAddress::Permanent`,
  `Ipv4Method::Auto`, `Ipv6Method::LinkLocal`, and an `Ssid` (bytes, as an access point actually
  names itself) that the current-network comparison goes through instead of `String::from_utf8`.
  A property whose object is absent now reads as `None` rather than a proxy for `/`, so
  `current_ssid` no longer compares a path against a literal and `accessory_wifi` says plainly that the
  activation has no IPv4 configuration yet - which is what the stray `IP4Config ... path "/"`
  warning in the logs was.
  Three `#[ignore]` tests in `wifi_connect` check it against the running daemon: the read path
  (`live_connection`, the permanent MAC) and a round-trip of the accessory's profile through
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
  `auto`, i.e. waiting on SLAAC - and the accessory answers no router solicitation, in `cfmoto_12` or
  `cfmoto_16` (only our own RSes appear, retried into silence). **The flag is sufficient and the
  DAD watcher is now deleted**: over every session that logged it, `wait_for_link_local` returned
  on its first pass 660-810 µs after `IP6_READY` - i.e. it never waited for anything.
  `wait_for_link_local`, its tests and the `rtnetlink` dependency are gone.
- **The HCI sidecar is no longer invoked**, and `just build` no longer setcaps it (`just setcap`
  does it by hand). Neither the CarPlay EIR UUID nor the iPhone-like class of device was ever
  shown to be required, and granting `cap_net_admin` on every build prompted through polkit,
  which broke non-interactive runs. `src/bin/carjack-hci-helper.rs` is kept, unused.

## Next move

0. **Retry the bike.** The bench now streams video end to end, and the three encode bugs fixed to
   get there (wrong-GPU backend, `auto` picking a GPU with no encode path, the modifier mismatch)
   all applied to the bike too - every run against it so far encoded on a path that could not
   negotiate. The bike's own blocker below may or may not survive that.
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
   accessory's display, and (now) a warning every second the keyframe gate waits; that output answers
   in one line what a capture cannot answer at all. Note which `CARJACK.SOURCE` was used.
1. **Run against the bike and capture** (`just test-run`, with a BT+WiFi capture as in
   `cfmoto_12`). In order, confirm:
   - the iAP2 handshake completes at all - `cfmoto_13` had none;
   - AirPlay reaches RECORD (as in `cfmoto_12`);
   - **video bytes appear on the screen socket within milliseconds of RECORD** - that is the
     whole point of the fix, and the thing `cfmoto_12` lacked;
   - the accessory does *not* send RFCOMM DISC ~2 s later.
2. **Then** the video path proper: whether the accessory requests keyframes, whether backpressure
   fires, and whether the picture holds up. Frame orientation is the first thing to eyeball.
3. **Also retest the bench unit** - the RECORD gate is on the shared `run_video_stream` path, so
   it affects the AUTO BOX too.

## Open questions

- **Frame orientation is unverified.** GL renders bottom-up and the readback orientation has
  never been checked against content with a clear top/bottom. A one-line output-transform fix if
  the accessory shows an upside-down image.
- **Multitouch is correct by construction but unvalidated** end-to-end against a real accessory, and
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
  association with the accessory's AP is flaky. Check that first on any new dev machine.
