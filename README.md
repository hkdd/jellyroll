# Jellyroll

A boot-to-app Jellyfin client for a Linux PC connected to a TV. It renders
straight to the GPU through KMS/DRM with no X11, no Wayland compositor and no
desktop environment, and you drive it with a TV remote (HDMI-CEC), a gamepad
or a keyboard.

Priorities, in order: **performance, reliability, visual polish, features.**
The design spec is the source of truth, and `docs/` records the decisions
and verified versions.

## Status

| Milestone | State |
|---|---|
| **M1** Skeleton: SDL3 KMSDRM, GL context, vsync loop, dirty redraw, perf HUD, systemd unit, dev mode | **Done.** Ran on the HTPC: boots to the test screen on a text console, input works, idle CPU near zero. Pacing numbers not yet recorded |
| **M2** Renderer trait + Skia: rects, rounded rects, gradients, text (incl. CJK), images, animation clock | **Done.** On the HTPC (Intel HD 530, 4K): steady 60 fps, 0 missed vsyncs; Skia kept. The spec's p99 budgets are not met on this GPU at native 4K (GPU ~8 ms, CPU ~3.3 ms p50); see `docs/DECISIONS.md` |
| **M3** libmpv playback: hwdec, GL handoff with the overlay, audio | **Done.** On the HTPC: 4K H.264 and HEVC 8-bit via VA-API at ~11-12% CPU with 0 dropped frames; HEVC 10-bit (unsupported on this GPU) is flagged, never silently degraded. See `docs/DECISIONS.md` |
| **M4** Input: keyboard, gamepad, HDMI-CEC; spatial focus graph; key repeat | **Built**, awaiting hardware validation ([`docs/M4-HARDWARE-CHECKLIST.md`](docs/M4-HARDWARE-CHECKLIST.md)). Gamepad tested end to end with an SDL virtual pad; CEC protocol handling unit-tested |
| M5 to M10 | Not started |

The app currently shows the M2 render benchmark: a home-screen-like layout
with a backdrop, a nav bar, 60 poster cards with shadows and titles,
Japanese and Cyrillic text, a focus animation, and a perf HUD on F3. Focus
moves spatially, and each row remembers where you left it. The data and
artwork are fake; the real UI and Jellyfin data arrive in M5/M6.

## Layout

```
crates/platform   jellyroll-platform: Platform trait, SDL3 backend, GL loader,
                  counting allocator. The only crate allowed `unsafe`.
crates/render     jellyroll-render: Renderer trait, Skia backend, text
                  shaping (cosmic-text) over the bundled fonts.
crates/video      jellyroll-video: libmpv render-API binding (FFI; may use
                  `unsafe`) and the VA-API decode-capability probe.
crates/app        jellyroll binary (#![forbid(unsafe_code)]): render loop,
                  input normalization + key repeat, perf stats/HUD, systemd.
dist/             systemd unit, example config
scripts/make-test-clips.sh  synthetic H.264 / HEVC 8-bit / HEVC 10-bit clips
scripts/          install.sh, dev-cage.sh, bench-startup.sh
assets/fonts/     bundled Noto fonts (subset; regenerate with
                  scripts/subset-fonts.sh)
docs/             VERSIONS.md, DECISIONS.md, M1/M2 hardware checklists
```

## Building

Rust stable (MSRV 1.88). Two ways to get SDL3:

```sh
cargo build --release                      # links the system libSDL3 (>= 3.2)
cargo build --release --features build-sdl # builds the pinned SDL 3.4.18 statically
```

Skia comes as a prebuilt static library that `skia-safe` downloads from
GitHub (rust-skia/skia-binaries) during the first build, so that build needs
network access. Offline builds can point `SKIA_BINARIES_URL` at a local copy
of the archive (`file://...`).

The prebuilt Skia links against the system's `libstdc++`, `freetype` and
`fontconfig`, so building needs their development packages (see the table
below). At runtime jellyroll only uses its compiled-in fonts and never asks
fontconfig for any. SDL `dlopen`s drm/gbm/EGL/GL/udev at runtime.

System packages (runtime, plus headers when using `build-sdl`):

