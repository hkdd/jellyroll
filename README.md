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
| **M1** Skeleton: SDL3 KMSDRM, GL context, vsync loop, dirty redraw, perf HUD, systemd unit, dev mode | **Implemented.** Verified in software (Mesa llvmpipe under Xvfb). **Not yet run on real hardware**; see [`docs/M1-HARDWARE-CHECKLIST.md`](docs/M1-HARDWARE-CHECKLIST.md) |
| M2 Renderer trait + Skia | Not started. The spec gates it on M1 passing on hardware |
| M3 to M10 | Not started |

M1 shows a test pattern: a tile grid with an animated focus frame, a
safe-area outline, and a perf HUD on F3. The real UI starts in M2/M6.

## Layout

```
crates/platform   jellyroll-platform: Platform trait, SDL3 backend, GL loader,
                  counting allocator. The only crate allowed `unsafe`.
crates/app        jellyroll binary (#![forbid(unsafe_code)]): render loop,
                  input normalization + key repeat, perf stats/HUD, systemd.
dist/             systemd unit
scripts/          install.sh, dev-cage.sh, bench-startup.sh
docs/             VERSIONS.md, DECISIONS.md, M1-HARDWARE-CHECKLIST.md
```

## Building

Rust stable (MSRV 1.88). Two ways to get SDL3:

```sh
cargo build --release                      # links the system libSDL3 (>= 3.2)
cargo build --release --features build-sdl # builds the pinned SDL 3.4.18 statically
```

With `build-sdl` the binary links only libc. SDL `dlopen`s drm/gbm/EGL/GL/udev
at runtime.

System packages (runtime, plus headers when using `build-sdl`):

| | Fedora (reference) | Debian/Ubuntu | Arch |
|---|---|---|---|
| SDL3 | `SDL3-devel` | build with `build-sdl` (24.04 has no SDL3) | `sdl3` |
| Mesa / KMS | `mesa-libEGL-devel mesa-libgbm-devel libdrm-devel mesa-dri-drivers` | `libegl-dev libgbm-dev libdrm-dev libgl1-mesa-dri` | `mesa libdrm` |
| input | `systemd-devel` (libudev) | `libudev-dev` | `systemd-libs` |
| build-sdl only | `cmake ninja-build` + X11/Wayland headers | see `.github/workflows/ci.yml` | `cmake ninja` |

libmpv, PipeWire and libcec come in at M3/M4.

## Running

```sh
# Development, in a window on your desktop (Wayland or X11):
target/release/jellyroll --dev --hud

# Development under cage (a kiosk Wayland compositor), e.g. from a spare VT:
scripts/dev-cage.sh --hud

# Production path (KMSDRM), from a text console with the service stopped:
target/release/jellyroll
```

| Flag | |
|---|---|
| `--dev` | Desktop window instead of KMSDRM. Esc quits |
| `--hud` | Show the perf HUD (F3 toggles) |
| `--allow-exit` | Esc/Back quits outside dev mode |
| `--double-buffer` | Double instead of triple buffering on KMSDRM |
| `--bench-pacing SECS` | Continuous animation. Prints CPU/GPU/present p50/p99, missed vsyncs and allocating frames, and exits 1 if a spec §2 budget is exceeded |
| `--bench-idle SECS` | Static screen. Fails if anything redraws or CPU ≥ 1% |
| `--print-diagnostics` | SDL/GL/driver/display-mode/DRI/CEC info, for support over SSH |

Keyboard (Kodi-like): arrows, Enter = select, Esc/Backspace = back,
Space = play/pause, `i` info, `c` menu, `f`/`r` seek, `+`/`-` volume, `m` mute,
`/` search, F3 HUD.

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
