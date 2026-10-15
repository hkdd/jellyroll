# M9: one display change for refresh rate + HDR — implementation plan

Status: see **Progress** at the end. Every step ends in a commit pushed to
`main` and `claude/build-this-1k8mhp`. Read this file and
`docs/M9-HDR-PLAN.md` (the HDR passthrough it builds on) before continuing.

## Why

HDR passthrough (M9 steps 1–4) works on beelink01, but starting an HDR film
changes the display twice: SDL switches the refresh rate (a modeset), then
jellyroll sets the HDR connector properties (a second modeset on i915).
Back to back, the LG TV intermittently stayed on "no signal". The fix that
shipped waits for the link to return plus `display.hdr_switch_delay_ms`
(3 s) between them: a guessed margin, and two black re-syncs. The TV gives
no general "picture locked" signal to wait for (CEC doesn't report it; SCDC
lock flags exist only on HDMI 2.0 sinks and need raw DDC access).

The real fix: make **one** atomic commit that sets the new mode *and* the
HDR properties together. The TV re-syncs once; no margin to guess.

## Two ways, and the order to do them

**A. Our own atomic commit on SDL's fd (this plan, do first).** Facts
checked in SDL 3.4.18 (`sdl3-src`, `src/video/kmsdrm/`):

- SDL's per-frame atomic commits (`KMSDRM_GLES_SwapWindowFenced` /
  `…DoubleBuffered`) set only the primary plane (`FB_ID`, `CRTC_ID`,
  `SRC_*`, `CRTC_*` from `window->w/h` and `dispdata->mode`) plus fences.
  `MODE_ID` is added **only** when SDL itself has a modeset pending.
- So if jellyroll changes the CRTC's mode itself and the **resolution stays
  the same** (4K60 → 4K23.976, the normal refresh-match case), SDL's later
  flips stay valid: same plane sizes, same framebuffer size.
- SDL publishes its DRM fd (`SDL.window.kmsdrm.drm_fd`, already used by
  `HdrSwitch`). It is DRM master. If SDL enabled atomic
  (`DRM_CLIENT_CAP_ATOMIC`, it does when the driver supports it), our
  atomic ioctl works on that fd.
- When the resolution changes, or atomic isn't available, or the TEST_ONLY
  commit fails: fall back to today's path (SDL `set_mode`, then HDR after
  the delay). Never worse than today.

