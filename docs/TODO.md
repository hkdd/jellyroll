# To do

Spec features not built yet (checked against the spec, 2026-10-14). In
priority order; strike through or delete when done.

1. ~~**Live updates and remote control (Jellyfin WebSocket, spec 8).**~~
   Built (D63); to check against the real server: `docs/LIVE-UPDATES-CHECKLIST.md`.
2. ~~**Item page: audio/subtitle quick-pick (spec 10.1).**~~ Built (D64).
3. ~~**Multiple users and servers (spec 8).**~~ Built (D65).
4. ~~**Library year filter (spec 10.1).**~~ Built (D66).
5. ~~**Settings (spec 7.1, 10.1):** playback, caches.~~ Built (D67).
6. **Config extras (spec 11):** hwdec override, input key remapping.
7. **Developer tracing (spec 13):** spans around update/layout/draw/
   upload/swap; optional Tracy or tracing-chrome behind a feature flag.

Deferred:
- M9 (HDR10 passthrough, native DRM backend, overlay-plane video, spec
  7.4) until newer hardware (D59).
- The 72-hour soak (M10): replaced by everyday use;
  `--soak` stays for chasing a suspected leak.
- Versions picker (spec 10.1: choose between grouped copies of a movie,
  e.g. 4K / 1080p): dropped 2026-10-14, the library in use has no
  grouped versions. jellyroll plays the server's first version.
