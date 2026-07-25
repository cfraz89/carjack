# CarJack — Wireless CarPlay Bridge

## Objective
Complete a wireless CarPlay bridge (carjack) that impersonates iPhone + accessory to connect to car head units via Bluetooth iAP2, then stream CarPlay video over the car's WiFi.

## Important Details
- **HE/VHT mismatch fixed**: iwlwifi module loads with `disable_11ax=1` via `/etc/modprobe.d/iwlwifi-disable-11ax.conf`. Link now uses VHT-MCS.
- WiFi connects successfully via NetworkManager (SSID=carplay_wifi_XXXX, passphrase=REDACTED).
- Use `just build` / `just run` (not bare cargo).
- Only NetworkManager (`nmrs`) used for WiFi — no `nl80211.rs` or `iw` fallback.
- Two Bluetooth profiles: `IAP_ACCESSORY_UUID` (Role::Client, auto_connect=false) + `IAP_DEVICE_UUID` (Role::Server, channel=1). `connect_profile(&IAP_ACCESSORY_UUID)` triggers connection.
- iAP2 magic bytes are `0xFF 0x5A`; iAP1 switch command starts with `FF 55 02 00 EE 10`.
- AirPlay port is TCP 7000 on the car (192.168.43.1).
- iPhone pklg capture shows TWO complete identification+auth+WiFi config cycles over Bluetooth before AirPlay. After WiFi config exchange, iPhone sends `WirelessCarPlayUpdate`, `DeviceLanguageUpdate`, `NowPlayingUpdate` — the EA session start (`0xEA00`) does **not** appear in the captured window.
- The car waits ~8 seconds after AirPlay TCP connects, then retransmits the `FF 55` switch command every ~1 second over Bluetooth to restart iAP2. This suggests the car is timing out waiting for a specific iAP2 message or video data state to begin AirPlay RTSP handshake.

## Work State
### Completed
- **HE/VHT driver fix applied**: `disable_11ax=1` via modprobe config.
- **Cleaned up nl80211 + iw fallback**: `nl80211.rs`, `libc` dep, `mod nl80211` removed; only NM used.
- **WiFi connection verified**: connects successfully to car AP with NM.
- **iAP2 codec buffer clearing fixed**: Changed from clearing entire buffer on non-`FF 5A` bytes to byte-at-a-time advance searching for valid magic, preserving any genuine iAP2 packets interleaved with noise. New log shows "Skipping 6 byte(s) before valid magic".
- **AirPlay RTSP client rewrite**: Previously acted as RTSP server (waiting for car to send ANNOUNCE). Now acts as RTSP client — sends ANNOUNCE (with SDP) → SETUP → PLAY → interleaved RTP video. This is the correct role for iPhone/CarPlay.
- **Video pipeline updated**: Changed from raw H264 AVC output to RTP packets via `rtph264pay pt=96`. The car expects RTP over interleaved TCP, not raw AVC.
- **Dead EA video channel removed**: Removed vestigial `video_rx`/`video_tx` channel from `CarplaySession`, `wireless_carplay`, and `main.rs`. Removed `send_video_frame` method. `CarplaySession::run()` now loops directly on `receive_packet()` without `select!`.
- **iPhone pklg capture re-analyzed**: Found 62 iAP2 `FF 5A` packets and 4 `FF 55` switch commands. Extracted full protocol exchange: SYN→StartIdentification→IdentificationInformation→Auth→WiFiConfig→WirelessCarPlayUpdate. **Zero EA session messages** found — EA establishment likely happens over WiFi as part of AirPlay handshake, not Bluetooth.

