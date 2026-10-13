# Decisions and open questions

## Open questions (spec §16)

| # | Question | Status |
|---|---|---|
| 1 | Can SDL3 KMSDRM do refresh-rate matching, or do we need direct DRM/GBM/EGL from the start? | **Open.** M1 lists modes via `--print-diagnostics`. Run it on the reference hardware and check that every TV mode (23.976/24/25/50/59.94/60) appears with exact numerator/denominator. Test actual switching in M8. |
| 2 | Skia vs a custom GL renderer | **Decided: Skia.** On the HTPC (Intel HD 530, 4K) the scene holds 60 fps with 0 missed vsyncs; recording takes ~1.1 ms. The remaining cost is driver submission (~2 ms) and 4K fill (~8 ms GPU), which a custom renderer would not remove. The spec's p99 budgets (CPU < 4 ms, GPU < 6 ms) are not met on this 2015 GT2 iGPU at native 4K; recorded as a hardware limit, revisit if M3 overlays need the headroom. See "M2 benchmark results". |
| 3 | redb vs SQLite | Open (M5). |
| 4 | HDR on DRM/KMS | Open (M9). |
| 5 | Minimum Jellyfin server version | Open (M5). |
| 6 | Retained tree vs immediate mode | **Leaning immediate-style** (D15): the scene is rebuilt on each dirty frame, with everything expensive cached by the renderer. Revisit with the M6 10k-item scroll benchmark. |
| 7 | NVIDIA | Open. M1 code is vendor-neutral, but only Mesa has been exercised. |
| 8 | Video decode on the reference hardware | **Constraint found.** The HTPC's Intel HD 530 (Skylake) decodes HEVC 8-bit and H.264 in hardware, but not HEVC 10-bit, VP9 or AV1 (intel/media-driver README codec table, SKL column, checked 2026-10-13). M3's exit test (4K HEVC 10-bit) cannot use hardware decode here. Confirmed on the HTPC with `vainfo` (iHD 26.1.5 from RPM Fusion): decode for MPEG-2, H.264 Main/High, VC-1, VP8, HEVC Main; no HEVC Main10, VP9 or AV1. CPU decode is not viable either (i3-6100T: 2 cores / 4 threads, 35 W). Fedora's own `libva-intel-media-driver` reports no H.264/HEVC at all (patent-restricted build); the full driver is RPM Fusion nonfree `intel-media-driver`. **Decided (D19):** direct play whatever the hardware decodes; say clearly when it can't; no silent software decode and no automatic transcoding. Unsupported content on this HTPC is the trigger for a hardware upgrade. |

## Decisions made in M1

**D1: Hand-rolled GL loader instead of a binding crate.** M1 calls about 15
GL functions (clear, scissor, timer queries). A small loader in
`crates/platform/src/gl.rs` avoids adding a dependency outside the spec's
list. Skia and mpv resolve their own symbols through `SDL_GL_GetProcAddress`.

**D2: Desktop GL 3.3 core first, GLES 3.0 fallback.** Timer queries are
core in GL 3.3. On GLES they need `GL_EXT_disjoint_timer_query`, and without
it the HUD shows GPU time as N/A. Both skia-safe (GL backend) and libmpv
support either API. Revisit if a target driver only offers GLES.

**D3: M1 drawing was scissored `glClear`s** (superseded in M2). No shaders,
textures or Skia, as spec §0 required: prove the loop first. The bitmap font
and rect batch were removed when the Skia renderer landed.

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

## Decisions made in M2

