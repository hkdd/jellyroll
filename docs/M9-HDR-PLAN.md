# M9: HDR10 passthrough — implementation plan

Status is tracked in the **Progress** table at the end. Each step ends in
a commit pushed to `main` and `claude/build-this-1k8mhp`, so work can
resume from any step. Read this whole file before continuing it.

## Why

On the Intel N150 (beelink01, LG TV on HDMI), a 4K HDR10 HEVC remux decodes
fine through VA-API (Info panel: `Dropped vo N decoder 0`), but the
**renderer** drops frames continuously. jellyroll outputs SDR only, so mpv
must tone-map every 4K frame from HDR (PQ, BT.2020) to SDR (BT.1886,
BT.709). That GPU work, plus mpv's default scaling and HDR peak detection,
takes longer than a frame (D85 added fast rendering as a stopgap).

The real fix is the one TV apps and streaming boxes use: tell the TV the
signal is HDR10 (an HDMI HDR static-metadata InfoFrame and BT.2020 colour)
and hand it the PQ picture unchanged. No tone mapping, real HDR on screen.

## How the display path works today (facts checked in the source)

- SDL 3.4.18 (`sdl3-src` crate) KMSDRM backend owns the DRM device. It uses
  **atomic** modesetting when the driver supports it (i915 does):
  `set_client_atomic_caps`, `SDL_kmsdrmvideo.c`.
- SDL creates the GBM scanout surface trying formats in a fixed order with
  **ARGB8888 first** (`KMSDRM_CreateSurfaces`); 10-bit formats are only
  used if no 8-bit one works. There is no hint to prefer 10-bit. So the
  framebuffer is **8 bits per channel**.
- SDL publishes its DRM fd as the window property
  `SDL_PROP_WINDOW_KMSDRM_DRM_FD_NUMBER` ("SDL.window.kmsdrm.drm_fd"). That
  fd is DRM master, so it may change connector properties.
- SDL never touches `HDR_OUTPUT_METADATA`, `Colorspace` or `max bpc`, so
  values jellyroll sets on the connector persist across SDL's page flips.
- mpv draws video into jellyroll's GL framebuffer through the libmpv render
  API (`vo=libmpv`); jellyroll's Skia UI/OSD is drawn on top, in sRGB.
- Refresh-rate matching already does a modeset at playback start and stop
  (`frame_loop.rs`: `set_mode`, `restore_mode`, `mode_switched_at`), and
  D84 ignores the TV's HDMI re-sync hot-plug blip for 10 s after a switch.

## Kernel interface (linux/drm_mode.h, linux/drm.h; 64-bit layouts)

Connector properties used (find ids by name with GETPROPERTY):

| Property | Type | HDR value | SDR value |
|---|---|---|---|
| `HDR_OUTPUT_METADATA` | blob | id of a `hdr_output_metadata` blob | 0 |
| `Colorspace` | enum | `BT2020_RGB` | `Default` |
| `max bpc` | range | leave as is (i915 default 12 on HDMI 2.0) | leave |

```c
struct hdr_metadata_infoframe {      // 26 bytes, align 2
  __u8 eotf;                         // 2 = SMPTE ST 2084 (PQ), 3 = HLG
  __u8 metadata_type;                // 0 = static metadata type 1
  struct { __u16 x, y; } display_primaries[3]; // units of 0.00002, R,G,B order
  struct { __u16 x, y; } white_point;
  __u16 max_display_mastering_luminance;       // cd/m², 1 unit
  __u16 min_display_mastering_luminance;       // 0.0001 cd/m²
  __u16 max_cll;                     // cd/m²
  __u16 max_fall;                    // cd/m²
};
struct hdr_output_metadata {         // 32 bytes (30 + pad), align 4
  __u32 metadata_type;               // 0 = HDMI_STATIC_METADATA_TYPE1
  union { struct hdr_metadata_infoframe hdmi_metadata_type1; };
};
```

ioctls (`DRM_IOWR('d', nr, struct)`, i.e. `0xC0000000 | size<<16 | 0x64<<8 | nr`):

