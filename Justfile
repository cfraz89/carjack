set dotenv-load
mod armbian

# Grant the `carjack-hci-helper` sidecar the cap_net_admin it needs for the raw HCI mgmt socket.
# Not part of `build` - the sidecar is currently unused (see src/bin/carjack-hci-helper.rs), and
# escalating on every build prompts through polkit.
[script]
setcap +flags="":
    # Only the sidecar carries the capability. The main carjack binary stays unprivileged so
    # xdg-desktop-portal can introspect it in desktop mode (a capability-carrying process is
    # /proc-locked to root under fs.suid_dumpable=2).
    binary="target/debug/carjack-hci-helper"
    case " {{flags}} " in *" --release "*|*" -r "*) binary="target/release/carjack-hci-helper";; esac
    # Only escalate when the capability is actually missing. Relinking the sidecar drops the cap,
    # so `getcap` is an exact test for "needs setting" - and skipping it otherwise avoids a `run0`
    # polkit prompt on every single build (which outright fails in a non-interactive shell).
    if getcap "$binary" 2>/dev/null | grep -q cap_net_admin; then
        echo "cap_net_admin already set on ${binary}"
    else
        run0 setcap cap_net_admin+ep "$binary"
        echo "Gave cap_net_admin to ${binary}"
    fi

toolbox_build := if env('TOOLBOX_PATH', '') != '' { '' } else if env('TOOLBOX_BUILD', '') == 'true' { 'toolbox run ' } else { '' }

run +flags="": (build flags)
    waydroid session stop && cargo run {{flags}} < /dev/null

#Run the app until timeout, then kill it and restore the original wifi connection
[script('bash')]
test-run timeout="60" +flags="": (build flags)
    set -uo pipefail
    original=""
    cleanup() {
        # Anchored: unanchored, this matches *any* process whose command line merely contains the
        # path - a tail on the log, an editor, a wrapper script driving this recipe - and kills it.
        pkill -f '^target/(debug|release)/carjack$' 2>/dev/null || true
        if [ -n "$original" ]; then
            current=$(nmcli -t -f NAME,ACTIVE connection show --active 2>/dev/null | awk -F: '$2=="yes"{print $1; exit}')
            if [ "$current" != "$original" ]; then
                echo "Restoring original connection: $original" >&2
                nmcli connection up id "$original" >/dev/null 2>&1 || true
            fi
        fi
    }
    trap cleanup EXIT
    trap cleanup ERR

    original=$(nmcli -t -f NAME,TYPE,ACTIVE connection show --active 2>/dev/null | awk -F: '$2!="loopback" && $3=="yes" {print $1; exit}')
    echo "Active connection before test: ${original:-none}" >&2

    waydroid session stop || true
    timeout --foreground --kill-after=5s "{{timeout}}s" cargo run {{flags}} < /dev/null || true

build +flags="":
    {{toolbox_build}}cargo build {{flags}} < /dev/null

# Run cargo tests
test +flags="":
    {{toolbox_build}}cargo test {{flags}} < /dev/null

test-bin +flags="": 
    cargo test --bin carjack {{flags}} < /dev/null

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
# libcuda comes from the host NVIDIA driver — ensure /usr/lib64/libcuda.so exists in
# the toolbox (create a symlink or bind-mount the host path).
install-cuda-dmabuf-deps:
    {{toolbox_build}}sudo dnf install -y libEGL-devel libgbm-devel gstreamer1-plugins-bad-free-devel