**D12: Skia via `skia-safe` 0.153.3, Ganesh GL backend, prebuilt binary.**
Skia shares SDL's GL context and draws straight into framebuffer 0
(`wrap_backend_render_target`, RGBA8, 8 stencil bits). The feature set is
the crate defaults plus `gl`, because rust-skia only publishes prebuilt
binaries for sets that include `jpeg` and `pdf`; anything else forces a
from-source Skia build (python, ninja, Skia's git deps, tens of minutes).
The unused jpeg/pdf code costs binary size only. All Skia calls are safe
Rust, so `jellyroll-render` is `#![forbid(unsafe_code)]`.

**D13: Text = cosmic-text shaping + Skia glyph rasterization, bundled fonts
only.** cosmic-text 0.19 (no default features) shapes and ellipsizes;
glyph ids and positions become a Skia `TextBlob` drawn with typefaces loaded
from the same font bytes through `FontMgr::custom_empty()`, so no system
font scan is involved (Skia's prebuilt library still links fontconfig and
freetype; see VERSIONS.md). A custom `Fallback` limits
font fallback to the bundled families (Noto Sans, then Noto Sans JP). Text
renders identically on every machine.

**D14: Bundled, subset Noto fonts (3.9 MB).** Regular and Bold of Noto Sans
(Latin/Greek/Cyrillic) and Noto Sans JP (kana + JIS X 0208 kanji),
subset by `scripts/subset-fonts.sh` from pinned upstream commits. Korean and
Chinese-only characters are not covered yet; adding them is a font-subset
change, not a code change.

**D15: Immediate-style scene, aggressive caching.** The UI is rebuilt each
dirty frame against the `Renderer`; nothing is re-shaped or re-rasterized:
- Shaped text is cached as `TextBlob`s keyed by (text, size, weight,
  max width), evicting entries unused for 120 frames once 4096 are cached.
- Shadows are blurred once into a small nine-patch texture per (radius,
  blur, colour): fixed corners, stretchable edges and a transparent centre,
  drawn as a Skia lattice in device pixels. Any card size or focus scale
  reuses it, and the area hidden under the card is never filled. The margin
  is 3 sigma of the blur.
- Backdrop legibility scrims are baked into the backdrop texture at upload
  (spec 6.5), so the backdrop costs one full-screen pass.
- Images that are fully opaque are uploaded as `AlphaType::Opaque`, so Skia
  draws them without blending.
- Text origins are snapped to whole device pixels. Skia reuses a blob's
  cached glyph runs only for whole-pixel moves, so fractional motion
  (scrolling, the bench sway) rebuilt every title every frame; snapping
  halved recording time (profiled with perf).
- Poster corners are cut into the texture at upload (spec 6.5), so cards
  are plain image draws, which Skia batches, instead of one rounded-rect
  image-shader draw each.
- No clear when an opaque backdrop covers the screen; the crossfade draws
  the outgoing backdrop at full opacity under the incoming one.
- Rounded artwork is one draw (image shader on a rounded rect), not a
  clip plus image.
- Cards outside the viewport are skipped (row virtualization).
- `push_opacity` (saveLayer) exists but the scene does not use it.

**D16: `display.ui_render_scale`** (0.5 to 1.0, default 1.0) renders the UI
into a smaller offscreen target and upscales it with linear filtering (spec
6.4). Verified at 0.5 in software at 3840×2160.

**D17: Artwork uploads have a per-frame budget (2 ms).** Posters arrive as
placeholders first and are generated and uploaded within the budget, so the
first frame is not delayed and later frames never hitch on uploads. This is
the shape M5's real image pipeline will use (decode on workers, upload on
the render thread within the budget).

**D18: HUD text is excluded from the steady-state allocation check.** HUD
lines change every 500 ms and are re-shaped then (cosmic-text allocates).
The loop counts the HUD's allocations separately, so `--bench-pacing --hud`
still checks the scene's frames.

### M2 benchmark results

