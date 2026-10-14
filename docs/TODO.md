# To do

Agreed 2026-10-14, in this order. Strike through when done.

1. ~~**Starting a video: no black wait, and the progress bar shows.**~~ Built (D69).
   - While a video starts (the TV switching refresh rate, mpv opening the
     stream), show the item's backdrop, dimmed, with its title and
     "Starting…", instead of a black screen.
   - The player's bar at the bottom appears when the first video frame
     does. (Today its 4-second timer starts when loading begins, so on a
     slow start it has already hidden by then.)
2. ~~**The console flashes on screen when a video starts**~~ Built (D70);
   to confirm on the HTPC with a 24p movie and Match refresh rate on. (Only on start,
   not on stop). Find what leaves the console visible for a moment (most
   likely the refresh-rate switch) and keep jellyroll's picture on screen
   throughout.
3. ~~**Ask before playing what this PC can't play directly.**~~ Built (D71); the
   "GPU can't decode" case to confirm on the HTPC with an HEVC 10-bit file. Only when it
   can't direct play; the choice is not remembered.
   - The server can send the file as it is but the GPU can't decode it
     (e.g. HEVC 10-bit, VP9, AV1 on the HD 530): **Software decode** /
     **Server transcode** / **Cancel**. For 4K, the dialog notes that
     software decoding will likely stutter on this CPU.
   - The server can't send the file as it is: **Server transcode** /
     **Cancel**.
   - Software decode: the red "software decoding" warning shows, then
     goes away after 5 seconds of playback.
   - Server transcode: follow Jellyfin's normal behaviour: tell the server
     what this device can decode and let it choose (in practice H.264).
   - Replaces D19's "never transcode" with "ask".
4. ~~**Year filter, finer.**~~ Built (D72). Keep the decade list; picking a decade then
   offers the single years within it (1990s → 1994), with "All of the
   1990s" at the top.
5. ~~**Screensaver.**~~ Built (D73). Settings: Off / 5 / 10 / 15 minutes. Starts after that
   long without input while browsing or while a video is paused. Any key
   wakes it and does nothing else. First style: The Matrix's falling green
   characters, built so more styles can be added.
6. **Watched mark on the item page.** A check mark in the item page's
   header (next to the year and rating) for watched items; cards already
   have one.

Done earlier (D63-D68): live updates and remote control, audio/subtitle
choice before Play, several users and servers, year filter by decade,
playback and storage settings, developer tracing.

Not doing:
- Config extras (hwdec override, key remapping): other changes coming
  make them unnecessary.
- Automatic subtitles for foreign-language audio: Jellyfin's per-user
  "Smart" subtitle mode already does it, and jellyroll starts with the
  server's default tracks.
- Media manager (identify unmatched movies, reconcile disk and library):
  removed for now.
- Versions picker: no grouped versions in the library.

Deferred:
- M9 (HDR10 passthrough, native DRM backend, overlay-plane video, spec
  7.4) until newer hardware (D59).
- The 72-hour soak (M10): everyday use instead; `--soak` stays.
