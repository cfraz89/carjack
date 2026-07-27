# Where things stand

**Last updated: 2026-07-27.** This is the living document - if it disagrees with
[investigation-log.md](investigation-log.md), this one wins. Durable facts about how the code
works belong in `CLAUDE.md`, not here.

## The goal

A wireless CarPlay bridge that impersonates an iPhone to a car head unit: Bluetooth iAP2 to
authenticate and get handed the car's WiFi credentials, then CarPlay video over that WiFi via
AirPlay.

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

## What is done but not yet retested against the bike

Three changes landed on 2026-07-27 and have unit/live-host coverage but no capture against the
car yet. **This is the next thing to do.**

1. **The AirPlay connect no longer races duplicate-address detection.** `cfmoto_11` failed
   because we connected to the car's link-local address ~1s before our own link-local finished
   DAD; `connect()` failed instantly with `EADDRNOTAVAIL` and took the whole Bluetooth session
   down with it. Now: wait for a non-tentative link-local (rtnetlink), fall back to the IPv4
   gateway, and retry rather than tearing down.
2. **We no longer reconnect to an AP we are already on**, which is what generated the fresh
   link-local that opened that DAD window in the first place.
3. **The link layer honours what the SYN negotiates** - send window, retransmission, cumulative
   acks, packet size - and the car's keyframe requests now actually reach the encoder. Details
   in `CLAUDE.md`; the nine-item review that produced it is the top entry of the log.

**Watch for:** the handshake is now lock-step (`max_outstanding = 1`) where it used to send
three packets at once. If the car behaves *worse* than `cfmoto_11`, relaxing
`LinkParams::max_outstanding` is the first thing to try.

## Next move

1. **Run against the bike and capture** (`just test-run`, with a BT+WiFi capture as in
   `cfmoto_11`). In order, confirm:
   - the iAP2 handshake still completes - the link-layer changes are the regression risk;
   - a TCP SYN to the car's AirPlay endpoint actually appears (there has never been one);
   - if it goes to the link-local address, that a neighbour solicitation precedes it.
2. **If AirPlay connects**, the open question is whether it completes without a HomeKit pairing
   id. We pass `remote_homekit_id: None` on the `0x4301` path, where the mDNS ctrl-probe path
   would have supplied one from the Bonjour record for a pair-verify fast path. If the car
   rejects the connection, look there first.
3. **Then** the video path proper: whether the car requests keyframes, whether backpressure
   fires, and whether the picture holds up.

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
