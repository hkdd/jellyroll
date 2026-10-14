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
- That changed nothing on the HTPC. The real cause: image uploads. The
  per-frame upload budget (2 ms) was checked only after each upload, so
  frames overran it (3 ms p50 per uploading frame), and on top of ~2 ms
  of other work at 4K that is over the 4 ms CPU budget. Uploads now stop
  *before* one is expected to overrun (measured ns/byte, moving average),
  and the budget is 1.5 ms. The benchmark also mislabeled uploading
  frames as steady (Skia copies pixels in C++, which the Rust allocation
  counter doesn't see); any frame that uploads or shapes new text is now
  a loading frame.
- Next HTPC run: steady frames 1.5 ms p50 (fine); nearly every scrolling
  frame uploads. Two fixes: the upload-time estimate is kept separately
  for small placeholders (mostly fixed cost) and artwork (one per-byte
  rate made every poster look like ~2 ms, so only one upload fit per
  frame); and pixels are premultiplied on the decoding thread, so Skia
  has nothing to convert on the render thread (rounded corners made
  posters non-opaque, uploaded as unpremultiplied). The benchmark prints
  per-upload timings (opaque check, copy, GPU upload).
- The GPU p99 is the HD 530 at native 4K, as measured in M2 (see
  "M2 benchmark results", open question 2): above the 6 ms budget, still
  60 fps with no missed vsyncs.

**D43: Reused textures for artwork.** `--bench-upload` on the HTPC (HD
530, Mesa iris), mean ms per image:

| | new texture each (old) | write into an existing one | new empty texture |
|---|---|---|---|
| poster 320x480 | 1.03 | 0.72 | 0.26 |
| placeholder 32x48 | 0.07 | 0.07 | 0.02 |
| backdrop 1920x1080 | 6.1 | 1.1 | 2.6 |

- Artwork of 64 KB or more is drawn from a pool of GPU textures keyed by
  (width, height, opaque): a released image's texture is kept (up to 48
  per size) and the next image of that size is written into it. This
  mostly matters for backdrops (6.1 to ~1.1 ms: a visible hitch whenever
  focus changed on the home and detail pages) and trims posters ~30%.
- Whether an image is opaque now comes from the decoding thread
  (premultiplying already visits every pixel); scanning on the render
  thread cost 1.8 ms per backdrop.
- Next: re-run `--bench-scroll`; if loading frames still exceed the CPU
  budget, choose between rendering the UI at 1080p with artwork sized to
  match, and documenting the overrun at native 4K.

