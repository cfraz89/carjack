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
- **`cfmoto_6.pcapng` (2026-07-26) is NOT a regression from the session-ID fix — cfmoto_5's iAP2 was
  luck.** The whole capture has **zero bytes on RFCOMM ch 1 in either direction**; the session-ID fix
  only changes payloads sent *after* `FF 55` + SYN, so it cannot be implicated. What differs is
  **who paged whom**:
  | | `cfmoto_5` (iAP2 worked) | `cfmoto_6` (no iAP2) |
  |---|---|---|
  | carjack `Create Connection` | t=28.23 → **Page Timeout (0x04)**, handle `0xffff` | t=13.16 → **success**, handle `0x0100` |
  | car `Connect Request` (inbound) | **yes**, t=28.57 → handle `0x0200` | never |
  | ACL that carried ch1 | **`0x0200` — the car's** | `0x0100` — carjack's |
  | Result on ch1 | car sends `FF 55` 302 ms after SABM | car **DISC**s 411 ms after SABM, silent |
  - So: **this head unit only speaks iAP2 over an ACL link it originated.** carjack's outbound page
    happened to time out in `cfmoto_5`, which forced the car to page us — and that is the only reason
    iAP2 ran. This is the same conclusion `iphonemycarcarplay.pklg` already gave (both
    iPhone-*initiated* sessions produced no `FF 55`; only the car-initiated one did); it just hadn't
    been acted on.
  - **Root cause in code: the "go passive" item was never implemented.** `src/wireless.rs:205-214`
    still spawns an unbounded `ExponentialBuilder::without_max_times()` retry of
    `device.connect_profile(&IAP_ACCESSORY_UUID)` racing the passive
    `acquire_profile_connection` wait. Whichever wins decides the session's fate, so runs alternate
    between working and not depending on whether the outbound page succeeds.
  - **Fixed 2026-07-26 — single-shot outbound dial (builds clean, not yet re-tested).** The dial
    can't just be deleted (a head unit only pages devices it is already bonded to, so the first
    contact has to be outbound), and **branching on `is_paired()` was tried and rejected**: on the
    *other* head unit the passive/bonded path waits a very long time for an inbound connection, so
    "bonded ⇒ never dial" strands that unit.
    What's there now: **one** `connect_profile(&IAP_ACCESSORY_UUID)` attempt, unconditional, racing
    the passive `acquire_profile_connection`; whichever lands first wins, and a failed dial is
    logged at info and dropped rather than retried. The key property is that a *single* page
    doesn't keep the controller busy, so it no longer starves our page scan and the car can still
    reach us immediately afterwards - which was the real damage the old unbounded
    `ExponentialBuilder::without_max_times()` retry did. The task is `drop`ped once a stream
    arrives so a still-pending page can't land a second connection.
  - **Plus: drop the ACL on a silent link.** Connection acquisition is now a loop. If a link comes
    up but `Iap2::start` fails — the car DISC'd ch 1 without `FF 55`, or the switch command never
    arrived (`perform_switch` bounds this at 12 s × 3, and returns immediately on EOF) — carjack
    calls `device.disconnect()` and goes back to waiting. Rationale: while *we* hold the ACL the
    car won't page us, and being paged is exactly what such a head unit is waiting to do. The loop
    deliberately does **not** re-dial, so every attempt after the first is purely passive.
    Structured as a `backon` `retry(...).context(...).when(...)`: the context carries the merged
    profile streams and the dial handle across attempts, and `.when` is what decides whether to go
    round again — it retries on `LinkAttemptError::Retryable` but aborts on
    `LinkAttemptError::ProfilesClosed` (BlueZ took our registrations away, so no connection can
    ever arrive and retrying would spin on an exhausted stream).
  - **Still open:** the near-continuous `Inquiry` (t=15.97 and 32.97 here, every 16 s in `cfmoto_5`,
    including while an ACL is up) also starves page scan, and now matters whenever the outbound
    dial loses and we fall back to being paged. Not yet traced to a source — `find_target_device`
    drops its discovery stream when it resolves the target, and takes the BlueZ-known-devices
    branch without starting discovery at all, so this may be another process on the host. `btmon`
    will show who issues it.
