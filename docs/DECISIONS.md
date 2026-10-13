# Decisions and open questions

## Open questions (spec §16)

| # | Question | Status |
|---|---|---|
| 1 | Can SDL3 KMSDRM do refresh-rate matching, or do we need direct DRM/GBM/EGL from the start? | **Open.** M1 lists modes via `--print-diagnostics`. Run it on the reference hardware and check that every TV mode (23.976/24/25/50/59.94/60) appears with exact numerator/denominator. Test actual switching in M8. |
| 2 | Skia vs a custom GL renderer | Open. Decided by the M2 benchmark. |
| 3 | redb vs SQLite | Open (M5). |
| 4 | HDR on DRM/KMS | Open (M9). |
| 5 | Minimum Jellyfin server version | Open (M5). |
| 6 | Retained tree vs immediate mode | Open (M2/M6). |
| 7 | NVIDIA | Open. M1 code is vendor-neutral, but only Mesa has been exercised. |

## Decisions made in M1

**D1: Hand-rolled GL loader instead of a binding crate.** M1 calls about 15
GL functions (clear, scissor, timer queries). A small loader in
`crates/platform/src/gl.rs` avoids adding a dependency outside the spec's
list. Skia and mpv resolve their own symbols through `SDL_GL_GetProcAddress`.

**D2: Desktop GL 3.3 core first, GLES 3.0 fallback.** Timer queries are
core in GL 3.3. On GLES they need `GL_EXT_disjoint_timer_query`, and without
it the HUD shows GPU time as N/A. Both skia-safe (GL backend) and libmpv
support either API. Revisit if a target driver only offers GLES.

**D3: M1 drawing is scissored `glClear`s.** No shaders, textures or Skia
yet, as spec §0 requires: prove the loop first. The HUD uses a 3x5 bitmap
font with horizontal run-merging. All of this goes away when the `Renderer`
trait arrives in M2.

**D4: `unsafe` is confined to `jellyroll-platform`.** The app crate is
`#![forbid(unsafe_code)]`. The counting global allocator, which needs
`unsafe impl GlobalAlloc`, lives in the platform crate.

**D5: The allocation counter is always on.** It costs one relaxed atomic add
per allocation. Debug builds warn once if a steady-state frame allocates.
`--bench-pacing` fails if any post-warm-up frame allocates.

**D6: No `env-filter`.** `RUST_LOG` is parsed with
`tracing_subscriber::filter::Targets`, which handles `info` and
`target=level,...`. This keeps `regex-automata` out of the binary. Logs go
to journald when `JOURNAL_STREAM` is set, otherwise to stderr.

**D7: The watchdog is pinged from the render loop.** The loop's wait timeout
is capped at the next ping deadline, so an idle app wakes every WatchdogSec/2
(10 s). That costs nothing measurable, and a hung loop still gets restarted.

**D8: SDL3 is statically built from source behind the `build-sdl` feature.**
Without the feature the system `libSDL3.so` is used, which Fedora ships.
With it, the pinned SDL 3.4.18 is compiled in, and it still `dlopen`s
drm/gbm/egl/udev at runtime. CI uses `build-sdl` because Ubuntu 24.04 has no
SDL3 package.

**D9: In-app key repeat.** OS auto-repeat events are dropped, and repeat
runs on the app clock: 350 ms initial delay, 80 ms interval, 40 ms after
1.5 s held. Keyboard, gamepad and CEC (M4) will therefore behave the same.

**D10: Triple vs double buffering is still to be measured.** The default is
SDL KMSDRM's triple buffering; `--double-buffer` switches. Compare
input-to-photon latency and missed vsyncs on hardware and record the result
here.

**D11: Config file uses the `toml` crate (approved 2026-10-13).** Options
considered: `toml`, `basic-toml`, a hand-written TOML subset, JSON via
`serde_json`, or deferring. Chosen: `toml` with only the `std`, `serde` and
`parse` features (no writer). It is the most widely used, tracks the TOML
spec, and reports errors with line and column. `serde` (already in the
approved stack) comes in at the same time. Rules:
- The app only reads `~/.config/jellyroll/config.toml`. Settings changed in
  the UI will go to the state store, so user comments are never overwritten.
- Loading never fails. A missing file or key uses defaults. Unknown keys and
  out-of-range values (clamped) are warned about. A file that does not parse
  or has a wrong type falls back to all defaults with a warning. Warnings go
  to the journal and a one-line notice is shown on screen.
- Precedence: defaults < config file < command line. RUST_LOG beats
  `log.level`.
- Unknown-key detection compares against the serialized defaults, so it
  cannot drift from the structs. `dist/config.example.toml` is checked by a
  unit test to parse cleanly and equal the defaults.
- New settings are added only when the feature that uses them exists.

## Dependency approvals

| Crate | Status |
|---|---|
| `toml` | Approved 2026-10-13 (D11) |
