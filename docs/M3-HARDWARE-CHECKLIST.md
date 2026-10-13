# M3 hardware validation

M3 exit criteria (spec §15): *4K direct play with hardware decode and CPU
under budget; the OSD toggles with no frame drops.* Per the playback
policy (DECISIONS D19), the app direct-plays what the hardware decodes and
says so clearly when it can't; it never silently decodes in software.

On the HTPC (Intel HD 530) that means 4K **H.264** and **HEVC 8-bit** must
play in hardware, and **HEVC 10-bit** must be reported as unsupported
(software decoding, red badge, warning in the log).

Run on a text console (Ctrl+Alt+F3) as a user in `video`, `render` and
`input`.

1. **Dependencies and build**
   ```sh
   sudo dnf install -y 'pkgconfig(mpv)' 'pkgconfig(libva)' 'pkgconfig(libva-drm)' ffmpeg
   cd ~/repo/jellyroll && git pull && cargo build --release
   ```
   Hardware decode also needs RPM Fusion's `intel-media-driver` (README).
   Fedora's default `ffmpeg-free` leaves out H.264/HEVC (including the
   x264/x265 encoders the clip script uses, and the decoders mpv's VA-API
   path is built on). With RPM Fusion enabled, swap in the full build:
   `sudo dnf swap -y ffmpeg-free ffmpeg --allowerasing`.
2. **What the GPU decodes:** `./target/release/jellyroll --print-diagnostics | grep -E "mpv|vaapi"`.
   Expect `vaapi.decode: MPEG-2, VC-1, H.264, VP8, HEVC 8-bit` and
   `vaapi.no_decode: HEVC 10-bit, VP9 8-bit, VP9 10-bit, AV1`.
3. **Test clips** (4K, 20 s each; x265 on this CPU takes a few minutes):
   ```sh
   scripts/make-test-clips.sh ~/test-media 3840x2160 20
   ```
4. **Watch one:** `./target/release/jellyroll --play ~/test-media/hevc8-2160p.mp4`
   - Picture and sound; Space pauses; ←/→ seek; `i` shows the decoder
     panel (expect `vaapi`); Esc stops and exits.
5. **Benchmarks** (each 15 s):
   ```sh
   for c in h264 hevc8 hevc10; do ./target/release/jellyroll --play ~/test-media/$c-2160p.mp4 --bench-play 15; done > ~/jellyroll-m3.txt 2>&1
   ```
   Pass for H.264 and HEVC 8-bit: `decoder: vaapi`, 0 dropped frames, 0
   missed vsyncs, 0 allocating frames, and a low render-thread CPU %.
   HEVC 10-bit is expected to fail with "software decoding": that is the
   policy working, not a bug.
6. **Your own media:** play a real 4K file from your library and a stream
   URL from your Jellyfin server (Dashboard → a movie → "Copy stream URL",
   or `http://SERVER:8096/Videos/ITEM_ID/stream?static=true&api_key=KEY`).
