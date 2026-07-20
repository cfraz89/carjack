set dotenv-load
mod armbian

# Prefix cargo commands with `toolbox run` when USE_TOOLBOX is set and we're not already inside
# a toolbox (TOOLBOX_PATH unset), to ease building on silverblue and the like
toolbox := if env('TOOLBOX_PATH', '') != '' { '' } else if env('USE_TOOLBOX', '') == 'true' { 'toolbox run ' } else { '' }

run $GST_DEBUG="*:2": build
    ./target/debug/carjack

#Run the app until timeout, then kill it and restore the original wifi connection
[script('bash')]
test-run timeout="60":
    set -uo pipefail
    original=""
    cleanup() {
        pkill -f 'target/(debug|release)/carjack' 2>/dev/null || true
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

build:
    {{toolbox}}cargo build < /dev/null

# Run a single cargo test filtered by name (e.g. `just test compositor`), in the toolbox (when
# USE_TOOLBOX is set) for the native build/link libs; the toolbox shares the host GPU/DRM the
# compositor test needs at runtime.
test test_name:
    {{toolbox}}cargo test {{test_name}} < /dev/null