| | Fedora (reference) | Debian/Ubuntu | Arch |
|---|---|---|---|
| SDL3 | `SDL3-devel` | build with `build-sdl` (24.04 has no SDL3) | `sdl3` |
| Mesa / KMS | `mesa-libEGL-devel mesa-libgbm-devel libdrm-devel mesa-dri-drivers` | `libegl-dev libgbm-dev libdrm-dev libgl1-mesa-dri` | `mesa libdrm` |
| input | `systemd-devel` (libudev) | `libudev-dev` | `systemd-libs` |
| Skia (link) | `gcc-c++ freetype-devel fontconfig-devel` (gcc-c++ keeps libstdc++-devel on the same version as gcc) | `libstdc++-dev libfreetype-dev libfontconfig-dev` | `gcc-libs freetype2 fontconfig` |
| build-sdl only | `cmake ninja-build` + X11/Wayland headers | see `.github/workflows/ci.yml` | `cmake ninja` |

Playback (M3) links libmpv (client API 2.x) and libva. Install them by
what the build needs; dnf picks whichever package provides each:

```sh
sudo dnf install -y 'pkgconfig(mpv)' 'pkgconfig(libva)' 'pkgconfig(libva-drm)'   # Fedora
sudo apt install libmpv-dev libva-dev                                               # Debian/Ubuntu
```

On Fedora, H.264/HEVC playback also needs the full ffmpeg from RPM Fusion
(`sudo dnf swap ffmpeg-free ffmpeg --allowerasing`); `ffmpeg-free` leaves
those codecs out.

libcec comes in at M4.