| Machine | Output | CPU p50/p99 ms | GPU p50/p99 ms | Missed vsyncs | Result |
|---|---|---|---|---|---|
| CI container, Mesa llvmpipe (software) | 1280×720 | 8.7 / 14.3 | 0.1 / 25.1 | 0 | Not meaningful: software rasterizer runs GPU work on the CPU |
| HTPC, Intel HD 530 (SKL GT2), first M2 build | 3840×2160 @ 60 | 10.7 / 14.0 | 16.1 / 17.4 | 1465 of 2135 (locked at 30 fps) | **Fail** |
| HD 530, fill-rate fixes (`7fdb778`) | 3840×2160 @ 60 | 6.6 / 10.8 (record 4.7, submit 1.9) | 10.9 / 11.2 | 0 (steady 60 fps) | Fail (budgets) |
| HD 530, same, `ui_render_scale = 0.75` | 3840×2160 @ 60 | 6.5 / 7.4 (record 4.7, submit 1.8) | 6.3 / 7.1 | 0 | Fail (budgets, marginal) |
| HD 530, text snapping + baked corners + no clear (`eff4fcb`) | 3840×2160 @ 60 | 3.3 / 5.5 (record 1.1, submit 2.2) | 8.0 / 10.6 | 0 (steady 60 fps) | Smooth; budgets not met |
| HD 530, same, `ui_render_scale = 0.75` | 3840×2160 @ 60 | 4.7 / 6.6 (record 1.8, submit 2.9) | 6.1 / 11.3 | 0 | No better than full resolution |

First hardware run analysis: GPU-bound on fill rate. The scene blended
roughly 31M pixels per 4K frame (backdrop, two near-full-screen scrim
gradients, and 60 shadows each about twice the card's area and mostly
hidden under it). A custom GL renderer would have to fill the same pixels,
so this is a scene-cost problem, not a Skia verdict. The fixes (shadow
nine-patch with transparent centre, scrims baked into the backdrop, opaque
images drawn without blending) cut that to roughly 16M pixels; in software
rendering at 4K the frame time dropped about 20%. The CPU number is now
split into `record` (building draw commands) and `submit` (Skia flush and
GL submission); a high `submit` while GPU-bound is the driver making the CPU
wait, not CPU work.

Third hardware run (after text snapping, baked corners, no redundant
clear): CPU 3.3 ms p50 with recording down to 1.1 ms; GPU 8.0 ms p50.
`ui_render_scale = 0.75` did not help on this machine. Intel iGPUs lower
their clock under lighter load, and the offscreen pass plus the 4K upscale
blit cost more than they save. Full resolution stays the default.

Second hardware run: steady 60 fps at full 4K, but GPU 10.9 ms and
recording 4.7 ms. A perf profile (same scene, this container) showed text
blob regeneration as over half of recording, fixed by pixel snapping
(above). The benchmark window now also starts only after the initial
artwork uploads finish, so upload frames no longer show up as steady-state
spikes. The HD 530 is a 2015 GT2 iGPU; if full-resolution 4K stays above
the 6 ms GPU budget, `ui_render_scale = 0.75` is the intended setting for
it (spec 6.4).

## Decisions made in M3

**D19: Playback policy: direct play by detected capability, never
silently degrade.** Decided (2026-10-13): the client must
work on any hardware, so what it can decode is detected at runtime
(VA-API profile query, D21) rather than assumed. Anything the hardware
decodes is direct-played. Anything it can't is reported clearly: a red
on-screen badge and a log warning naming the codec. There is no silent
software decode (spec 7.1) and no automatic server transcode; if content
won't play on this HTPC, the hardware gets replaced. Server transcoding may
come back later as an explicit, off-by-default setting.

**D20: Hand-written libmpv binding, non-blocking on the render thread.**
`crates/video` declares the ~25 libmpv functions it uses (client API 2.x,
checked against the 2.2 headers) instead of adding a binding crate. With
`MPV_RENDER_PARAM_ADVANCED_CONTROL` the render thread must never wait for
mpv's core (render.h, "Threading"): the first build deadlocked when the
render thread called `mpv_get_property_string` during a video reconfig
while the core waited on the renderer. The binding now only uses
non-blocking calls on that thread: every value (time, duration, pause,
decoder, codec, size, fps, drop counters) is an observed property
delivered as an event, and every command is `mpv_command_async`.
Synchronous getters were removed from the API so the mistake can't recur.
mpv's wakeup and render-update callbacks only set an atomic flag and push
an SDL user event (the platform waker).

