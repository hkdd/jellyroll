# M10 validation on the HTPC

M10 exit criteria (spec §15): *no RSS growth over soak; clean restart on
killed process.*

Run on a text console (Ctrl+Alt+F3) as before. Write output to files and
copy them from the desktop with `cat FILE | tee >(wl-copy)`.

1. **Build:**
   ```sh
   cd ~/repo/jellyroll && git pull && cargo build --release
   ```
2. **Soak** (the long one; overnight at least, 72 h ideally). It uses the
   built-in mock server and a throwaway profile, so your server, account
   and settings are untouched. Give it a video file the GPU decodes
   (H.264 or 8-bit HEVC, any length):
   ```sh
   cd ~ && ~/repo/jellyroll/target/release/jellyroll --soak 72 --soak-media /path/to/movie.mkv > ~/soak.out 2> ~/soak.log
   ```
   The screen loops by itself: home rows, a 2000-movie grid scrolled deep,
   an item page, a minute of the video, search. It writes `~/soak.csv`
   (memory every minute) and prints a verdict at the end. You can stop it
   early with Ctrl+C; it still prints the summary (the verdict needs at
   least 35 minutes).
3. **Server lost and back** (2 minutes): with jellyroll running normally,
   open a library, then stop the Jellyfin server (or unplug the network).
   Open another library: it says "Can't reach the server. Retrying
   automatically…" and the home screen shows "Offline". Start the server
   again (plug back in): within a few seconds the page fills in by
   itself.
4. **Stop during playback:** play something for a minute, then from SSH or
   another console `pkill -TERM jellyroll`. In Jellyfin's web app the
   movie's resume point is where it was stopped.
5. **Optional, when you make it boot-to-app** (it boots to text mode; the
   desktop stays available with `systemctl isolate graphical.target`, and
   `scripts/uninstall.sh` undoes it):
   ```sh
   sudo scripts/install.sh --user "$USER"
   sudo systemctl start jellyroll
   ```
   It must come up on tty1 with sound and the power menu working (the
   unit is now sandboxed, D61). Then `sudo pkill -9 jellyroll`: it is back
   within a couple of seconds, showing the home screen.
6. **Results**, from the desktop:
   ```sh
   { cat ~/soak.out; tail -3 ~/soak.csv; grep -hE "WARN|ERROR" ~/soak.log | sort | uniq -c | sort -rn | head -20 | cut -c1-200; } | tee >(wl-copy)
   ```

Record the results in `docs/DECISIONS.md`.