**D44: UI resolution setting; native by default.** With
textures reused (D43), an artwork upload during scrolling still cost
~2 ms on the HD 530 at native 4K (the driver copies synchronously while
the GPU is busy), so frames that load new artwork exceed the 4 ms CPU
budget, and GPU p99 sits at the 6 ms line (5.98 and 6.45 ms in two runs).
Decided:
- `[display] ui_resolution = "auto" | "native" | "1080p"` replaces
  `ui_render_scale`. `auto` (default) draws the UI at 1080p and upscales
  on outputs taller than 1080 lines, and natively otherwise. Video is
  unaffected (mpv renders at the output's resolution).
- Artwork is requested at the size the UI is drawn at, so at 1080p
  uploads are a quarter of the 4K size.
- Settings has a Display toggle (shown only above 1080p): "1080p,
  upscaled (smoother)" / "full resolution (sharpest)". It applies at once
  (surfaces rebuilt, textures re-requested at the new size) and is saved
  in `~/.local/share/jellyroll/ui.json`, which wins over config.toml.
- Results on the HTPC (HD 530, 3840x2160, `--bench-scroll 20`):

  | UI resolution | CPU p50 / p99 | loading frames p99 | steady frames p99 | GPU p50 / p99 | artwork upload | missed vsyncs |
  |---|---|---|---|---|---|---|
  | 1080p upscaled | 2.7 / 4.8 ms | 4.8 ms | 2.5 ms | 2.9 / 3.4 ms | 0.47 ms | 0 |
  | native 4K | 4.5 / 6.9 ms | 6.9 ms | 3.4 ms | 4.8 / 6.7 ms | 1.84 ms | 0 |

  At 1080p the GPU budget passes with room and CPU is just over on frames
  that load artwork during the (faster than key repeat) scripted scroll.
- **Compared both on the TV: native looks better.** Native is the default (`auto` and `1080p`
  stay available in Settings and config). Accepted deviation from spec 2
  on this GPU at native 4K: CPU p99 ~7 ms on frames that load new artwork
  and GPU p99 ~6.5 ms, with steady 60 fps, 0 missed vsyncs and 0
  allocating frames; frames with nothing new to load are within budget.

## Decisions made in M7

**D45: Playback requests: direct play only, verified API.** Checked
against the pinned 12.2 spec (D31): `POST /Items/{id}/PlaybackInfo` with
a device profile allowing every codec in the containers mpv plays and
`EnableTranscoding=false` (D19), then `/Videos/{id}/stream?static=true`.
A media source the server won't send as-is is a clear error ("transcoding
is off"), never a transcode. The item fetch brings `Chapters` and
`Trickplay`; `/MediaSegments/{id}` (10.10+) the intro/recap/preview/
commercial/outro ranges (missing on older servers: no skip buttons);
`/Shows/{id}/Episodes?startItemId=&limit=2` the next episode.

**D46: Tracks are chosen by the app, mapped to mpv by order.** The
server's default audio/subtitle (from the user's language preferences) is
applied at load through mpv's `aid`/`sid` (spec 7.1: `sub-auto=no`).
Jellyfin indexes streams globally; mpv numbers each type from 1 in stream
order, so an embedded track's mpv id is its position among embedded tracks
of its type. External subtitles (sidecar files the server delivers with
`DeliveryMethod=External`) are added with `sub-add` when chosen and take
the next ids. All track and seek changes go through mpv's async commands
(D20: the render thread never waits for mpv). Subtitles are styled for
the couch (larger, outlined, sized to the video).

**D47: Playback reports go through a persistent retry queue.** Start,
progress (every 10 s and on pause, seek or track change) and stopped
(EOF reports the full duration, so the server marks the item watched) are
queued on the network thread, sent in order, and retried with backoff
(5 s to 2 min). Consecutive progress reports for one session coalesce.
The queue is written to `~/.local/share/jellyroll/pending-reports.json`
while anything is unsent, so quitting offline doesn't lose where you
stopped; it is sent on the next start once signed in. Reports the server
rejects for good (4xx other than 401/408/429) are dropped and logged.
Found while testing: `Client::send_no_content` didn't send JSON bodies;
fixed, with an end-to-end test against the mock.

**D48: The player UI lives in the browsing app; the frame loop owns
mpv.** `PlayerUi` (home/player.rs) has the network, image cache and page
stack it needs; the frame loop keeps the GL context and mpv (`Playback`)
and asks the UI what to do each frame. Keys: Left/Right scrub (held:
10 s, then 30 s, then 60 s steps), committed 0.7 s after the last press
or with Select; trickplay thumbnails above the target (tile sheets through
the image pipeline, drawn with the new `Renderer::image_region`); Down or
Menu opens audio and subtitle lists; Select skips the current segment,
plays the next episode now, or pauses; Info shows codec, decoder, play
method and drops; Next/Prev jump between chapters; Back closes, cancels,
dismisses the next-episode card, or stops. The next-episode card appears
at the credits segment (else 30 s before the end) with a 10 s countdown in
playback time, then plays it without returning to the page; Back on the
card plays to the end and stops. `--play FILE` keeps the minimal M3 OSD.

**D49: M7 HTPC results and follow-ups.** All M7 walkthrough steps passed
on the HTPC (movie: OSD, scrub, tracks, info, resume point on the server;
episode: skip, next-up autoplay, watched state); the offline step was
skipped. Fixes from the run:
- Rows: Left and Right stay in the focused row, and Right on the last card
  wraps to the row's first card (user preference). The page's spatial
  graph used to pick the nearest card of the next row.
