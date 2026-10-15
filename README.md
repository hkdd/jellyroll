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
| **M4** Input: keyboard, gamepad, HDMI-CEC; spatial focus graph; key repeat | **Done.** Keyboard verified on the HTPC; gamepad tested end to end with an SDL virtual pad. CEC is built and unit-tested, but the HTPC's DisplayPort-to-HDMI adapter doesn't carry CEC, so the TV remote waits for an adapter that does (`docs/DECISIONS.md` D29) |
| **M5** Jellyfin client: discovery, Quick Connect, auth, home data, images with caching; mock server | **Done.** On the HTPC against a Jellyfin 12.2 server: real home screen, cached start in ~0.4 s, first frame 393 ms. See `docs/DECISIONS.md` |
| **M6** Browsing: libraries (10k-item grids), detail pages, search, settings, password sign-in; play from the server | **Built.** On the HTPC: search and series pages verified; scroll benchmark 0 missed vsyncs, 0 allocating frames; native 4K over the CPU/GPU budgets on frames that load artwork (accepted, D44; 1080p upscaled in Settings). Rest of the walkthrough: `docs/M6-HARDWARE-CHECKLIST.md` |
| **M7** Player UX: OSD, track menus, trickplay, skip segments, next-up, progress reporting, resume | **Done.** On the HTPC: movie and episode flows, resume points and watched state on the server, next-episode autoplay (D49) |
| **M8** Display features: refresh-rate matching, audio passthrough, hotplug/TV standby, power menu | **Done** except two retests: on the HTPC 24p plays at 24 Hz, the power menu works, a TV power cycle is survived; open: sound after the power cycle, passthrough (D58) |
| M9 HDR10, native DRM, overlay-plane video | **Deferred** to newer hardware (e.g. an Intel N150): the HD 530 can neither decode HEVC 10-bit nor send HDR metadata through its adapter (D59) |
| **M10** Hardening: error states, soak test, restart, install | **Built**, awaiting the HTPC checklist (`docs/M10-HARDWARE-CHECKLIST.md`) |

The app connects to your Jellyfin server and shows its home screen: My
Media, Continue Watching, Next Up, the latest additions to each library and
Favorites, with the focused item's backdrop and details on top. Sign-in
uses Quick Connect: the TV shows a code, and you approve it from the
Jellyfin app on your phone or computer. After the first run the home
screen appears instantly from the cache and refreshes in the background.
From the nav bar at the top you can open each library as a grid (sort,
filters, A-Z jump), Search, and Settings. Selecting an item opens its
detail page, where Play streams the file directly (never transcoded)
from the resume point. The player shows chapters and trickplay previews
while seeking, lets you pick audio and subtitle tracks, skips intros and
credits, plays the next episode, and keeps the server's resume points and
watched state current (even if the server is briefly unreachable).
Changes made in other Jellyfin apps (watched, favorites, new items) show
up within a second or two, and those apps can control this one: Jellyfin's
"Play On" and remote control menus pause, seek, stop, change tracks,
navigate, send messages and start playback here.
Several people and servers can share the TV: Settings → Switch user
shows "Who's watching?", where you can also add another user or server;
it also appears at startup when more than one account is signed in. On
a movie or episode page, Audio and Subtitles let you pick tracks before
pressing Play.
While a video plays, the TV runs at the refresh rate that fits it (24 Hz
for films), and Settings chooses the audio output and passthrough to a
receiver. Back on the home screen opens the power menu (restart, shut
down).
`--demo` still shows the synthetic M2 render benchmark.

## Layout

