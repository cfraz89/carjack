set dotenv-load
mod armbian

# Runs carjack inside a nested sway compositor (a sway window inside your current desktop
# session) rather than on your real desktop. This matches the target device (sway/cage) and
# lets the Wayland capture/input protocols (ext-image-copy-capture-v1,
# wlr-virtual-pointer-unstable-v1) exercise a real compositor without touching your actual
# screen or needing xdg-desktop-portal permission prompts. Requires `sway` to be installed.
[script('bash')]
run $GST_DEBUG="*:2": build
    set -euo pipefail
    # Re-exec inside the toolbox (same runner `build` uses) unless we're already in one -
    # sway and its dependencies live there, not on the host.
    if [ "${USE_TOOLBOX:-}" = "true" ] && [ -z "${TOOLBOX_PATH:-}" ]; then
        exec toolbox run just run
    fi

    if ! command -v sway >/dev/null; then
        echo "sway not found on PATH - install it to run carjack in a nested sway session" >&2
        exit 1
    fi

    # No readiness-detection needed: sway's own `exec` directive only runs a command once
    # sway has finished initializing, and it sets WAYLAND_DISPLAY/SWAYSOCK correctly for that
    # child itself - so let sway launch cargo run directly instead of guessing from outside
    # (log-scraping and directory-diffing were both tried and are unreliable/racy).
    statusfile=$(mktemp)
    configfile=$(mktemp)
    trap 'rm -f "$statusfile" "$configfile"' EXIT

    cat > "$configfile" <<EOF
    exec sh -c 'cargo run; echo \$? > "$statusfile"; swaymsg exit'
    EOF

    echo "Starting carjack in a nested sway session..."
    sway -c "$configfile"

    if [ ! -s "$statusfile" ]; then
        echo "nested sway exited before carjack finished running - sway probably failed to start" >&2
        exit 1
    fi
    exit "$(cat "$statusfile")"

# Safety-netted wrapper around `run` for exercising real hardware access (NetworkManager,
# BlueZ, GBM/DRM) during development. carjack drives the *real* host NetworkManager/BlueZ over
# D-Bus - including from inside toolbox, which shares the host D-Bus session - so a run that
# hangs or gets killed can leave the host's own network disconnected. This caps the run to
# `timeout` seconds and unconditionally restores whatever NetworkManager connection was active
# beforehand, killing any leftover carjack/sway processes first in case `timeout` alone didn't
# reach them (sway may detach exec'd children into their own session, outside its process
# group, so a plain `timeout <cmd>` isn't guaranteed to kill the whole tree).
[script('bash')]
test-run timeout="60":
    set -uo pipefail
    original=""
    cleanup() {
        pkill -f 'target/(debug|release)/carjack' 2>/dev/null || true
        pkill -x sway 2>/dev/null || true
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

    # `run` handles the toolbox re-exec itself now, so just call it directly.
    timeout "{{timeout}}s" just run || true

[script]
build:
    if [ "${USE_TOOLBOX:-}" = "true" ] && [ -z "${TOOLBOX_PATH:-}" ]; then
        toolbox run cargo build
    else
        cargo build
    fi