- Series page: selecting a season loaded its episodes below the fold, so
  nothing visibly changed. Focus now moves to the season's episodes when
  they arrive (unless the user moved off the seasons meanwhile).
- mpv logged `OpenGL error INVALID_OPERATION` on every playback. Our GPU
  frame timer held a `GL_TIME_ELAPSED` query open across mpv's render, and
  mpv times its own passes with the same query type (only one may be
  active). The timer now starts after mpv's frame, so it measures our
  drawing only; with mpv's `gpu-debug` on, no GL errors remain. Timestamp
  queries would include mpv's work, but on llvmpipe they read near zero,
  so the measured method stays as in the M6 benchmarks.

Verified on the HTPC: season select, row wrap, and 0 `INVALID_OPERATION`
lines in a playback log. M7 is accepted.

**D50: Refresh-rate matching through SDL (spec 5.2, open question 1).**
SDL 3.4.18's KMSDRM backend switches modes: `SDL_SetWindowFullscreenMode`
records the mode and the next swap does `drmModeSetCrtc` with it, so no
native DRM backend is needed for this. The frame rate comes from the
server's stream info (`AverageFrameRate`, else `RealFrameRate`) before mpv
starts, so the switch happens first: black frames until the modeset is
presented, then `display.refresh_switch_delay_ms` (default 0) for slow
TVs, then mpv. The pick (`refresh::pick`) keeps the current resolution
and wants a refresh that is a whole multiple of the frame rate: exact
(< 0.05%) before close (< 0.15%: 24 Hz for 23.976 when there is no
23.976 Hz mode), then the preferred multiple (film at its own rate, other
rates at the smallest multiple of at least 47 Hz: 25 to 50, 29.97 to
59.94). Nothing fits: play at the current mode. The mode is kept when
the next episode autoplays at the same rate, and the desktop mode comes
back when the UI does. `video-sync=display-resample` only when the mode
matches, else `audio` (spec 7.1). On/off is remembered per output
resolution in `ui.json` (default on, config `display.match_refresh`).
Not possible in a desktop window (`--dev`).

**D51: Audio output, passthrough, power menu.** mpv lists outputs
(`audio-device-list`, from a short-lived mpv with no video, probed in the
background at startup). Settings picks the device, passthrough per
device (`audio-spdif`, default formats `ac3,eac3,dts-hd,truehd`) and
channels (`audio-channels`); they apply from the next video. Passthrough
over PipeWire needs the sink configured for IEC958; an ALSA HDMI device
(`alsa/hdmi:...`) passes it straight through, at the cost of exclusive
use. The power menu opens with Back where there is nothing left to go
back to (it replaces "Back at the top quits", which is now its Exit item
with `--allow-exit`/`--dev`). It focuses Cancel first. Restart and Shut
down call logind's `Reboot`/`PowerOff` non-interactively over D-Bus
(`zbus`, its own short-lived runtime per call so the render thread never
waits); `CanReboot`/`CanPowerOff` answers of "challenge"/"no" are
explained on screen. logind's default policy already allows the active
local session; `docs/polkit-jellyroll.rules` (installed by
`scripts/install.sh`) also covers "other sessions are open" (SSH).

**D52: Display hotplug.** KMSDRM does not report connector changes, so a
thread polls `/sys/class/drm/card*-*/status` every second (cached values;
reading does not probe the TV). All connectors disconnected: playback
pauses (like CEC standby). Connected again: the current mode is set again
(`SDL_SetWindowFullscreenMode` with the same mode makes KMSDRM recreate
its surfaces and modeset on the next swap) and the screen is redrawn.
Rendering continues while disconnected: the kernel keeps the CRTC
running, so page flips still complete. To be confirmed on the HTPC with
a TV power cycle (many TVs keep the hotplug line up in standby, in which
case nothing changes at all).

