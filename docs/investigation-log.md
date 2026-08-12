# Investigation log

Reverse-chronological, newest first. Each entry is what a capture or a session actually showed,
what was concluded from it, and what changed as a result - including the things that turned out
to be wrong, which is most of the value here.

Entries are **historical records, kept as written**. Where a later entry supersedes an earlier
one it says so; nothing is edited in place. For where things stand *now*, read
[status.md](status.md) instead - this file is deliberately not a summary.

Captures referenced by name are inventoried in [captures.md](captures.md).

## 2026-08

- **2026-08-12 (`logs/cfmoto-iphone.pcapng`, re-parsed): the bike gets HEVC from an iPhone and H264
  from us - and the PTS "fix" two entries below was wrong, now reverted.** The reference capture
  had been lost (`logs/` is gitignored) and was restored; parsing its screen socket frame by frame
  answers several open questions at once. 77 video frames, one `VideoConfig`, 8 keep-alives, over a
  6.6 s window.

  **It sends HEVC.** The config payload is a `hvc1` sample entry - fourcc `68 76 63 31`,
  compressorname "HEVC", coded 800x1280 - with `small_param[1] = 0x1e`
  (`RespectTimestamps | Encrypted | UseFormatDescription | 0x10`) against our `0x06`. So the whole
  time we have been sending H264 to a unit that a real phone only ever gives H265. It takes ours,
  ACKs it, never asks for a keyframe, and shows nothing - which is what an undecodable stream looks
  like from the sender's side. `0x10` is a flag catplay does not name.

  **It declares where the video goes.** `params[3..6]`: origin `(0, 223)`, size `(800, 1056)`,
  twice. That matches `/info`'s `displayPanels.initialVideoStreams.originYPixels = 224` under an
  800x1280 panel - a 224 px instrument strip above a 1056-tall video area. carjack sends only
  `params[1] = (800, 1280)`; catplay's decode path has the view params commented out.

  **It stops sending when nothing changes** - the original question. Frames arrive in bursts a
  millisecond or two apart, separated by idle gaps of 0.504 s, 0.986 s, 0.984 s and **4.044 s**,
  with `KeepAliveWithBody` in between. 376 B to 63 KB per frame, median 11 KB: VBR, no filler,
  1.66 Mbit/s while active and nothing while idle. carjack renders on a fixed 30 fps timer *by
  design* (`src/compositor.rs:6`) and pads every frame to the CBR target, so it sends a flat
  3.0 Mbit/s whether the screen moves or not.

  **And it puts its frame PTS one NTP epoch below its own time sync.** Its `t3` read
  `2209933588.87` (epoch + 944,788 s of uptime); the PTS on frames half a second later read
  `944789.37`. Bare monotonic, no epoch - exactly what catplay did before the change below. That
  change is reverted; all that survives is a doc comment and a test on `NTP_UNIX_EPOCH_OFFSET_NS`
  (fork `283adaf`, on `fix/airplay-device-identity`) pinning the split so it does not get "fixed"
  again. The bike staying black after the change should have been read as the fix being wrong
  rather than as a second cause.

- **2026-08-12 (`logs/cfmoto-12-08-0136.*` + local encode dumps): the PTS fix is on the wire and
  the encode path is proven correct, so a black screen is now most likely a black *source*. Also:
  this unit never sends `disableBluetooth`.** Three things settled, one still open.

  **The PTS fix works.** Frame `params[0]` now reads `2208988800 + elapsed`, the same domain as the
  timing exchange, where it used to be bare elapsed. The bike still showed black, so the 70-year
  skew was a real bug but not the whole story.

  **The encode path is not the problem, and frame size never meant what it looked like.**
  `tests/encode_dump.rs` (new, `#[ignore]`d) writes what the encoder would send to a file, through
  whichever backend `main` picks - unlike the screenshot driver, which forces SHM and so proves
  nothing about the dmabuf chain. At the bike's own 800x1280 through `vapostproc ! vah264enc`, the
  decoded output is a correct, upright picture. But the *empty* compositor - verifiably pure black,
  `mean=0 stddev=0` - still costs ~10 KB a frame, because `rate-control=cbr` pads every frame with
  filler to exactly `bitrate/fps` = 12500 bytes. So:

  ```
  cfmoto-12-08-0050: 2880 frames, keyframes to 120 KB   -> live content on the wire
  cfmoto-12-08-0136: 2207 frames, nothing over 12516 B  -> indistinguishable from black
  ```

  Locally, a live home screen goes over the cap within seconds (69 KB P-frames during the wallpaper
  animation); 73 s without a single overshoot fits a static or black source. The `0136` run also
  opens with a 281-byte IDR and 29 45-byte frames - byte for byte the signature a uniform black
  frame produces in every local dump - and carries **none** of waydroid's startup output
  (`Starting waydroid session`, `Android with user 0 is ready`), which a fresh launch prints
  through the same inherited stdio. Waydroid was found afterwards `RUNNING` with
  `Wayland display: wayland-1`, a socket belonging to a carjack that had already exited: the stale
  session of [[black-feed-stale-waydroid]], which paints nothing and takes ~18 s to paint even when
  started clean. Not proven for that run, but it is the reading the evidence supports.

  **`disableBluetooth` never arrives.** Not over AirPlay, not over iAP2, not through the tunnel -
  in any capture. The car's actual handover signal is the tunnel itself: it brought the tunnelled
  iAP2 link up at 15:37:05.0617 and reset the RFCOMM channel 76 ms later. So the graceful-shutdown
  path now fires on the tunnel link coming up as well as on the command, which is what makes it
  fire on this unit at all.

- **2026-08-12 (`logs/cfmoto-12-08-0050.pcapng` + `.log`): the bike's black screen is a frame PTS
  70 years in the past - catplay's timing server and its frame timestamps are in different time
  domains.** The session itself is the healthiest yet: 2880 video frames (36 MB) delivered over
  96 s on the screen socket, every one TCP-ACKed, no RST, no `ForceKeyFrame` from the car, 33
  `HidSendReport` touches coming back, and the iAP2 tunnel linking for the first time. The car
  takes everything we send and displays nothing.

  The screen frame header is plaintext (only `VideoFrame` bodies are ChaCha'd), so the capture can
  be parsed directly. It is well-formed: one `VideoConfig` (opcode 1) carrying an
  AVCDecoderConfigurationRecord - High profile, level 3.2, 4-byte NAL lengths, 1 SPS + 1 PPS -
  `params[1] = (800.0, 1280.0)` matching `/info`, `small_param[1] = 0x06` =
  `Encrypted | RespectTimestamps`, then 2880 `VideoFrame`s and 94 keep-alives.

  What does not line up is `params[0]`, the presentation timestamp:

  ```
  timing (UDP, 44 exchanges): car t3 = 2208988800.6856   we answer 2208988800.0723
  screen frame params[0]:     first  =          0.2229   ... +1/30 s per frame
  ```

  Both sides of the timing exchange are `session elapsed + NTP epoch offset` (2 208 988 800 s) -
  `MediaClockSession::respond`. `encode_local`, which stamps the frames, is session elapsed with
  **no** epoch offset. So the car, which does sync (it pinged us every ~2 s for the whole run),
  places every frame ~70 years before the clock it just synchronised to, and we asked it to honour
  that with `RespectTimestamps`.

  The obvious reading of the bench AUTO BOX showing video from the same code is that it never runs
  a timing exchange: with no PLL, `decode_remote` returns `None` and catplay's own decode path falls
  back to presenting on arrival, so only a unit that actually syncs is affected. Not verified - no
  capture of a bench session that reached video.

  Changed, in the fork on `fix/media-clock-ntp-domain` (4fe3097, off `fix/airplay-device-identity`,
  itself one commit off an in-sync upstream master): all four `MediaClock` methods now convert at
  the NTP boundary rather than only the sync packets doing it. Writing the round-trip test turned up
  a second trap - `NtpU64::as_nanos` folds its seconds through `i32`, so an absolute NTP value comes
  back wrapped (that fold is why `test_negative_roundtrip` passes, so it stays). The decode side
  therefore subtracts the epoch as a *difference* against the same-wrapped constant, which is how
  `as_nanos` is used everywhere else anyway. 69 crate tests pass; `media_pll::test_drift_effect`
  fails on the base branch too (`DISABLE_PLL = true`).

  carjack keeps `respect_timestamps: true` and both manifests point at the new branch. **Untested on
  the bike.**

- **2026-08-05 (code walk + `logs/cfmoto.log`): root cause is the AirPlay `/setup` identity - carjack
  sends a fake identity that matches nothing. Fixed on a catplay fork branch, untested on the bike.**
  The refused screen SETUP is not the problem - the bytes are fine and the bike once *answered* a
  like SETUP (`cfmoto_16`). The thing our stack sends that a real iPhone never does is the `/setup`
  bootstrap identity:

  ```
  "deviceID":   "ff:ee:dd:cc:bb:aa",
  "macAddress": "aa:bb:cc:dd:ee:ff",
  ```

  These are hardcoded literals in catplay's `do_initial_setup` (the `AirPlayTransmitterBootstrap`
  never carried them), and carjack never overrode them. catplay documents `deviceID` as the
  device's **Bluetooth MAC address** - the exact field a head unit uses to tie the WiFi session
  back to the device it authenticated over Bluetooth (same rule as `_carplay-ctrl`'s `id`, and the
  reason `cfmoto_16` answered a SETUP then still refused: the unit is lenient until the stream
  actually starts, then validates). So the bike sees us claim `ff:ee:dd:cc:bb:aa`/`aa:bb:cc:dd:ee:ff`
  while the station on its AP is `FE:E8:AC:31:AD:C9` and the BT identity was `4C:79:6E:DA:29:4B` -
  an identity matching nothing, dropped at the moment the screen stream begins.

  Fix (both repos): catplay now plumbs `device_id` + `mac_address` through `AirPlayTransmitterBootstrap`
  into the `/setup`; carjack supplies the adapter BT MAC (`device_id`, same as the `_carplay-ctrl`
  id and the `0x4300` transport identifier) and the announced/associated WiFi MAC (`mac_address`,
  same as `0x4300` group 1). The fork's `master` was synced to `catplay-labs/catplay` master, then
  branched `fix/airplay-device-identity`; Cargo.toml + `libcarjack/Cargo.toml` point at it.
  Builds, `just test-lib` green. **Untested against the bike** - that is the next step.