### Active
- **iAP2 never starts on the MYCAR bike — identity/CoD filter suspected** (from `mycar.pcapng`,
  2026-07-21, carjack↔MYCAR-1234, vs `bluetoothd-hci-latest.pklg` iPhone reference):
  - carjack's whole capture has **zero `FF 5A` (iAP2) and zero `FF 55` (switch)** bytes. The iPhone
    ref has 61 and 12. carjack fails *before* iAP2 — earlier than the RTSP/AirPlay theory below.
  - The bike **fully sets up HFP (RFCOMM ch 13, AT commands) and A2DP/AVRCP** with carjack, but on
    the iAP RFCOMM channel it opens, exchanges credits, sits idle, and sends **DISC ~117 ms later**
    without ever sending `FF 55 02 00 EE 10`. Generic BT profiles work; the Apple/CarPlay path is
    refused → classic device-fingerprint gate.
  - In the iPhone ref: iPhone *opens* ch 1 (Sent SABM, "Serial Port"), car then sends `FF 55` (~580
    ms after credits) and iPhone **echoes the same 6 bytes** → iAP2 begins. carjack never gets there.
  - **Class of Device: RULED OUT.** `/etc/bluetooth/main.conf` already sets `Class = 0x7a020c`;
    bluetoothd live-advertises `0x007c020c` = Phone/Smartphone (iPhone-shaped) and the bike still
    refused. bluetoothd owns the class, so the sidecar's raw `SET_DEV_CLASS` is redundant/fighting
    it. (Also: mgmt `SET_DEV_CLASS` takes 2 bytes `[major,minor]`, not the 3-byte CoD - the service
    byte is kernel-derived from the enabled UUIDs. Sidecar constant left as `[0x02,0x0c]`, harmless.)
  - Ruled out: Apple vendor DID (0x004c) spoofed correctly in EIR; both iAP2 UUIDs (`…cafe`/`…caff`)
    present (iPhone has both).
  - **RFCOMM role/direction: FIXED, but not sufficient** (from `mycar_2.pcapng`/`mycar_3.pcapng`,
    2026-07-24, in `~/Documents/carjack/`):
    - `mycar_2` = old behavior: car connects IN, only HFP (ch13); carjack never opens iAP.
    - `mycar_3` = after the connect-out fix: carjack now **connects OUT** to the car's iAP service —
      correct target: car advertises it as **"RFCOMM-SPP", UUID `…caff`, RFCOMM ch 2** (both sides
      also share the CarPlay vendor UUIDs `03b80e5a…` and `00005005…0002ee000001`). Mux + PN + SABM
      ch2 + MSC + credits all succeed.
    - **New blocker:** the car then sends **DISC ~26 ms after credits** (9 ms after its own credit
      grant), with **zero iAP bytes from either side** — no `FF 55`. Then it falls back to HFP-only
      for the rest of the session; no iAP retry.
  - **Ruled out this round:** car never SDP-queries carjack (not an SDP-record rejection); CoD/
    vendor/UUIDs all match. carjack sends **no iAP bytes at all** — its iAP2 stack waits to *receive*
    rather than sending a Link SYN / `FF 55` on connect. The reference `.pklg` is a **different head
    unit** (TESTUNIT-5678), where the car spoke first after 580 ms — MYCAR behaves differently, so we
    can't assume its first-speaker/timing from that capture.
  - **RESOLVED by the iPhone↔MYCAR capture** (`iphonemycarcarplay.pklg`, 2026-07-25) — a *working*
    CarPlay session on **this** bike. Three sessions in it; only the third gets iAP2:
    | | Session A (t≈275) | Session B (t≈394) | **Session C (t≈412)** |
    |---|---|---|---|
    | ACL initiated by | iPhone (`Sent Create Connection`) | iPhone (`Sent Create Connection`) | **CAR** (no Create Connection) |
    | HFP | iPhone opens ch10 | iPhone opens ch10 | **car opens ch8 (AG)** |
    | iAP ch1 | car opens, held 107 s | car opens, **DISC 62 ms** | car opens, **`FF 55` after 6 s** |
    | Result | no iAP2 | no iAP2 | **iAP2 + auth OK** |
    - **The car opens the iAP channel INBOUND (ch 1) and sends `FF 55 02 00 EE 10` FIRST; the iPhone
      echoes it 191 ms later.** So carjack's `perform_switch` (wait-then-echo) is **already correct** —
      it just has to be listening on the car's *inbound* connection.
    - **The iPhone's own outbound dial to the car's ch 2 was DISC'd by the car** (session A: Sent SABM
      ch2 @281.789 → car DISC @282.204) — *exactly* what happened to carjack in `mycar_3`. **The
      connect-out change was wrong**; it reproduced a failure the iPhone also hits. Revert to passive.
    - **The successful session is the one the CAR initiated end-to-end.** Both iPhone-initiated
      sessions failed to produce `FF 55`. → carjack must **stop dialing** (drop the 40× `connect_profile`
      retry loop) and wait, matching catplay's phone-side model (register profiles, `AutoConnect=true`,
      handle `NewConnection`).
    - A fast car-side DISC of ch1 is **not** proof of rejection: the iPhone got a 62 ms DISC in session
      B. carjack's 139 ms drop in `mycar.pcapng` is the same benign pattern.
  - **HFP identity: carjack answers `ERROR` where the iPhone identifies itself** (car asks over HFP):
    | Command | iPhone | carjack |
    |---|---|---|
    | `AT+CGMI` | `+CGMI: "APPLE INC."` | **ERROR** |
    | `AT+CGMM` | `+CGMM: IPHONE16,1` | **ERROR** |
    | `AT+COPS?` | `+COPS: 0,0,"BELONG"` | **ERROR** |
    | `AT+NREC=0` | `OK` | **ERROR** |
    | `AT+CCLK?` | `+CCLK: "26/07/25, 11:52:21"` | **ERROR** |
    | `AT+CNUM` | real MSISDN | `OK`, empty |
    Caveat on causality: in session A the iPhone answered `CGMI`/`CGMM` correctly and *still* got no
    `FF 55` in that session — so correct identity is plausibly **necessary but not sufficient**; the
    car-initiated connection looks like the actual trigger. Note these AT commands are answered by the
    host HFP-AG (oFono / PipeWire-WirePlumber), **not** by carjack's own code — see open question below.
  - **Side observations (not yet the cause):** carjack's LE Pairing Request to the car goes
    unanswered (BLE pairing stalls); carjack advertises a kitchen-sink UUID set (MAP/PBAP/OBEX-OPP/
    IrMC/Ranging/LE-audio, both HF roles) an iPhone doesn't.
  - **Open question:** who answers HFP AT commands on the host — oFono, or PipeWire/WirePlumber's
    bluez5 HFP-AG? Determines how to make the make/model answers iPhone-like (config vs. carjack
    owning the HFP-AG profile itself, which requires disabling the system one first). Not yet checked.
