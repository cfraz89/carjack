# Capture inventory

Packet captures live in the repo root. What each one shows, newest first, so a name mentioned in
[investigation-log.md](investigation-log.md) can be looked up.

Two head units appear throughout:

- **the bike** - the CFMOTO motorcycle head unit, the real target;
- **the AUTO BOX** - a standalone bench unit, which speaks an older dialect (flat `0x5703` WiFi
  configuration rather than the grouped `0x4301`, and no AirPlay address, so it needs the mDNS
  discovery path).

## carjack ↔ bike

| Capture | Date | What it shows |
| --- | --- | --- |
| `cfmoto.pcapng` (+ `/tmp/carjack.btsnoop`) | 2026-08-03 | **The bike does no Bonjour at all.** First bike run of the invite path. carjack advertised `_carplay-ctrl._tcp` with the Bluetooth address as `id`, once a second for 53 s across two iAP2 sessions; no invite came. The bike's whole IP presence is three packets - DHCP offer, DHCP ack, and one ARP for us at t=22.07 that led nowhere - with no mDNS in either direction and no TCP at all. It repeats `0x4301` every ~3 s instead, byte-identical to what the iPhone gets in `iphone-cfmoto.pklg`, where the phone's iAP2 ends 3 s later at the handoff. Also: **the association survived the entire run** (no deauth cycling), which retires both the `/info`-timeout evidence and, for now, the 11 s deauth loop. |
| `cfmoto_16.pcapng` (+ `/tmp/iw-events.log`) | 2026-07-31 | **Proves the head unit sends the deauth, and pins the trigger.** `iw event` alongside the run: `deauth 92:83:8e:ef:a7:d3 -> fe:b2:9f:ed:ea:72 reason 2`, `disconnected (by AP)`. Ran with `disable_11ac=1` + `power_scheme=1` active and behaved identically, killing the local-driver theories. Best bike run so far and the first on the in-memory WiFi profile: fresh association on a new subnet, full AirPlay bootstrap, **SETUP answered (314 -> 225)**, event socket and screen data socket both established - then the AP deauths **77 ms after the screen socket's three-way handshake**, before RECORD, with no data ever sent on it. `cfmoto_12` has the identical ordering. |
| `cfmoto_15.pcapng` (+ `cfmoto_2.log`) | 2026-07-31 | **The AirPlay session dies because the WiFi association does, not because of anything CarPlay.** Desktop mode. The SETUP-timing fix holds - screen SETUP goes out **12 ms** after the modes response - but the accessory never even TCP-ACKs it: the segment is retransmitted four times into silence, and the accessory's last packet in either direction is a UDP timing reply 12 ms later. The journal explains it: `iwlwifi: Unhandled alg: 0x707` (an RX hardware-decrypt status error) 31 ms after the modes exchange, then mac80211 deauthing with `PREV_AUTH_NOT_VALID`. Cross-checking the journal against `cfmoto_12`/`13`/`14` shows **every** bike run ends in a reason-2 deauth at that same point - `cfmoto_12` merely finished RECORD microseconds before it. Also caught carjack reusing a 46s-old association from a leftover autoconnecting NM profile. |
| `cfmoto_14.pcapng` (+ `cfmoto.log`, `cfmoto_stderr.log`) | 2026-07-27 | **The first run with carjack's own logs**, which is what made it diagnosable. Accessory is 800x1280 @ 30fps. Session 1 lost 4.3s to a cold GStreamer registry inside the session (first keyframe 4.92s); session 2 got one in 296ms and *still* never sent SETUP - the accessory's last packet is the `385→182` modes exchange at t=71.027, after which its AP is gone (unanswered NS, DHCP Discovers). Also caught carjack accepting an inbound page from the **bench AUTO BOX** 58s after the bike's link failed, and streaming to it. Three fixes came from this one. |
| `cfmoto_13.pcapng` | 2026-07-27 | **Not a clean retest.** Contains **no iAP2 at all** - only HFP (DLCI `0x1a`), zero `FF 55`/`FF 5A` - so this run never did the CarPlay handshake with the bike (aggressive-reconnect state, and/or `.env` pointing at another unit). AirPlay still reaches `assert_modes` at t=20.970, then stops dead at the new `wait_for_first_keyframe` gate: no screen SETUP, no RECORD, screen socket never created. Accessory drops BT 2.3s later. Proves the ~2s watchdog is not RECORD-specific, and that it disproved the `cfmoto_12` warm-up theory. |
| `cfmoto_12.pcapng` | 2026-07-27 | BT + WiFi together, and **the first capture containing any TCP**. The full AirPlay session establishes - pair-setup, pair-verify, screen SETUP, UDP timing, RECORD 200 OK at t=62.011 - and then carjack pushes no video, so the accessory tears everything down (RFCOMM DISC t=63.992, AP gone until t=75.9). First frame was ready at t=64.118, ~100 ms too late. The capture the RECORD-gate fix was written against. |
| `cfmoto_11.pcapng` | 2026-07-26 | BT + WiFi together. The iAP2 handshake is completely clean in ~1s and the accessory hands over credentials; the AirPlay connect then dies in a race with IPv6 duplicate-address detection. **No TCP in the whole capture.** The capture the current fixes were written against. |
| `cfmoto_10.pcapng` | 2026-07-26 | BT + WiFi together. Sequence-number fix confirmed working. Shows the WiFi standoff: the accessory ARPs us but never sends the `ctrl-int/1/connect` invite, and our gateway fallback aimed at `.1` when the head unit was its own gateway at `.85`. |
| `cfmoto_9.pcapng` + `cfmoto_wifi_9.pcapng` | 2026-07-26 | Packet-length fix landed, but keepalives were still *consuming* a sequence number, deadlocking the link. Two consecutive runs, not one synchronised recording. |
| `cfmoto_8.pcapng` | 2026-07-26 | Auth and the WiFi handoff both work. The keepalive packet declared the wrong length and destroyed the accessory's framing. |
| `cfmoto_7.pcapng` | 2026-07-26 | The passive-connection and session-ID fixes work on the bike. |
| `cfmoto_6.pcapng` | 2026-07-26 | carjack's outbound page *succeeded*, so the accessory never paged us; it opened ch1 and disconnected without `FF 55`. Zero ch1 bytes - proof the head unit requires an accessory-originated ACL, not a session-ID regression. |
| `cfmoto_5.pcapng` | 2026-07-26 | First capture where iAP2 actually starts: `FF 55` echo and the full link SYN succeed, then `StartIdentification` goes out on the wrong session id (1 instead of 2) and is ACKed-and-dropped. |
| `cfmoto_4.pcapng`, `~/Documents/carjack/cfmoto.pcapng` | 2026-07-21/25 | Earlier bike attempts. The repo-root `cfmoto.pcapng` is a *later* run, above - the July one only survives under `~/Documents/carjack/`. |
| `mycar.pcapng` | 2026-07-21 | HFP + A2DP succeed but iAP2 never starts - no `FF 55`/`FF 5A`. Basis for the (later disproved) class-of-device filter hypothesis. |
| `mycar_2/3.pcapng` | 2026-07-24 | In `~/Documents/carjack/`. Before/after the connect-out change. |
| `mycar_4.pcapng` | 2026-07-25 | carjack was connecting to the **wrong device**. Led to removing scan-and-score selection in favour of a configured target. |

