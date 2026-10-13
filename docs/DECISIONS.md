# Decisions and open questions

## Open questions (spec §16)

| # | Question | Status |
|---|---|---|
| 1 | Can SDL3 KMSDRM do refresh-rate matching, or do we need direct DRM/GBM/EGL from the start? | **Open.** M1 lists modes via `--print-diagnostics`. Run it on the reference hardware and check that every TV mode (23.976/24/25/50/59.94/60) appears with exact numerator/denominator. Test actual switching in M8. |
| 2 | Skia vs a custom GL renderer | **Decided: Skia.** On the HTPC (Intel HD 530, 4K) the scene holds 60 fps with 0 missed vsyncs; recording takes ~1.1 ms. The remaining cost is driver submission (~2 ms) and 4K fill (~8 ms GPU), which a custom renderer would not remove. The spec's p99 budgets (CPU < 4 ms, GPU < 6 ms) are not met on this 2015 GT2 iGPU at native 4K; recorded as a hardware limit, revisit if M3 overlays need the headroom. See "M2 benchmark results". |
| 3 | redb vs SQLite | **Decided: redb** (D30). Both are under 1 ms for everything the cache does; redb is pure Rust and a key-value store fits the data. |
| 4 | HDR on DRM/KMS | Open (M9). |
| 5 | Minimum Jellyfin server version | **10.10.0** (D31), the same floor as the official Jellyfin SDK. Written against API 12.2.0 (the SDK's current spec); tested against a **12.2** server. Media Segments/Trickplay availability is checked again in M7. |
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

## Decisions made in M4

**D24: One input pipeline for every source.** Keyboard, gamepad and
HDMI-CEC events are normalized in `crates/app/src/input.rs` into a
`Button` (which physical control: key, pad button or remote key) plus the
`InputAction` it maps to. Auto-repeat is done by the app (spec 5.4) and is
keyed by `Button`, so holding a D-pad, a stick, a remote arrow or a
keyboard arrow repeats at the same rate with the same acceleration. OS key
repeats are dropped. Releasing the button that is repeating stops it,
whatever the source.

**D25: Gamepad via the SDL gamepad API.** SDL's controller database gives
one layout for Xbox, PlayStation, Switch Pro and generic pads. Map:
D-pad/left stick move, South (A/Cross) select, East (B/Circle) back, West
play/pause, North info, shoulders seek, triggers previous/next, Start menu,
Back/Select search, Guide home. The stick and triggers become digital
buttons with hysteresis (press at 60%, release below 35%), which works as
the spec's deadzone and keeps a stick resting near the threshold from
chattering; they then repeat like any other button. Hotplug is handled.
The SDL path is tested end to end with an SDL virtual gamepad
(`sdl::tests::virtual_gamepad_end_to_end`), so no hardware is needed in CI.

**D26: HDMI-CEC through the kernel CEC API, not libcec.** The spec allows
either. The kernel API (`/dev/cecN`, `linux/cec.h`) is a handful of ioctls
and needs no extra library. It covers the adapters a PC is likely to have:
the Pulse-Eight USB adapter (`pulse8-cec`, attached with `inputattach`),
DisplayPort-to-HDMI adapters that tunnel CEC over DP AUX (i915, amdgpu,
nouveau), and Chromebox/NUC-class onboard CEC. libcec would add a C++
dependency licensed GPL-2.0-or-later. The kernel handles the CEC protocol
basics (address polling, retries, abort replies). jellyroll:
- claims a Playback logical address if none is configured (OSD name
  "Jellyroll");
- once it has a physical address, sends Image View On (turns the TV on,
  `cec_wake_tv`) and Active Source (switches the TV's input), and again
  whenever the address changes (cable replugged, TV power cycled);
- answers Give Device Power Status ("on") and Menu Request ("active");
- turns User Control Pressed/Released into press/release, with a 550 ms
  hold timeout in case a release is lost (the CEC spec has remotes repeat
  presses every 200–500 ms);
- treats Standby from the TV as "pause playback" (spec 5.4). Powering the
  TV off on shutdown comes with the power menu in M8.
`--print-diagnostics` only reads the device (capabilities, addresses) and
never claims an address or wakes the TV. CEC needs read-write access to
`/dev/cecN`, which belongs to the `video` group.

Native HDMI ports on Intel iGPUs have no CEC support in the i915 driver;
such machines need one of the adapters above. The HTPC drives its TV
from DisplayPort (DP-1) and gets `/dev/cec0` from i915's DP CEC tunneling.

**D27: Spatial focus graph, no row memory.** `crates/app/src/focus.rs`.
Every focusable node gives an id and its layout rect; the scene rebuilds
the graph when the layout changes (logical width, safe area), keeping
focus if its id still exists. Moving in a direction:
1. an explicit override for (node, direction) wins;
2. otherwise candidates are nodes lying in that direction; those
   overlapping the current node on the cross axis ("in the beam") beat
   those that don't, then the lowest `13·major² + minor²` wins (major =
   gap along the direction, minor = center offset across it; the weighting
   Android's focus finder uses).

Spec 10.2 asks for focus memory per container (returning to a row
restores its last position). It was built, tried on the HTPC, and removed
by request (2026-10-13): Up/Down always go to the item
straight above/below. Scroll and focus restoration on Back (spec 10.2's
back stack) is a different feature and still planned for M6.
The graph is the only record of focus: the scene asks it what is focused
and keeps no copy. Unit-tested on grids, rows of different sizes (nav
pills above cards), overrides and relayout. In the test scene, Select on a
nav pill picks that section and Select on a card shows its title, so
Select can be checked from every input source.

**D28: Play and Pause both toggle for now.** Remotes and keyboards have
separate Play and Pause keys, but the M4 action set has only `PlayPause`,
so pressing Play while playing pauses. M7 (player UX) adds explicit play
and pause actions.

**D29: M4 accepted with HDMI-CEC unverified on hardware.** Decided (2026-10-13): the HTPC's DisplayPort-to-HDMI link does not carry
CEC (below), so CEC control waits for hardware: a Pulse-Eight USB-CEC
adapter (inline, keeps the current video path) or a DP-to-HDMI adapter the
kernel's CEC guide lists as working. jellyroll now warns in the log when
it has a physical address but can't claim a logical one after 10 s, which
is what a dead CEC line looks like.

### M4 results on the HTPC (2026-10-13)

| Source | Result |
|---|---|
| Keyboard (console, KMSDRM) | **Pass.** Spatial moves, hold-to-repeat, Select (card title, nav pill) and Esc with `--allow-exit`. Row focus memory tried and removed (D27) |
| Gamepad | Not tested on hardware (no pad on the HTPC). The SDL path passes end to end with an SDL virtual gamepad in CI |
| HDMI-CEC | **Hardware can't do it.** `/dev/cec0` comes from i915 DP CEC tunneling on DP-1 (physical address 1.0.0.0, the TV's input 1), and jellyroll opens it. But the adapter never wins a logical address and `cec-ctl -S` shows an empty topology: nothing answers on the line, not even the TV (LG OLED G3, SIMPLINK on). The DP-to-HDMI adapter reports CEC tunneling but doesn't carry the CEC wire, a failure the kernel's CEC admin guide describes as common |

## Decisions made in M5

**D30: Metadata cache in redb.** Spec open question 3. Benchmarked the
cache's real workload (open at startup, read 8 × 20 KB home rows, write
one row and commit) in release builds: redb 4.3.0 open 0.33–0.83 ms,
read 0.09–0.22 ms, write+commit 0.55–1.4 ms; rusqlite 0.40.2 (bundled
SQLite, WAL) open 0.26–0.39 ms, read 0.08–0.18 ms, write 0.81–1.31 ms.
Both are far inside the 1.5 s start-to-interactive budget, so speed
doesn't decide. redb wins on being pure Rust (no bundled C build) and
on matching the data: whole home screens stored by key
(`home/<server>/<user>`), no queries. Library browsing pages come from the
server (spec 8), not a local database. A corrupt or outdated cache file is
deleted and rebuilt; any other open error (for example another process
holding it) leaves it alone. A test covers this, after the first version
deleted a cache that was merely locked.