- **iAP2 STARTS on the bike (when the car initiates); it dies at the first control message — wrong session ID**
  (`cfmoto_5.pcapng`, 2026-07-26, vs `iphone-cfmoto.pklg`). The passive/car-initiated change worked:
  the car opens RFCOMM ch 1 inbound (t=30.72), sends `FF 55 02 00 EE 10`, carjack echoes, and the
  **Link SYN handshake completes** (car SYN → carjack SYN+ACK → car ACK). Two identical attempts in
  the capture (t≈31 and t≈112), both failing the same way.
  - The car's SYN payload is `01 01 1000 03e8 01f4 0f 00 | 02 00 02` — the trailing triple declares
    **exactly one session: id=2, type=0 (Control), version=2**. No EA session is declared.
  - carjack then sends `StartIdentification` on **session ID 1**:
    `ff5a 0029 40 9e 59 **01** 46 | 4040 001f 1d00 …`
    The iPhone sends it on **session ID 2**: `ff5a 0010 40 40 74 **02** a1 | 4040 0006 1d00`.
    Every iPhone control packet in the whole reference capture uses session `02`; bare link ACKs use
    session `00`.
  - The car **link-layer ACKs** carjack's packet (`ff5a 0009 40 59 9e 00`) — the retransmit layer is
    happy — then **discards the payload and never replies**, because session 1 doesn't exist. carjack
    sits silent for 44 s and DISCs ch1 at t=75.3. That silence is the whole failure.
  - **Both bugs fixed 2026-07-26 (built clean, not yet re-tested against the bike):**
    1. `libcarjack/src/carplay_session.rs` hardcodes session id `1` at every send site
       (`:106` keep-alive, `:124` `do_identification`, `:180` `send_ctrl`). It must use the id the
       accessory declared in its SYN (`table.find_type(SessionType::Control)`), and bare link ACKs /
       keep-alives should use session `0`, like the iPhone.
    2. `iap2-proto/src/ops/syn.rs:76-89` injects hardcoded entries `{id:0, Control, v1}` and
       `{id:2, ExternalAccessory, v1}` *after* adding the accessory's real sessions. Because
       `SessionTable::add` does `retain(|s| s.id != entry.id)` (`stream.rs:41`), the fake
       `{id:2, EA}` **overwrites the car's `{id:2, Control}`**. So even with (1) fixed, inbound
       control messages on session 2 would be misrouted to `handle_ea` by `dispatch_packet`
       (`carplay_session.rs:148-164`). Drop the hardcoded entries; trust the accessory's table.
  - **Minor mismatch (fix while there):** `ControlMessage::start_identification()`
    (`iap2-proto/src/ops/control.rs:381`) attaches three parameters (`DISPLAY_NAME "CarJack"`,
    `VERSION`, `FEATURES`). Per spec and per the reference capture, `StartIdentification` (0x1D00)
    carries **no parameters** — the iPhone's is 6 bytes total. Not proven fatal (the car never got
    far enough to complain), but it should be empty.
  - What the car does next, once this is fixed (from the iPhone capture): `IdentificationInformation`
    (0x1D01, ~330 bytes, "Motorcycle Media Players") → we send `IdentificationAccepted` (0x1D02) →
    `RequestAuthenticationCertificate` (0xAA00) → serial/cert/challenge/response → 0xAA05 →
    then the car sends `WirelessCarPlayUpdate`-ish 0x4300 messages with SSID `CFMOTO-999700` etc.
