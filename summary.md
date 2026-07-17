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
- **Root cause fixed: wrong RTSP role**: The `start_video_stream` function was waiting for the car to initiate RTSP (server behavior), but the iPhone acts as RTSP client — sending ANNOUNCE, SETUP, PLAY. Fixed in `airplay.rs`. Now sends ANNOUNCE with SDP → SETUP → PLAY → interleaved RTP.
- **Encoder output fixed**: Changed from raw H264 AVC to RTP packets (`rtph264pay`). Car expects RTP over interleaved RTSP TCP, not raw AVC.

### Blocked
- Need to test with actual car hardware to verify RTSP handshake completes.

## Next Move
1. Run carjack with the fixed AirPlay client code.
2. Verify RTSP handshake completes (ANNOUNCE → 200 OK, SETUP → 200 OK, PLAY → 200 OK).
3. Verify interleaved RTP video data reaches car.
4. If still failing, capture WiFi-side TCP dump to inspect RTSP exchange.

## Relevant Files
- `src/airplay.rs`: Now acts as RTSP client — sends ANNOUNCE (with SDP) → SETUP → PLAY, then streams interleaved RTP.
- `src/encode.rs`: GStreamer pipeline now outputs RTP packets via `rtph264pay pt=96`.
- `src/main.rs`: Phase 3 orchestration — video channel cleaned up.
- `libcarjack/src/carplay_session.rs`: iAP2 state machine — video channel removed, now loops on `receive_packet()` directly.
- `iap2-proto/src/iap2.rs`: `Iap2Codec::decode()` — byte-at-a-time skip implemented.
- `carjack.log`: Previous test run log showing codec skip messages but no AirPlay RTSP exchange.
- `bluetoothd-hci-latest.pklg`: iPhone HCI capture with full iAP2 exchange up to WiFi config + post-config updates. No EA session messages found.