## carjack ↔ bench unit

| Capture | Date | What it shows |
| --- | --- | --- |
| `bench_4.pcapng` (+ `bench_4.log`) | 2026-07-28 | **The head unit falls back to Android Auto when carjack's iAP SDP record is missing.** It queried `...decacafe` at t=1.665 and t=4.945 and got `Attribute Lists [count = 0]` both times, then spoke AA framing at us (`00 08 00 04 08 01`), which arrives as `UnexpectedBytes("Unexpected switch response")`. The record blinks in and out because the profiles were registered per session; `cfmoto_14` answered `count = 1` every time and worked. Drove hoisting registration to process lifetime (`IapHost`). |
| `bench_3.pcapng` (+ `bench.log`) | 2026-07-28 | Why committing to one channel loses. carjack accepted a **stale** channel (`profile=accessory`, dlci `0x02`, left from before it started), blocked 3s, then sat ~38s in the recovery path's `device.connect()`. Meanwhile the head unit opened a fresh channel (dlci `0x03`, into our device profile) at t=20.45 and sent `ff550200ee10` **21 times, once a second, t=21.5..41.1**, all unread. carjack finally echoed one at t=52.3, 11s after the head unit gave up - hence `Skipping 113 byte(s) before valid magic` and repeated `Timeout waiting for SYN`. Drove the concurrent-accept rewrite. |
| `bench_2.pcapng` | 2026-07-27 | The AUTO BOX regressed again, this time from the link-layer rewrite. The SYN handshake never completes: our SYN+ACK claims `ack = 0` instead of the head unit's SYN sequence `0xa8`, and the third leg (a bare ack) was being swallowed before the handshake could see it. The head unit retransmits its SYN, then falls back to sending `FF 55` every ~1s. |
| `bench_1.pcapng` | 2026-07-26 | The AUTO BOX regressed; the cause was the bike-targeted fix. Why both dialects have to keep working. |

### The two units negotiate very different link parameters

From their SYNs. Worth keeping in view - anything that assumes one unit's numbers will break on
the other, which is what `bench_2.pcapng` is a record of.

| | CFMOTO (bike) | AUTO BOX (bench) |
| --- | --- | --- |
| `max_outstanding` | **1** | **127** |
| `max_length` | 4096 | 65535 |
| `retx_timeout` | 1000 ms | 2000 ms |
| `cumack_timeout` | 500 ms | 1000 ms |
| `max_retx` | 15 | 30 |
| `max_cumack` | **0** (no limit) | **6** |
| control session id | **2** | **1** |

## Reference recordings - a real iPhone doing working CarPlay

These are the ground truth. When behaviour is in doubt, the answer is usually in one of these.

