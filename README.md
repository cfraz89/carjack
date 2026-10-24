# carjack

Wireless CarPlay for things that aren't iPhones. carjack makes a Linux machine or an Android
phone look like an iPhone to a CarPlay head unit - a car stereo, a motorcycle dash - and streams
its own screen to it, with touch and the unit's buttons coming back.

## Why

carjack started as a way to get better off-road navigation on a CFMOTO 1000MT-X. The bike's dash
is a CarPlay head unit: it shows only what an iPhone sends it, and CarPlay shows only the apps
Apple allows on a car screen. carjack puts any app on that screen instead - a proper off-road
navigation app with GPX routing support, running on an Android phone or a small Linux board.

The Android app, Slipstream, is now where development is focused. I have no need for the Linux app
currently, so haven't looked at it in a while.

## What's here

| | |
| --- | --- |
| [`carjack-linux/`](carjack-linux) | The Linux app: runs an app (Waydroid, say) or your desktop in its own compositor and streams it to the unit. Built for small boards, packaged as a `.deb`. |
| [`carjack-android/`](carjack-android) | The Android app, **Slipstream**: turns an Android phone into the unit's CarPlay source, showing its apps on the unit's screen. Runs on a stock phone with [Shizuku](https://shizuku.rikka.app/). |
| [`libcarjack/`](libcarjack) | The shared core: Bluetooth iAP2, the CarPlay session, AirPlay screen streaming. |
| [`iap2-proto/`](iap2-proto) | iAP2 message encoding and decoding. |
| [`cuda-dmabuf/`](cuda-dmabuf) | Optional NVIDIA zero-copy encode for the Linux app. |
| [`docs/`](docs) | Investigation notes: what each capture showed, and where things stand. |

AirPlay pairing and streaming come from [catplay](https://github.com/cfraz89/catplay).

## How it was made

Clean-room. carjack was written by studying packet captures - Bluetooth HCI logs, iAP2 traffic and
AirPlay sessions between real iPhones and real head units - and working out the protocol from what
went over the wire. No Apple source, SDKs or accessory documentation were used.
[`iap2.lua`](iap2.lua) is the Wireshark dissector that came out of it.

## Getting it

### Android (Slipstream)

So far Slipstream has only been tested with a Pixel 8 and a CFMOTO 1000MT-X. Other phones and
head units may well work, but haven't been tried.

It needs [Shizuku](https://shizuku.rikka.app/) installed and running; Slipstream walks you through
the rest when it opens.

The easiest way to install it and keep it updated is [Obtainium](https://obtainium.imranr.dev/),
which installs apps straight from GitHub releases:

1. Install Obtainium from its [releases page](https://github.com/ImranR98/Obtainium/releases).
2. In Obtainium, tap **Add app** and enter `https://github.com/cfraz89/carjack` as the source URL.
3. Tap **Add**, then **Install**.

Obtainium then checks for new releases and offers each update. To install by hand instead,
download the APK from the [releases](https://github.com/cfraz89/carjack/releases).

### Linux

See [`carjack-linux/README.md`](carjack-linux/README.md).

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your
option.

carjack isn't affiliated with or endorsed by Apple. CarPlay and AirPlay are trademarks of Apple
Inc.
