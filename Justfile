set dotenv-load
mod armbian

[script]
_setcap +flags="":
    # Only the tiny `carjack-hci-helper` sidecar carries cap_net_admin (for the raw HCI mgmt socket).
    # The main carjack binary stays unprivileged so xdg-desktop-portal can introspect it in
    # desktop mode (a capability-carrying process is /proc-locked to root under fs.suid_dumpable=2).
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
test-run timeout="60": build
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

    original=$(nmcli -t -f NAME,TYPE,ACTIVE connection show --active 2>/dev/null | awk -F: '$2!="loopback" && $3=="yes" {print $1; exit}')
    echo "Active connection before test: ${original:-none}" >&2

    # `--foreground` is what makes Ctrl-C work. Without it, `timeout` puts the command in a *new
    # process group*; the terminal only ever sends SIGINT to its **foreground** process group, so
    # Ctrl-C reaches this script but never reaches carjack - it just runs on (spewing RUST_LOG
    # output) until the timeout expires, which reads as a wedged terminal. The cost of
    # `--foreground` is that on expiry `timeout` signals only its direct child (cargo), leaving
    # carjack orphaned - the `pkill` in `cleanup` above is what reaps it.
    #
    # Deliberately NOT `just run`: that re-runs the `build` recipe, and its `_setcap` step shells
    # out to `run0`, which wants a PTY and its own polkit auth. Building as a recipe dependency
    # keeps that out of the timed section and stops a cold compile eating the budget. Keep these
    # two lines in sync with the `run` recipe above.
    waydroid session stop || true
    timeout --foreground --kill-after=5s "{{timeout}}s" cargo run < /dev/null || true

build +flags="": && (_setcap flags)
    {{toolbox_build}}cargo build {{flags}} < /dev/null

# Run a single cargo test filtered by name (e.g. `just test compositor`), in the toolbox (when
# USE_TOOLBOX is set) for the native build/link libs; the toolbox shares the host GPU/DRM the
# compositor test needs at runtime.
test test_name: (build "--tests")
    cargo test {{test_name}} < /dev/null

# The iAP2 protocol unit tests (message encode/decode against bytes captured from a working
# iPhone session). Pure Rust - no GPU, gstreamer or toolbox needed, unlike `just test`.
test-proto test_name="":
    cargo test -p iap2-proto {{test_name}} < /dev/null

# Install -devel packages that cuda-dmabuf links against (libEGL, libgbm, libgstcuda).
# libcuda comes from the host NVIDIA driver — ensure /usr/lib64/libcuda.so exists in
# the toolbox (create a symlink or bind-mount the host path).
install-cuda-dmabuf-deps:
    {{toolbox_build}}sudo dnf install -y libEGL-devel libgbm-devel gstreamer1-plugins-bad-free-devel