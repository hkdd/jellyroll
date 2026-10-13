#!/usr/bin/env bash
# Generates short synthetic test clips for playback testing (M3):
# moving test pattern + tone, 23.976 fps, in the codecs that matter for
# hardware-decode checks. Requires ffmpeg with libx264 and libx265.
# Usage: scripts/make-test-clips.sh [OUT_DIR] [WIDTHxHEIGHT] [SECONDS]
set -euo pipefail
OUT="${1:-test-media}"
SIZE="${2:-3840x2160}"
SECS="${3:-20}"
mkdir -p "$OUT"
SRC=(-f lavfi -i "testsrc2=size=${SIZE}:rate=24000/1001" -f lavfi -i "sine=frequency=440:sample_rate=48000")
COMMON=(-t "$SECS" -c:a aac -b:a 128k -shortest -y -loglevel error -stats)
tag="${SIZE#*x}p"

echo "H.264 8-bit"
ffmpeg "${SRC[@]}" -c:v libx264 -preset veryfast -crf 20 -pix_fmt yuv420p "${COMMON[@]}" "$OUT/h264-${tag}.mp4"
echo "HEVC 8-bit (Main)"
ffmpeg "${SRC[@]}" -c:v libx265 -preset ultrafast -crf 22 -pix_fmt yuv420p -tag:v hvc1 "${COMMON[@]}" "$OUT/hevc8-${tag}.mp4"
echo "HEVC 10-bit (Main10)"
ffmpeg "${SRC[@]}" -c:v libx265 -preset ultrafast -crf 22 -pix_fmt yuv420p10le -tag:v hvc1 "${COMMON[@]}" "$OUT/hevc10-${tag}.mp4"
ls -l "$OUT"