- **2026-08-05 (`logs/cfmoto.log`, `catplay_carplay=info`): the car answers every AirPlay request
  through `assert_modes`, and is silent exactly at the screen SETUP.** This was the cheap test
  status.md planned - it settles whether the car replies to the screen SETUP before deauthing.

  The RTSP exchange this run (13 of 14 requests answered, all but the last 200 OK):

  | CSeq | request | response |
  |---|---|---|
  | 0 | `SETUP /setup` (unencrypted) | 401 Unauthorized |
  | 1 | `POST /pair-verify` | 200 OK |
  | 2 | `POST /pair-verify` | 200 OK |
  | 3 | `SETUP /setup` (crypto) | 200 OK (keepAlive/event/timing ports) |
  | 4 | `GET /info` | 200 OK (32 KB, display `800x1280`, uuid `e5f7a68d-…`) |
  | 5 | `POST /command` `modesChanged` | 200 OK |
  | 6 | **`SETUP /` (screen stream)** | **no response, ever** |

  Two things are new and both rule out the Bluetooth/bootstrap side:

  - **The crypto-upgrade path now works end to end.** Known HomeKit accessory
    `9a011f99-597e-424d-b490-806fd7c07800` -> expecting pair-verify; the first bare `SETUP /setup`
    gets `401`, pair-verify x2 returns 200, the encrypted `SETUP /setup` returns 200 with ports,
    `/info` returns the full plist. So persistence paid off and `/setup` is not the trigger.
  - **`assert_modes` is definitively not the trigger.** The `modesChanged` command was answered
    200 OK at .809 (42 bytes, `{}`), the screen SETUP goes out at .814 with a fresh
    `stream_connection_id` and the `/info` uuid, and then - nothing. No `Screen setup response
    received` line, no ack, no RST. The request times out 11 s later -> `Request/channel timeout`
    at 05:43:10.8, restart, Bluetooth page timeout on retry.

  So the car handles the entire control session, including the modes assertion, and refuses the
  single request that actually starts the video stream. The candidate plist is small and clean
  (`streamConnectionID` u64, `type: 110`, `latencyMs: 100`, `uuid` matching `/info`); `cfmoto_16`
  once *answered* a SETUP like it and still dropped the station 77 ms later when the data socket
  opened. Either way the head unit tears down at the moment the video screen session begins - now
  shown to be with no ACK to the SETUP at all in this run, not a response-then-drop.

  No change made; the trigger is confirmed car-side and the SETUP bytes themselves remain the only
  remaining suspect (plist contents vs. the car reconfiguring its AP when a session moves to
  streaming).

- **2026-08-03 22:53 (`cfmoto.pcapng` + `/tmp/carjack.btsnoop` + the journal): the bike does no
  Bonjour at all, and the WiFi association survived the whole run.** The first bike test of the
  invite path, and it answers the question cleanly in the negative.

  carjack advertised `_carplay-ctrl._tcp` correctly - `id=4C:79:6E:DA:29:4B`, the adapter's
  Bluetooth address, announced once a second from t=17.0 to the end of capture at t=69.8, across
  two complete iAP2 sessions - and no invite ever came. The bike's entire IP presence in 70 s is
  three packets: DHCP offer, DHCP ack, and one ARP (`Who has 192.168.69.57? Tell 192.168.69.58`)
  at t=22.07, which carjack answered in 17 µs and which was followed by nothing. Not one mDNS
  query, not one announcement, no TCP in either direction. It does not browse for a phone and does
  not publish `_airplay._tcp` either. The bench measurement stands (that unit invites in 69 ms once
  the `id` is the Bluetooth address); it simply does not generalise to this one.

  What the bike does instead is repeat `0x4301` every ~3 s for as long as the session lasts - six
  times in session 1 (t=3.8…18.9) and six more in session 2 - always the same 158 bytes: SSID
  `CFMOTO-999700`, passphrase, channel 157, `fe80::9083:8eff:feef:a7d3`, port 7000, MAC
  `3D:20:3D:D3:EB:06`, pairing id `9a011f99-597e-424d-b490-806fd7c07800`. Decoded against
  `iphone-cfmoto.pklg`, that message is **byte-identical** to the one the working iPhone receives -
  and in that capture the phone's iAP2 traffic ends 3 s after the second copy (the whole iAP2 span
  is 426.0…437.4 s), which is the handoff. So the repeats are the bike waiting to be connected to,
  and the iPhone connects out. carjack, having had its direct-connect path removed the commit
  before, sat waiting for an invite that this unit has no mechanism to send.

  Two side findings, both worth more than the primary one:

  - **The association did not die.** `associated` at 22:53:41, still up when the capture stopped
    56 s later, `Connection to AP … lost` only at 22:54:45 after carjack had exited. No deauth
    cycling whatsoever. The ~11 s locally-generated deauth loop of the 21:02 run did not
    reproduce, and the difference in this run is that avahi is emitting mDNS every second - which
    is exactly the traffic mac80211's connection-loss detection wants to see. Not proven, but it
    is the first candidate.
  - **The `/info` timeout was never evidence against connecting out.** It came from the 21:02 run,
    whose WiFi was deauthing every ~11 s; a request that sat 10 s unanswered was sitting on a link
    that was in the middle of dying. The conclusion drawn from it - "the bike accepts the TCP
    connection and never answers `/info`" - is withdrawn.

  Changed: the invite stays preferred and the advertisement stays unconditional, but when `0x4301`
  names a receiver the wait is 3 s and carjack then connects there. `WifiCredentials` now also
  carries the pairing id from `WIFI_CONFIG_UUID` (`0x0004`), which was being parsed by nothing -
  though reading catplay showed it buys nothing yet: `HomekitStorageFile::memory()` gives carjack a
  new identity and an empty pairing table per connection, so `PairingHelperTx`'s `/pair-verify`
  fast path can never fire and a pairing id can only *fail* pair-setup on a mismatch. Untested on the bike.

- **2026-08-03, following on: HomeKit pairing is not merely unpersisted, it is never run.** Chasing
  the pairing ids further, catplay's transmitter bootstrap turns out to handle `Connected` with
  `start_setup_task()` alone - `start_pair_task()` and `start_auth_setup_task()` are commented out
  on the two lines below it (`carplay_tx/transmitter.rs`). So `/pair-setup` and `/pair-verify` are
  never issued, the control channel stays `AirPlayStreamEncryption::Unconfigured`, and the
  `HomekitStorageRef` carjack passes in is neither read nor written. Confirmed on the bench rather
  than assumed: two full sessions to RECORD, and the storage file was unchanged on disk and still
  reported `0 accessory pairing(s) known` at the start of the second. Both units accept an
  unencrypted control channel.

  Persistence was implemented regardless (`init_homekit_storage` →
  `~/.local/share/carjack/homekit.bin`; identity verified stable across four runs including two car
  sessions), and `connect_airplay` now passes a pairing id only when storage already knows it -
  unknown, an id can only make `/pair-setup` assert an identity match, which risks a session on a
  guess about what the field means and wins nothing. So this is groundwork, not a fix: nothing seen
  so far needs HomeKit, and it should not be read as a lead until a unit refuses an unencrypted
  control channel.

- **2026-08-03 21:02 (`cfmoto.log` + `/tmp/carjack.btsnoop` + the journal): the deauth is
  *locally* generated, and the car is fine throughout. This supersedes the 2026-07-31 entry
  below, which read the same event as coming from the AP.**
  - No WiFi pcap for this run; the evidence is the kernel/NM/wpa_supplicant journal plus the
    btsnoop.
  - The run got further than any before it on the identity front, with **no env overrides**:

    ```
    11:01:58.680  iAP2 link established
    11:02:01.802  Accessory WiFi: ssid=CFMOTO-999700          <- 0x4301, default identity
    11:02:05.659  Associated as the announced address actual=FE:E8:AC:31:AD:C9
    11:02:05.692  AirPlay endpoint given by the car: [fe80::9083:8eff:feef:a7d3%2]:7000
    11:02:15.957  /info failed: Request/channel timeout
    11:02:18.461  TCP connect timed out … (and every attempt after)
    11:02:31.579  No route to host
    ```

  - The journal (local time, UTC+10) says who dropped the link:

    ```
    21:02:03  associated (RX AssocResp … status=0 aid=1); dhcp4 lease 192.168.149.57; activated
    21:02:16  deauthenticating from 92:83:8e:ef:a7:d3 by local choice (Reason: 2=PREV_AUTH_NOT_VALID)
    21:02:17  associated again  -> 21:02:28 deauth (3) -> 21:02:39 deauth (3) -> 21:02:49 deauth (3)
    ```

    wpa_supplicant: `CTRL-EVENT-DISCONNECTED bssid=92:83:8e:ef:a7:d3 reason=2
    **locally_generated=1**`. NetworkManager logs `ip:dhcp4: restarting` ~60 ms *before* the
    supplicant reports `completed -> disconnected`, i.e. both are reacting to the kernel's deauth,
    not causing it.
  - **The AP was never the problem.** It answers every re-association with `status=0 aid=1`, and
    the btsnoop shows the bike doing SDP + PBAP (`Get final "telecom/pb.vcf"`, answered `Not
    Found`) at **21:02:43** - half a minute after the WiFi began cycling. The car stayed up and
    stayed connected over Bluetooth; only the WiFi association failed. So the "a head unit walks
    away a couple of seconds after carjack stops making progress, taking Bluetooth with it"
    pattern does not describe this failure.
  - **It is not `assert_modes`.** The session never reached it - `/info` went unanswered. Every
    earlier capture's 20-45 ms `assert_modes` → deauth correlation was coincidence: the modes
    assertion is just what a session that has survived ~12 s happens to be doing when the link
    goes.
  - **The announce/associate mismatch is not the cause.** This run announced and associated as the
    same `FE:E8:AC:31:AD:C9` (`617f28d`), on the bike, and died anyway. The fix stays - it is
    correct - but the theory it was built on is closed.
  - Timeline shape to explain: associate → DHCP fine → traffic flows ~2 s (TCP to the car's
    link-local connects) → silence → ~10 s later the local stack declares the auth invalid.
    A group-key rekey the AP never completes fits; nothing in the journal confirms it. **The
    missing evidence is `wpa_supplicant -dd` or a monitor capture on 5785 MHz across the
    association** - neither has ever been collected.
  - Also settled, incidentally: Waydroid is not implicated. Its Android init spams the kernel log
    with `wifi.rc` actions two seconds before the first deauth, but the container is on a `veth`
    to `waydroid0` in its own netns and cannot see `wlp0s20f3`.
  - **Byte-diff of the whole iAP2 exchange against `iphone-cfmoto.pklg` (the same bike).** The
    message *sequence* is identical - `StartIdentification` → `IdentificationInformation` →
    `IdentificationAccepted` → `RequestAuthenticationCertificate` → `AccessoryAuthenticationSerialNumber`
    → `RequestAuthenticationCertificate` → `AuthenticationCertificate` →
    `RequestAuthenticationChallengeResponse` → `AuthenticationResponse` → `AuthenticationSucceeded`
    → `DeviceInformationUpdate` → `0x4300` → `0x4301` (twice, ~3 s apart) - but two things differ:
    1. **`0x4300`'s contents.** Same shape (two groups, each `{0: 0x01, 1: string}`), different
       strings:

       | group | iPhone | carjack |
       |---|---|---|
       | 0 | `"00008130001C29C40206001C"` (24 chars, no colons) | `"4C:79:6E:DA:29:4B"` (BT MAC) |
       | 1 | `"ac:df:a1:b3:87:9d"` (its **Bluetooth** MAC, lowercase) | `"FE:E8:AC:31:AD:C9"` (the **WiFi** MAC) |

       So group 1 is the *Bluetooth* address for the iPhone and the *WiFi* address for us, and
       group 0's format is not even the same kind of string. **The iPhone never tells this car a
       WiFi MAC at all** - which independently explains why making announce == associate changed
       nothing.
    2. **Three messages we send that the iPhone does not send to this car**: `DeviceTimeUpdate`,
       `DeviceLanguageUpdate` and `WirelessCarPlayUpdate`, at +2/+43/+80 ms after `0x4301`. The
       iPhone sends nothing at all on the iAP2 channel for the remaining **45.8 s** of the
       capture, so this is real silence, not a truncated capture. (It *does* send those three to
       the bench unit in `iphone-first-pair.pklg`, right after that unit's flat `0x5703` - so the
       messages are legitimate iPhone behaviour, just not what this bike is shown.)

    Not differences: `DeviceInformationUpdate` differs only by name ("Tamagotchi" vs "carjack"),
    and the ordering is otherwise identical. Timing does differ - the iPhone leaves 3.6 s between
    `DeviceInformationUpdate` and `0x4300`, carjack 37 ms.
  - Removed as a result of this run's log: the rtnetlink DAD watcher. Across every session that
    ever logged it, `wait_for_link_local` returned 660-810 µs after `IP6_READY` - it never waited
    for anything, so `IP6_READY` alone is the gate and the `rtnetlink` dependency is gone.

