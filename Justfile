set dotenv-load
mod armbian

# Grant the `carjack-hci-helper` sidecar the cap_net_admin it needs for the raw HCI mgmt socket.
# Not part of `build`: escalating on every build prompts, and a build only needs this when the
# relink has dropped the capability. Run it after rebuilding the sidecar.
#
# Passwordless via /etc/sudoers.d/carjack-setcap:
#   chris ALL=(root) NOPASSWD: /usr/bin/setcap cap_net_admin+ep <abs>/target/debug/carjack-hci-helper, \
#                              /usr/bin/setcap cap_net_admin+ep <abs>/target/release/carjack-hci-helper
# sudoers matches arguments literally, which is why the paths below are absolute.
[script]
setcap +flags="":
    # Only the sidecar carries the capability. The main carjack binary stays unprivileged so
    # xdg-desktop-portal can introspect it in desktop mode (a capability-carrying process is
    # /proc-locked to root under fs.suid_dumpable=2).
    binary="{{justfile_directory()}}/target/debug/carjack-hci-helper"
    case " {{flags}} " in *" --release "*|*" -r "*) binary="{{justfile_directory()}}/target/release/carjack-hci-helper";; esac
    # Only escalate when the capability is actually missing. Relinking the sidecar drops the cap,
    # so `getcap` is an exact test for "needs setting" - and skipping it otherwise avoids a prompt
    # on every single build (which outright fails in a non-interactive shell).
    if getcap "$binary" 2>/dev/null | grep -q cap_net_admin; then
        echo "cap_net_admin already set on ${binary}"
    else
        sudo setcap cap_net_admin+ep "$binary"
        echo "Gave cap_net_admin to ${binary}"
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
    original=""
    cleanup() {
        # Anchored: unanchored, this matches *any* process whose command line merely contains the
        # path - a tail on the log, an editor, a wrapper script driving this recipe - and kills it.
        if [ -n "$original" ]; then
            current=$(nmcli -t -f NAME,ACTIVE connection show --active 2>/dev/null | awk -F: '$2=="yes"{print $1; exit}')
            if [ "$current" != "$original" ]; then
                echo "Restoring original connection: $original" >&2
                timeout --foreground --kill-after=5s 10s nmcli connection up id "$original" >/dev/null 2>&1 || true
            fi
        fi
    }
    trap cleanup EXIT
    trap cleanup ERR

    original=$(nmcli -t -f NAME,TYPE,ACTIVE connection show --active 2>/dev/null | awk -F: '$2!="loopback" && $3=="yes" {print $1; exit}')
    echo "Active connection before test: ${original:-none}" >&2

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
    openssl-devel mesa-libgbm-devel libdrm-devel libxkbcommon-devel mesa-libEGL-devel mesa-libGLES-devel
