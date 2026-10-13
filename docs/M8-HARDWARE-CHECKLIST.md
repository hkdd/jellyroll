# M8 validation on the HTPC

M8 exit criteria (spec §15): *24p content plays judder-free; passthrough
verified on AVR; TV power cycle survives.*

Run on a text console (Ctrl+Alt+F3) as before. Write output to files and
copy them from the desktop with `cat FILE | tee >(wl-copy)`.

1. **Build:**
   ```sh
   cd ~/repo/jellyroll && git pull && cargo build --release
   ```
2. **What the TV offers** (lists display modes and audio outputs):
   ```sh
   ./target/release/jellyroll --print-diagnostics > ~/jellyroll-m8-diag.txt 2>&1
   ```
3. **Run:**
   ```sh
   ./target/release/jellyroll --allow-exit 2> ~/jellyroll-m8.log
   ```
4. **Refresh-rate matching** (Settings → Display shows "Match refresh
   rate: on"):
   - Play a film (23.976 fps). The screen goes black for a moment while
     the TV switches, then it plays. The TV's info button should show 24
     Hz (or 23.976). Watch a slow camera pan: it should glide, not stutter
     every half second.
   - If the start of the video is cut off while the TV switches, add
     `refresh_switch_delay_ms = 1500` under `[display]` in
     `~/.config/jellyroll/config.toml` and try again.
   - Stop (Back): the TV goes back to 60 Hz with the menus.
   - Play an episode of a 25 or 29.97 fps show if you have one: 50 or
     59.94 Hz. Let it autoplay the next episode: no second switch.
   - Settings → Display → Match refresh rate: off. Play the film again:
     no switch (60 Hz). Turn it back on.
5. **Audio** (Settings → Audio):
   - Output: Select cycles through what mpv found. Note which one is your
     TV/receiver's HDMI (for passthrough, an `alsa/hdmi:...` entry usually
     works best).
   - With a receiver or soundbar that decodes Dolby/DTS: Passthrough: on,
     then play something with Dolby Digital (AC3) or DTS audio. The
     receiver's display should show Dolby/DTS, not PCM. Info (`i`) in the
     player shows the audio codec.
   - Without one, leave passthrough off: audio plays as before.
   - Channels: auto/stereo/5.1/7.1 for decoded audio (try stereo if a
     surround track sounds thin on TV speakers).
6. **TV power cycle:** play something, turn the TV off with its remote,
   wait 10 s, turn it on. Playback should be paused (if the TV dropped
   the connection) or still running (if it didn't); either way the picture
   comes back and jellyroll is still running. Also try it from the menus.
7. **Power menu:** on the home screen press Back: Restart, Shut down,
   Cancel (and "Exit to console", because of `--allow-exit`). Cancel
   closes it. Restart reboots the HTPC (save your work elsewhere first).
   If it says the user may not do that, install the polkit rule:
   ```sh
   sudo sed 's/"jellyroll"/"'"$USER"'"/' docs/polkit-jellyroll.rules | sudo tee /etc/polkit-1/rules.d/50-jellyroll.rules
   ```
8. **Results**, from the desktop:
   ```sh
   { grep -E "^display|mode:|current:|audio" ~/jellyroll-m8-diag.txt; grep -hE "output mode|frame rate|refresh|display (dis)?connected|watching display|audio outputs|setting changed|power|hardware decod|WARN|ERROR" ~/jellyroll-m8.log | cut -c1-200; } | tee >(wl-copy)
   ```
   And tell me how the pan looked, what the receiver showed, and what
   happened on the TV power cycle.

Record the results in `docs/DECISIONS.md`.