## 2026-07

- **2026-07-31 (later): the head unit sends the deauth, and it lands ~80 ms after the AirPlay
  *screen data socket* opens. `cfmoto_16` + `/tmp/iw-events.log`.**
  - **No longer an inference.** `iw event -t` running alongside the run:

    ```
    ...204.215  assoc 92:83:8e:ef:a7:d3 -> fe:b2:9f:ed:ea:72 status: 0: Successful
    ...204.215  connected to 92:83:8e:ef:a7:d3
    ...206.269  del station 92:83:8e:ef:a7:d3
    ...206.271  deauth 92:83:8e:ef:a7:d3 -> fe:b2:9f:ed:ea:72 reason 2
    ...206.271  disconnected (by AP) reason: 2: Previous authentication no longer valid
    ```

    `deauth <AP> -> <us>`, **`disconnected (by AP)`**. Every subsequent disconnect in the log is
    `(local request) reason 3` - NM tearing down afterwards. One AP-sent deauth per run.
  - **The local-driver theories are dead.** This run had `disable_11ac=Y` and `iwlmvm
    power_scheme=1` active and behaved identically. `Unhandled alg: 0x707` was a *symptom* of the
    security association being pulled, not its cause. Both options reverted; `swcrypto=1` is not
    even accepted by iwlmvm. Nothing on this side prevents the drop.
  - **Best run against the bike so far**, and the first exercising the in-memory profile: fresh
    association at 23:06:44.215 on a **new subnet** (192.168.155.x, new stable-privacy link-local
    `fe80::c384:2032:175e:8bc6`), DHCP in 25 ms, full AirPlay bootstrap, `assert_modes`
    (385 -> 182), **SETUP answered (314 -> 225)**, event socket up, and the screen data socket
    established. Only RECORD never went out.
  - **The trigger is that second socket.** Ordering, to the millisecond:

    | t (23:06:xx) | event |
    | --- | --- |
    | 46.162 | event socket 41074 -> 35565 SYN/SYN-ACK/ACK |
    | 46.177 | screen SETUP (314 bytes) |
    | 46.1925 | car answers SETUP (225 bytes) |
    | 46.1937 | **screen data socket 41958 -> 44165 SYN**, ACK at 46.1975 |
    | **46.271** | **AP deauth** |
    | 48.389 | Bluetooth ACL drops |

    `cfmoto_12` is the same shape: event socket 45.418, SETUP answered 45.4486, screen socket
    45.4497, RECORD answered 45.4548 - and the deauth in that same 20:16:45 second. The screen
    socket carries **no data at all** before the deauth in `cfmoto_16` - three-way handshake and
    nothing else. The BT link following ~2 s later matches the known "the unit walks away, taking
    both radios" behaviour.
  - **Correction, same day: it is not the screen socket - it is `assert_modes`.** The proposed
    "don't open the screen socket" experiment had already been run by `cfmoto_13` and `cfmoto_14`,
    which never sent SETUP at all and died anyway. Lining every capture up against the *kernel*
    clock (the `iw event` timestamp is userspace and lags ~60 ms):

    | capture | car's answer to `assert_modes` | deauth (kernel) | delta |
    | --- | --- | --- | --- |
    | `cfmoto_12` | 20:16:45.421940 | 20:16:45.457468 | **35.5 ms** |
    | `cfmoto_13` | 21:37:07.119807 | 21:37:07.149448 | **29.6 ms** |
    | `cfmoto_14` s1 | 00:08:36.645846 | 00:08:36.665462 | **19.6 ms** |
    | `cfmoto_14` s2 | 00:09:14.284908 | 00:09:14.307490 | **22.6 ms** |
    | `cfmoto_15` | 16:06:07.895420 | 16:06:07.926514 (`Unhandled alg`) | **31.1 ms** |
    | `cfmoto_16` | 23:06:46.167654 | 23:06:46.212410 | **44.8 ms** |

    **20-45 ms after the car answers the modes assertion, in every capture ever taken.** SETUP, the
    screen socket, RECORD and keyframe latency are all downstream of it and none of them matter -
    they are simply what the runs that got that far happened to be doing. The head unit accepts the
    `ModesChanged` request, replies 200 OK, and then deauthenticates the station.
  - **Ruled out as a signal:** the car repeats `0x4301` at 45.767, 3.0 s after the first and 0.5 s
    before the deauth. Tempting, but `iphone-cfmoto.pklg` shows the bike repeating `4301` at a real
    iPhone too - at 434.414 and 437.427, the same 3.0 s gap - which the phone just bare-acks. The
    repeat is normal.

- **2026-07-31: what carjack tells the bike about itself in `0x4300` does not match the iPhone.
  Three differences, from `iphone-cfmoto.pklg`.**
  - **Why this is the thread to pull:** `0x4300`
    (`RequestAccessoryWiFiConfigurationInformation`) is where the device tells the accessory *which
    station will join its AP*. If the bike cannot match the station it sees against what it was
    told, "I have no valid authentication for you" - reason 2 - is exactly the deauth it would
    send, and `assert_modes` (claiming the main screen as controller) is the first point at which
    it has any reason to check.
  - **The wire encoding was already correct.** `request_accessory_wifi_config_grouped` is pinned
    byte-for-byte against the iPhone by `wifi_config_request_matches_iphone`, whose test vector
    already uses a 24-character identifier and a lowercase MAC. The fault was entirely in the
    values `DeviceIdentity` was built from. Decoded side by side:

    | field | iPhone | carjack (before) |
    | --- | --- | --- |
    | group 0 identifier | `00008130001C29C40206001C` - 24-char UDID | `4C:79:6E:DA:29:4B` - the **Bluetooth MAC**, 17 chars |
    | group 1 identifier | `ac:df:a1:b3:87:9d` - **lowercase** | `FE:B2:9F:ED:EA:72` - **uppercase** |
    | that address | `ac:df:a1` - a real Apple OUI | locally administered, **NetworkManager-randomised** |

  - **The third one is a latent bug in its own right.** `get_wifi_mac` read the device's *current*
    address, but NM randomises per connection profile and carjack builds a **new in-memory profile
    every session** - so the address announced over Bluetooth before associating is not reliably
    the address that then associates. On this machine the permanent address is `4c:79:6e:da:29:47`
    while the device was carrying `fe:b2:9f:ed:ea:72` during the car runs and `1a:00:63:87:7a:49`
    afterwards on another profile.
  - **Fixed:** `transport_identifier` now emits the 24-character shape (zero-padded adapter
    address); the MAC is lowercased; `get_wifi_mac` returns `permanent_hw_address`; and the car's
    profile pins `802-11-wireless.assigned-mac-address = "permanent"` so the announced address is
    the one that associates. The D-Bus key name was confirmed against a throwaway NM profile rather
    than guessed - NM stores the keyfile's `cloned-mac-address` as `assigned-mac-address` over
    D-Bus. **Untested against the bike.**
  - **Also worth correcting:** `captures.md` says the iPhone sends `WirelessCarPlayUpdate`,
    `DeviceLanguageUpdate` and `NowPlayingUpdate` after the WiFi config. That is not in
    `iphone-cfmoto.pklg` - its iAP2 traffic ends at the second `4301` and its bare ack, 29 frames
    total. Whichever capture that came from, it is not this one.
  - **Not changed, but still a divergence:** carjack sends `0xEA00`
    (`StartExternalAccessoryProtocolSession`, `com.apple.carplay`); the iPhone sends none. It is
    3.5 s upstream of the deauth so it is not the trigger, but it is a state the bike never sees
    from a real phone.

- **2026-07-31: the session does not die at SETUP - the WiFi association does. `cfmoto_15` +
  `cfmoto_2.log`, desktop mode against the bike.**
  - **The SETUP-timing fix works.** carjack sends screen SETUP **12 ms** after the modes response
    (frame 563, 16:06:07.9075), so nothing sits between `assert_modes` and `setup_screen` any more.
    The AirPlay exchange is the same shape as `cfmoto_12` right up to the point it stops:

    | | `cfmoto_12` (reached RECORD) | `cfmoto_15` |
    | --- | --- | --- |
    | `assert_modes` req -> resp | 385 -> 182 | 385 -> 182 |
    | screen SETUP | 314 -> **225** | 314 -> **nothing** |
    | RECORD | 80 -> 73 | never sent |

  - **The car never even TCP-ACKs SETUP.** No ack, no RST, no FIN - the segment is retransmitted
    four times (t=08.12, 08.33, 08.74, 09.59) into silence. The car's last packet in either
    direction is a UDP timing reply at 16:06:07.919; after that, unanswered neighbour solicitations
    and DHCP Discovers. So the bytes went to a dead radio link, not to a CarPlay stack that
    rejected them.
  - **The journal names the cause**, 31 ms after the modes exchange:

    ```
    16:06:07.9265  iwlwifi 0000:00:14.3: Unhandled alg: 0x707
    16:06:09.7794  wlp0s20f3: deauthenticating from 92:83:8e:ef:a7:d3
                   by local choice (Reason: 2=PREV_AUTH_NOT_VALID)
    ```

    `Unhandled alg` is `iwl_mvm_rx_crypto` reporting an RX frame whose hardware-decrypt status it
    cannot classify (`0x700` = the security-encryption-error bits). mac80211 drops the association
    1.85 s later. carjack does not notice for another 9 s, when `setup_screen` finally gives up
    ("Request/channel timeout" at 06:06:18.9).
  - **This has happened on every bike run, including the one that "worked".** Checking the journal
    against each capture, every session ends in a reason-2 `PREV_AUTH_NOT_VALID` deauth at the
    moment the screen session starts:

    | capture | deauth | who sent it | relative to the session |
    | --- | --- | --- | --- |
    | `cfmoto_12` | 20:16:45 | **the AP** | RECORD 200 OK landed at 45.45 - it finished microseconds ahead |
    | `cfmoto_13` | 21:37:07 | **the AP** | at the modes exchange (07.116) |
    | `cfmoto_14` | 00:08:36, 00:09:14 | **the AP** | both sessions |
    | `cfmoto_15` | 16:06:09.78 | locally, after the decrypt error | 1.9 s after the modes exchange |

    So `cfmoto_12` was not a working configuration that later regressed - it was a race won by a
    hair. **The security association between the laptop and the head unit's AP breaks the instant
    CarPlay video is meant to start**, and it has been under every capture since. The SETUP-timing
    work fixed a real bug; it was not this one.
  - **carjack was riding a stale association.** The log says `Already connected to CFMOTO-999700;
    reusing the live connection`, because an on-disk NM profile
    (`CFMOTO-999700-8da53b88-....nmconnection`, mtime 2026-07-28 00:09, predating the in-memory
    change, `autoconnect: yes`) auto-activated at **16:05:23** - 39 s before carjack even started
    its session. Two consequences: the association was 46 s old rather than fresh (`cfmoto_12`, the
    run that got furthest, is the only one that freshly associated), and the
    `AddAndActivateConnection2` / `persist = "memory"` path has never actually run against the car.
  - **Changed:** deleted that profile. Also tried `options iwlwifi swcrypto=1` to take the failing
    hardware decrypt engine out of the path - **that does not exist as an option**: iwlmvm refuses
    it outright (`iwlmvm doesn't allow to disable HW crypto, check swcrypto module parameter`) and
    the error persists. Reverted. Replaced with `/etc/modprobe.d/iwlwifi-carplay.conf`:
    `disable_11ac=1` (this card already needs `disable_11ax=1` to negotiate rates with this AP, and
    a mis-handled A-MPDU/A-MSDU presents exactly as SEC_UNKNOWN) and `iwlmvm power_scheme=1` (CAM -
    buffered group-addressed frames across a DTIM/rekey are the other classic source). **Untested.**
  - **The status word decodes.** `0x707` is the full `iwl_rx_mpdu_desc` status: bits 0-2
    `CRC_OK | OVERRUN_OK | SRC_STA_FOUND`, and the security field `0x700` = **`SEC_UNKNOWN`**, with
    `KEY_VALID`, `ICV_OK` and `MIC_OK` all clear. So the frame arrived intact and from a known
    station, and the hardware could not work out which cipher applied to it - a protected frame
    with no matching key, not a corrupt one.
  - **A local module option can only ever fix the `cfmoto_15` variant.** In `12`/`13`/`14` the
    **AP** sent the deauth, which no driver tweak touches. And after the drop, the reassociation at
    16:06:11.78 sat in `4way_handshake` for the full 10 s and timed out with Bluetooth completely
    idle - the head unit's own AP is unstable at exactly that moment, which reads more like it
    restarting its AP/WPA state when the screen session starts than like anything local.
  - **Hardware note:** the card is a **CNVi** part (Intel 500-series, AX201), so Bluetooth and WiFi
    are the same silicon on the same antennas - and carjack holds a continuously busy BT ACL
    (iAP2 + HFP + A2DP + AVRCP, active mode, multi-slot, no sniff) across the whole session. No
    SCO/eSCO is ever set up (checked - the capture has no `Setup Synchronous Connection`), so it is
    ACL airtime only, but it is constant. `bt_coex_active=0` is the test for that, held back
    because turning coex off can degrade the Bluetooth link, which is the fragile half.
  - **Worth fixing in carjack regardless:** the radio died at 07.92 and the first sign in the log is
    a generic `Request/channel timeout` 11 s later. Watching NM's device state and logging "the
    car's WiFi link dropped" would have made this a 30-second diagnosis.

