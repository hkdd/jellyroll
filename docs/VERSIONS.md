# Verified versions

Spec §0 says to verify every pin against upstream rather than training data.
Each entry records where and when it was checked. Pins in `Cargo.toml` use
`=` so upgrades are deliberate.

## Toolchain

| Item | Version | Verified |
|---|---|---|
| rustc / cargo | **1.99.0**, pinned in `rust-toolchain.toml` | CI used unpinned `stable`; when 1.99 shipped, its new `chunks_exact_to_as_chunks` lint failed `clippy -D warnings` while local 1.97 passed. Pinned 2026-10-13 so every build uses the same compiler and lints |
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
| `serde` | 1.0.229 | `derive` feature; config structs (and Jellyfin DTOs from M5) |
| `toml` | 0.9.8 | `default-features = false`, features `std`, `serde`, `parse` (read-only). Pulls in `toml_parser` 1.1.5, `winnow` 0.7.15, `serde_spanned` 1.1.2, `toml_datetime` 0.7.5 |

## System libraries

The release binary links only to libc/libm/libgcc_s (checked with `ldd`).
SDL3 `dlopen`s everything else at runtime: `libdrm.so.2`, `libgbm.so.1`,
`libEGL.so.1`, `libGL.so.1`/`libGLESv2.so.2`, `libudev.so.1`, `libdbus-1.so.3`,
and optionally Wayland/X11 (dev mode), `libpipewire-0.3.so.0` and
`libasound.so.2`.

Checked in the CI/dev container (Ubuntu 24.04): Mesa 25.2.8 (llvmpipe, GL 4.5
core). **The reference HTPC hardware still has to be filled in (spec §17).**

## Crates (M2)

Checked against the crates.io index on 2026-10-13.

| Crate | Pinned | Notes |
|---|---|---|
| `skia-safe` | 0.153.3 | Skia milestone 153. Features: the crate defaults (`binary-cache`, `embed-icudtl`, `pdf`, `jpeg`) plus `gl`. Only feature sets that rust-skia publishes prebuilt binaries for avoid a from-source Skia build; this one maps to `skia-binaries-b7f043e0b1e2a850e702-x86_64-unknown-linux-gnu-ganesh-gl-jpegd-jpege-pdf.tar.gz` in the 0.153.3 release of rust-skia/skia-binaries (checked on the release page). `gl` alone has no prebuilt binary. |
| `cosmic-text` | 0.19.0 | `default-features = false`, feature `std` (no fontconfig, no swash rasterizer: Skia rasterizes glyphs) |
| `unicode-script` | 0.5.8 | Already in the tree via cosmic-text; named directly because the `Fallback` trait takes its `Script` type |

The release binary grew from 4.6 MB to 13.9 MB (stripped), including 3.9 MB
of fonts. The prebuilt Skia links dynamically against `libstdc++.so.6`,
`libfreetype.so.6` and `libfontconfig.so.1` (plus their own deps: expat,
zlib, bzip2, libpng, brotli). Building needs the development packages:
Fedora `gcc-c++ freetype-devel fontconfig-devel` (`gcc-c++` rather than `libstdc++-devel` alone, so the libstdc++ dev files match the installed gcc version; a mismatch leaves `-lstdc++` unresolved). jellyroll loads
fonts only from its own bundled bytes (`FontMgr::custom_empty`), so
fontconfig is linked but never consulted. rust-skia also publishes an
`ftembed` (embedded FreeType) variant, but only with `textlayout`, which
adds ICU and HarfBuzz we don't use.

## M3 system libraries

| Library | Checked | Notes |
|---|---|---|
| libmpv | client API 2.2 (mpv 0.37, Ubuntu 24.04 sandbox) | The binding requires client API major 2 (unit test). HTPC (Fedora 44): client API **2.5**, confirmed via `--print-diagnostics`. |
| libva / libva-drm | 1.20 headers | VA profile enum values used by the probe are from va.h and stable across libva 2.x. |
| Intel media driver (HTPC) | iHD 26.1.5 (RPM Fusion `intel-media-driver`); the app's VA-API probe on the HTPC reports decode for MPEG-2, VC-1, H.264, VP8, HEVC 8-bit and none for HEVC 10-bit, VP9, AV1 (matches `vainfo`) | Fedora's own `libva-intel-media-driver` 26.3.5 lists no H.264/HEVC. |

## M4 input

| Item | Checked | Notes |
|---|---|---|
| Kernel CEC uAPI (`linux/cec.h`) | Linux 6.x headers; the API has been stable since 4.10 | ioctl numbers and struct layouts are hand-declared in `crates/platform/src/cec.rs` and checked by size/offset unit tests. No libcec (D26). |
| SDL gamepad API | SDL 3.4.18 (bundled through sdl3-sys 0.7.2) | Includes SDL's controller mapping database; virtual gamepads used in tests. |

## M5 Jellyfin client

Crate versions checked with `cargo info` against crates.io on 2026-10-13
(newest release of each).

| Item | Version | Notes |
|---|---|---|
| Jellyfin API (target) | **12.2.0**, OpenAPI 3.0.4; tested against a 12.2 server | `openapi.json` from jellyfin/jellyfin-sdk-typescript @ `7685bb8e2c3aeda89ca831c2228923affd022fbb` (2026-10-06), sha256 `bc8c668b…ab80`; that SDK's `MINIMUM_VERSION` is 10.10.0 (D31) |
| `tokio` | 1.53.2 | `rt`, `time`, `sync`, `macros` (app); current-thread runtime on the `net` thread |
| `reqwest` | 0.13.5 | `default-features = false`, `rustls` (aws-lc-rs + rustls-platform-verifier). 0.13 made rustls the default TLS and renamed the feature from `rustls-tls` |
| `serde_json` | 1.0.154 | |
| `image` | 0.25.10 | `default-features = false`, `jpeg` (zune-jpeg), `png`, `webp` (image-webp) |
| `fast_image_resize` | 6.1.0 | default features |
| `redb` | 4.3.0 | `begin_read` moved to the `ReadableDatabase` trait in 4.x (D30) |
| `rusqlite` | 0.40.2 | Benchmarked only (D30), not a dependency |
| `zbus` | 5.19.0 (2026-08-09, newest on crates.io 2026-10-13) | M8, logind power actions. `default-features = false`, `tokio`; a short-lived current-thread runtime per call (D51) |

## Fonts

Bundled in `assets/fonts/`, generated by `scripts/subset-fonts.sh` with
fonttools 4.66.1. All are SIL OFL 1.1 (`assets/fonts/OFL.txt`).

| File | Source | Size |
|---|---|---|
| NotoSans-{Regular,Bold}.ttf | notofonts/notofonts.github.io @ `4e15001b9fb484cc871d7b2ad930cd2226edf260`, unhinted TTF, subset to Latin/Latin Extended/Greek/Cyrillic/punctuation/symbols | ~230 KB each |
| NotoSansJP-{Regular,Bold}.otf | notofonts/noto-cjk @ `f8d157532fbfaeda587e826d4cd5b21a49186f7c`, `Sans/SubsetOTF/JP`, subset to kana, CJK punctuation, full/half-width forms and all JIS X 0208 kanji (levels 1 and 2) | ~1.7 MB each |