| ioctl | nr | struct (size) |
|---|---|---|
| `MODE_GETPROPERTY` | 0xAA | `drm_mode_get_property` {u64 values_ptr, u64 enum_blob_ptr, u32 prop_id, u32 flags, char name[32], u32 count_values, u32 count_enum_blobs} (64) |
| `MODE_CREATEPROPBLOB` | 0xBD | `drm_mode_create_blob` {u64 data, u32 length, u32 blob_id} (16) |
| `MODE_DESTROYPROPBLOB` | 0xBE | `drm_mode_destroy_blob` {u32 blob_id} (4) |
| `MODE_OBJ_GETPROPERTIES` | 0xB9 | `drm_mode_obj_get_properties` {u64 props_ptr, u64 prop_values_ptr, u32 count_props, u32 obj_id, u32 obj_type} (32 with pad) |
| `MODE_OBJ_SETPROPERTY` | 0xBA | `drm_mode_obj_set_property` {u64 value, u32 prop_id, u32 obj_id, u32 obj_type} (24) |

`obj_type` for a connector is `DRM_MODE_OBJECT_CONNECTOR = 0xc0c0c0c0`.
Enum values (e.g. `BT2020_RGB`) come from `enum_blob_ptr` entries
`drm_mode_property_enum {u64 value; char name[32]}` (40 bytes).

The connector id is in `/sys/class/drm/card*-HDMI-A-*/connector_id` on
current kernels (pick the connector whose `status` is `connected`; the same
files hotplug.rs already polls).

Legacy `OBJ_SETPROPERTY` on an atomic driver is applied as an atomic commit
with modeset allowed (`drm_atomic_state_init` sets `allow_modeset`). i915
treats a changed HDR metadata or colorspace as a mode change, so **the TV
re-syncs (black for 1–3 s), like a refresh switch**. Do it together with
the refresh switch, and count it as a mode switch for D84's grace period.
If SDL has a page flip in flight the ioctl can return `EBUSY`: retry after
the next swap (up to ~10 frames), then give up and stay SDR.

TV capability: the EDID (`/sys/class/drm/<connector>/edid`) CTA-861
extension block (tag 0x02) holds an "HDR Static Metadata" data block:
extended tag (tag code 7, first payload byte 0x06). Its byte 2 bits are the
supported EOTFs: bit0 SDR, bit1 traditional HDR, **bit2 SMPTE ST 2084 (PQ)**,
bit3 HLG. Bytes 4–6 (optional) are desired max luminance, max frame-average
and min luminance (coded values).

mpv (0.37+; check the installed version): the video's HDR metadata is in
`video-params` sub-properties `gamma` ("pq", "hlg"), `primaries`
("bt.2020"), `max-luma`, `min-luma`, `max-cll`, `max-fall`, and the
mastering primaries `prim-red-x` … `prim-white-y`. The render target is
set at runtime with the options `target-trc=pq`, `target-prim=bt.2020`,
`target-peak` (cd/m²); with a PQ target whose peak is at least the
source's, mpv does no tone mapping. Back to SDR: set all three to `auto`.

## Steps

### Step 1 — Read-only HDR probe (in `--print-diagnostics` and debug reports)

Platform crate, new `crates/platform/src/drm_hdr.rs` (unsafe allowed here):

- `connected_connectors()` → for each connected `/sys/class/drm/card*-*`:
  name, `connector_id`, card node path, EDID bytes.
- `parse_edid_hdr(&[u8]) -> Option<HdrCaps {pq, hlg, max_lum, max_fall_lum,
  min_lum}>` (pure, unit-tested with a synthetic EDID).
- `connector_props(fd, connector_id)` → names, ids, current values, and
  enum names for `Colorspace`; works on any open card fd without master.
- `drm_hdr::report()` lines (`hdr.<connector>.tv`, `.HDR_OUTPUT_METADATA`,
  `.Colorspace` with its choices, `.max_bpc`) are printed first thing by
  `--print-diagnostics` (before SDL starts, so they print even while the
  service holds the display).