- **2026-07-28: a full session works on the AUTO BOX, 4/4 runs. Three faults, all required.**
  - **Method:** iterated `just test-run` directly against the bench unit (setcap disabled, so no
    human gate; `--source app` to skip the portal consent dialog, since the bug was below the video
    layer). The logs alone were enough - no capture needed.
  - **Fault 1 - the SDP record was per-session.** Fixed by `IapHost` (see the `bench_4` entry).
  - **Fault 2 - carjack committed to the first channel offered**, which is always its own accessory
    channel, which is always silent. Fixed by accepting all matching requests and racing
    `Iap2::start` across them. Every successful run since completes on `profile=device`.
  - **Fault 3 - and the one that had been masking everything:** the recovery path called
    `connect_profile(&IAP_ACCESSORY_UUID)` before `device.connect()`. `connect_profile` leaves a
    connection in progress, so **`connect()` failed `br-connection-busy` on every single retry** and
    HFP/A2DP never came up. Without them the unit never engages and never opens its channel. Log
    evidence, three runs in a row: `Reconnected the accessory profile iap=Ok(())` immediately
    followed by `Reconnected remaining profiles all=Err(... "br-connection-busy")`. Replacing the
    pair with a plain spawned `device.connect()` made the very next run complete the handshake.
    This one is mine: it was introduced answering "would `connect_profile` block less" - it does,
    but it also breaks the thing that matters.
  - **Also tried and discarded:** an 8s grace after disconnect to let the unit re-establish the link
    itself. Measured - the AUTO BOX never reconnects on its own (`Unit did not reconnect; paging it`
    every time), so it was pure delay. Removed.
  - **Result:** `iAP2 handshake completed ... profile=device` -> `Accessory identified: AUTO BOX` ->
    auth -> `ssid=carplay_wifi_6840` -> Bonjour finds `192.168.43.1:7000` -> `AirPlay bootstrap
    complete`, display 1080x600@30 -> `First keyframe after 7.98ms` -> `RECORD sent` -> 30.0 fps
    sustained, `HidSendReport` touch arriving. ~19s start to stream after trimming the dead first
    attempt with `IDLE_AFTER_FAILURE`.
  - **Untested:** the bike, and desktop mode.

- **Why carjack "can't connect to any profile" when the laptop initiates (2026-07-28): it is not
  profiles, it is Page Timeout. Two changes.**
  - **The question:** why does it only work when the bike starts the connection?
  - **The measurement**, across every bike capture:

    | capture | our pages | Page Timeouts (`status=0x04`) | bike-initiated | `FF 55` on |
    | --- | --- | --- | --- | --- |
    | `cfmoto_11` | 2 | **0** | 1 | dlci `0x04` - carjack's own outbound ch 2 |
    | `cfmoto_12` | 4 | **4** | 2 | dlci `0x05` - carjack's own outbound ch 2 |
    | `cfmoto_13` | 3 | **3** | 2 | none |
    | `cfmoto_14` | 3 | **2** | 1 | dlci `0x02` - the bike's inbound ch 1 |

    **9 of 12 outbound pages were never answered.** Page Timeout is a baseband
    outcome - the bike's controller did not respond - so SDP, profile resolution and RFCOMM were
    never reached. Every "carjack can't connect to the profile" reading of this was wrong.
  - **The converse proves the stack is fine:** `cfmoto_11` had **zero** page timeouts, and iAP2 then
    ran over dlci `0x04` - carjack's *own outbound* channel to the bike's accessory service. Laptop
    page, laptop-opened iAP channel, complete session. When the page lands, everything above works.
  - **Also settles an earlier claim:** the bike has never rejected carjack on the accessory profile.
    There is not one `Rcvd DM` in any capture, and `cfmoto_12` shows
    `Sent SABM Channel=2` -> `Rcvd UA` -> the bike sending `FF 55` on that channel. The outbound
    path is real and works; it arrives via `auto_connect: Some(true)`, not `connect_profile` (which
    is not in the code).
  - **Changed:**
    1. The unconditional `device.disconnect()` at session start is **removed entirely**. Connected:
       dropping a healthy link is what puts the bike out of page-scan and into paging us. Not
       connected: there is nothing to drop, and bluer documents that `disconnect()` "can be also
       used to cancel a preceding Connect call before a reply to it has been received" - so on an
       idle device it could only cancel a connection in flight, including one the *bike* was
       making. All three `Disconnect`s in the `cfmoto_14` run returned `Ok(())`, so the
       not-connected branch was never even exercised. The retry loop still disconnects on a genuine
       failure, which is what the original call was for.
    2. `connect_accessory` logs the page outcome explicitly - "Outbound page succeeded" with timing,
       or "Outbound page timed out - the head unit is not page-scanning". Counting those two lines
       is now a one-glance check that previously needed HCI from a capture.
  - **What this does not explain:** why the bike's page-scan window opens when it does. It does not
    correlate simply with time since our disconnect (`cfmoto_11` paged successfully 2.8s after one;
    `cfmoto_14` timed out 16s after one). Both changes are untested against the unit.

- **The "stuck waiting for iAP2 switch" investigation (2026-07-28). I proposed a livelock and was
  wrong about the mechanism; the HCI layer refuted it. Recording both, because the refutation is
  the useful part.**
  - **The hypothesis:** carjack disconnects and pages out in the same millisecond, wins its own
    race, and ends up on a channel the bike will never drive - because "the bike only drives iAP2
    on a connection it originated" (`cfmoto_6`: "zero ch1 bytes"). The log seemed to fit: session 1
    had `Attempting to connect accessory` at 14:07:59.790, `Got connect request` 14:08:01.564, and
    `Accessory connection r=Ok(())` 14:08:01.743.
  - **Refuted, by `cfmoto_14` session 2 - which worked end to end.** HCI:
    `Sent Create Connection` 00:09:08.087 -> `Connect Complete status=0x00` 00:09:08.373. **Our own
    page created that ACL.** The bike then sent `Rcvd SABM Channel=1` at 00:09:09.818 on it, `FF 55`
    248ms later, and the full handshake plus AirPlay to RECORD followed. So the unit is perfectly
    willing to speak iAP2 over a carjack-originated ACL, and the `cfmoto_6` conclusion is at best
    too strong.
  - **The reasoning error, worth naming:** `Accessory connection r=Ok(())` was read as "our page
    succeeded". It is not. `connect_accessory` resolves Ok just as readily when the unit connected
    first and our in-flight `device.connect()` merely completed. Session 2 proves it: our page
    returned `br-connection-page-timeout` **twice** (`Connect Complete status=0x04` at 00:08:59.957
    and 00:09:06.083), and the eventual Ok arrived **7ms after** an inbound connect request.
    Session 1's Ok, 179ms after its request, has exactly that shape and is no evidence either way.
    **Only HCI answers "who paged": `Sent Create Connection` vs `Rcvd Connect Request`, plus the
    `Connect Complete` status byte.**
  - **Reverted:** the `CAR_FIRST_MOVE_GRACE` delay before paging. It rested entirely on the wrong
    premise, and would have added 5s to every connection - including the ones where *our* page is
    what wakes the unit, which session 2 shows is a real case.
  - **Kept, because each stands on its own:**
    1. `SWITCH_TIMEOUT` 3s, single attempt (was 12s x 3 = ~38s). Re-reading a mute channel cannot
       make it speak; the bike sends `FF 55` in 268ms and the AUTO BOX repeats every ~1s, so 3s has
       ~10x margin and failing fast is what lets the caller go get a different channel.
    2. ~~`perform_switch` dispatches on the **2-byte** magic~~ - built, then **reverted the same day
       after testing against the unit: an accessory that does not send the switch never sends a SYN
       either.** The `FF 5A`-first state it was written for does not occur, so the dispatch (and the
       `FramedParts.read_buf` seeding behind it) was removed and the plain six-byte read restored.
       Recorded because it was a plausible-looking fix for a case that simply is not real - the
       "don't redo the switch on reconnect" idea has now been closed off by measurement rather than
       argument.
    3. Inbound requests log which `ProfileKind` they arrived on.
  - **Still unexplained, and now the open question:** what distinguishes session 1's channel from
    session 2's. Both arrived as inbound RFCOMM requests from the configured unit; session 1's
    carried no `FF 55` at all, session 2's carried it in 248ms. The Bluetooth link was already up
    before carjack launched in session 1, so the switch may have been sent before anything was
    listening - but that is a hypothesis, not a finding, and session 1's channel opened before the
    capture started so it cannot be checked from `cfmoto_14`. **Next run: capture from before
    carjack starts.**

