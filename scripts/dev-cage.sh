#!/usr/bin/env bash
# Runs the client inside cage (a Wayland kiosk compositor) from a desktop
# session or a spare VT, using the same code path as production apart from
# SDL's video driver choice. Extra arguments go to jf-htpc.
set -euo pipefail
BIN="${BIN:-target/release/jf-htpc}"
command -v cage >/dev/null || { echo "cage not installed" >&2; exit 1; }
exec cage -- "$BIN" --dev "$@"