| Capture | What it shows |
| --- | --- |
| `logs/cfmoto-iphone.pcapng` | iPhone ↔ **this bike**, WiFi capture, 2026-08-05, working CarPlay session. **The reference for the AirPlay side.** Opens `POST /pair-verify` as the very first control request (stored HomeKit pairing - no `/pair-setup`, **no unauthenticated `/setup`**), then everything is encrypted. Three TCP channels (control `:7000`, event, screen-data), streams, clean two-way FIN teardown at t≈74 - no deauth. **Zero CarPlay mDNS anywhere** (no `_carplay-ctrl`): the phone connects straight out to the receiver the bike named over iAP2 (`fe80::9083:…:7000`), `_airplay`/`_raop` come from an ambient MacBook. Basis for: carjack's direct-connect (no mDNS) and pair-verify-first on reconnect. |
| `iphone-first-pair.pklg` | iPhone ↔ the **bench AUTO BOX**, 2026-08-02, first pairing. **The reference for what a fresh bond looks like, and it is not fast:** bonding completes at t=98.961 (`Authentication Complete`) and iAP2 does not start until t=122.471 - **23.5s**, against carjack's 7-19s. The phone opens the RFCOMM mux and connects HFP outbound within 100ms of bonding (`Sent SABM Channel=0`, then `Channel=2 (Hands-Free)`), then **refuses the head unit's iAP channel 8 times over 20s** with RFCOMM `DM` (t=99.370, 101.626, 103.901, 107.181, 110.409, 113.776, 117.094, 119.234), the head unit re-running its SDP search for the iAP UUID before each attempt. At t=119.437 the phone **tears the RFCOMM session down itself** (L2CAP Disconnection Request, PSM 0x0003); the head unit rebuilds the mux at t=121.603, the phone `UA`s ch 1 at t=121.637, and `ff 55 02 00 EE 10` arrives 0.8s later. So the dead first channel is what this unit does to everyone - the phone's answer is to refuse until ready, keeping the head unit in its 2-3s retry loop, where carjack accepts and has to cycle the ACL instead. |
| `iphone-cfmoto.pklg` | iPhone ↔ **this bike**, working. The byte-level reference for the control session id (`02`) and the post-identification auth chain. **Also the reference for connection direction** (checked 2026-07-28): the phone opens an outbound iAP channel to the bike's *accessory* service at t=281.789 (`Sent SABM Channel=2` -> `Rcvd UA`) and **never sends a byte on it**; at that same moment it `DM`-rejects the bike's attempt on its own device service. The session that works is at t=419.341 - the bike opens ch 1 into the phone's *device* service, the phone `UA`s it, and `FF 55` follows at t=425.385. So outbound iAP is establishable but inert, and the phone refuses some inbound attempts before accepting one. |
| `iphonemycarcarplay.pklg` | iPhone ↔ the same bike, 2026-07-25. Shows the accessory opening iAP ch1 inbound and sending `FF 55` first, and the iPhone's HFP identity answers. (The note that the iPhone's outbound dial was *rejected* was wrong - in `iphone-cfmoto.pklg` the phone's outbound page succeeds and simply leads nowhere.) |
| `iap.pcapng`, `bluetoothd-hci-latest.pklg` | Earlier iPhone HCI captures with the full iAP2 exchange up to WiFi config. |

### What the iPhone references established

- **The control session id is whatever the accessory's SYN says** - `02` on this bike. Messages
  sent on any other id are link-ACKed and then silently dropped.
- **Acknowledgements repeat the last data packet's sequence number** rather than taking one of
  their own. The phone sends data as seq `0x47` and then acknowledges two separate messages with
  `0x47` again; the accessory does the same with `0x78` before moving to `0x79`.
- **The iPhone answers neither repeat of `0x4301`** - it just bare-acks. Its iAP2 traffic simply
  *ends* at `4300`→`4301`. So the repeat is normal, not a fault, and `ea00` (which the iPhone
  never sends at all) should not be sent per repeat.
- **Two complete identification + auth + WiFi-config cycles** happen over Bluetooth before
  AirPlay. Afterwards the iPhone sends `WirelessCarPlayUpdate`, `DeviceLanguageUpdate` and
  `NowPlayingUpdate`.
- **Zero EA session messages over Bluetooth** in the captured window - EA establishment happens
  as part of the AirPlay handshake, not over BT.

## Reading them

`tshark` is the tool. Useful starting points:

```sh
# Protocol breakdown, and whether there is any TCP at all
tshark -r cfmoto_11.pcapng -q -z io,phs

# The iAP2 exchange with direction, as hex
tshark -r cfmoto_11.pcapng -Y 'btrfcomm && data.data' \
  -T fields -e frame.number -e frame.time_relative \
  -e bthci_acl.src.bd_addr -e data.data

# The WiFi half of a combined capture
tshark -r cfmoto_11.pcapng -Y eth \
  -T fields -e frame.time_relative -e _ws.col.Info
```

`iap2.lua` in the repo root is a Wireshark dissector for the iAP2 layer.