- Also add the same lines to the debug report (D83).
- Test on beelink01:
  `ssh -t jon@beelink01 'sudo -u jellyroll /usr/local/bin/jellyroll --print-diagnostics 2>&1 | grep ^hdr' | tee >(xclip -selection clipboard)`
  Expect: `PQ yes`, `HDR_OUTPUT_METADATA present`, `Colorspace: … BT2020_RGB …`.

### Step 2 — Set and clear HDR on the connector

Platform crate (`drm_hdr.rs`), plus `Platform` trait methods:

- `fn set_hdr(&mut self, meta: Option<&HdrMetadata>) -> Result<(), String>`
  (`None` = back to SDR). `HdrMetadata { eotf, primaries [(x,y);3],
  white (x,y), max_lum, min_lum, max_cll, max_fall }` in natural units;
  converted to the kernel units above.
- SDL backend: DRM fd from the window property
  `SDL.window.kmsdrm.drm_fd`; connector id from sysfs (connected HDMI);
  create blob → set `HDR_OUTPUT_METADATA`; set `Colorspace` = `BT2020_RGB`.
  Clear: set `HDR_OUTPUT_METADATA` = 0, `Colorspace` = `Default`, destroy
  the old blob. Retry on `EBUSY` as above. Desktop (non-KMSDRM) backend:
  return an error ("HDR passthrough needs KMSDRM").
- Struct layout tests (sizes 32/16/4/32/24/64, ioctl numbers).
- Always clear on exit (Drop of the SDL platform, and the exit-to-console
  path) so the console isn't left in HDR mode.

### Step 3 — mpv renders PQ for an HDR target

Video crate:

- `Player::hdr_metadata() -> Option<HdrMetadata>` from `video-params`
  (falls back to BT.2020 primaries / D65 / 1000 nit mastering / 0.005 min
  when the file has none).
- `Player::set_hdr_output(on: bool)`: `target-trc=pq`, `target-prim=bt.2020`,
  `target-peak=10000` (no tone mapping; the TV maps to its panel) — or all
  `auto` when off. Keep `dither` on in HDR (8-bit framebuffer, see Step 6);
  D85's auto fast profile turns dithering off, so re-enable it with
  `dither=ordered` (cheap) when HDR output is on.
- `PlaybackState.hdr_output: bool` for the Info panel.

### Step 4 — Playback flow, setting, D84 grace

App crate:

- Setting **HDR passthrough**: `Automatic` (default) / `Off`
  (config `playback.hdr = "auto" | "off"`, ui_prefs, settings row next to
  tone mapping; same plumbing as `render_quality`).
- At playback start (`frame_loop.rs`, where refresh matching runs), once
  the video params are known (mpv `VideoReconfig`): if setting is Auto, the
  video `gamma` is `pq`, and the TV's EDID says PQ → `p.set_hdr(Some(meta))`
  then `player.set_hdr_output(true)`; set `mode_switched_at = now` (D84
  grace). Log `hdr passthrough on` with the metadata, or why not (TV lacks
  PQ, property missing, ioctl error) — never silent.
- At playback end (where `restore_mode` runs) and on exit: `set_hdr(None)`.
- HLG: out of scope for now (tone-mapped as today).
- Dolby Vision profile 5 (no HDR10 base layer) stays tone-mapped; profile
  7/8 have an HDR10 base layer and go through passthrough as HDR10.

### Step 5 — OSD and UI in PQ while HDR is on

The Skia OSD/menus are sRGB; drawn as-is over a PQ signal they look dim and
grey. While HDR output is on, draw the overlay into an offscreen texture
and composite it with a small GLSL pass converting sRGB → linear → BT.2020
→ PQ at 203 cd/m² for white (ITU-R BT.2408 reference white). Only the
player overlay is shown while HDR is on (menus close playback first), so
this is limited to the player's draw path.

### Step 6 — 10-bit framebuffer (optional, after testing Step 4)

8-bit PQ with dithering is usually fine but can band in dark gradients. SDL
3.4 always prefers ARGB8888. Options, in order: (a) ask upstream / carry a
tiny patch via an SDL hint to prefer `XRGB2101010`; (b) the native
DRM/GBM/EGL backend (the other M9 item). Decide after seeing Step 4 on the
TV.

### Step 7 — Docs