- **(Older, WiFi/video layer — only relevant once iAP2 succeeds)** RTSP role fixed: `airplay.rs` now
  acts as RTSP client (ANNOUNCE+SDP → SETUP → PLAY → interleaved RTP); encoder emits RTP via
  `rtph264pay` not raw AVC.

- **`mycar_4.pcapng` (2026-07-25): carjack was connecting to the WRONG DEVICE.** Capture has zero
  RFCOMM/SDP/BR-EDR ACL - only BLE. carjack issued `Create Connection` over and over to
  `aa:bb:cc:dd:ee:02` (and `aa:bb:cc:dd:ee:03`), each returning **Page Timeout (0x04)** after ~5.1 s,
  while the bike (`MYCAR-1234`, `aa:bb:cc:dd:ee:ff`, RSSI −59, class `0x240408`, advertising both
  `ec884348-…` and `…deca-caff`) sat discovered and ignored. Those targets are BLE-only random
  addresses - unpageable over BR/EDR. Cause chain in `src/wireless.rs`:
  1. `CARPLAY_DEVICE_NAME` is unset (it's commented out in `.env`), so `matches_carplay_device`
     returned `Err`;
  2. `is_candidate_accessory` did `.unwrap_or(true)` on that → **accepted any device**;
  3. `pair_available_accessory` scans `adapter.device_addresses()` (every device BlueZ has ever
     seen, including LE-scan junk), bypassing the BR/EDR+CarPlay-UUID discovery filter;
  4. `try_prepare_accessory` returns `true` unconditionally → committed to the first junk device.
  **Why the bike didn't just connect *to* us:** it has no bond with carjack - `aa:bb:cc:dd:ee:ff`
  isn't in BlueZ's device list at all. Head units only page devices they're already bonded to (the
  car-initiated session C in the iPhone capture was to an already-bonded iPhone). So carjack must
  land the first outbound connection + bond; only then does the inbound path become available.
  carjack was also in near-continuous inquiry+page (t=13→43 s) which starves page scan.
  **The identity filter alone is NOT enough:** the stale bonded entries *look* right. `TESTUNIT-5678`
  (`aa:bb:cc:dd:ee:02`, the old test rig, powered off) still reports class `0x00240418`, a cached
  RSSI of -61, **and** both `ec884348-…` and `…deca-caff`. Worse, `bluer::discover_devices` replays
  every known device as `DeviceAdded` *before* any genuine discovery hit, so both the pre-check loop
  and the discovery loop meet it first.
  **Resolved (2026-07-25) by removing auto-selection entirely.** The intermediate fixes (identity
  filter, reachability probe, live-sighting-only detection, RSSI ranking) are all gone - the target
  head unit is now **configured**, not guessed:
  - `[bluetooth] device` in `config.toml`, accepting a **MAC** (`AA:BB:CC:DD:EE:FF`) or an
    **advertised name** (`MYCAR-1234`). Env `CARJACK.BLUETOOTH.DEVICE`, flag `--bt-device`,
    normal precedence (flag > env > file). `Config::bluetooth_device()` returns `None` when
    unset/blank and `main` errors out at startup with a message naming all three ways to set it.
  - `wireless::find_target_device` replaces `pair_available_accessory` + `is_candidate_accessory` +
    `advertises_carplay` + `try_prepare_accessory`: match the configured device among BlueZ's known
    devices, else wait for it in a discovery aimed at that one target (`DISCOVERY_WAIT`, 60 s);
    everything else is ignored. `prepare_device` pairs (tolerating failure - BlueZ often bonds
    during the profile connect) and trusts.
  - The legacy bespoke `CARPLAY_DEVICE_NAME` env var is gone, folded into the config system.
  **Why it had to go - measured (2026-07-25):** BlueZ **never clears a stale RSSI**. The powered-off
  `TESTUNIT-5678` still reported `-61` after a full 12 s discovery pass and still listed the CarPlay
  UUIDs + a BR/EDR class, i.e. it was indistinguishable from a present device by any cached property,
  and `bluer::discover_devices` replays known addresses *ahead* of live hits so it was met first.
  (A live discovery *event* is a valid presence signal - that device emitted zero across a 15 s scan -
  but with an explicit target none of that inference is needed.)
  Note: the old commented `.env` line read `MYCAR-1234`, missing the `Y` - it would never have
  matched. `.env` now sets `CARJACK.BLUETOOTH.DEVICE=MYCAR-1234` (correct spelling), with the MAC
  as a commented alternative.

