#!/usr/bin/env bash
# Build + run the carjack screenshot driver in one step.
#
# Builds inside the toolbox (that is where the gstreamer/EGL/drm -devel libs live; a bare
# `cargo build` on the host fails to link) and then runs the resulting binary ON THE HOST, which
# is where the GPU/DRM render node it needs actually is.
#
# Usage:  .claude/skills/run-carjack/shot.sh [--out PATH] [--width N] [--height N]
#                                            [--frames N] [--command CMD] [--no-client]
# Default output: /tmp/carjack-screenshot.png
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../../.." # -> repo root

BIN=target/debug/examples/screenshot

# `< /dev/null` is required: `toolbox run` allocates a podman --tty and hangs forever without it.
if [ "${TOOLBOX_PATH:-}" != "" ]; then
    cargo build --example screenshot < /dev/null
else
    toolbox run cargo build --example screenshot < /dev/null
fi

exec "$BIN" "$@"
