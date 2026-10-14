# To do

Spec features not built yet (checked against the spec, 2026-10-14). In
priority order; strike through or delete when done.

1. **Live updates and remote control (Jellyfin WebSocket, spec 8).**
   Library, user-data and session messages from `/socket`: changes made
   elsewhere (watched, favorites, new items) show at once; Jellyfin's apps
   can pause, seek and stop playback here. Reconnect with jittered
   backoff; invalidate cached data on library events (spec 9).
2. **Item page: versions picker and audio/subtitle quick-pick (spec
   10.1).** Choose between media sources (4K / 1080p); choose tracks before
   Play.
3. **Multiple users and servers (spec 8).** User picker at startup;
   "Switch user" in Settings without signing out.
4. **Library year filter (spec 10.1).**
5. **Settings (spec 7.1, 10.1):** playback (interpolation toggle,
   deinterlacing, HDR tone mapping for HDR on an SDR output), caches (sizes, clear).
6. **Config extras (spec 11):** hwdec override, input key remapping.
7. **Developer tracing (spec 13):** spans around update/layout/draw/
   upload/swap; optional Tracy or tracing-chrome behind a feature flag.

Deferred:
- M9 (HDR10 passthrough, native DRM backend, overlay-plane video, spec
  7.4) until newer hardware (D59).
- The 72-hour soak (M10): replaced by everyday use;
  `--soak` stays for chasing a suspected leak.
