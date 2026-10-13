# M4 hardware validation

M4 exit criterion (spec §15): *navigate a synthetic grid by all three
sources (keyboard, gamepad, HDMI-CEC) with consistent behavior.*

Run on a text console (Ctrl+Alt+F3) as a user in `input` (keyboard and
gamepad) and `video` (CEC). Write output to a file, then copy it from the
desktop with `cat FILE | tee >(wl-copy)`.

1. **Build**
   ```sh
   cd ~/repo/jellyroll && git pull && cargo build --release
   ```
2. **Devices:** `./target/release/jellyroll --print-diagnostics > ~/jellyroll-m4-diag.txt 2>&1`
   - `input_devices`: readable event devices (a gamepad adds one).
   - `gamepads`: the count, then each connected pad by name.
   - `cec`: each `/dev/cecN` with its physical and logical address, or why
     it can't be opened. No `/dev/cec*` at all means this machine
     has no CEC adapter (see the README).
3. **Keyboard** (`./target/release/jellyroll --hud`)
   - Arrows move focus card by card; Up from the first row reaches the
     nav pills; Enter on a pill selects it (it stays highlighted).
   - Row memory: in row 2 move to the 6th card, go Up, move Left twice,
     go Down: focus returns to the 6th card in row 2.
   - Holding an arrow repeats after ~350 ms and speeds up after 1.5 s;
     releasing stops immediately (no run-on).
4. **Gamepad** (plug in before or after starting; both work)
   - Log shows `gamepad connected` with its name.
   - D-pad and left stick move exactly like the arrows, including hold to
     repeat; a stick resting slightly off center doesn't drift.
   - A/Cross on a nav pill selects it; B/Circle is Back (exits with
     `--allow-exit`).
   - Unplug and replug: `gamepad disconnected`, then works again.
5. **TV remote over CEC**
   - With the TV on another input or in standby, start jellyroll: the TV
     turns on and switches to this PC (disable with `cec_wake_tv = false`).
   - Remote arrows move focus; holding an arrow repeats; OK selects; Back
     goes back.
   - Play a clip (`--play ~/test-media/h264-2160p.mp4`) and turn the TV off
     with its remote: playback pauses.
   - If the TV doesn't respond, check that CEC is enabled in the TV's
     settings (Anynet+, Bravia Sync, SimpLink, ...) and look for `cec`
     lines in the log.
6. **Consistency:** the three sources move focus identically: same
   targets, same repeat feel, same row memory.
7. **Regression:** `./target/release/jellyroll --bench-pacing 20` and
   `--bench-idle 10` give the same results as M2 (the nav bar adds a few
   draws).

Record results (pad model, CEC adapter and TV model, anything odd) in
`docs/DECISIONS.md`.