**D31: Jellyfin API target.** The spec says not to assume the API. The
pinned source is the `openapi.json` checked in to the official
jellyfin-sdk-typescript (commit `7685bb8`, Jellyfin API **12.2.0**,
sha256 in `scripts/gen-jellyfin-dto.py`). Minimum server **10.10.0**, as
in that SDK (`MINIMUM_VERSION`); older servers get a clear "not supported"
screen. Every endpoint used (`/UserViews`, `/UserItems/Resume`,
`/Shows/NextUp`, `/Items/Latest`, `/Items`, Quick Connect,
`/Users/AuthenticateWithQuickConnect`) exists in that range. The
`Authorization` header copies the SDK's format exactly (`MediaBrowser
Client=…, Device=…, DeviceId=…, Version=…, Token=…`, values
`encodeURIComponent`-encoded).

**D32: DTOs from our own small generator.** The spec asks for DTOs
generated from OpenAPI by a pinned generator. Generating the whole API
(about 300 paths and 360 schemas) would add tens of thousands of lines
and slow builds, for the ~10 schemas we use. `scripts/gen-jellyfin-dto.py`
(stdlib Python) generates only the selected schemas and fields from the
pinned spec, with every field optional, missing fields defaulted, and a
catch-all variant on every enum, so a newer or older server can't make
parsing fail (spec 12). Requests ask the server for the same minimal field
sets (`fields=`, `enableImageTypes`, `imageTypeLimit=1`,
`enableTotalRecordCount=false`). To regenerate against a real server:
`scripts/gen-jellyfin-dto.py <(curl -s SERVER/api-docs/openapi.json)`.

**D33: Network thread and image pipeline.**
- The network side runs on one thread with a single-threaded tokio
  runtime (`net` thread); disk and decode work goes to its small blocking
  pool (4 threads).
- It talks to the render thread through bounded channels: 64 commands in,
  256 events out. The render side uses `try_send`/`try_recv` and never
  blocks; each event also wakes the render loop through the platform
  waker.
- Images:
  1. The UI computes exactly what's on screen, plus a few cards ahead,
     and the size in output pixels (4K: posters 320×480).
  2. The server resizes (`maxWidth`/`maxHeight`, `format=Webp`); the
     `tag` in the URL changes when the artwork does.
  3. The bytes are cached on disk under an FNV-1a hash of server+path,
     with a size cap and least-recently-used eviction by mtime
     (`cache.images_mb`).
  4. The worker pool decodes (image crate: WebP/JPEG/PNG), center-crops
     and resizes to the exact pixel size (fast_image_resize, Catmull-Rom),
     and bakes in card corners or the backdrop scrim.
  5. The render thread only uploads: at most 2 ms per frame, with an LRU
     texture cap (`cache.textures_mb`).
- Each new list of wanted images replaces the previous queue, so work for
  rows scrolled away is dropped (spec 4).
- BlurHash placeholders (from item metadata) decode on the same pool and
  show until the real image arrives.

**D34: Sign-in is Quick Connect in M5.** The TV shows a 6-digit code;
the user approves it from any signed-in Jellyfin app. If Quick Connect is
turned off on the server, the screen explains how to turn it on.
Username/password sign-in needs the on-screen keyboard (M6). Tokens are
kept in `~/.local/share/jellyroll/state.json` (mode 0600, atomic writes)
with a random per-install DeviceId; passwords are never stored. A token
the server rejects (401) is forgotten and sign-in starts again. The state
file holds several servers and users; the picker UI comes with M6.

**D35: Home screen behavior.**
- Rows in jellyfin-web's default order: My Media, Continue Watching, Next
  Up, Latest per library (except playlists, collections, live TV and
  folders), then Favorites. Card shapes: posters for movies/shows,
  landscape for episodes and libraries, square for music.
- Stale-while-revalidate: at startup the last home screen is drawn from
  the cache at once, then replaced by fresh data. A refresh keeps focus on
  the same item and keeps each row's scroll. The home screen refreshes
  every 5 minutes.
- If the server is unreachable while cached data is shown, an "Offline ·
  showing saved data" badge appears and the app keeps retrying with
  backoff (2 s up to 60 s). With no cache, a full-screen explanation with
  Select to retry (spec 8: never a blank screen).
- Server choice: `[server] url` in the config, else the last server used,
  else the **server screen** (D37). Discovery is never automatic.
- `--demo` keeps the synthetic M2 scene; `--bench-pacing`/`--bench-idle`
  imply it, so render benchmarks and CI never depend on a server.

**D37: Choosing a server is a screen, discovery is a button.** Decided after the first HTPC run (2026-10-13): automatic discovery failed on
the network and the fallback (edit a config file that didn't exist yet) was
a dead end. Now the first screen is "Connect to your Jellyfin server":
- **Typing:** an address field. A physical keyboard types directly (SDL
  text input, so layout and Shift apply, and shortcut keys like `c`/`i`
  are suspended while typing; Enter connects). The remote or a gamepad
  uses an on-screen keyboard: digits, letters, `. - : /`, Delete, Clear,
  `:8096` and `https://`.