**B. Native DRM/GBM/EGL backend (later, separate plan).** Replaces SDL's
video entirely: own GBM surface (allows a **10-bit** `XRGB2101010`
framebuffer, M9 HDR plan step 6), own page flips, overlay-plane video.
Needs evdev keyboard input (SDL's KMSDRM video provides it today) and raw
FFI to libgbm/libEGL. Much larger; only worth it after A, if banding or
overlay planes matter.

## Kernel interface for A (64-bit layouts; `DRM_IOWR('d', nr, T)`)

| ioctl | nr | struct (size) |
|---|---|---|
| `MODE_GETCONNECTOR` | 0xA7 | `drm_mode_get_connector` {u64 encoders_ptr, modes_ptr, props_ptr, prop_values_ptr; u32 count_modes, count_props, count_encoders, encoder_id, connector_id, connector_type, connector_type_id, connection, mm_width, mm_height, subpixel, pad} (80) |
| `MODE_ATOMIC` | 0xBC | `drm_mode_atomic` {u32 flags, count_objs; u64 objs_ptr, count_props_ptr, props_ptr, prop_values_ptr, reserved, user_data} (56) |
| `MODE_CREATEPROPBLOB` etc. | | already in `drm_hdr.rs` |

`drm_mode_modeinfo` (68 bytes): u32 clock; u16 hdisplay, hsync_start,
hsync_end, htotal, hskew, vdisplay, vsync_start, vsync_end, vtotal, vscan;
u32 vrefresh, flags, type; char name[32]. Refresh in Hz =
`clock*1000 / (htotal*vtotal)` (×2 if interlaced, ÷2 if doublescan).

Atomic flags: `PAGE_FLIP_EVENT 0x01`, `TEST_ONLY 0x0100`,
`NONBLOCK 0x0200`, `ALLOW_MODESET 0x0400`.
Object types: connector `0xc0c0c0c0`, CRTC `0xcccccccc`.

One commit, objects and properties:

- CRTC (id = the connector's `CRTC_ID` property value): `MODE_ID` = blob of
  the new `drm_mode_modeinfo`, `ACTIVE` = 1.
- Connector: `CRTC_ID` = same CRTC, `HDR_OUTPUT_METADATA` = metadata blob
  (or 0 for SDR), `Colorspace` = `BT2020_RGB` (or `Default`).
- Flags: `ALLOW_MODESET`; first with `TEST_ONLY`, then for real (blocking,
  no `NONBLOCK`). Retry `EBUSY` (SDL flip in flight) like `with_retry`.
- Destroy the old mode blob after success.

## Steps

### Step 1 — Atomic plumbing (platform crate, `drm_hdr.rs` or new `drm_kms.rs`)

- Structs + ioctl numbers above, with layout tests (sizes 80, 56, 68).
- `connector_modes(fd, conn) -> Vec<DrmMode>` (GETCONNECTOR twice:
  counts, then arrays). `DrmMode { info: [u8; 68] or struct, w, h, hz }`.
- `object_props(fd, id, type)` generalising `connector_props` to CRTCs.
- `atomic_commit(fd, &[(obj, prop, value)], flags)`: groups by object as
  the ioctl requires (objs, count_props per obj, props, values).

### Step 2 — `Platform::set_mode_hdr` (SDL backend)

- `fn set_mode_hdr(&mut self, mode: Option<&ModeInfo>, hdr: Option<&HdrMetadata>) -> Result<SwitchKind, String>`
  where `SwitchKind` is `Single` (one atomic commit done) or `Unsupported`
  (caller falls back). Default trait impl: `Unsupported`.
- SDL impl: KMSDRM only; target mode's resolution must equal the current
  framebuffer size; find the DRM mode matching `ModeInfo`
  (w, h, refresh within 0.01 Hz, progressive); TEST_ONLY then commit;
  remember the "current mode" ourselves (`refresh_hz()`/`current_mode()`
  must report it, since SDL's own idea is now stale); `HdrSwitch` state
  (`blob`) updated to match.
- `None` mode = the desktop mode (what SDL set at startup). Restoring at
  playback end uses the same single commit (desktop mode + SDR).
- `reapply_mode` (TV power-cycled, D84/D54) must re-commit **our** mode and
  HDR state, not SDL's stale one.
- Log one line per switch: `display switched (single commit)` with mode,
  hz, hdr; or the reason it fell back.

### Step 3 — Frame loop uses it

- `prepare_play`: if HDR or refresh needs changing, try `set_mode_hdr`
  first. `Single` → `needs_present = true`, no `hdr_next`, `pp.hdr` =
  wanted. `Unsupported`/error → today's two-step path.
- `restore_mode`: same, to desktop + SDR.
- D84 grace applies unchanged (one re-sync).
- `display.hdr_switch_delay_ms` stays for the fallback path only.

### Step 4 — Test on beelink01, docs

- Expect in the journal: `display switched (single commit)` at start and
  stop, one black gap each, TV HDR badge, no "no signal".
- Check `--print-diagnostics` still fine; check exit to console restores the
  console mode (SDL restores its saved CRTC on exit).
- README HDR section, DECISIONS D87, mark the fallback in docs.

## Risks

- SDL legacy (non-atomic) mode on some drivers: then our fd lacks the
  atomic cap → `Unsupported`, fallback. Don't set the cap on SDL's fd
  behind its back.
- SDL might later recreate surfaces (e.g. a window event) and modeset to
  its stale mode: watch for `KMSDRM: Trying format` in the log after a
  single switch; if seen, call SDL's `set_mode` with the same mode too, or
  move to backend B.
- `vrefresh` rounding: match on exact clock/htotal/vtotal-derived Hz to
  tell 23.976 from 24.

## Progress

| Step | State | Notes |
|---|---|---|
| Plan (this file) | done | |
| 1 Atomic plumbing | done | in `drm_hdr.rs`: `DrmMode`, `pick_mode`, `connector_modes`, `atomic_commit`, `HdrSwitch::commit_mode_hdr` (TEST_ONLY then commit); layout tests |
| 2 set_mode_hdr | done | `Platform::set_mode_hdr -> Result<bool>` (false = fall back); SDL impl keeps `kms: (ModeInfo, DrmMode, meta)` for `current_mode` and `reapply_mode` while SDL's own mode is stale |
| 3 Frame loop | done | `prepare_mode` tries `set_mode_hdr` whenever HDR changes; `restore_mode` tries it for desktop + SDR; refresh-only changes still go through SDL. Untested on hardware |
| 4 Test + docs | done | beelink01 2026-10-15: both HDR films started and stopped with one change each (`display switched (single commit)` ×4), HDR badge, no drops, no SDL re-modeset afterwards. Exit to console after HDR checked: fine |

## Resume here

Steps 1–3 are pushed (948672f). Next: test on beelink01. In the journal
(`sudo journalctl -t jellyroll | grep -Ei "single commit|one change|two-step|refused|unavailable"`)
expect `display switched (single commit)` and `output mode and HDR for
playback (one change)` at start, `output mode and SDR restored (one
change)` at stop. If it says `refused (…)`/`unavailable`, the reason is in
that line; the two-step path still runs, so playback isn't broken.
Watch for: SDL modesetting back to its stale mode (`KMSDRM: Trying
format` after a single switch, outside playback start/stop), and exit to
console restoring the console.

## Result (2026-10-15)

Works on beelink01 + LG TV: every HDR start and stop was a single commit;
the two-step fallback and `display.hdr_switch_delay_ms` were not needed.
Exit to console checked (fine). Open: Step 5 of the HDR plan
(OSD brightness in HDR); the native backend (10-bit) only if banding shows.
