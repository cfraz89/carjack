set dotenv-load
mod armbian

# Capabilities for the sidecar. Run after rebuilding it - relinking drops them. Kept out of
# `build` because escalating on every build prompts, which fails non-interactively.
#
# Passwordless via /etc/sudoers.d/carjack-setcap. Every comma must be backslash-escaped, or sudoers
# reads it as the Cmnd_List separator; quotes don't work, and `visudo -c -f` catches it:
#   chris ALL=(root) NOPASSWD: /usr/bin/setcap cap_chown\,cap_dac_override\,cap_net_admin+ep <abs>/target/debug/carjack-wireless-helper, \
#                              /usr/bin/setcap cap_chown\,cap_dac_override\,cap_net_admin+ep <abs>/target/release/carjack-wireless-helper
[script]
setcap +flags="":
    # Never the main binary: a capability-carrying process is /proc-locked to root under
    # fs.suid_dumpable=2, which stops xdg-desktop-portal introspecting it in desktop mode.
    binary="{{justfile_directory()}}/target/debug/carjack-wireless-helper"
    case " {{flags}} " in *" --release "*|*" -r "*) binary="{{justfile_directory()}}/target/release/carjack-wireless-helper";; esac
    # Checked individually - a binary from an earlier capability set has some already.
    caps="$(getcap "$binary" 2>/dev/null || true)"
    missing=""
    for cap in cap_chown cap_dac_override cap_net_admin; do
        grep -q "$cap" <<<"$caps" || missing="${missing} ${cap}"
    done
    if [ -z "$missing" ]; then
        echo "cap_chown,cap_dac_override,cap_net_admin already set on ${binary}"
    else
        echo "Missing:${missing}"
        sudo setcap cap_chown,cap_dac_override,cap_net_admin+ep "$binary"
        echo "Gave cap_chown,cap_dac_override,cap_net_admin to ${binary}"
    fi

toolbox_build := if env('TOOLBOX_PATH', '') != '' { '' } else if env('TOOLBOX_BUILD', '') == 'true' { 'toolbox run ' } else { '' }

# NVIDIA zero-copy encode is an opt-in cargo feature (`cuda`, off by default) so the project builds
# on non-NVIDIA machines without linking libcuda/libgstcuda. Pass `cuda=true` (or
# `CARJACK_CUDA=true`) to enable it locally. Just ends up appending `--features cuda`.
# `cuda=true`/`CARJACK_CUDA=true` enables the opt-in NVIDIA zero-copy feature.
cuda := env('CARJACK_CUDA', 'false')
cuda_features := if cuda == 'true' { '--features cuda' } else { '' }

run +flags="": (build flags)
    waydroid session stop && cargo run {{cuda_features}} {{flags}} < /dev/null

#Run the app until timeout, then kill it and restore the original wifi connection
[script('bash')]
test-run timeout="60" +flags="": (build flags)
    set -uo pipefail
    # The station iwd drives, from the kernel rather than from iwctl's table: a wireless netdev is
    # the one with a phy80211 link, and that is true whatever iwd calls it. A glob is no good here
    # - /sys/class/net/* are symlinks and `/sys/class/net/*/phy80211` matches every one of them.
    dev=""
    for d in /sys/class/net/*; do
        if [ -e "$d/phy80211" ]; then dev=$(basename "$d"); break; fi
    done
    # `iwctl station <dev> show` prints the SSID in a row labelled "Connected network"
    # (iwd's client/station.c). Strip any colour escapes and the row's padding rather than
    # `xargs`, which would mangle an SSID containing spaces or quotes.
    connected() {
        iwctl station "$dev" show 2>/dev/null \
            | sed -e 's/\x1b\[[0-9;]*m//g' -n -e 's/^ *Connected network  *//p' \
            | sed -e 's/[[:space:]]*$//' -e 1q
    }
    original=""
    cleanup() {
        # carjack leaves the car's network provisioned (AutoConnect=false, so iwd will not go back
        # to it on its own) and stays associated to it. Put the dev box back where it was.
        if [ -n "$original" ] && [ "$(connected)" != "$original" ]; then
            echo "Restoring original connection: $original" >&2
            timeout --foreground --kill-after=5s 20s iwctl station "$dev" connect "$original" >/dev/null 2>&1 || true
        fi
    }
    trap cleanup EXIT
    trap cleanup ERR

    original=$(connected)
    echo "Active connection before test: ${original:-none} (on ${dev:-no wireless device})" >&2

    waydroid session stop || true
    timeout --foreground --kill-after=5s "{{timeout}}s" cargo run {{cuda_features}} {{flags}} < /dev/null || true

build +flags="":
    {{toolbox_build}}cargo build {{cuda_features}} {{flags}} < /dev/null

# Run cargo tests
test +flags="":
    {{toolbox_build}}cargo test {{cuda_features}} {{flags}} < /dev/null

# The carjack binary's own unit tests. Needs the toolbox like `just build` - the binary links
# dbus/EGL/gbm/gstreamer, none of which the host has.
test-bin +flags="":
    {{toolbox_build}}cargo test --bin carjack {{cuda_features}} {{flags}} < /dev/null

# The iAP2 protocol unit tests (message encode/decode against bytes captured from a working
# iPhone session). Pure Rust - no GPU, gstreamer or toolbox needed, unlike `just test`.
test-proto +flags="":
    cargo test -p iap2-proto {{flags}} < /dev/null

# The libcarjack unit tests (iAP2 session logic, against bytes captured from real head units).
# Links against gstreamer/catplay so it builds in the toolbox like `just build`, but needs no
# GPU and no car. `cargo test` alone doesn't reach it - that only covers the root package.
test-lib +flags="":
    {{toolbox_build}}cargo test -p libcarjack --lib {{flags}} < /dev/null

# Install -devel packages that cuda-dmabuf links against (libEGL, libgbm, libgstcuda).
# Only needed when building with the opt-in `cuda` feature (`just cuda=true build`).
# libcuda comes from the host NVIDIA driver — ensure /usr/lib64/libcuda.so exists in
# the toolbox (create a symlink or bind-mount the host path).
install-cuda-dmabuf-deps:
    {{toolbox_build}}sudo dnf install -y libEGL-devel libgbm-devel gstreamer1-plugins-bad-free-devel

# Install dependencies in toolbox
install-deps:
    {{toolbox_build}}sudo dnf install gcc g++ gstreamer1-devel gstreamer1-plugins-base-devel pipewire-gstreamer dbus-devel \
    openssl-devel mesa-libgbm-devel libdrm-devel libxkbcommon-devel mesa-libEGL-devel mesa-libGLES-devel \
    libacl-devel