- **(Superseded) iAP2 never starts on the bike — identity/CoD filter suspected** (from `mycar.pcapng`,
  2026-07-21, carjack↔head unit, vs `bluetoothd-hci-latest.pklg` iPhone reference):
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
    unit** (the old bench test rig), where the car spoke first after 580 ms — the bike behaves
    differently, so we can't assume its first-speaker/timing from that capture.
  - **RESOLVED by the iPhone↔head-unit capture** (`iphonemycarcarplay.pklg`, 2026-07-25) — a *working*
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
  while the bike (`aa:bb:cc:dd:ee:ff`, RSSI −59, class `0x240408`, advertising both
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
  **The identity filter alone is NOT enough:** the stale bonded entries *look* right. The old bench
  test rig (`aa:bb:cc:dd:ee:02`, powered off) still reports class `0x00240418`, a cached
  RSSI of -61, **and** both `ec884348-…` and `…deca-caff`. Worse, `bluer::discover_devices` replays
  every known device as `DeviceAdded` *before* any genuine discovery hit, so both the pre-check loop
  and the discovery loop meet it first.
  **Resolved (2026-07-25) by removing auto-selection entirely.** The intermediate fixes (identity
  filter, reachability probe, live-sighting-only detection, RSSI ranking) are all gone - the target
  head unit is now **configured**, not guessed:
  - `[bluetooth] device` in `config.toml`, accepting a **MAC** (`AA:BB:CC:DD:EE:FF`) or an
    **advertised name**. Env `CARJACK.BLUETOOTH.DEVICE`, flag `--bt-device`,
    normal precedence (flag > env > file). `Config::bluetooth_device()` returns `None` when
    unset/blank and `main` errors out at startup with a message naming all three ways to set it.
  - `wireless::find_target_device` replaces `pair_available_accessory` + `is_candidate_accessory` +
    `advertises_carplay` + `try_prepare_accessory`: match the configured device among BlueZ's known
    devices, else wait for it in a discovery aimed at that one target (`DISCOVERY_WAIT`, 60 s);
    everything else is ignored. `prepare_device` pairs (tolerating failure - BlueZ often bonds
    during the profile connect) and trusts.
  - The legacy bespoke `CARPLAY_DEVICE_NAME` env var is gone, folded into the config system.
  **Why it had to go - measured (2026-07-25):** BlueZ **never clears a stale RSSI**. The powered-off
  old bench test rig still reported `-61` after a full 12 s discovery pass and still listed the CarPlay
  UUIDs + a BR/EDR class, i.e. it was indistinguishable from a present device by any cached property,
  and `bluer::discover_devices` replays known addresses *ahead* of live hits so it was met first.
  (A live discovery *event* is a valid presence signal - that device emitted zero across a 15 s scan -
  but with an explicit target none of that inference is needed.)
  Note: the old commented `.env` line had a typo'd name - it would never have
  matched. `.env` now sets `CARJACK.BLUETOOTH.DEVICE` to the correct name, with the MAC
  as a commented alternative.

### Blocked
- Nothing blocked on captures any more - the iPhone↔head-unit reference answered the open questions.

## Next Move
1. **Re-test against both head units and capture.** Two things to check, in order:
   - **Who won the race, and what happens after a silent link.** Expect exactly **one**
     `Sent Create Connection` from carjack, no more. On the bike it will likely succeed and then
     produce no `FF 55` (that unit wants to originate the link); the sequence to confirm is
     car `DISC` on ch 1 → carjack drops the ACL (`Sent Disconnect`) → **`Rcvd Connect Request`**
     from the car → `FF 55` on the car's own link. On the other head unit the single dial should
     just win outright and carry the session with no drop at all.
   - **Then** the iAP2 payload: `StartIdentification` should go out as
   `ff5a 0010 40 <seq> <ack> 02 <ck> | 4040 0006 1d00` (session **2**, no params), and the car to
   answer with `IdentificationInformation` (0x1D01, ~330 bytes) instead of silence, then the auth
   chain (0xAA00…0xAA05) and the WiFi/`0x4300` messages.
2. Watch for a **new** failure at `handle_control_message`: the auth chain wants a real Apple
   authentication coprocessor response for 0xAA02/0xAA03. `generate_challenge` is a PRNG stub, so
   `AuthenticationSucceeded` may be a lie the car rejects. Unknown until we get there.
3. Fix the HFP identity answers (`AT+CGMI` → Apple Inc., `AT+CGMM` → an iPhone model, plus `COPS`/
   `NREC`/`CCLK`) - resolve the "who owns HFP-AG" question above first. Not implicated in this
   failure (iAP2 got past it), so lower priority than it looked.
6. Once iAP2 proceeds: continue to the WiFi/AirPlay RTSP steps.

## Relevant Files
- `src/airplay.rs`: Now acts as RTSP client — sends ANNOUNCE (with SDP) → SETUP → PLAY, then streams interleaved RTP.
- `src/encode.rs`: GStreamer pipeline now outputs RTP packets via `rtph264pay pt=96`.
- `src/main.rs`: Phase 3 orchestration — video channel cleaned up.
- `libcarjack/src/carplay_session.rs`: iAP2 state machine — video channel removed, now loops on `receive_packet()` directly.
- `iap2-proto/src/iap2.rs`: `Iap2Codec::decode()` — byte-at-a-time skip implemented.
- `carjack.log`: Previous test run log showing codec skip messages but no AirPlay RTSP exchange.
- `bluetoothd-hci-latest.pklg`: iPhone HCI capture with full iAP2 exchange up to WiFi config + post-config updates. No EA session messages found.
- `mycar.pcapng`: carjack↔bike BT HCI capture (2026-07-21) showing HFP+A2DP succeed but iAP2 never starts (no `FF 55`/`FF 5A`); basis for the CoD-filter hypothesis.
- `mycar_2.pcapng` / `mycar_3.pcapng` (in `~/Documents/carjack/`): carjack↔bike, 2026-07-24 - before/after the connect-out change.
- **`cfmoto_6.pcapng`** (2026-07-26): carjack↔bike, after the session-ID fix. carjack's outbound page
  *succeeded*, so the car never paged us; it opened ch1 on carjack's ACL and DISC'd without `FF 55`.
  Zero ch1 bytes — proof the head unit requires a car-originated ACL, not a session-ID regression.
- **`cfmoto_5.pcapng`** (2026-07-26): carjack↔bike, the first capture where iAP2 actually starts —
  `FF 55` echo + full Link SYN handshake succeed, then `StartIdentification` on the wrong session id
  (1 instead of 2) is ACKed-and-dropped. Two identical attempts, t≈31 and t≈112.
- **`iphone-cfmoto.pklg`**: iPhone ("Tamagotchi") ↔ this bike, working CarPlay. The byte-level
  reference for the control session id (`02`) and the post-identification auth chain.
- **`iphonemycarcarplay.pklg`**: the key reference - a real iPhone doing *working* CarPlay on this same bike (2026-07-25). Shows the car opening iAP ch1 inbound and sending `FF 55` first, the iPhone's outbound dial being rejected, and the iPhone's HFP identity answers.
- `src/bin/carjack-hci-helper.rs`: privileged sidecar — now sets both the CarPlay EIR UUID (`ADD_UUID`) and an iPhone-like Class of Device (`SET_DEV_CLASS`, `CARPLAY_DEV_CLASS`). Takes no arguments; resolves the adapter itself via bluer's default adapter.