- **Searching:** "Search my network" runs LAN discovery on demand and
  lists what answers; picking one connects.
- **Errors:** an address that fails returns to this screen with the
  reason, keeping the text. A remembered server that's down shows a
  problem screen where Select retries and Back goes to this screen; Back
  from the Quick Connect code does the same.
- The config file's `[server] url` still wins when set, for unattended
  installs.

**D38: Startup order.** Measured on the HTPC with the M5 build: first
frame at 843 ms (budget 500). Breakdown: ~235 ms opening all 18
`/dev/input` devices for the access check (opening can wake a device
from USB autosuspend), 181–194 ms SDL KMSDRM video init, ~100 ms GL
context, 234 ms gamepad subsystem, 0–13 ms CEC setup. Fixes: the access
check stops at the first readable device (14 ms), and gamepads and CEC
start right after the first frame (`Platform::start_input_devices`;
pads already connected arrive as hotplug events). Result in the M5
results below.

### M5 results on the HTPC (2026-10-13)

Server: Jellyfin **12.2.0** on a public domain (HTTPS with a
public certificate, system roots, no extra config).

| Check | Result |
|---|---|
| Server screen → address typed on the keyboard → Quick Connect (code entered at `/web/#/quickconnect`) | **Pass** |
| Real home screen: rows, artwork, backdrops, navigation | **Pass.** |
| Stale-while-revalidate: second start shows the cached home, no sign-in | **Pass.** Cached home ready at 345 ms, before the first frame |
| First frame (budget < 500 ms) | **393 ms** (was 843: input check 14 ms, SDL video 194 ms, GL 105 ms) |
| Start to interactive home, cached (budget < 1.5 s) | **≈ 0.4 s** |
| Start to home, fresh fetch on first run (budget < 2.5 s) | Not timed precisely; "a second or two" including Quick Connect approval |