- **`cfmoto_14.pcapng` + `cfmoto.log` (2026-07-27): the first run with carjack's own logs
  alongside a capture, and they answer nearly everything. Three separate faults, three fixes.**
  - **Get the logs. Two of these were invisible in the captures.** Both previous diagnoses were
    made blind, and one was wrong. `cfmoto.log` settled all of it in about a minute.
  - **The car is 800x1280 @ 30fps - portrait.** Worth knowing; the desktop is 3456x2160 landscape.
  - **Fault 1: a cold GStreamer registry, spent inside the session.** Session 1 asserted modes at
    14:08:36.646 and then produced *nothing* for 4.3s - the gate's new per-second warnings fire
    at 1s, 2s, 3s, 4s with `0 delta frames` - until `desktop renders on GPU driver i915` finally
    appears at 14:08:40.975. **First keyframe after 4.92s.** Session 2, with the registry warm,
    logged the same line 1.7ms after the session started and **First keyframe after 296ms**. The
    stderr log shows why: a `gst-plugin-scanner` child enumerating msdk/VA/nvcodec. In desktop
    mode nothing called `gst::init()` until the first encoder task, so the whole registry build
    landed inside a session the head unit gives ~2s. **Fixed:** `run_desktop` now calls
    `encode::init_gstreamer` (new) on a blocking thread at startup. App mode already got this via
    `resolve_backend`.
  - **Fault 2: the keyframe gate was in the wrong place - my own regression.** Even at 296ms,
    session 2 never sent SETUP. The capture shows the control channel stopping dead at the
    `385→182` modes exchange (t=71.027), which is **the car's last packet in the entire capture**;
    from t=73.10 our neighbour solicitations go unanswered and DHCP Discovers get no offer, so the
    SETUP bytes never reached the wire at all. Across four captures the pattern is exact:
    `cfmoto_12` sent SETUP **2ms** after the modes assertion and the car went on to answer RECORD;
    `cfmoto_13` (never), `cfmoto_14` s1 (4.9s) and s2 (296ms) all lost the unit within ~2s of the
    assertion. **Fixed:** `wait_for_first_keyframe` moved back to where it was first designed to
    go - after `setup_screen`, before `record`. Nothing may sit between `assert_modes` and SETUP.
  - **Fault 3: carjack streamed to the wrong head unit.** The log is unambiguous: targeting
    `CFMOTO-G2Y70E` (B6:5D:B5:04:8E:FC), the bike's link failed at 14:09:29.838, carjack went back
    to `Waiting for iAP2 connection requests`, and **58 seconds later** something else paged it -
    `14:10:30.467 Accessory identified: AUTO BOX`, followed by a complete handshake, that unit's
    own credentials (`ssid=carplay_wifi_6840`) and a NetworkManager switch onto its AP.
    `attempt_iap2_link` accepted whatever arrived on the profile stream and never compared it to
    `ctx.device`, which was sitting right there. Our iAP profiles are registered for the whole
    adapter, so any bonded unit that pages us landed in that accept. **Fixed:** compare
    `rq.device()` against the configured target and `reject()` anything else. Inbound still has to
    be accepted in general - the bike requires a car-originated ACL (`cfmoto_6`).
  - **Still open:** whether a session survives past RECORD. Every capture so far has lost the unit
    before or at that point, and the bike's own link kept failing early in this run too
    (`Timed out waiting for iAP2 switch command` twice, then `Connection reset by peer`) - which
    is the aggressive-reconnect state, so **power-cycle before reading much into a run**.

- **`cfmoto_13.pcapng` (2026-07-27): not a repeat of `cfmoto_12`, and it **supersedes the
  mechanism** in the entry below. Two things: the encode pipeline is not slow (measured), and this
  run never did iAP2 with the bike at all.**
  - **The correction.** The entry below dates "first frame ready" in `cfmoto_12` to the neighbour
    solicitation at t=64.118, ~2.1s after the encoder spawn, and concludes the pipeline warm-up
    loses the watchdog race by ~100ms. **That inference was wrong.** There is a gratuitous ARP
    announcement at t=64.343 immediately next to it, which is what NetworkManager sends when it
    reconfigures an address - so that NS burst is a link flap flushing the neighbour cache, not
    carjack having video to send. The only solid fact in `cfmoto_12` is that the first frame hit
    the socket at t=78.133, ~16s after RECORD.
  - **Measured, so it stops being a guess.** `tests/encode_startup.rs` (new) drives the real
    `run_session` sequence - compositor start, `reconfigure`, encoder spawn - and times the first
    keyframe: **66ms** on a no-op reconfigure, **40ms** on a real resize, **34ms** when the car
    reports `max_fps=0`. `tests/desktop.rs` now reports the same number for the portal/PipeWire
    path: **658ms**. Neither is anywhere near the ~2s budget. **The encoder is not the
    bottleneck in either mode**, and "warm-up loses the race" is false.
  - **What this capture actually shows.** The AirPlay half is byte-identical to `cfmoto_12`
    through `assert_modes` (the `385→182` exchange) at t=20.970. Then it stops: no screen SETUP,
    no RECORD, and only one data socket (the event channel at 44051) - the screen socket is never
    created. That is exactly where `wait_for_first_keyframe` sits, so **the gate blocked and never
    released**. The car sent RFCOMM DISC at t=23.252 (reason `0x13`), 2.3s later. One SYN to port
    7000 in the whole 181s capture - no retry.
  - **So the watchdog is not specifically about RECORD.** `cfmoto_12`: 2.0s after RECORD.
    `cfmoto_13`: 2.3s after `assert_modes`. The unit gives up ~2s after carjack stops making
    progress, whatever the last step was.
  - **But this run never spoke iAP2 to the bike.** The only RFCOMM DLCIs present are `0x00`
    (control) and `0x1a` (HFP). **Zero `FF 55`, zero `FF 5A`, no DLCI `0x05`** - where
    `cfmoto_12` has the complete handshake. HFP comes up with `b6:5d:b5:04:8e:fc` (the bike) and
    nothing else does. This is the signature of the documented "aggressive reconnect" state, and
    the same shape as `cfmoto_6` (channel opened, no `FF 55`): **power-cycle the head unit.**
  - **And the config points somewhere else.** `.env` has `CARJACK.BLUETOOTH.DEVICE=CAR-BT_AB17`
    with the `CFMOTO-G2Y70E` line commented out, plus `CARJACK.SOURCE=desktop`. carjack connects
    to exactly the configured device and there is no fallback, so if that run targeted the bench
    unit it would never have started iAP2 with the bike - the HFP traffic here is just BlueZ's
    normal bonded-device behaviour. **Confirm what was configured before reading anything else
    into this capture.**
  - **Changed as a result:** the gate now logs a warning every second it waits (a silent 10s stall
    is the hardest thing to debug from a capture - this one logged nothing at all), and its
    timeout error names the likely culprit per source mode. The two timing tests above are new.
  - **Still unknown:** why the video source produced nothing in this run. The capture cannot say,
    and carjack's console output was not kept (`just test-run` does not save a log). Needs a rerun
    on a power-cycled unit with the console output captured.

- **`cfmoto_12.pcapng` (2026-07-27): the furthest carjack has ever got - AirPlay connects, pairs
  and reaches RECORD - and then the bike tears the whole session down two seconds later because
  we sent it no video. We lose the car's no-video watchdog by about 100 ms. Fixed by holding
  RECORD until the encoder has produced a keyframe.**
  - **Everything the 2026-07-27 fixes were written for works.** The iAP2 handshake is clean in
    ~1.1 s under the new lock-step link layer (SYN → identification → `AA00`/`AA06`/`AA02`/`AA03`/
    `AA05` → `4300`/`4301` with SSID `CFMOTO-999700`). The AirPlay connect no longer races DAD:
    at t=61.719 a neighbour solicitation for `fe80::9083:8eff:feef:a7d3` gets an advertisement,
    and the TCP SYN to port 7000 follows 70 µs later. **This is the first capture with any TCP in
    it at all.**
  - **The HomeKit question is answered: `remote_homekit_id: None` is fine.** `/setup` → 401 →
    `pair-setup` (CSeq 1-3) → `pair-verify` (CSeq 4-5) all return 200, and the connection goes
    encrypted from CSeq 6. The open question about needing a Bonjour pairing id for a
    pair-verify fast path can be closed.
  - **The session is fully established.** Screen SETUP completes, the UDP timing channel
    exchanges five 40-byte round trips (43511 ↔ 54534), both data sockets open
    (`52680→44195` at t=61.974, `39598→35907` at t=62.006), and **RECORD is answered 200 OK at
    t=62.011**.
  - **Then carjack sends nothing, and the car leaves.** Not one byte is ever written to the
    screen socket. From t=62.05 to t=64.11 there is no traffic on the wire at all - no data, no
    ARP, no neighbour solicitation. At **t=63.992 the accessory sends RFCOMM DISC**, tears down
    HFP/AVDTP/A2DP, and the ACL drops at t=64.314 with reason `0x13` (remote user terminated).
    Its AP goes with it: ten neighbour solicitations from t=64.118 to t=73.525 go unanswered,
    DHCP Discovers at t=66.05 and t=70.82 get no offer, and the unit only comes back at t=75.9
    (re-association, DHCP ACK, and the car re-paging us over Bluetooth).
  - **The timing is the whole story.** RECORD answered at t=62.011; car gives up at t=63.992
    (1.98 s later); carjack's first video frame is ready at t=64.118 (2.11 s later). The first
    frame's arrival is dated by the neighbour solicitation at t=64.118 - the first moment we had
    anything to send - and there is no NS or data between t=61.72 and then. The single 1428-byte
    segment that finally appears at t=78.133 is that same frame's queued retransmit going out
    once the neighbour resolves at t=77.609, into a session that is long dead: RST at t=78.134,
    and a RST on the control socket at t=78.646.
  - **It is the video watchdog, not `ea00` and not the EA/iAP2 layer.** The obvious other suspect
    was the `ea00` (`StartExternalAccessoryProtocolSession`) that we send and a real iPhone never
    does. `cfmoto_11.pcapng` rules it out: there `ea00` goes out at t=9.047, is acked at t=9.092,
    and the car keeps Bluetooth up until t=18.874 - **9.8 seconds**, cheerfully repeating `4301`
    twice in the meantime. The only difference in `cfmoto_12` is that we reached RECORD. So the
    teardown is a post-RECORD no-video timeout of roughly two seconds.
  - **Root cause.** `run_session` builds the entire GStreamer pipeline - `gst::init()`, element
    construction, backend build, the state change to PLAYING - and spawns it about 40 ms before
    `run_video_stream` sends RECORD. That warm-up plus the first IDR costs ~2.1 s, and it was
    being spent entirely inside the car's watchdog window. The comment at the frame drain
    ("running and buffering since before AirPlay setup completed") described a head start that
    did not exist: `setup_screen` and `record` happen *inside* `run_video_stream`, milliseconds
    after the spawn, so the drain never had anything to drain either.
  - **Fixed** by `wait_for_first_keyframe`, called before `setup_screen`: drain anything stale
    (asking for a fresh IDR if we dropped something, rather than sitting out the encoder's ~1 s
    keyframe interval), wait for a frame carrying an AVCC config, and only then SETUP, RECORD and
    push that frame. RECORD-to-first-frame is now a few milliseconds regardless of warm-up cost.
    A 10 s timeout means a dead encoder fails the session instead of leaving an AirPlay
    connection open that will never carry video. Three unit tests cover it.
  - **Not done, deliberately:** moving the encode pipeline to launch time (next to the compositor
    and `EncodeBackend`) would hide the warm-up entirely, but the appsrc latches its caps once, so
    the pipeline has to be built after `reconfigure` has settled the output size. With RECORD
    gated on a real keyframe the warm-up no longer causes a failure, only a slower start.
  - **Untested against the bike.** The fix is unit-tested and builds; nobody has watched it
    complete a session on the unit.

