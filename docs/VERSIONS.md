# Verified versions

Spec §0 says to verify every pin against upstream rather than training data.
Each entry records where and when it was checked. Pins in `Cargo.toml` use
`=` so upgrades are deliberate.

## Toolchain

| Item | Version | Verified |
|---|---|---|
| rustc / cargo (stable) | 1.97.0 | `rustc --version`, 2026-10-13 |
| Edition | 2024 | |
| MSRV (`rust-version`) | 1.88 | needed for `let` chains |

## Crates (M1)

All versions are the newest non-yanked release in the crates.io sparse index
(`index.crates.io`) on 2026-10-13.

| Crate | Pinned | Notes |
|---|---|---|
| `sdl3` | 0.20.0 | Safe wrapper. Features used: `build-from-source-static`, `build-from-source-unix-console` (via our `build-sdl` feature) |
| `sdl3-sys` | 0.7.2+SDL-3.4.18 | Bundles **SDL 3.4.18** source for `build-sdl` builds |
| `sd-notify` | 0.5.0 | `notify(&[NotifyState])`, `watchdog_enabled() -> Option<Duration>` |
| `tracing` | 0.1.44 | |
| `tracing-subscriber` | 0.3.23 | `default-features = false`, features `fmt`, `std`, `ansi` (no `env-filter`/regex) |
| `tracing-journald` | 0.3.2 | Used when `JOURNAL_STREAM` is set |

## System libraries

The release binary links only to libc/libm/libgcc_s (checked with `ldd`).
SDL3 `dlopen`s everything else at runtime: `libdrm.so.2`, `libgbm.so.1`,
`libEGL.so.1`, `libGL.so.1`/`libGLESv2.so.2`, `libudev.so.1`, `libdbus-1.so.3`,
and optionally Wayland/X11 (dev mode), `libpipewire-0.3.so.0` and
`libasound.so.2`.

Checked in the CI/dev container (Ubuntu 24.04): Mesa 25.2.8 (llvmpipe, GL 4.5
core). **The reference HTPC hardware still has to be filled in (spec §17).**

## Not yet verified (pin when the milestone starts)

| Item | Milestone |
|---|---|
| `skia-safe` (and its prebuilt binaries for the target) | M2 |
| `cosmic-text` or `parley` | M2 |
| libmpv (render API, `hwdec`, `tone-mapping` option names) and a binding approach | M3 |
| libcec, or the kernel CEC uAPI | M4 |
| `tokio`, `reqwest` (rustls), `serde`, `serde_json`, image crates, `fast_image_resize` | M5 |
| Jellyfin server version, OpenAPI spec, OpenAPI generator | M5 |
| `redb` vs `rusqlite` | M5 (benchmark) |
| `zbus` | M8 |
