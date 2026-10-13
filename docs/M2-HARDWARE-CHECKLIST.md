# M2 hardware validation

M2 exit criteria (spec §15): *a 4K scene of 60 cards plus text holds
60 fps; benchmark numbers recorded; go/no-go on Skia vs a custom GL
renderer.* Only real hardware can answer this; software rendering numbers
mean nothing.

Run everything on a text console (Ctrl+Alt+F3) as a user in the `input`
group, with the jellyroll service stopped if it is installed.

1. **Build** (the first build downloads a prebuilt Skia from GitHub, and
   linking it needs three development packages):
   ```sh
   sudo dnf install -y gcc-c++ freetype-devel fontconfig-devel
   cd ~/jellyroll && git pull && cargo build --release
   ```
   Check the binary is about 14 MB (`ls -l target/release/jellyroll`). If the
   build fails, the old binary stays in place and still shows the M1 screen.
2. **Look at it:** `./target/release/jellyroll --hud --allow-exit`
   - Japanese and Cyrillic text renders (header line and the fourth row
     title), long titles end in "…".
   - Arrow keys move the focus; the card scales up smoothly and the
     backdrop crossfades when the row/column changes.
   - Text is sharp at the TV's native resolution.
3. **Benchmark** (60 s, everything moving):
   ```sh
   ./target/release/jellyroll --bench-pacing 60 --hud > ~/jellyroll-m2.txt 2>&1; tail -12 ~/jellyroll-m2.txt
   ```
   Pass: CPU p99 < 4 ms, GPU p99 < 6 ms, 0 missed vsyncs, 0 allocating
   frames. Watch that the swaying rows move without stutter.
4. **Idle:** `./target/release/jellyroll --bench-idle 30` → `frames_rendered: 0`.
5. **Startup:** `scripts/bench-startup.sh 10` → max < 500 ms.
6. If step 3 fails on the GPU number, try `ui_render_scale = 0.75` under
   `[display]` in `~/.config/jellyroll/config.toml` and rerun, and record
   both.

Record the numbers in the "M2 benchmark results" table in
`docs/DECISIONS.md` (open question 2).