### Blocked
- Nothing blocked on captures any more - the iPhone↔MYCAR reference answered the open questions.

## Next Move
1. **Revert the connect-out change** in `src/wireless.rs`: drop the 40× `connect_profile(&IAP_ACCESSORY_UUID)`
   retry loop. Go passive like catplay's phone side - register the profiles (client profile with
   `AutoConnect=true`) and run the iAP2 session on the connection the **car** opens inbound. carjack's
   `perform_switch` already does the right thing (wait for `FF 55`, echo it) once it's on that socket.
2. Re-test and capture: expect `Rcvd SABM Channel=<n>` from the car, then `Rcvd` `FF 55 02 00 EE 10`,
   then carjack's echo, then `FF 5A` iAP2 traffic.
3. Make the connection **car-initiated** where possible (the only session that worked was one the car
   started; consider whether carjack should avoid initiating ACL/pairing at all and just be connectable).
4. Fix the HFP identity answers (`AT+CGMI` → Apple Inc., `AT+CGMM` → an iPhone model, plus `COPS`/
   `NREC`/`CCLK`) - resolve the "who owns HFP-AG" question above first.
5. Once iAP2 proceeds: continue to the WiFi/AirPlay RTSP steps.

## Relevant Files
- `src/airplay.rs`: Now acts as RTSP client — sends ANNOUNCE (with SDP) → SETUP → PLAY, then streams interleaved RTP.
- `src/encode.rs`: GStreamer pipeline now outputs RTP packets via `rtph264pay pt=96`.
- `src/main.rs`: Phase 3 orchestration — video channel cleaned up.
- `libcarjack/src/carplay_session.rs`: iAP2 state machine — video channel removed, now loops on `receive_packet()` directly.
- `iap2-proto/src/iap2.rs`: `Iap2Codec::decode()` — byte-at-a-time skip implemented.
- `carjack.log`: Previous test run log showing codec skip messages but no AirPlay RTSP exchange.
- `bluetoothd-hci-latest.pklg`: iPhone HCI capture with full iAP2 exchange up to WiFi config + post-config updates. No EA session messages found.
- `mycar.pcapng`: carjack↔MYCAR bike BT HCI capture (2026-07-21) showing HFP+A2DP succeed but iAP2 never starts (no `FF 55`/`FF 5A`); basis for the CoD-filter hypothesis.
- `mycar_2.pcapng` / `mycar_3.pcapng` (in `~/Documents/carjack/`): carjack↔bike, 2026-07-24 - before/after the connect-out change.
- **`iphonemycarcarplay.pklg`**: the key reference - a real iPhone doing *working* CarPlay on this same MYCAR bike (2026-07-25). Shows the car opening iAP ch1 inbound and sending `FF 55` first, the iPhone's outbound dial being rejected, and the iPhone's HFP identity answers.
- `src/bin/carjack-btmgmt.rs`: privileged sidecar — now sets both the CarPlay EIR UUID (`ADD_UUID`) and an iPhone-like Class of Device (`SET_DEV_CLASS`, `CARPLAY_DEV_CLASS`).
