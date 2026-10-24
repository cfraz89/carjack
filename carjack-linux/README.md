# carjack for Linux

Turns a Linux machine into a wireless CarPlay source. carjack pairs with the head unit over
Bluetooth, joins its WiFi, and streams a screen to it over AirPlay, with the unit's touchscreen
and buttons coming back as input. What it streams is either an app running in carjack's own
headless compositor (Waydroid by default) or your existing Wayland desktop.

## Requirements

- Bluetooth through **BlueZ**, and a WiFi adapter that can join the unit's access point, both
  managed by **NetworkManager**. A second adapter can keep the machine's own uplink.
- **PipeWire/WirePlumber**, configured to leave the hands-free gateway role to carjack (below).
- **GStreamer** 1.24+ with an H.264 or HEVC encoder: VA-API, V4L2, NVENC, x264/x265 or
  OpenH264.
- For the default `app` source, something to run in it; [Waydroid](https://waydro.id/) is what
  it's set up for. For the `desktop` source, a Wayland session with an xdg-desktop-portal backend.

## Build

From the repo root:

```sh
just build            # cargo build; add cuda=true for NVIDIA zero-copy encode
just deb              # an arm64 .deb, built in a Debian container
```

## Setup

1. Hand the hands-free gateway over from PipeWire, which otherwise answers the unit's identity
   check with an error:

   ```sh
   just install-hfp-config
   ```

2. Have the machine advertise itself as a phone: merge [`bluetooth-main.conf`](bluetooth-main.conf)
   into the `[General]` section of `/etc/bluetooth/main.conf` and restart `bluetooth`.

3. Copy [`config.toml.example`](config.toml.example) to `~/.config/carjack/config.toml` and adjust
   it. The defaults take any CarPlay unit in range and run Waydroid.

4. Pair the machine with the unit from the unit's phone menu, as you would an iPhone.

## Run

```sh
just run                  # or: cargo run -- [flags]
just install-carjack-service   # install and start it as a systemd user service
```

With the `.deb`, the service is installed but left disabled: enable it with
`systemctl --user enable --now carjack`. It reads `/etc/carjack/config.toml` when the user has no
config of their own.

carjack then waits for the unit, starts a session when it connects, and retries on its own when a
session drops.

### Config portal

Unless `[portal] enabled = false`, carjack raises a setup access point, **`carjack-setup`**, with
the password `carjack1`, and serves a config page at `http://10.42.0.1/`. It asks for a new AP
password first, then lets you pick the head unit and the WiFi uplink from a phone. Turn it off on
a machine that's set up and installed: anyone who reaches it can change both.

### Options

Every setting in `config.toml` can be overridden by a flag (or a `CARJACK.`-prefixed environment
variable); flags win.

| Flag | Overrides |
| --- | --- |
| `-c, --config <PATH>` | The config file to load |
| `--source <app\|desktop>` | Stream an app in carjack's compositor, or the existing desktop |
| `--command <CMD>` | `[app] command`, the app to run |
| `--resizable <BOOL>` | `[app] resizable` |
| `--bt-device <auto\|MAC\|NAME>` | `[bluetooth] device`, the head unit to use |
| `--drm-device <DEVICE>` | `[graphics] drm_device`: `auto`, a driver, a render node or an index |
| `--zerocopy <auto\|true\|false>` | `[graphics] zerocopy` |
| `--bitrate <KBPS>` | `[graphics] bitrate` |
| `--width`, `--height`, `--fps` | `[display]`, the size the compositor starts at |

Logging follows `RUST_LOG` (default `debug`).

## Tests

```sh
just test-bin    # the app's unit tests
just test        # everything, including the GPU tests in tests/ (needs a render node)
```