```
crates/platform   jellyroll-platform: Platform trait, SDL3 backend, GL loader,
                  counting allocator. The only crate allowed `unsafe`.
crates/render     jellyroll-render: Renderer trait, Skia backend, text
                  shaping (cosmic-text) over the bundled fonts.
crates/video      jellyroll-video: libmpv render-API binding (FFI; may use
                  `unsafe`) and the VA-API decode-capability probe.
crates/jellyfin-api  Jellyfin client: DTOs generated from the server's
                  OpenAPI spec (scripts/gen-jellyfin-dto.py), auth header,
                  endpoints, HTTP client (reqwest + rustls), LAN discovery.
crates/mock-server   jellyroll-mock: a fake Jellyfin server for development
                  and CI (synthetic library of any size, Quick Connect).
crates/app        jellyroll binary (#![forbid(unsafe_code)]): render loop,
                  input normalization + key repeat, focus graph, network
                  thread and caches, home screen, perf stats/HUD, systemd.
dist/             systemd unit, example config
scripts/make-test-clips.sh  synthetic H.264 / HEVC 8-bit / HEVC 10-bit clips
scripts/          install.sh, dev-cage.sh, bench-startup.sh
assets/fonts/     bundled Noto fonts (subset; regenerate with
                  scripts/subset-fonts.sh)
docs/             VERSIONS.md, DECISIONS.md, per-milestone hardware checklists
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

On Fedora, H.264/HEVC playback also needs the full FFmpeg and VA-API driver
from RPM Fusion; see [Fedora 44: codecs and hardware decode](#fedora-44-codecs-and-hardware-decode).

HDMI-CEC uses the kernel's CEC devices directly (no libcec). The
Jellyfin client needs no extra system packages: TLS (rustls with
aws-lc) builds with the C compiler already required above.

### Fedora 44: codecs and hardware decode

Fedora leaves patented codecs out of its own packages, so out of the box
jellyroll can't decode H.264 or HEVC (8- or 10-bit) in hardware and the item
page says "Can't play directly". Two packages need replacing with RPM Fusion's
full builds: the FFmpeg that mpv decodes with, and the VA-API driver. Run
these on the HTPC. Tested on an Intel N150 (Twin Lake), where it enables HEVC
10-bit and AV1.

1. See what the driver reports now:

   ```sh
   sudo dnf install -y libva-utils
   vainfo --display drm --device /dev/dri/renderD128 2>&1 | grep -E "Driver version|HEVC"
   ```

2. Enable RPM Fusion (free and nonfree), unless it already is:

   ```sh
   sudo dnf install -y https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-$(rpm -E %fedora).noarch.rpm https://mirrors.rpmfusion.org/nonfree/fedora/rpmfusion-nonfree-release-$(rpm -E %fedora).noarch.rpm
   ```

3. Intel GPUs (Broadwell and newer, including N100/N150): replace Fedora's
   VA-API driver with the full one:

   ```sh
   sudo dnf swap -y libva-intel-media-driver intel-media-driver --allowerasing
   ```

   AMD GPUs instead need Mesa's full VA-API driver:
   `sudo dnf swap -y mesa-va-drivers mesa-va-drivers-freeworld --allowerasing`.

4. Replace the codec-stripped FFmpeg (prints "not installed" if it already
   has been):

   ```sh
   rpm -q ffmpeg-free && sudo dnf swap -y ffmpeg-free ffmpeg --allowerasing
   ```

5. Check, and restart jellyroll (it asks the driver at every play, so no
   rebuild):

   ```sh
   vainfo --display drm --device /dev/dri/renderD128 2>&1 | grep -E "Driver version|HEVCMain10"
   sudo systemctl restart jellyroll
   ```

   Expect `Intel iHD driver` and `VAProfileHEVCMain10 : VAEntrypointVLD`.
   After playing something, `sudo journalctl -t jellyroll -b | grep -E
   "VA-API hardware decode|hardware decoding"` shows what jellyroll found and
   used.

If HEVC stops working after a large update (reinstalling Fedora's
multimedia group can bring the stripped driver back), check
`rpm -q intel-media-driver libva-intel-media-driver` and repeat step 3.

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
| `--dev` | Desktop window instead of KMSDRM (implies `--allow-exit`) |
| `--window WxH` | Dev window size, e.g. `3840x2160` to check 4K layout |
| `--hud` | Show the perf HUD (F3 toggles) |
| `--allow-exit` | Back on a top-level screen (e.g. a problem screen) quits the app (always on with `--dev`). "Exit to console" in the power menu is always there |
| `--double-buffer` | Double instead of triple buffering on KMSDRM |
| `--demo` | Synthetic render benchmark scene instead of the Jellyfin home screen |
| `--bench-pacing SECS` | Continuous animation. Prints CPU/GPU/present p50/p99, missed vsyncs and allocating frames, and exits 1 if a spec §2 budget is exceeded |
| `--bench-idle SECS` | Static screen. Fails if anything redraws or CPU ≥ 1% |
| `--bench-upload` | Time the ways of uploading artwork to the GPU on this machine, then exit |
| `--soak HOURS` | Drive the UI for HOURS against the built-in mock server (throwaway profile), log memory every minute to `soak.csv`, exit 1 if it keeps growing. Ctrl+C stops early with a summary |
| `--soak-media FILE` | With `--soak`: play FILE for a minute in every cycle |
| `--bench-scroll SECS` | Scroll a 10,000-item library grid (built-in mock server, no account needed) and check the same budgets as `--bench-pacing` |
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

During playback:

| Key | Does |
|---|---|
| Left / Right | Seek back / forward (hold to go faster); shows a preview |
| Select (Enter) | Confirm a seek, skip the intro/credits, play the next episode now, or pause |
| Play/pause | Pause or resume |
| Down, Menu | Audio and subtitle tracks |
| Info | Codec, decoder, play method, dropped frames |
| Previous / next | Previous / next chapter |
| Back | Close the menu, cancel a seek or the next-episode countdown, or stop |

### Debug info (playback problems)

On an item page, **Debug info** writes everything needed to diagnose why that
item doesn't play well into one text file: this machine's decode
capabilities, display and audio outputs, installed driver packages and
settings; the server's full description of the file and its playback
decision; jellyroll's own decision; an 8-second hidden test decode with the
player's verbose log; and recent log lines. Access tokens are left out. The
dialog shows the path, e.g.
`/home/jellyroll/.local/share/jellyroll/debug/jellyroll-debug-heat-20261015-010203.txt`.
Fetch it with `sudo cat` (over SSH) and attach it to a bug report.

### HDMI-CEC (TV remote)

jellyroll talks to the kernel's CEC devices (`/dev/cec0`, ...) directly. On
start it turns the TV on and switches it to this input, and when the TV
goes to standby it pauses playback. `--print-diagnostics` lists the CEC
devices and their addresses. Settings: `cec`, `cec_wake_tv` and
`cec_device` under `[input]` in the config.

- Many PCs have **no CEC on their HDMI port**. Intel's integrated graphics
  never expose it on native HDMI. A DisplayPort output (or a
  DisplayPort-to-HDMI adapter) that passes CEC through usually works, and
  shows up as `/dev/cec0`; otherwise use a Pulse-Eight USB-CEC adapter.
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

### Connecting to Jellyfin

1. **Server:** the first time, jellyroll asks for your server's address:
   type it with a keyboard, or use the on-screen keyboard with the remote
   or a gamepad (`http://` and port 8096 are added if you leave them out).
   **Search my network** lists servers that answer on the local network
   instead. After that it reconnects to the same server automatically;
   press Back on a connection-problem or sign-in screen to choose another.
   (`[server] url` in the config, if set, always wins.) Servers 10.10.0
   and newer are supported.