Note: the jellyfin-web Quick Connect page is in the user menu (avatar,
top right), not in the profile or settings pages, and only appears while
Quick Connect is enabled. The sign-in screen now points to the direct
URL.

**D36: TLS and the mock server.**
- reqwest 0.13's `rustls` feature: aws-lc-rs crypto, and system trust via
  rustls-platform-verifier. `[server] ca_file` adds a PEM CA or a
  self-signed server certificate (spec 12).
- `crates/mock-server` (`jellyroll-mock`) is a small hand-written HTTP/1.1
  server. It implements exactly the endpoints jellyroll calls, over a
  synthetic library of any size (`--movies 10000` for M6), with generated
  artwork, a self-approving Quick Connect (code 424242), token checks and
  discovery.
- An end-to-end test runs the real network thread against it: connect,
  Quick Connect, home rows, a poster and a placeholder decoded, then a
  restart that shows the cached home first with no sign-in.

**D39: M6 browsing architecture.**
- A page stack over the home screen: Library, Detail, Search and
  Settings. Back pops a page; Home clears the stack; `/` (or the remote's
  Search button) opens Search from anywhere. The home screen gains a nav
  bar: Home, each library, Search, Settings.
- Pages never block on the network. They ask through a `Ctx` that
  collects commands; a fetch worker on the network thread (4 at a time)
  answers each with `Event::Fetched { id, .. }`, and a page ignores answers
  for requests it no longer cares about.
