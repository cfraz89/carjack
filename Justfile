set dotenv-load
mod armbian

# Environment variables to forward when running inside a toolbox (toolbox run may not
# pass through arbitrary env vars from .env by default).
_run_rust_log := env('RUST_LOG', '')
_run_desktop_cmd := env('CARJACK_DESKTOP_CMD', '')
_run_zerocopy := env('CARJACK_ZEROCOPY', '')
_run_render_node := env('CARJACK_RENDER_NODE', '')
run_env := f'RUST_LOG="{{_run_rust_log}}" CARJACK_DESKTOP_CMD="{{_run_desktop_cmd}}" CARJACK_ZEROCOPY="{{_run_zerocopy}}" CARJACK_RENDER_NODE="{{_run_render_node}}"'

toolbox_build := if env('TOOLBOX_PATH', '') != '' { '' } else if env('TOOLBOX_BUILD', '') == 'true' { 'toolbox run ' } else { '' }
toolbox_run := if env('TOOLBOX_PATH', '') != '' { '' } else if env('TOOLBOX_RUN', '') == 'true' { 'toolbox run env ' + run_env + ' ' } else { '' }

run $GST_DEBUG="*:2" RUST_LOG="info,catplay_hap=error,catplay_carplay=error,catplay_util=error": build
    {{toolbox_run}} cargo run < /dev/null

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
    {{toolbox_build}}cargo build < /dev/null

# Run a single cargo test filtered by name (e.g. `just test compositor`), in the toolbox (when
# USE_TOOLBOX is set) for the native build/link libs; the toolbox shares the host GPU/DRM the
# compositor test needs at runtime.
test test_name:
    {{toolbox_run}}cargo test {{test_name}} < /dev/null

# Install -devel packages that cuda-dmabuf links against (libEGL, libgbm, libgstcuda).
# libcuda comes from the host NVIDIA driver — ensure /usr/lib64/libcuda.so exists in
# the toolbox (create a symlink or bind-mount the host path).
install-cuda-dmabuf-deps:
    {{toolbox_build}}sudo dnf install -y libEGL-devel libgbm-devel gstreamer1-plugins-bad-free-devel