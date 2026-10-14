# M7 validation on the HTPC

M7 exit criterion (spec §15): *full movie and episode watch flow with
correct server-side progress.*

Run on a text console (Ctrl+Alt+F3) as before. Write output to files and
copy them from the desktop with `cat FILE | tee >(wl-copy)`.

1. **Build:**
   ```sh
   cd ~/repo/jellyroll && git pull && cargo build --release
   ```
2. **Server features (optional, for previews and skip buttons):**
   - Trickplay previews: in Jellyfin's Dashboard → Libraries → your
     library → enable trickplay image extraction, then let the scheduled
     task generate them (it can take a while).
   - Skip intro/credits: the server needs a media segment provider, e.g.
     the Intro Skipper plugin. Without one, everything else still works.
3. **Run:**
   ```sh
   ./target/release/jellyroll --allow-exit 2> ~/jellyroll-m7.log
   ```
4. **Movie** (H.264 or 8-bit HEVC, so it decodes in hardware):
   - Play it. The title, a time bar with chapter marks, and a hint line
     appear, then hide after a few seconds; any key shows them again.
   - Seek: tap Right a few times, then hold it. The target moves faster
     the longer you hold, with a preview picture above it (if trickplay
     is generated). It jumps there when you stop or press Enter.
   - Down: audio and subtitle lists. Switch audio; turn subtitles on (try
     an external .srt if the movie has one) and off. Back closes it.
   - Info: the details panel should say a hardware decoder (`vaapi`) and
     DirectPlay.
   - Pause (Enter or Space), resume. Then Back at a time you note down.
   - In Jellyfin's web app: the movie is in Continue Watching at about
     that time. Open it again in jellyroll: "Resume" starts there.
5. **Episode:**
   - Play an episode of a show that has the next one available.
   - During the intro (if segments exist): **Skip intro** appears
     bottom-right; Enter skips it.
   - Seek near the end (credits): the **Up next** card counts down 10 s,
     then the next episode starts by itself. Try Enter (play now) and,
     on another episode, Back (cancel: it plays to the end and stops).
   - In the web app: the finished episode is marked watched; the next one
     shows your position when you stop.
6. **Offline progress:** play something, unplug the network (or stop the
   server), press Back, quit jellyroll. Check the report waited:
   ```sh
   ls -l ~/.local/share/jellyroll/pending-reports.json
   ```
   Reconnect, start jellyroll again: within a few seconds the file is
   gone and the web app shows where you stopped.
7. **Results**, from the desktop:
   ```sh
   grep -hE "playback|hardware decod|next episode|report|WARN|ERROR" ~/jellyroll-m7.log | cut -c1-200 | tee >(wl-copy)
   ```
   And tell me anything that looked wrong, felt slow, or was confusing.

Record the results in `docs/DECISIONS.md`.