- Library grid: 100-item pages, requested around the visible rows only,
  so a 10,000-item library costs the same as a small one. Only rows near
  the viewport are drawn. Sort (name, date added, year, rating), unwatched,
  favorites and genre filters run on the server; the A-Z column (name sort)
  asks the server how many items sort before a letter and jumps there.
- Detail pages for movies, series, seasons and episodes: Play or Resume /
  From the start, watched and favorite toggles, seasons, episodes, cast,
  similar items. Video and audio badges come from the media streams;
  a codec this GPU can't decode turns its badge red and says so (D19:
  never a silent fallback).
- Play from the detail page streams the file as-is (`static=true`, the
  server never transcodes) through mpv with the token as an HTTP header,
  from the resume point. Progress reporting, next episode and track
  choice are M7.
- Search: 300 ms debounce, separate rows for movies, shows and episodes.
  A keyboard types directly; Enter jumps to the results. The on-screen
  keyboard is there for a remote or gamepad.
- Settings: account (sign out, change server) and "About this device"
  (versions, GPU, display, hardware decode support).
- Password sign-in (spec 8 fallback): from the Quick Connect screen,
  username and password with the server's public users to pick from. The
  password is sent once and never stored.
- User-visible strings live in `home/strings.rs` (spec 10.4); a test
  checks that every character exists in the bundled fonts.

**D40: What a steady-state frame is.** M6 added two things to the
allocation check (D5):
- **The counter is per thread.** It used to count every thread, so image
  decoding and JSON parsing on the network thread showed up as render
  frame allocations (tens of thousands per frame while scrolling). The
  render thread is the one the check is about.
- **Loading frames are reported, not failed.** A frame that starts
  loading something (sends requests, rebuilds the image wants list),
  uploads a texture or shapes a string for the first time must allocate.
  Those frames are counted as `loading_frames` and their allocations are
  excluded. Every other frame, including scrolling over content that is
  already there, must allocate nothing.
- Image wants are rebuilt at most every 100 ms while images keep arriving
  (immediately on input), instead of after every upload.

**D41: `--bench-scroll SECS` (spec 13, M6 exit criterion).** Starts the
mock server in-process with 10,000 movies and a throwaway signed-in state
(nothing of the user's is touched, no network needed), opens Movies from
the nav bar like a user, waits for the grid, then presses Down every 50 ms
(faster than key repeat) for SECS while pages and artwork load. It prints
the same numbers as `--bench-pacing` plus presses and loading frames, and
fails on the spec 2 budgets (CPU p99 < 4 ms, GPU p99 < 6 ms, no missed
vsyncs, no allocating steady-state frames). Under Xvfb with llvmpipe
(software GL) it reports 0 allocating frames; CPU/GPU numbers there are
meaningless, so the hardware checklist runs it on the HTPC.

**D42: Artwork textures are outside Skia's cache budget.** The first
`--bench-scroll` runs on the HTPC (HD 530, 4K) had 0 missed vsyncs and 0
allocating frames, but CPU p99 8-11 ms and GPU p99 7.4-8.4 ms.
- The library grid fetched the focused item's backdrop on every step and
  never drew it (a 1920x1080 upload per press at 4K). Removed.
- Artwork was created as *budgeted* Skia textures. The app keeps up to
  512 MB of artwork (its own LRU, `[cache] textures_mb`); Skia's budget is
  256 MB, so Skia was always over it, purging and rebuilding its own
  scratch resources and slowing uploads (3 ms p50 per uploading frame,
  steady frames at 5.5 ms p50). Artwork is now unbudgeted; the app's LRU
  is its only cap, and Skia's budget covers its own resources.
- The GPU p99 is the HD 530 at native 4K, as measured in M2 (see
  "M2 benchmark results", open question 2): above the 6 ms budget, still
  60 fps with no missed vsyncs.

## Dependency approvals

| Crate | Status |
|---|---|
| `toml` | Approved 2026-10-13 (D11) |
| `tokio`, `reqwest` (rustls), `serde_json`, `image`, `fast_image_resize`, `redb` | Listed in spec §3; versions in `docs/VERSIONS.md` (M5) |