**D53: One way to choose (request after the M8 run).** A choice
among several values opens a list that slides in from the right (`Blade`:
dimmed page, current value marked, opens on it, Up/Down wrap, Select
picks, Back/Left closes): Settings' interface resolution, audio output and
channels; the library's Sort and Genre. Buttons that open one end with
"›". On/off settings are toggles with a switch drawn in the button and
flip on Select: match refresh rate, passthrough, the library's Unwatched
and Favorites. Nothing cycles through values any more (cycling >10 audio
outputs was slow and unforgiving). The player's audio/subtitle panel was
already a list on the right.

**D54: Audio after a TV power cycle.** On the HTPC, after the TV was
turned off and on, the movie kept playing with no sound: the connector
never reported a disconnect, but the HDMI audio path (TV/soundbar) was
re-established, and mpv's open stream stayed silent. The hotplug thread
also fingerprints the HDMI audio sinks' ELD (`/proc/asound/card*/eld#*`)
and reports a change; during playback, a change (or a display reconnect)
makes mpv reopen its audio output (`ao-reload`). To be confirmed on the
HTPC.

**D55: Multi-select genres, reversible sort (by request).** The
Genre list is checkboxes: Select ticks or unticks and the list stays open;
closing it (Back) applies the selection, so the grid reloads once, not per
tick. It opens with the selected genres first (above a divider) so they
are easy to untick, and "Clear all" above them when two or more are
selected (it clears and closes). Several genres show items in any of them
(Jellyfin's `genres=A|B`). The Sort list starts with a "Reverse order"
checkbox above the sorts; the A-Z column is hidden while the name sort is
reversed (the jump index assumes ascending names). The bar shows "Sort:
Name, reversed" and "Genres: Drama +2".

**D56: Browsing gaps found on the HTPC.** (1) A description cut to
three lines can take focus (Up from the buttons) and then grows in place
to the full text over the page (D57). (2) People are selectable: a
person's page shows their biography and their movies and shows
(`/Items?personIds=`, newest first); no Play or Mark watched there. (3)
Playlists and collections (box sets, folders) open a page listing what
they contain (`/Playlists/{id}/Items`, `/Items?parentId=`); they used to
open an empty item page. (4) Watched items get a check mark in an accent
circle, top right of the card, everywhere cards are drawn (the font has
no check glyph; the renderer gained `polyline`).

**D57: The description expands where it is (chosen after review).** A More
button read as unrelated to the text. Of three options (focus it and open
a side panel; focus it and Enter expands it in place; focus alone expands
it), the last was chosen: the cut description is a focus-graph node
above the buttons; on focus it grows (180 ms) to the whole text on a dark
panel (no outline), drawn over the buttons and rows, and shrinks
when focus leaves. Down goes back to where focus came from, not the
spatially nearest button. Short descriptions are not focusable.

**D58: M8 status at merge.** Verified on the HTPC: refresh-rate
matching (24p plays at 24 Hz), the power menu (Restart reboots), surviving
a TV power cycle (playback continued). Open: sound after a TV power cycle
with the ELD-triggered audio reopen (D54), and passthrough to a receiver
(no Dolby/DTS-capable receiver tested yet). Merged with M7 in one PR.

**D59: M9 deferred to new hardware.** Checked before building (spec 5.2,
M9): on the reference HTPC, HDR10 is blocked twice over. The HD 530
(Skylake) has no full HEVC Main10 decode (the M7 log: `no_decode=HEVC
10-bit`), and HDR content is almost all 10-bit HEVC; software decode of
4K Main10 is beyond its CPU and D19 forbids it silently. And i915 sends
HDR metadata on gen9 only through MCA LSPCON parts, not a separate
DP-to-HDMI adapter. Overlay-plane video would need mpv to hand frames to
the app (libmpv has no such API), and the native DRM backend alone buys
nothing measurable (0 missed vsyncs, refresh switching works through
SDL). M9 waits for newer hardware such as an Intel N150 (Twin Lake, Xe graphics: HEVC 10-bit and
AV1 decode, native HDMI 2.0 with HDR in i915).
M10 comes first.

**D60: Losing the server mid-session (M10 audit).** Audited under Xvfb:
server down at first start (status screen, automatic retry, Back to pick
another server), restart while it is down (cached home, "Offline" badge),
corrupted state, cache and preferences files (each logged and replaced
with defaults; sign-in again if the state was lost), no audio device
(mpv's null output, M3). The gap: a server that went away while
browsing was only noticed at the next 5-minute refresh, and pages showed
the raw error (a long URL). Now a request that fails to reach the server
returns one plain message ("Can't reach the server. Retrying
automatically…"), the app goes offline at once and asks the session to
check; the session's backoff retries until the server answers, and then
the page that failed (library grid, item page) loads again by itself.

**D61: Exit and restart, sandboxing, soak (M10).** Quitting (SIGTERM
from `systemctl stop`, which SDL turns into a quit event, or Exit in the
power menu) during playback now sends the Stopped report and waits up to
1 s for the network thread to confirm it is queued (`Cmd::Flush`); the
queue is on disk, so the server gets it now or at the next start.
Verified under Xvfb: SIGTERM mid-play leaves Start/Progress/Stopped on
the server; `kill -9` mid-play restarts cleanly (redb needs no repair,
cached home in 0.17 s). The unit is now sandboxed (`ProtectSystem=strict`,
`ProtectHome=read-only` with the data dir and `~/.cache` writable, kernel,
cgroup, clock and hostname protection, no SUID, native syscalls only);
nothing restricts `/dev` or `/run`, so DRM, input, CEC, audio and logind
still work, but it is unproven on the HTPC until the service is installed
(checklist step 5). `--soak HOURS [--soak-media FILE]` loops the UI
against the in-process mock (home rows, a 2000-item grid scrolled deep, an
item page, a minute of playback, search), samples RSS, texture and Skia
cache memory every minute to `soak.csv`, and fails if RSS 10 minutes
after a 15-minute warm-up and RSS over the last 10 minutes differ by more
than 10% (and 32 MB). Home on the home screen now returns to the top (nav
bar), which keeps the script's every cycle anchored. CI runs a 2-minute
soak as a smoke test.

**D62: The 72-hour soak is replaced by everyday use.** Decided not to
run the M10 72-hour soak; problems found in daily use get fixed as
they come up. `--soak` stays for chasing a suspected leak. A 45-minute
run under Xvfb held RSS at about 570 MB after warm-up.

**D63: Live updates and remote control over the WebSocket (spec 8).**
Checked against the Jellyfin server source rather than assumed:
`/socket` authenticates the upgrade like any request, so the app sends its
`Authorization` header (no token in the URL); the server sends
`ForceKeepAlive` (60 s) and drops a socket that is silent that long, so
the app sends `KeepAlive` every half-timeout and treats 1.5 timeouts with
nothing heard as a dead connection; a session takes remote control only
with a socket open *and* capabilities posted with `SupportsMediaControl`
(`POST /Sessions/Capabilities/Full`, which also lists the general
commands the app carries out). The handshake runs through reqwest's
HTTP/1.1 upgrade (a second client without the 20 s request timeout), so
TLS settings are shared with every other request; `tokio-tungstenite`
0.30.0 (approved 2026-10-14) only frames messages. The socket task
reconnects with exponential backoff (1 s doubling to 60 s, times a random
0.5-1.5) and keeps out of everything else's way: browsing and playback
never wait for it.
- Data: `UserDataChanged` (this user only) and `LibraryChanged` ids are
  gathered for 1 s, then the home screen refreshes and each open page
  that shows a changed item (or every page, for library additions and
  removals, or after a reconnect) loads it again. Detail pages keep focus
  (rows rebuild by key and card id); library grids reload visible pages,
  keep the old cards until the new ones arrive, and pull focus back
  inside the grid if items were removed. The server already skips
  progress-report noise (`UserDataChangeNotifier` ignores
  `PlaybackProgress`).
- Remote control: Pause/Unpause/PlayPause/Seek/Stop/Rewind/FastForward/
  Next/Previous, navigation (arrows, Select, Back, Home, Search, menu,
  stats), typing (`SendString`), messages (`DisplayMessage`, shown as a
  toast; at the top of the screen over video), audio and subtitle track
  by Jellyfin stream index, and "play now" of an item (it stops and
  reports the current one first). Play-next/play-last queues are not
  offered (no play queue). Commands wait while a video is starting.
- The mock server speaks the same protocol and can push any message
  (`POST /Mock/Send`), which the tests and manual testing use.

**D64: Choosing tracks on the item page (spec 10.1).** Movie and episode
pages show "Audio: … ›" (when there is more than one audio track) and
"Subtitles: … ›" (when there are subtitles) next to Play; each opens the
slide-in list (D53). They start on what the server would choose for this
user: `GET /Items/{id}` already returns the default media source with
`DefaultAudioStreamIndex`/`DefaultSubtitleStreamIndex` worked out from the
user's language settings, so no extra request is needed (`MediaSources`
added to the generated `BaseItemDto`). Play passes the choice
(`TrackChoice`, server stream indexes, subtitle -1 = off) with the play
request; it replaces the PlaybackInfo defaults the player starts with.
A choice the file can't honour (an external subtitle the server won't
deliver) falls back to the default. The choice lasts while the page is
open; it isn't remembered per item, and autoplayed next episodes use the
server's defaults.

**D65: Several users and servers (spec 8).** The saved state always held
several servers and users; now they are usable. "Who's watching?" lists
every signed-in account (user tiles with an initial, the server's name
under each when there is more than one server) plus Add user and Add
server. It appears at startup when more than one account is signed in,
from Settings (Switch user, which replaces Change server: the picker
offers Add server), and after signing out while others remain. Picking
an account signs nobody out; Back returns to whoever was watching.
- A picked account's server is used over `[server] url` and kept for
  retries; if it can't be reached, Back on that screen returns to the
  picker (with several accounts) rather than the address screen.
- Add user signs a new person in on the current server (Quick Connect or
  password); Back from it returns to the picker.
- Playback reports are tagged with their user when queued and sent only
  while that user is signed in (the server attributes a report to the
  token's user, so after a switch a queued report would otherwise mark
  the wrong person's progress). Reports saved by older versions are
  untagged and go out with whoever is signed in.
- Accounts can't be removed one by one except by signing them out.
- The mock server has a second user, `kid` (password sign-in, no
  password).

**D66: Year filter by decade (spec 10.1).** Library grids get "Year: All ›"
after Genre. It opens the slide-in list with "All years" and each decade
the library actually has (from `GET /Years` for the library and its item
types), newest first; picking one filters to that decade's ten years
(`years=` on the item query). Decades rather than single years: a list of
70 years is slow to pick from with a remote, and "something from the
90s" is how people browse.

**D67: Playback and storage settings (spec 7.1, 10.1).** Settings gets a
Playback row and a Storage row; config.toml gets `[playback]` with the
same defaults (Settings choices win, as for audio). Option names and
values were checked against mpv 0.37 here and current mpv's manual.
- Motion smoothing: mpv `interpolation` (off by default, spec 7.1). It
  needs a `display-*` video sync, so with it on the player uses
  `display-resample` even when the refresh rate wasn't matched.
- Deinterlacing: Automatic / Always / Off. Automatic deinterlaces videos
  the server marks interlaced (`MediaStream.IsInterlaced`) rather than
  using mpv's `deinterlace=auto`, which older mpv lacks and whose frame
  detection has false positives (mpv's manual says so).
- HDR tone mapping: `tone-mapping` auto, bt.2390, spline, hable, mobius,
  reinhard, clip; only matters for HDR video on an SDR output.
  `hdr-compute-peak` stays mpv's default until it is benchmarked there.
- Storage: the artwork disk cache size (512 MB to 16 GB; the GPU texture
  cap stays in config.toml, it is a tuning knob) and Clear cache, which
  empties the artwork and metadata caches and drops GPU textures, then
  reloads the home screen. The page shows how much artwork is on disk.
- The Settings page now scrolls to keep the focused row visible.

**D68: Developer tracing (spec 13).** The frame loop opens TRACE-level
spans around each stage of a loop iteration: `wait` (for input), then
`frame` containing `net` (network events), `update` (input, animation,
requests), `video` (mpv's render), `draw` (recording the UI, with
`upload` for artwork and `submit` for Skia's flush) and `swap`; the
network thread's artwork decoding is `decode`. Layout has no span of its
own: it happens inside `update` and `draw`.
- The log's level filter is now per layer instead of global, so these
  spans stay disabled at their call sites under the usual `info` level (a
  cached check; no allocation: the scroll benchmark's allocation count is
  unchanged).
- Recording uses `tracing-chrome` 0.7.2 behind the `profile` cargo
  feature: `--trace FILE` writes Chrome trace JSON (one track per thread)
  for ui.perfetto.dev or chrome://tracing. The spec offers Tracy or
  tracing-chrome; tracing-chrome is pure Rust and needs no viewer of a
  matching version, where Tracy builds a C++ client that must match the
  Tracy GUI. It records jellyroll's spans plus info-level events, not
  dependencies' trace output. Without the feature, `--trace` prints how
  to build with it and is ignored.
- Measured under Xvfb (`--bench-scroll 10`): with tracing compiled in
  but off, 0 allocating frames and steady-frame CPU p50 2.2 ms, as before
  the spans (2.3 ms). With `--trace`, recording allocates, so a traced
  run's allocation check is meaningless; its 10 s trace was 17.7k events,
  2.6 MB, 7 threads.

**D69: A "Starting…" screen instead of black.** From Play until the
video's first frame (PlaybackRestart), the page stays on screen, dimmed,
with a spinner, the title and "Starting…": while the playback info
loads, while the TV switches refresh rate (previously black frames) and
while mpv opens the stream (mpv still renders underneath, as its video
output expects, and is covered). It fades in over 150 ms so a quick start
doesn't flash. The player's bar is shown again on the first frame: its
4 s auto-hide used to start when the player was created, so a slow start
(refresh switch, slow server) hid it before the video appeared.

**D70: Blank console under the display.** The console showing when a
video starts comes from SDL's KMSDRM backend (3.4.18,
`KMSDRM_DestroySurfaces`): a mode change marks the window's surfaces
dirty, and rebuilding them first points the screen back at "the original
TTY buffer" (the console's framebuffer, saved when SDL started) until a
frame is presented in the new mode. So with refresh-rate matching, every
switch to a film's rate showed the console for a moment (switching back
after playback goes to the console's own mode, where it is too brief to
notice). SDL never puts the console in graphics mode, so its text stays
drawn there. jellyroll now clears the console (screen and scrollback)
and hides its cursor just before SDL takes the display, on KMSDRM only,
so that moment is black; the cursor comes back on exit. It uses escape
sequences, not `KDSETMODE` graphics mode, which would leave the console
blank after a crash until something reset it. Not seen on the HTPC yet
(the sandbox has no KMS); on a pseudo-terminal the sequences are written
and undone as intended.

## Dependency approvals

| Crate | Status |
|---|---|
| `toml` | Approved 2026-10-13 (D11) |
| `tokio`, `reqwest` (rustls), `serde_json`, `image`, `fast_image_resize`, `redb` | Listed in spec §3; versions in `docs/VERSIONS.md` (M5) |
| `zbus` | Listed in spec §3 (logind power actions); version in `docs/VERSIONS.md` (M8) |
| `tokio-tungstenite` (and `futures-util` for its stream traits) | Approved 2026-10-14 (D63) |
| `tracing-chrome` | Named in spec §13 (optional profiling); `profile` feature only (D68) |