**D21: Decode capability probe via libva.** At startup and in
`--print-diagnostics` the app opens each DRM render node and asks VA-API
which profiles have a decode (VLD) entry point, the query `vainfo` makes,
mapped to codecs (H.264, HEVC 8/10-bit, VP9 8/10-bit, AV1, ...). M5's
Jellyfin device profile will be built from this. Linked: `libva`,
`libva-drm`.

**D22: mpv configuration (spec 7.1).** `hwdec` is `vaapi`, plus `nvdec`
only when the NVIDIA kernel driver is loaded (`auto-safe` probed CUDA and
VDPAU everywhere and logged errors). `video-sync=display-resample`,
`interpolation=no`, demuxer cache 150 MiB / 50 MiB back, no config files,
user scripts, built-in scripts (stats, console, select, ytdl, ...), OSC
or default key bindings. `audio-fallback-to-null=yes`, so a missing audio
device (TV or receiver off) doesn't stop video (spec 12).

`gpu-hwdec-interop=vaapi` (or `auto` with the NVIDIA driver):
`auto` also loads the DRM overlay-plane and CUDA interops, which need
resources we don't pass and log errors.

**VA-API on a bare console needs the render node.** mpv gets its VA-API
display from the windowing system (X11, Wayland) or from a DRM render node
passed as `MPV_RENDER_PARAM_DRM_DISPLAY_V2.render_fd`. On the KMS console
there is no X11/Wayland, and the first HTPC run silently decoded HEVC
8-bit in software (the software-decode badge caught it). The player now
opens the render node found by the VA-API probe and passes it; the struct
and file stay alive for the render context's lifetime.

**D23: Frame handoff (spec 6.3).** While video advances, the loop renders
every vsync: reset GL to defaults (mpv requires default state; Skia leaves
blending, scissor, bound buffers and programs set), mpv renders into FBO 0,
then Skia's GL state cache is invalidated (`GrDirectContext::reset`) and
the OSD is drawn only if something is visible. With nothing on screen,
Skia is skipped entirely. `mpv_render_context_report_swap` follows each
swap. Paused, the loop idles and redraws only on input or when the OSD
appears/disappears. The GL error flag is cleared during the reset so mpv
isn't blamed for earlier errors.

### M3 results on the HTPC (Intel HD 530, 3840×2160 @ 60, 2026-10-13)

`--bench-play 15` on 20 s synthetic 4K 23.976 fps clips
(`scripts/make-test-clips.sh`), OSD toggled every 2 s during each run:

| Clip | Decoder | CPU, process | CPU, render thread | Dropped / late |
|---|---|---|---|---|
| H.264 | vaapi | 10.8% | 5.3% | 0 / 0 |
| HEVC 8-bit | vaapi | 12.2% | 5.7% | 0 / 0 |
| HEVC 10-bit | software (flagged: badge + warning) | 196.7% | 21.7% | 0 / 0 |

For comparison, software decode before the render-node fixes: H.264 94%,
HEVC 8-bit 151% of a core. M3's exit criteria are met for every codec this
GPU decodes; HEVC 10-bit behaves as D19 requires.

Two bugs found on the way: mpv had no VA-API display on the bare console
(fixed by passing the render node, D22), and the render node was opened
read-only, which is enough for the capability query but not for allocating
decode surfaces ("Failed to create decode context: 2 (resource allocation
failed)"). Known noise: mpv logs `after creating texture: OpenGL error
INVALID_OPERATION` twice at video start on this driver; playback is
unaffected.

## Dependency approvals

| Crate | Status |
|---|---|
| `toml` | Approved 2026-10-13 (D11) |
