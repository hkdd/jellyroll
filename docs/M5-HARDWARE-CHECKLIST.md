# M5 validation against a real server

M5 exit criteria (spec §15): *real home screen populated from a real
server; stale-while-revalidate working.* Budgets (spec §2): start to
interactive home screen < 1.5 s with cached data, < 2.5 s with a fresh
fetch on the LAN.

Run on a text console (Ctrl+Alt+F3) as before. Write output to files and
copy them from the desktop with `cat FILE | tee >(wl-copy)`.

1. **Build** (no new system packages):
   ```sh
   cd ~/repo/jellyroll && git pull && cargo build --release
   ```
2. **Server version**, from the desktop (replace SERVER):
   ```sh
   curl -s http://SERVER:8096/System/Info/Public | tee >(wl-copy)
   ```
   jellyroll needs 10.10.0 or newer. **Quick Connect must be on:** Dashboard
   → General → Quick Connect.
3. **First run: discovery and sign-in.** Leave `[server] url` empty to
   test LAN discovery (or set it if your server is on another subnet):
   ```sh
   ./target/release/jellyroll --hud --allow-exit 2> ~/jellyroll-m5-first.log
   ```
   - "Looking for Jellyfin servers…", then a 6-digit code.
   - In the Jellyfin app on your phone or computer: profile → Quick
     Connect → enter the code. Within ~3 s the TV shows your home screen.
   - Check: your libraries (My Media), Continue Watching with progress
     bars, Next Up, Latest per library, Favorites. Posters fade in from
     blurred placeholders; the focused item's backdrop fills the top.
   - Move around: rows scroll sideways; Up/Down go to the item straight
     above/below; the backdrop and details follow focus.
   - Esc quits.
4. **Second run: instant cached home (stale-while-revalidate):**
   ```sh
   ./target/release/jellyroll --allow-exit 2> ~/jellyroll-m5-cached.log
   ```
   - The home screen appears at once, without a code. Change something on
     the server (e.g. mark an episode played on your phone): within a few
     seconds of the next start the rows update without the focus jumping.
5. **Offline:** stop the server (or set `[server] url` to a wrong port) and
   start jellyroll: the saved home screen appears with an "Offline ·
   showing saved data" badge. Start the server again: within a minute the
   badge disappears.
6. **Results**, from the desktop:
   ```sh
   grep -hE "connected|cached|signed in|first frame|WARN|ERROR" ~/jellyroll-m5-*.log | cut -c1-200 | tee >(wl-copy)
   ```
   Timing: `first frame presented` (process start to first frame) and the
   time from that to `showing cached home screen` / the first fresh home.
7. **Optional, for DTO tests with real data** (spec 13): save one response
   so parsing is tested against your server's actual output. Replace
   SERVER and TOKEN (the token is in `~/.local/share/jellyroll/state.json`;
   don't post it):
   ```sh
   curl -s -H 'Authorization: MediaBrowser Token="TOKEN"' \
     'http://SERVER:8096/UserItems/Resume?limit=3&fields=PrimaryImageAspectRatio,Overview&enableImageTypes=Primary,Backdrop,Thumb&imageTypeLimit=1' \
     > ~/jellyroll-resume.json
   ```

Record the server version and results in `docs/DECISIONS.md`.