**Hardware video decode on Fedora (Intel):** Fedora's own
`libva-intel-media-driver` leaves out H.264 and HEVC. Install the full
driver from RPM Fusion nonfree (enable RPM Fusion per
https://rpmfusion.org/Configuration first):

```sh
sudo dnf swap libva-intel-media-driver intel-media-driver --allowerasing
vainfo   # (libva-utils) should list VAProfileH264* and VAProfileHEVCMain
```

## Running

```sh
# Development, in a window on your desktop (Wayland or X11):
target/release/jellyroll --dev --hud

# Development under cage (a kiosk Wayland compositor), e.g. from a spare VT:
scripts/dev-cage.sh --hud

# Production path (KMSDRM), from a text console with the service stopped:
target/release/jellyroll
```

On a text console, SDL reads the keyboard, remote and gamepads straight from
`/dev/input/event*`. Those devices belong to root and the `input` group, and
logging in on a console does not grant access to them. To run jellyroll by
hand, add your account to that group once, then log out and back in:

```sh
sudo usermod -aG input "$USER"
```

Without access, jellyroll exits with this advice instead of showing a screen
that ignores the keyboard. `--print-diagnostics` reports how many input
devices are readable. The service user gets the group from `install.sh`.

| Flag | |
|---|---|
| `--config PATH` | Config file (default `~/.config/jellyroll/config.toml`) |
| `--dev` | Desktop window instead of KMSDRM. Esc quits |
| `--window WxH` | Dev window size, e.g. `3840x2160` to check 4K layout |
| `--hud` | Show the perf HUD (F3 toggles) |
| `--allow-exit` | Esc/Back quits outside dev mode |
| `--double-buffer` | Double instead of triple buffering on KMSDRM |
| `--bench-pacing SECS` | Continuous animation. Prints CPU/GPU/present p50/p99, missed vsyncs and allocating frames, and exits 1 if a spec §2 budget is exceeded |
| `--bench-idle SECS` | Static screen. Fails if anything redraws or CPU ≥ 1% |
| `--print-diagnostics` | SDL/GL/driver/display modes, DRI/CEC/input devices, mpv version, and which codecs the GPU decodes in hardware |
| `--play FILE_OR_URL` | Play a video (M3 harness). Space pause, ←/→ seek 10 s, ↑/↓ 60 s, `i` decoder diagnostics, Esc stop |
| `--bench-play SECS` | With `--play`: measure dropped frames, missed vsyncs and CPU, toggling the overlay every 2 s |

Settings live in `~/.config/jellyroll/config.toml`. Every key is optional;
see [`dist/config.example.toml`](dist/config.example.toml) for all keys and
defaults. Command-line flags override the file. Mistakes in the file never
stop the app: it uses defaults for anything it can't read, logs why, and
shows a one-line notice on screen.

### Controls

All three sources drive the same actions, and holding any direction
repeats at the same rate.

| Action | Keyboard (Kodi-like) | Gamepad | TV remote (CEC) |
|---|---|---|---|
| Move | arrows | D-pad, left stick | arrows |
| Select | Enter | A / Cross | OK / Select |
| Back | Esc, Backspace | B / Circle | Back / Return |
| Play/pause | Space, `p` | X / Square | Play, Pause |
| Info | `i` | Y / Triangle | Info |
| Menu | `c` | Start | Setup / Contents menu, Guide |
| Home | Home | Guide | Root menu / Home |
| Seek back / forward | `r` / `f` | LB / RB | Rewind / Fast-forward |
| Previous / next | `,` / `.` | LT / RT | Skip back / forward, Channel −/+ |
| Search | `/` | Back / Select | Blue |
| Volume, mute | `+` `-` `m` | | Volume, Mute (if the TV forwards them) |
| Perf HUD | F3 | | |

### HDMI-CEC (TV remote)

jellyroll talks to the kernel's CEC devices (`/dev/cec0`, ...) directly. On
start it turns the TV on and switches it to this input, and when the TV
goes to standby it pauses playback. `--print-diagnostics` lists the CEC
devices and their addresses. Settings: `cec`, `cec_wake_tv` and
`cec_device` under `[input]` in the config.

- Many PCs have **no CEC on their HDMI port**. Intel's integrated graphics
  never expose it on native HDMI. Use a Pulse-Eight USB-CEC adapter, or a
  DisplayPort-to-HDMI adapter with CEC support.
- **Pulse-Eight USB adapter:** the kernel driver is `pulse8-cec`, but it
  must be attached to the serial port first:
  `sudo inputattach --pulse8-cec /dev/ttyACM0 &` (Fedora package
  `linuxconsoletools`). A udev rule or a small systemd unit can make that
  permanent.
- `/dev/cec*` belongs to the `video` group. The service user gets it from
  `install.sh`; to run by hand, `sudo usermod -aG video "$USER"`, then log
  out and back in.
- Some TVs need CEC switched on in their own menus (Anynet+, Bravia Sync,
  SimpLink, ...).

## Installing as the boot-to-app session

```sh
cargo build --release
sudo scripts/install.sh --dry-run   # shows every action
sudo scripts/install.sh
sudo reboot
```

The script is idempotent. It:

- creates an `jellyroll` user (`video`/`render`/`input` as a fallback to logind
  ACLs)
- installs `/usr/local/bin/jellyroll` and `jellyroll.service`
- copies the example config to `~jellyroll/.config/jellyroll/config.toml`
  (only if there isn't one already)
- sets the default target to **`multi-user.target`**
- masks any display manager

> **Use `multi-user.target`, not `rescue.target`.** "Single user" mode
> (`rescue.target`) starts no networking. A text-mode `multi-user.target`
> with no display manager is the equivalent you want.

The unit runs the app in a logind session on tty1 (`PAMName=login`), which
grants DRM master and input access. It sends `READY=1` after the first frame
and pings `WATCHDOG=1` from the render loop (`WatchdogSec=20`), with
`Restart=always`.

To get a console back: `sudo systemctl stop jellyroll && sudo systemctl start getty@tty1`.
Logs: `journalctl -u jellyroll -f`.

## Testing

```sh
cargo fmt --check
cargo clippy --features build-sdl --all-targets -- -D warnings
cargo test --features build-sdl
xvfb-run -a target/release/jellyroll --dev --bench-idle 3
xvfb-run -a env BIN=target/release/jellyroll scripts/bench-startup.sh 5 --dev
```

CI runs all of the above. Frame-time budgets only mean something on the
reference hardware, so `--bench-pacing` is part of the hardware checklist and
not CI.