- **`bench_2.pcapng` (2026-07-27): the AUTO BOX stopped working, and it is a regression from the
  link-layer rewrite - two bugs, both in the SYN handshake, both mine. Fixed, with the test that
  should have caught them.**
  - **What the capture shows.** Bluetooth connects, HFP runs, RFCOMM channel 1 opens. At t=14.29
    the switch command goes both ways, then the unit sends its SYN
    (`ff5a 0017 80 a8 0000 68 | 01 7fffff 07d0 03e8 1e 06 01 00 02`) - seq `0xa8`, and a
    parameter set very different from the bike's: `max_outstanding=127, max_length=65535,
    retx=2000ms, cumack=1000ms, max_retx=30, max_cumack=6`, **control session id 1**.
  - **Bug 1: our SYN+ACK carried `ack = 0x00`, not `0xa8`.** `ff5a 0017 c0 9d 00 00 33 ...`. The
    old code got this right - `cfmoto_11.pcapng` frame 821 shows `c0 9d **2a** 00` against a SYN
    at seq `0x2a`. The rewrite's `receive_packet` returned SYN packets early, *before* the call
    that advances the receive sequence, with a comment claiming a SYN was "the handshake's
    business" and shouldn't touch it. It is a numbered packet like any other and its number is
    exactly what the SYN+ACK has to acknowledge.
  - **Bug 2: the handshake could never see its own third leg.** The rewrite made `receive_packet`
    swallow bare acknowledgements as link-layer noise - and the third leg of the SYN handshake
    *is* a bare acknowledgement (`ff5a 0009 40 a8 9d 00`, visible at t=14.387). `Syn` was calling
    that same filtered receive, so it sat waiting for a packet that would never be handed to it.
  - **How it ends:** each SYN retransmit from the unit arrives where `Syn` expected the ack, so
    `perform_inner` errors and retries - our SYN+ACK goes out again as seq `0x9e`, then `0x9f`.
    After the retries are exhausted the unit gives up on iAP2 and falls back to sending `FF 55`
    every ~1 s from t=19.5 to the end of the capture.
  - **Fixed** by splitting the receive path in two. `receive_raw` does the link-layer bookkeeping
    and returns *every* packet, bare acks and SYNs included; `receive_packet` is the session-level
    filter layered on top. `Syn` uses `receive_raw`. Anything carrying a payload advances the
    receive sequence, **including the SYN**.
  - **Why nothing caught it:** there was no test that drove `Syn::perform` at all. The link tests
    all used `Iap2::wrap` directly and skipped the handshake. There is now one
    (`syn_handshake_acknowledges_the_accessorys_sequence`) that replays this capture's SYN bytes
    through the real handshake and asserts both the ack field and the negotiated parameters; it
    was verified to fail on each bug independently before the fix went in.
  - **Worth internalising:** the two head units negotiate almost nothing in common (window 1 vs
    127, control session 2 vs 1, `max_cumack` 0 vs 6). The bench unit is not a weaker version of
    the bike - it is the better regression test for anything touching the link layer, because it
    exercises the opposite end of every parameter.

- **iAP2 link-layer + video flow-control review (2026-07-27). Nine gaps found against what the
  SYN actually negotiates and what the car asks for over AirPlay; all nine addressed. Builds and
  tests pass; none of it is retested against the bike yet.**
  - The car's SYN decodes as `version=1, max_outstanding=1, max_length=4096, retx_timeout=1000ms,
    cumack_timeout=500ms, max_retx=15, max_cumack=0, session{id=2, Control, v2}`. Every field
    except the session table was parsed and thrown away: `Syn::perform_inner` echoed the SYN back
    verbatim (so we *agreed* to all of it) and returned only a `SessionTable`.
  - **What was wrong, and what changed:**
    1. **Window violated.** We sent `aa05`/`4e09`/`4300` inside 0.3 ms against a window of one
       (`cfmoto_11.pcapng` frames 851-853). Now `LinkState` tracks in-flight packets and
       `send_packet` *queues* past the window. It deliberately does **not** block: the session
       dispatches replies from inside its own receive loop, so waiting for an ack there would
       stop the loop reading the very ack it waits for - a guaranteed deadlock at window 1.
    2. **`ForceKeyFrame` discarded.** `handle_car_commands` matched only `ChangeModes` and
       `HidSendReport`; everything else got `respond_ok()` and no action. Now forwarded to the
       encoder.
    3. **`NeedsKeyframe` only logged**, so we kept pushing deltas catplay was throwing away until
       the next natural IDR (up to 1s at `key-int-max=30`). Now requests a keyframe and holds off
       until it arrives.
    4. **No backpressure.** `push_frame` *blocks* when the car's queue is full, which converts a
       backlog into latency. Now drops the frame and forces a keyframe (every later delta
       references the dropped one), counted in the streaming heartbeat.
    5. **No retransmission** (`retx_timeout`/`max_retx` unused). Now retransmits from `maintain`,
       and `RetransmitsExhausted` ends the session instead of retransmitting into the void.
    6. **No cumulative-ack timer** - acks only piggybacked or rode the hardcoded 1s keepalive,
       i.e. up to double the 500ms we agreed to. Now timer-driven. **Note `max_cumack = 0` means
       "no limit", not "ack every packet"**: reading it the other way would have put a bare ack
       before every reply and changed the handshake's shape on the wire for no benefit.
    7. **Naive ack tracking** - `self.ack.store(packet.header.seq)` unconditionally, no
       next-expected sequence, no duplicate detection. Now `accept_rx` dedupes (a duplicate is
       re-acked, not dispatched twice - it would have replayed the message).
    8. **`RST` ignored and `Reset` dead code** (unconstructable, no public ctor). Incoming `rst`
       now surfaces as `Iap2Error::LinkReset`; `ops/reset.rs` is replaced by `Iap2::send_reset`,
       sent on teardown so the accessory can re-SYN instead of waiting out its own timeout.
       `EAK` is now consumed too; `sleep` is logged.
    9. **`max_length` unenforced.** Now checked before build; oversize is an error rather than an
       accessory buffer overrun.
  - **The subtle one worth not regressing:** acks retire in-flight packets by a *window-bounded*
    comparison, not "is this seq at or before the ack". Sequence numbers wrap in a `u8`, and an
    accessory that has not heard from us yet sends `ack = 0` - which under a naive half-space
    test "covers" our initial `0x9D` and retires a packet that was never acknowledged.
    `an_unrelated_ack_retires_nothing` pins this.
  - Keyframes reach the encoder as an **upstream force-key-unit event on the encoder's src pad**,
    which every backend handles - so no per-`HW_BACKENDS`-row special-casing. Requests are
    deduplicated while one is outstanding, or a sustained stall would pin the encoder to an
    all-IDR stream and worsen the congestion it is reacting to.
  - Tests: iap2-proto is up to 31 (link params from the real SYN bytes, window fill/drain, the
    wrapping-ack case, dedup, EAK, retransmit timing and exhaustion against a paused clock, plus
    duplex-backed integration tests for queueing, oversize rejection, reset and duplicates).
  - **Regression risk to watch on the bike:** the handshake is now lock-step where it used to
    burst, and a reply that previously went out immediately may now be queued behind an ack. If
    the car behaves worse than `cfmoto_11`, `max_outstanding` enforcement is the first thing to
    disable (`LinkParams::max_outstanding`).

- **`cfmoto_11.pcapng` (2026-07-26 22:43, BT + WiFi in one capture): the whole iAP2 handshake now
  runs clean in ~1s and the car hands over credentials — but the AirPlay connect dies instantly,
  in a race with IPv6 duplicate-address detection. Fixed; not yet retested against the bike.**
  - **Everything up to the handover is healthy.** At t=8.24 the switch command, then SYN →
    identification → `aa00`/`aa06`/`aa02`/`aa03`/`aa05` auth → `4300` → `4301` at t=9.05. Sequence
    numbers behave (keepalives repeat `0xa6` without consuming a number), the car acks everything,
    and `ea00` fires exactly once. The `0x4301` TLVs decode as expected: ssid `CFMOTO-999700`,
    passphrase `12345678`, channel 157, address `fe80::9083:8eff:feef:a7d3`, port 7000.
  - **The failure, to the millisecond:**

        16.348  DHCP ACK           IPv4 up (us .152, car .94)
        16.471  ARP announce       NetworkManager marks the connection ACTIVATED
        16.576  NS from ::         DAD starts on our new fe80::d819:2e6:a304:3de0 (TENTATIVE)
        16.587  Sent DISC Ch=2     carjack tears the iAP2 RFCOMM channel down   <- 11 ms later
        17.584  MLD from fe80::…   DAD done, the address is finally usable

    `connect_to_car_wifi` returns as soon as **IPv4** finishes — NM does not wait for IPv6. carjack
    then connects straight to the car's **link-local** address, but our own link-local is still
    tentative for another ~1s, and the kernel will not select a tentative address as a source for a
    link-local destination. `connect()` fails immediately with `EADDRNOTAVAIL`, which is why there
    is **not one TCP packet and not one neighbour solicitation for `fe80::9083:8eff:feef:a7d3` in
    the whole capture** — the connect never reached the wire.
  - Then `?` on `connect_airplay` propagates, `AbortOnDropHandle` drops the carplay task → RFCOMM
    DISC at 16.587 → full ACL teardown at 18.87 → `Create Connection` at 18.89 → a fresh SDP/AVRCP
    handshake at 21.7–22.9 as the session loop restarts. One transient connect error costs an
    entire Bluetooth reconnect cycle.
  - It was definitely the connect and not WiFi setup: `connect_to_car_wifi` wraps everything in
    `.retry()`, so a fast error there would show up as four activations over ~6s (four DHCP
    cycles); there is exactly one. Everything between it and `connect_airplay` is infallible.
  - **So the `cfmoto_10` fix was right, it just fired ~1s too early.** The old gateway-guess path
    burned 15s probing first, which incidentally hid this.
  - **Fixed, three parts:**
    1. `wifi_connect::wait_for_link_local` polls **rtnetlink** (`RTM_GETADDR`, filtered to the
       interface index) until a `fe80::/10` address on the car's interface is neither `Tentative`
       nor `Dadfailed` — 250 ms × 16, so ~4s of headroom against a 1s DAD. The full 32-bit flag set
       arrives in the `IFA_FLAGS` **attribute**; the header byte only holds the low eight bits, so
       the attribute wins wherever both are present (verified against a live kernel dump: every
       IPv6 address carries `Flags(...)`, and the settled link-local reads `Permanent |
       Noprefixroute`).
    2. **IPv4 gateway fallback.** On this unit the AirPlay receiver *is* the AP — the `0x4301`
       link-local and the DHCP gateway resolve to the same MAC (`92:83:8e:ef:a7:d3` =
       `192.168.199.94`) — so the gateway is the same endpoint by another route, and one needing no
       link-local source at all. `establish_carplay` now builds a best-first candidate list
       (car-named address, then gateway) instead of a single address.
    3. **Retry.** `connect_airplay_endpoints` retries each candidate with `backon`
       (`ConstantBuilder`, 500 ms × 4) before falling through to the next, so a transient failure no
       longer costs a Bluetooth reconnect. The car repeats `0x4301` for ~15s, so there is budget.
    4. **Don't reconnect to an AP we are already on.** `connect_to_car_wifi` now checks the live
       connection first (`live_connection`) and reuses it when the SSID already matches and has an
       address, instead of activating a new profile over the top. This is not just a saving: the
       host in `cfmoto_11.pcapng` was *already* associated to `CFMOTO-999700` from t=1.2, and
       reactivating at t=16.3 is what generated the new link-local (`fe80::37bd:…` →
       `fe80::d819:…`) and opened the DAD window in the first place. `ActiveWifiConnection` carries
       no D-Bus path, so the `ActiveConnection` path comes from the device via
       `GetDeviceByIpIface` + `Device.ActiveConnection`.
  - Tests: four unit tests over the flag/address parsing (`IFA_FLAGS` beating the header byte,
    tentative/dadfailed rejection, IPv4 and global-scope v6 ignored) plus an `#[ignore]`d live check
    (`CARJACK_TEST_IFACE=… cargo test --bin carjack -- --ignored`) that runs the real netlink dump, and
    another (`CARJACK_TEST_SSID=…`) that checks the reuse path against live NetworkManager - both
    read-only, neither activates or tears down a connection.