README (HDR section, setting), DECISIONS D86 (passthrough design, facts
above), TODO.md (M9 status), debug report fields, config.example.toml.

## Risks and checks

- Changing connector properties while SDL page-flips: serialize with the
  frame loop (call between swaps), retry on EBUSY.
- TV re-sync on every HDR toggle: combined with the refresh switch it is one
  black gap at start and one at stop, the same as today.
- If `OBJ_SETPROPERTY` returns `EACCES`, the fd isn't master: check SDL's fd
  is the one used (not a separately opened card node).
- i915 may reject `BT2020_RGB` with an 8-bit RGB pipe on some HDMI paths;
  if the probe or the set fails, try leaving `Colorspace` at `Default` and
  sending only the metadata (TVs switch to HDR on the InfoFrame).

## Progress

| Step | State | Commit |
|---|---|---|
| Plan (this file) | done | |
| 1 Probe | done (`drm_hdr.rs`: EDID, connector props, blob/setprop ioctls; tested in unit tests, awaiting beelink01 output) | see git log |
| 2 Set/clear on connector | done (`drm_hdr::HdrSwitch`, `Platform::set_hdr` / `hdr_caps`; SDL fd from the window property; untested on hardware) | see git log |
| 3 mpv PQ target | done (`Player::video_hdr`, `set_hdr_output`; mpv 0.37 checked with an x265 HDR10 clip: gamma/primaries/max-luma/min-luma/max-cll/max-fall work, `prim-*-x/y` do not exist there, so primaries fall back to BT.2020/D65) | see git log |
| 4 Flow + setting + grace | done (`frame_loop::switch_hdr` in `prepare_play`, off in `restore_mode`; HDR10 known from Jellyfin's VideoRangeType before mpv starts so the TV re-syncs once, with the refresh switch; metadata is the generic default (1000 cd/m², BT.2020) since mpv's values only arrive after playback starts; setting `HDR passthrough` in the display row; Info panel says SDR/HDR10 to the TV; untested on hardware) | see git log |
| 5 OSD in PQ | not started | |
| 6 10-bit framebuffer | not started | |
| 7 Docs | partly (README "HDR10 passthrough", D86; finish after hardware test) | see git log |

## Resume here

Steps 1–4 work on beelink01 (cb0f79f; see Field notes). Next:

1. Get the probe output from beelink01 (`--print-diagnostics | grep ^hdr`)
   and check `PQ yes`, `HDR_OUTPUT_METADATA present`, `BT2020_RGB` listed.
2. Play an HDR10 film; check the journal (`sudo journalctl -t jellyroll`)
   for `HDR10 passthrough on=true` or the reason it didn't switch, the TV's
   HDR badge, the Info panel's "HDR10 to the TV", and whether `vo` drops
   stop.
3. If the set fails with `EACCES`/`EINVAL`, see **Risks and checks**.
4. Then Step 5 (OSD in PQ), Step 6 if banding shows, Step 7.

### Field notes

- 2026-10-15, beelink01 + LG TV: a Dolby Vision (HDR10 base layer) film
  played in HDR10, TV showed its HDR badge. Then, intermittently, films
  (the same one too) ended on "no signal" while jellyroll's log showed the
  link back and mpv rendering. The HDR property change and SDL's refresh
  modeset were back to back (the HDR set happened in `prepare_play`, the
  refresh modeset at the next swap). Changed: the HDR switch now waits
  until the refresh switch is presented, the link is back, and
  `display.hdr_switch_delay_ms` (default 3000) has passed; mpv starts after that
  (`PendingPlay.hdr_next`). If it still fails, compare
  `/sys/kernel/debug/dri/*/i915_display_info` (pipe bpp, output format)
  between a working and a failing start, and consider Colorspace
  `Default` (metadata only) or capping `max bpc`.
- 2026-10-15, after cb0f79f: both films (HDR10 and Dolby Vision with an
  HDR10 base layer) start reliably in HDR10 on beelink01 with the
  sequenced switch. Steps 1–4 confirmed on hardware. Next: Step 5 (OSD
  brightness in HDR), then Step 6 only if banding is visible.
