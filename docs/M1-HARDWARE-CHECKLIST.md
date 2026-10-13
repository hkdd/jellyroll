# M1 hardware validation

M1 exit criteria (spec §15): *boots to a colored screen from
`multi-user.target`; frame timing visible; 0 CPU idle.* These steps need a
real machine. CI only covers the software (llvmpipe) smoke tests. Paste the
results into this file or a PR.

1. **Build and install**
   ```sh
   cargo build --release            # system SDL3 (Fedora: dnf install SDL3-devel)
   # or: cargo build --release --features build-sdl
   sudo scripts/install.sh --dry-run   # review first
   sudo scripts/install.sh
   ```
2. **Diagnostics** from an SSH session, with the service stopped so DRM
   master is free:
   ```sh
   sudo systemctl stop jellyroll
   sudo -u jellyroll jellyroll --print-diagnostics
   ```
   Record the GL renderer/version, `gl.timer_queries`, `swap_interval` and the
   full mode list. The mode list answers open question 1.
3. **Boot test:** `sudo reboot`. Expect the colored tile grid on the TV with
   no login prompt and no flicker. Then check
   `journalctl -b -u jellyroll | grep 'first frame'`. The budget is < 500 ms
   from process start.
4. **Frame timing visible:** press F3 on a USB keyboard. The HUD shows CPU
   and GPU p50/p99, present interval, missed vsyncs, alloc count, RSS and the
   frame-time graph.
5. **Idle:** with the HUD off and nothing moving, `top -p $(pidof jellyroll)`
   should read 0.0% CPU. For the scripted check, stop the service, switch to
   tty2 (Ctrl+Alt+F2), log in as a user in `video`/`render`/`input`, and run:
   ```sh
   jellyroll --bench-idle 30        # expect: frames_rendered: 0, result: PASS
   ```
6. **Pacing:** on the same VT, run `jellyroll --bench-pacing 60 --hud`. The white bar
   sweeps continuously and should move smoothly, with no tearing or judder.
   Expect `result: PASS`: CPU p99 < 4 ms, GPU p99 < 6 ms, 0 missed vsyncs,
   0 allocating frames. Repeat with `--double-buffer` and record both (D10).
7. **Watchdog:** `sudo kill -STOP $(pidof jellyroll)`. Within about 20 s
   systemd should kill and restart it (`journalctl -u jellyroll`).
8. **Crash restart:** `sudo kill -9 $(pidof jellyroll)`. It should be back
   within about 1 s.
9. **Input:** arrow keys move the focus frame with a short glide. Holding an
   arrow repeats after 350 ms and speeds up after 1.5 s. Enter toggles a tile.

Fill in the reference hardware (spec §17) and the verified Mesa/kernel
versions in `docs/VERSIONS.md`.
