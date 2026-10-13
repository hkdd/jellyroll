# M6 validation on the HTPC

M6 exit criteria (spec §15): *browsing works end to end against a real
server; the scroll benchmark passes all §2 budgets with 10k items.*

Run on a text console (Ctrl+Alt+F3) as before. Write output to files and
copy them from the desktop with `cat FILE | tee >(wl-copy)`.

1. **Build:**
   ```sh
   cd ~/repo/jellyroll && git pull && cargo build --release
   ```
2. **Scroll benchmark** (built-in mock server with 10,000 movies; it does
   not touch your server, account or saved state):
   ```sh
   ./target/release/jellyroll --bench-scroll 20 > ~/jellyroll-m6-scroll.txt 2> ~/jellyroll-m6-scroll.log
   ```
   The screen opens Movies and scrolls down by itself for 20 s, then exits.
   Pass: `result: PASS` (CPU p99 < 4 ms, GPU p99 < 6 ms, 0 missed vsyncs,
   0 allocating frames). It uses `[display] ui_resolution` from the config
   (default `auto`: 1080p upscaled on 4K; the output's first lines show the
   render scale). For comparison, run it once more at native resolution:
   ```sh
   printf '[display]\nui_resolution = "native"\n' > /tmp/native.toml
   ./target/release/jellyroll --config /tmp/native.toml --bench-scroll 20 > ~/jellyroll-m6-scroll-native.txt 2>/dev/null
   ```
3. **Browse your server:**
   ```sh
   ./target/release/jellyroll --hud --allow-exit 2> ~/jellyroll-m6.log
   ```
   - **Nav bar:** press Up from the top row. Home, your libraries, Search
     and Settings.
   - **Library:** open your largest library. Scroll quickly (hold Down):
     placeholders, then posters; no stutter. Up to the bar: change Sort,
     toggle Unwatched / Favorites, pick a Genre. With Sort: Name, press
     Right at the end of a row to reach the A-Z column and jump to a letter.
   - **Detail:** select a movie. Check the badges (resolution, codec, HDR,
     audio): anything this GPU can't decode (e.g. HEVC 10-bit, AV1) is red
     with a warning line. Toggle Mark watched and Add to favorites, then
     check them in Jellyfin's web app.
   - **Series:** select a show: seasons, then episodes of the selected
     season; selecting an episode opens its page.
   - **Play:** on a movie with H.264 or 8-bit HEVC, press Play (or Resume).
     It should start at the resume point. Esc stops and returns to the
     page. (Progress is not reported to the server until M7.)
   - **Search:** press `/` anywhere, type a title, Enter jumps to results.
   - **Settings:** check "About this device" (GPU, display, hardware
     decode). Under Display, switch the interface between "1080p,
     upscaled" and "full resolution" and look at text and posters from the
     couch; the choice is remembered. Leave Sign out / Change server for
     last: Sign out returns to
     the Quick Connect screen; there, **Sign in with password** works too.
   - Home key returns to the home screen from any page; Esc goes back one
     page, and quits from the home screen.
4. **Results**, from the desktop:
   ```sh
   { cat ~/jellyroll-m6-scroll*.txt; grep -hE "first frame|playback|WARN|ERROR" ~/jellyroll-m6*.log | cut -c1-200; } | tee >(wl-copy)
   ```
   And tell me anything that looked wrong, felt slow, or was confusing.

Record the results in `docs/DECISIONS.md`.