2. **Sign in:** a 6-digit Quick Connect code appears on the TV. In
   Jellyfin's web app on a phone or computer, open the menu under your
   profile picture (top right) and choose **Quick Connect** (or go to
   `https://YOUR-SERVER/web/#/quickconnect`), then enter the code. Quick Connect must be turned on in the
   server's Dashboard (General). Or choose **Sign in with password** on that
   screen.
3. **Remembered:** the access token (never your password) is kept in
   `~/.local/share/jellyroll/state.json` (readable only by you). Caches
   live in `~/.cache/jellyroll/` and can be deleted at any time.

For an HTTPS server with a self-signed certificate, set `[server] ca_file`
to its certificate (PEM).

To try jellyroll without a server, or for development, run the mock:

```sh
cargo run --release -p jellyroll-mock-server -- --port 8096
# then [server] url = "http://127.0.0.1:8096" (Quick Connect code 424242, approves itself)
```

The mock can also act as another Jellyfin app controlling this one:

```sh
curl -X POST --data '{"MessageType":"Playstate","Data":{"Command":"PlayPause"}}' localhost:8096/Mock/Send
```

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
- sets the default target to **`multi-user.target`** (remembering the old
  one). A desktop's display manager is left installed and enabled: it only
  starts with `graphical.target`, so `sudo systemctl stop jellyroll && sudo
  systemctl isolate graphical.target` still gets the desktop
- unmasks display managers an older version of the script masked

`sudo scripts/uninstall.sh` undoes it: removes the service, binary and polkit
rule, puts the old default target back (and the display manager, if one is
installed), and starts the login prompt on tty1. The `jellyroll` user and its
sign-in and caches stay unless you add `--purge`.

> **Use `multi-user.target`, not `rescue.target`.** "Single user" mode
> (`rescue.target`) starts no networking. A text-mode `multi-user.target`
> is the equivalent you want.

The unit runs the app in a logind session on tty1 (`PAMName=login`), which
grants DRM master and input access. It sends `READY=1` after the first frame
and pings `WATCHDOG=1` from the render loop (`WatchdogSec=20`), with
`Restart=always`.

Getting a shell: on the home screen press Back to open the power menu and
choose **Exit to console**. jellyroll quits, systemd doesn't restart it, and a
login prompt appears on the TV; `sudo systemctl start jellyroll` (or a reboot)
brings it back. Ctrl+Alt+F*n* doesn't switch consoles while jellyroll runs
(SDL turns off the console's keyboard handling so keys don't leak into it).
Also: SSH, or from SSH `sudo chvt 2` for a login prompt on the TV with the app
still running (`sudo chvt 1` goes back), or stop it:
`sudo systemctl stop jellyroll && sudo systemctl start getty@tty1`. With no
network, boot with `systemd.unit=rescue.target` added to the kernel line in
GRUB (`e` on the entry, Ctrl+X to boot).

Logs: `sudo journalctl -t jellyroll -f`. The app writes to the journal
directly under its own name, and `-u jellyroll` shows only systemd's lines.

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

### Profiling a slow frame

A build with the `profile` feature can record a timeline of every frame
(waiting for input, network events, input and update, video, artwork
uploads, drawing, GPU submit, swap) and of the artwork decoding on the
network thread:

```sh
cargo build --release --features profile
target/release/jellyroll --trace /tmp/jellyroll-trace.json   # use it, then quit
```

Open the file at <https://ui.perfetto.dev> (or `chrome://tracing`). Each
thread is a track; zoom in on a long `frame` to see which stage took the
time. Recording allocates, so a traced run's allocation numbers don't
count. Normal builds don't include the recorder; the spans cost nothing
there (D68).
