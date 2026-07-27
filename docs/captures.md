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
| `cfmoto_11.pcapng` | 2026-07-26 | BT + WiFi together. The iAP2 handshake is completely clean in ~1s and the car hands over credentials; the AirPlay connect then dies in a race with IPv6 duplicate-address detection. **No TCP in the whole capture.** The capture the current fixes were written against. |
| `cfmoto_10.pcapng` | 2026-07-26 | BT + WiFi together. Sequence-number fix confirmed working. Shows the WiFi standoff: the car ARPs us but never sends the `ctrl-int/1/connect` invite, and our gateway fallback aimed at `.1` when the unit was its own gateway at `.85`. |
| `cfmoto_9.pcapng` + `cfmoto_wifi_9.pcapng` | 2026-07-26 | Packet-length fix landed, but keepalives were still *consuming* a sequence number, deadlocking the link. Two consecutive runs, not one synchronised recording. |
| `cfmoto_8.pcapng` | 2026-07-26 | Auth and the WiFi handoff both work. The keepalive packet declared the wrong length and destroyed the accessory's framing. |
| `cfmoto_7.pcapng` | 2026-07-26 | The passive-connection and session-ID fixes work on the bike. |
| `cfmoto_6.pcapng` | 2026-07-26 | carjack's outbound page *succeeded*, so the car never paged us; it opened ch1 and disconnected without `FF 55`. Zero ch1 bytes - proof the unit requires a car-originated ACL, not a session-ID regression. |
| `cfmoto_5.pcapng` | 2026-07-26 | First capture where iAP2 actually starts: `FF 55` echo and the full link SYN succeed, then `StartIdentification` goes out on the wrong session id (1 instead of 2) and is ACKed-and-dropped. |
| `cfmoto_4.pcapng`, `cfmoto.pcapng` | 2026-07-21/25 | Earlier bike attempts. |
| `mycar.pcapng` | 2026-07-21 | HFP + A2DP succeed but iAP2 never starts - no `FF 55`/`FF 5A`. Basis for the (later disproved) class-of-device filter hypothesis. |
| `mycar_2/3.pcapng` | 2026-07-24 | In `~/Documents/carjack/`. Before/after the connect-out change. |
| `mycar_4.pcapng` | 2026-07-25 | carjack was connecting to the **wrong device**. Led to removing scan-and-score selection in favour of a configured target. |

## carjack ↔ bench unit

| Capture | Date | What it shows |
| --- | --- | --- |
| `bench_2.pcapng` | 2026-07-27 | The AUTO BOX regressed again, this time from the link-layer rewrite. The SYN handshake never completes: our SYN+ACK claims `ack = 0` instead of the unit's SYN sequence `0xa8`, and the third leg (a bare ack) was being swallowed before the handshake could see it. The unit retransmits its SYN, then falls back to sending `FF 55` every ~1s. |
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
| `iphone-cfmoto.pklg` | iPhone ↔ **this bike**, working. The byte-level reference for the control session id (`02`) and the post-identification auth chain. |
| `iphonemycarcarplay.pklg` | iPhone ↔ the same bike, 2026-07-25. Shows the car opening iAP ch1 inbound and sending `FF 55` first, the iPhone's outbound dial being rejected, and the iPhone's HFP identity answers. |
| `iap.pcapng`, `bluetoothd-hci-latest.pklg` | Earlier iPhone HCI captures with the full iAP2 exchange up to WiFi config. |

### What the iPhone references established

- **The control session id is whatever the accessory's SYN says** - `02` on this bike. Messages
  sent on any other id are link-ACKed and then silently dropped.
- **Acknowledgements repeat the last data packet's sequence number** rather than taking one of
  their own. The phone sends data as seq `0x47` and then acknowledges two separate messages with
  `0x47` again; the car does the same with `0x78` before moving to `0x79`.
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
