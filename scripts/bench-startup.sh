#!/usr/bin/env bash
# Startup benchmark (spec 13): runs the client N times and reports
# process start -> first frame presented, as logged by the render loop.
# Budget (spec 2): < 500 ms. Usage: scripts/bench-startup.sh [N] [jellyroll args...]
set -euo pipefail
N="${1:-10}"; shift || true
BIN="${BIN:-target/release/jellyroll}"
times=()
for _ in $(seq "$N"); do
  out="$("$BIN" --bench-idle 0.1 "$@" 2>&1 >/dev/null || true)"
  ms="$(grep -o 'first frame presented ms=[0-9]*' <<<"$out" | grep -o '[0-9]*$' || true)"
  [[ -n "$ms" ]] || { echo "no first-frame log line; output was:"; echo "$out"; exit 1; }
  times+=("$ms")
done
printf '%s\n' "${times[@]}" | sort -n | awk -v budget=500 '
  { a[NR]=$1 } END {
    p50=a[int((NR+1)/2)]; max=a[NR];
    printf "runs %d  p50 %d ms  max %d ms  (budget %d ms)\n", NR, p50, max, budget;
    exit (max < budget ? 0 : 1) }'