- **`cfmoto_10.pcapng` (2026-07-26, BT + WiFi in one capture): the link layer is FIXED and the
  whole iAP2 handshake is now clean. What is left is a standoff on WiFi — the car waits for us
  to connect to the AirPlay address it gave us, and we wait for it to invite us.**
  - **The sequence fix works.** Keepalives repeat seq `0xa6` without consuming a number, and the
    car acknowledges every packet from then on (`a6`→`a7`→`a8`…, its own seq advancing
    `0x62`→`0x69`). No more frozen ack, no more deadlock. The full chain runs in 0.7 s and the
    car hands over credentials in `0x4301`.
  - **The `0x4301` repeat is normal, not a fault.** `iphone-cfmoto.pklg` shows the car sending
    it twice ~3 s apart and the iPhone answering neither - it just bare-acks. The iPhone's iAP2
    traffic simply *ends* at `4300`→`4301`. We were answering every repeat with another
    `ea00` (`start_ea_session`), a message the iPhone never sends at all, eight times over.
  - **The standoff, from the WiFi half of the same capture:** we join the car's AP at t=94.8
    (DHCP `192.168.55.152`, car at `.85`), advertise `_carplay-ctrl._tcp` from t=97.1, and the
    car **ARPs us at t=100** (`Who has 192.168.55.152? Tell 192.168.55.85`) - so it knows
    exactly where we are. Then nothing: it never browses mDNS, never sends the
    `ctrl-int/1/connect` invite. We send it nothing either - **zero packets to its address or
    to port 7000 in the entire capture**. `wait_for_carplay_ctrl_probe` burns its full 15 s.
  - **And the fallback aimed at the wrong host.** `guess_gateway()` rewrites the last octet to
    `.1`, but this unit is its own DHCP server and gateway at `.85`. carjack ARPs
    `192.168.55.1` three times (t=112.1, 113.2, 114.2), never gets a reply, never emits a SYN,
    and tears down RFCOMM at t=114.15.
  - **Fixed by using the address the car already gave us.** `0x4301` carries the AirPlay
    receiver's address and port (`fe80::9083:8eff:feef:a7d3`, 7000) and we were parsing both
    and throwing them away - `WifiCredentials` only had `{ssid, passphrase, bssid}`. It now
    carries `airplay: Option<SocketAddr>`, and `establish_carplay` connects straight there when
    present, skipping the probe entirely. `with_scope_id` stamps the WiFi interface's index
    into link-local addresses (`fe80::/10` is unusable without a scope id on a multi-interface
    host). The older flat `0x5703` carries no address, so **the AUTO BOX keeps the mDNS
    discovery path unchanged**.
  - `start_ea_session` now fires once per session instead of once per `0x4301` repeat.
  - Tests: the `0x4301` parse test now pins the IPv6 address, and two tests cover `with_scope_id`
    (link-local gets the index; routable/already-scoped/unknown-interface are left alone).
  - Not yet retested against the bike. **The open question is whether connecting out is enough
    on its own** - we pass `remote_homekit_id: None` on this path, where the ctrl-probe path
    would have supplied a pairing id from the Bonjour record for a pair-verify fast path. If
    the car rejects the connection, that is the first thing to look at.
- **`cfmoto_9.pcapng` + `cfmoto_wifi_9.pcapng` (2026-07-26): the packet-length fix landed, but
  the link still deadlocks — our keepalives were also *consuming a sequence number*, which
  acknowledgements must not do. WiFi confirms carjack gets all the way to waiting for an
  invite that the car will never send while Bluetooth is stuck.**
  - The length fix is confirmed on the wire: keepalives are now `ff5a 0009 …`, 9 bytes,
    correctly declared (frame 644). But the symptom is unchanged — the car's cumulative ack
    freezes at `0xa6` and it retransmits `0x4301` nine times before giving up.
  - **Cause: `send_packet` incremented `seq` for every packet, including empty ones.** A
    packet with no payload is a pure acknowledgement and does **not** take a sequence number.
    `iphone-cfmoto.pklg` settles it from both sides: the phone sends data as seq `0x47`, then
    acknowledges two separate messages with seq `0x47` again (frames 30961, 31381); the car
    likewise repeats `0x78` across three acks before moving on to `0x79`. So an ack repeats
    the **last data packet's** number.
  - What our version did: `EA00` at seq `0xa6` (acked fine), then three 1 s keepalives took us
    to `0xa7`, `0xa8`, `0xa9`, and the next real message went out as `0xaa`. The accessory was
    waiting for `0xa7`, saw a gap it could never have filled, and ignored everything from there
    on. **The keepalive - added to stop the link timing out - is what was killing the link.**
  - **Fixed** in `iap2-proto/src/iap2.rs`: `send_packet` only advances `seq` when there is a
    payload, and `make_packet` gives an empty packet the previous number
    (`seq.wrapping_sub(1)`) so it repeats the last data packet rather than pre-announcing the
    next. Two `#[tokio::test]`s over a `tokio::io::duplex` pair pin the resulting sequence
    (`Iap2::wrap` was factored out of `start` so a test can skip the switch/SYN handshake).
  - **`cfmoto_wifi_9.pcapng` says the WiFi side is healthy and blocked on Bluetooth**, not
    broken on its own: we associate to the car's AP, complete DHCP (`192.168.212.152` from the
    car at `.138`), and advertise `_carplay-ctrl._tcp` 84 times - then nothing. **Zero TCP
    packets in the entire capture**, and no neighbour solicitation for the AirPlay address the
    car gave us. The car never sends the `ctrl-int/1/connect` invite that
    `wait_for_carplay_ctrl_probe` waits 15 s for, which is exactly what you would expect while
    its iAP2 state machine is still stuck re-sending `0x4301`. (Caveat: the two captures cover
    different time windows - BT 16:39:19-16:40:15, WiFi 16:41:57-16:42:27 - so they are
    consecutive runs of the same failure, not one synchronised recording.)
  - Not yet retested against the bike.
- **`cfmoto_8.pcapng` (2026-07-26): auth and the WiFi handoff BOTH work on the bike now. The
  stall moved to the link layer — our keepalive packet declares the wrong length and destroys
  the accessory's framing.**
  - How far it gets, all in ~0.7 s: SYN → identification → `AA00`/`AA06`/`AA01` → **32-byte
    `AA02` challenge answered with a real 64-byte signature** (frame 621 — the cert-driven
    length is right) → `AA05` → `0x4E09` ("carjack") → **`0x4300`** → and at t=29.825 the car
    answers **`0x4301` with the actual credentials**: SSID `CFMOTO-999700`, passphrase
    `12345678`, channel 157, IPv6 `fe80::9083:8eff:…`. So the dialect work is confirmed
    end-to-end on real hardware.
  - **Then it dies.** We send `EA00` (start EA session) at t=29.827, the car acks it (seq
    `0xa6`), and at **t=30.068 we send our first keepalive — after which the car never
    acknowledges another packet of ours.** Its ack field is pinned at `0xa6` for the remaining
    20 s while it retransmits its `0x4301` (seq `0xe8`, byte-identical) six times, gives up at
    ~t=38, and drops the link.
  - **Cause: `make_packet` computed `length = payload.len() + 10` unconditionally.** For an
    empty payload there *is* no payload checksum, so the packet is 9 bytes — and we sent 9 —
    but the header claimed 10. Confirmed off the wire: our keepalive is
    `ff5a 000a 40 a7 e7 00 cf` inside an RFCOMM frame whose own length field says **9**
    (frame 640; the trailing `0x40` is the RFCOMM FCS, not ours). The car's own bare acks say
    `ff5a 0009 …` (frame 639). Reading `length`, the accessory consumes our 9 bytes and then
    takes the first byte of the *next* packet as the tenth — so every subsequent packet is
    mis-framed, its cumulative ack freezes, and the session deadlocks. The 1 s keepalive
    guarantees this happens in every session, right after the handshake goes quiet.
  - **Fixed**: packet construction moved into `Packet::new` in `iap2-proto/src/packet.rs`,
    which sizes `length` as header + header checksum + payload + payload checksum *only when
    there is a payload*; `Iap2::make_packet` delegates. Three tests pin it: the empty packet
    encodes byte-for-byte to the car's own bare ack, a payload packet to our own accepted
    `AA05`, and both round-trip through our decoder with nothing left over.
  - **Observed but deliberately not "fixed":** we re-send `EA00` on every retransmitted
    `0x4301`, because nothing dedupes a repeated sequence number. With framing correct the
    retransmissions shouldn't happen at all, so adding dedupe now would be guessing at a
    problem we can't see — revisit only if a capture shows genuine duplicates.
  - Not yet retested against the bike.
- **`bench_1.pcapng` (2026-07-26): the AUTO BOX bench unit regressed, and the cause was the
  32-byte auth challenge. The challenge length is now derived from the accessory's
  certificate instead of being guessed-and-retried.**
  - Capture reads (direction from `hci_h4.direction`: `0x01` = from the car, `0x00` = from us).
    Everything up to authentication is healthy: car pages us, RFCOMM opens, SYN → SYN|ACK →
    `StartIdentification` → `IdentificationInformation` ("AUTO BOX") → `IdentificationAccepted`
    → `AA00` → `AA06` serial → `AA00` → `AA01` certificate. At **t=13.967 we send `AA02` with a
    32-byte challenge** (`4040002a aa02 0024 0000 …`, frame 390).
  - **The car link-ACKs it (frame 392) and then says nothing.** No `AA03` at all — not even the
    empty one the bike sends. At t=18.7 the *car* starts re-sending `FF 55 02 00 EE 10` once a
    second through the end of the capture, i.e. it gave up and tried to restart iAP2; carjack
    sends nothing further. **So the 32→20 retry could never fire: it triggers on an empty
    `AuthenticationResponse`, and this unit's failure mode is silence.**
  - **The certificates say outright which digest each unit wants**, so there is no need to guess:
    | | bench (AUTO BOX), frame 386 | bike (CFMOTO), `cfmoto_7` frame 829 |
    |---|---|---|
    | cert size | 908 B | 609 B |
    | signature alg | `sha1WithRSAEncryption` (`06092a864886f70d010105`) | `ecdsa-with-SHA256` (`06082a8648ce3d040302`) |
    | key | RSA-1024 | ECC |
    | issuer CN | "Apple iPod Accessories Certification Authority" | "Apple Accessories Certification Authority - 00000002" |
    | coprocessor | **2.0C → 20-byte SHA-1** | **3.0 → 32-byte SHA-256** |
  - **Fixed** in `libcarjack/src/carplay_session.rs`: `challenge_len_for_certificate()` scans the
    `AuthenticationCertificate` blob for the ansi-X9-62 EC OID prefix `2a 86 48 ce 3d` — present
    → 32 bytes, absent (or no certificate readable at all) → 20.
  - **The 32↔20 retry is now deleted rather than kept as insurance.** It can't help anything
    we've seen: the bench unit rejects a wrong-sized challenge by going *silent*, which never
    reaches the empty-response branch, and the bike gets the right length up front now. It
    isn't free either — an unexpected second `AA02` mid-auth is a plausible way to trigger the
    aggressive-reconnect sulk. What **is** kept is the empty-response *check*: it logs an
    `error!` naming the length we sent and withholds `AuthenticationSucceeded`, which is what
    turned this class of failure from invisible ("Authentication complete") into diagnosable.
  - Two `libcarjack` unit tests pin the discriminator against the real certificate prefixes from
    both captures. `cargo test` alone doesn't reach them (root package only) so there is a new
    **`just test-lib`** recipe alongside `just test-proto`.
  - The rest of the new dialect code checks out against this capture: the bench unit's
    `MessagesReceivedFromDevice` (param 7) is `{0x5702, 0x4E0A, 0x4E0D, 0x4E0B, 0x5001}` — no
    0x4300 and no 0x4E09 — so `accepts()` correctly picks the **legacy 0x5702** request and
    skips `DeviceInformationUpdate`. (The bike's is `{0x4E09, 0x4300}`.) That answers the open
    question above: the AUTO BOX **does** declare param 7, so dialect selection is driven by
    real data on both units rather than by the empty-list fallback.
  - Not yet retested against either unit.
- **`cfmoto_7.pcapng` (2026-07-26): the passive-connection + session-ID fixes WORK on the bike.
  iAP2 now runs all the way through authentication; the stall moved to the post-auth WiFi handoff.**
  - Sequence in the capture: our outbound page still Page-Timeouts (t=13.65 → status `0x04`), we
    stay quiet, and at **t=74.37 the car pages us**. It sets up HFP/A2DP/AVRCP, opens **RFCOMM ch 1**
    at t=78.02, sends `FF 55`, and the whole chain runs on **session 02**: SYN → `StartIdentification`
    → `IdentificationInformation` ("Motorcycle Media Players") → `IdentificationAccepted` → `AA00` →
    `AA06` serial → `AA00` → `AA01` cert → `AA02` challenge → `AA03` → `AA05`.
  - **Everything stops at t=78.584.** We send `AuthenticationSucceeded` + `0x5702`; the car
    link-ACKs both and says nothing for the remaining 13 s. Two independent bugs, both found by
    diffing against `iphone-cfmoto.pklg`:
    1. **The auth challenge was the wrong length — authentication actually FAILED.** We sent a
       **20-byte** challenge (`4040001e aa02 0018 0000 …`); an iPhone sends **32 bytes**
       (`4040002a aa02 0024 0000 …`). This unit has an Apple auth coprocessor 3.0 (cert CN
       `IPA_24FB0606…`), which signs a 32-byte SHA-256 digest — given 20 bytes it returned an
       **empty** `AuthenticationResponse` (`40400006 aa03`) where the iPhone gets a 64-byte
       signature. `carplay_session.rs` logged "Authentication complete" and sent `AA05` regardless,
       so the failure was invisible.
    2. **We asked for the WiFi config with a message this head unit does not implement.** Its own
       `IdentificationInformation` declares `MessagesSentByAccessory = {0xAA06, 0x4301}` (param
       0x0006) and `MessagesReceivedFromDevice = {0x4E09, 0x4300}` (param 0x0007). We sent
       **0x5702**, which isn't in that set, so it was dropped. An iPhone sends `0x4E09`
       (device name) then `0x4300`, and the car answers
       `0x4301` ~2.8 s later with SSID `CFMOTO-999700`, passphrase `12345678`, channel 157, IPv6
       `fe80::9083:8eff:feef:a7d3`, **port 7000**, its WiFi MAC and a session UUID.
  - **Wire format of 0x4301 (was being parsed wrong too):** the credentials are **nested** inside
    the group at top-level parameter **1** — `{0: SSID, 1: passphrase, 2: channel u8, 3: IPv6 LL,
    4: u8}` — while port (2, u32), accessory MAC (3), UUID (4) and firmware version (5) sit at the
    top level. The old flat `get_param(SSID=0x0001)` read the *group* bytes as a string and, because
    they start with `0x00`, yielded an **empty SSID rather than an error**.
  - **Fixed 2026-07-26 (builds clean, unit-tested, NOT yet retested against the bike)** — in
    `.claude/worktrees/iap2-auth-wifi-config`, uncommitted:
    - `generate_challenge()` → 32 bytes; an empty/missing `AuthenticationResponse` now logs an
      `error!` and does **not** send `AuthenticationSucceeded`.
    - **TWO WiFi-config dialects are supported, because the two head units disagree.** This was
      nearly a silent regression: `carjack.log` (2026-07-17) shows the **AUTO BOX answering our
      old 0x5702 with a 0x5703** whose parameters are **flat** — `0x0001` =
      `carplay_wifi_9900\0`, `0x0002` = `12345678\0`, `0x0003` = `[02]`, `0x0004` = `[24]`.
      So `0x5702`/`0x5703` were **not** wrong, they're that unit's dialect; the bike simply
      speaks a different one (`0x4300`/`0x4301`, nested groups). Both pairs now exist as
      distinct `ControlMessageId` variants (`…Grouped` = the 0x43xx, nested pair), and the
      response handler parses flat or nested according to which arrived.
    - **Dialect choice is driven by the accessory, not guessed:** `IdentificationInformation`
      parameter **7** (`MessagesReceivedFromDevice`) is now parsed into `accepted_messages`.
      Declares 0x4300 → send the grouped request; otherwise → send legacy 0x5702, which is
      exactly the old behaviour, so a unit that declares nothing (possibly the AUTO BOX — its
      param list was never captured) is unaffected. `DeviceInformationUpdate` (0x4E09) is
      likewise only sent when declared. **The iPhone's ~3.6 s gap before 0x4300 is deliberately
      not implemented** — add it if the car still doesn't answer.
    - **The 32-byte challenge falls back to 20.** The AUTO BOX was working with a 20-byte
      challenge, and we can't tell a coprocessor's generation before asking, so `send_challenge`
      tries **32 (SHA-256) first and retries once at 20 (SHA-1)** if the accessory returns an
      empty `AuthenticationResponse`. Only after both fail do we log the error and withhold
      `AuthenticationSucceeded`. That keeps the bike's fix without betting the bench unit on it.
      **SUPERSEDED — `bench_1.pcapng` proved "try 32 first" cannot be recovered from; the
      length is now read off the certificate. See the next entry.**
    - Nested parameters ("groups") and NUL-terminated strings are handled through the existing
      `IntoParameter`/`TryFromParam` traits rather than bespoke helpers: `IntoParameter for
      Vec<Parameter>` writes a group, `TryFromParam for Vec<Parameter>` reads one (so
      `get_param::<Vec<Parameter>>(id)` descends a level), and `IntoParameter for &str/String`
      now appends the NUL terminator that iAP2 strings carry on the wire.
      **Side effect to watch:** that makes `start_ea_session` send `com.apple.carplay\0` where it
      previously sent no terminator — a path that had reached "AirPlay bootstrap complete!" on the
      AUTO BOX unit. Spec-correct and matches every string in the captures, but it is a live change
      to a previously-working message. `wifi_information` (0x5701) changes the same way; nothing in
      any capture asks us for that, so it is untested either way.
    - New `DeviceIdentity {name, transport_identifier, wifi_mac}` plumbed
      `wireless.rs → carplay.rs → CarplaySession`; `wifi_connect::local_wifi_mac()` reads the
      station address from sysfs (needed *before* the WiFi link exists). The iPhone's
      `transport_identifier` is an opaque 24-hex-char blob that appears **nowhere else** in its
      capture — nothing the accessory sent — so the accessory can't be validating it; our
      Bluetooth address stands in. First thing to revisit if 0x4300 is rejected.
    - Accessory name now read from parameter 0 (fixes `Accessory identified: Unknown`).
    - 5 unit tests in `iap2-proto/src/ops/control.rs` encode/decode against the exact bytes from
      `iphone-cfmoto.pklg` (0x4E09 and 0x4300 match the iPhone **byte-for-byte**; the car's real
      0x4301 parses to the right SSID/passphrase/channel/port/MAC). Run with `just test-proto` —
      a new recipe, because `just test` runs plain `cargo test` (root package only) and
      `--workspace` can't link libcarjack/cuda-dmabuf outside the toolbox.
- **Session-ID fix confirmed working — on the STANDALONE TEST HEAD UNIT ("AUTO BOX"), not the bike**
  (`just test-run`, 2026-07-26). With the control session addressed correctly, that unit answers
  `StartIdentification` and the whole chain runs: `IdentificationInformation` (15 params, SSID
  `carplay_wifi_895E`) → `AccessoryAuthenticationSerialNumber` → `AuthenticationCertificate` →
  WiFi → **`AirPlay bootstrap complete!`**. So the fix is real, but it says **nothing about the
  bike**, whose blocker is the who-originates-the-ACL problem above; that still needs its own
  capture.
  - Minor bug spotted in the same log: `carplay_session.rs` logs `Accessory identified: Unknown`
    because it reads `ParameterId::DISPLAY_NAME`, while this unit puts its name (`AUTO BOX`) in
    `ParameterId(0)`. Cosmetic, but the lookup is wrong.
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


## Earlier, undated observations

Carried over from the old `summary.md` "Important Details". Kept because they are observations
about the *car's* behaviour rather than about our code, but note they predate the working iAP2
handshake and may since have been superseded.

- The car waits ~8 seconds after the AirPlay TCP connection is established, then retransmits the
  `FF 55` switch command every ~1 second over Bluetooth to restart iAP2. Read at the time as the
  car timing out waiting for a specific iAP2 message, or for video data to start.

## Earlier work

Undated, from before the log was kept per-capture. Several of these describe code that has since
been replaced (the RTSP/RTP pipeline in particular - video now goes through catplay as AVC, not
`rtph264pay`), and are kept for the reasoning rather than as a description of the current code.

- **HE/VHT driver fix applied**: `disable_11ax=1` via modprobe config.
- **Cleaned up nl80211 + iw fallback**: `nl80211.rs`, `libc` dep, `mod nl80211` removed; only NM used.
- **WiFi connection verified**: connects successfully to car AP with NM.
- **iAP2 codec buffer clearing fixed**: Changed from clearing entire buffer on non-`FF 5A` bytes to byte-at-a-time advance searching for valid magic, preserving any genuine iAP2 packets interleaved with noise. New log shows "Skipping 6 byte(s) before valid magic".
- **AirPlay RTSP client rewrite**: Previously acted as RTSP server (waiting for car to send ANNOUNCE). Now acts as RTSP client — sends ANNOUNCE (with SDP) → SETUP → PLAY → interleaved RTP video. This is the correct role for iPhone/CarPlay.
- **Video pipeline updated**: Changed from raw H264 AVC output to RTP packets via `rtph264pay pt=96`. The car expects RTP over interleaved TCP, not raw AVC.
- **Dead EA video channel removed**: Removed vestigial `video_rx`/`video_tx` channel from `CarplaySession`, `wireless_carplay`, and `main.rs`. Removed `send_video_frame` method. `CarplaySession::run()` now loops directly on `receive_packet()` without `select!`.
- **iPhone pklg capture re-analyzed**: Found 62 iAP2 `FF 5A` packets and 4 `FF 55` switch commands. Extracted full protocol exchange: SYN→StartIdentification→IdentificationInformation→Auth→WiFiConfig→WirelessCarPlayUpdate. **Zero EA session messages** found — EA establishment likely happens over WiFi as part of AirPlay handshake, not Bluetooth.
