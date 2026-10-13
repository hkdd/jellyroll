//! Video playback through libmpv's render API (spec 7), drawing into the
//! app's GL context. This crate and `jellyroll-platform` are the only places
//! `unsafe` is allowed; everything here is a thin, documented FFI boundary.
//!
//! Threading: mpv calls the wakeup and render-update callbacks from its own
//! threads. They only set a flag and call the platform waker; all work
//! (events, rendering) happens on the render thread (spec 4).
//!
//! The render thread must never wait for mpv's core: the core can be
//! blocked waiting for the renderer (e.g. during a video reconfig), so a
//! synchronous `mpv_get_property` or `mpv_command` there deadlocks
//! (render.h, "Threading"). So this wrapper only uses non-blocking calls:
//! every value it reports is an observed property delivered as an event,
//! and every command is `mpv_command_async`. There is deliberately no
//! synchronous getter.

mod ffi;
pub mod va;

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ffi::*;

/// Called from mpv threads to wake the render loop.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Debug)]
pub struct PlayerOptions {
    /// mpv `hwdec` list. Default: [`default_hwdec`]. Software decode is
    /// visible in `hwdec-current` and reported, never silent.
    pub hwdec: String,
    /// `video-sync`; `display-resample` for smooth playback (spec 7.1).
    pub video_sync: String,
    /// Demuxer cache caps (spec 7.1).
    pub cache_max_bytes: String,
    pub cache_max_back_bytes: String,
    /// Minimum mpv log level forwarded to tracing.
    pub log_level: String,
    /// DRM render node for VA-API interop (e.g. /dev/dri/renderD128). On a
    /// bare console there is no X11/Wayland display for mpv to get a
    /// VA-API display from, so without this hwdec silently falls back to
    /// software. Normally the node found by [`va::probe`].
    pub render_node: Option<std::path::PathBuf>,
}

impl Default for PlayerOptions {
    fn default() -> Self {
        Self {
            hwdec: default_hwdec(),
            video_sync: "display-resample".into(),
            cache_max_bytes: "150MiB".into(),
            cache_max_back_bytes: "50MiB".into(),
            log_level: "info".into(),
            render_node: None,
        }
    }
}

/// Hardware decoders to try, in order (spec 7.1): VA-API first (Intel and
/// AMD on Mesa); NVIDIA's NVDEC only when the NVIDIA kernel driver is
/// loaded, so other machines don't probe CUDA/VDPAU and log errors.
pub fn default_hwdec() -> String {
    if nvidia_driver() { "vaapi,nvdec".into() } else { "vaapi".into() }
}

fn nvidia_driver() -> bool {
    std::path::Path::new("/proc/driver/nvidia/version").exists()
}

/// GPU interop mpv loads for hardware frames: only VA-API's (EGL/DMA-BUF,
/// zero-copy) unless the NVIDIA driver is present. "auto" would also load
/// the DRM overlay-plane and CUDA interops, which need resources we don't
/// provide (spec 7.4 is later) and log errors.
pub fn default_hwdec_interop() -> String {
    if nvidia_driver() { "auto".into() } else { "vaapi".into() }
}

/// Playback state kept current from mpv property-change events.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlaybackState {
    pub time_pos: f64,
    pub duration: f64,
    pub paused: bool,
    /// Active hardware decoder (`hwdec-current`); "no" or empty = software.
    pub hwdec: String,
    pub video_codec: String,
    pub width: i64,
    pub height: i64,
    pub fps: f64,
    pub audio_codec: String,
    pub file_loaded: bool,
    pub drops: FrameDrops,
}

impl PlaybackState {
    pub fn software_decode(&self) -> bool {
        self.file_loaded && (self.hwdec.is_empty() || self.hwdec == "no")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PlayerEvent {
    FileLoaded,
    /// Video format known/changed (codec, size, decoder now valid).
    VideoReconfig,
    PlaybackRestart,
    EndFile {
        reason: EndReason,
        error: Option<String>,
    },
    Shutdown,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EndReason {
    Eof,
    Stop,
    Quit,
    Error,
    Other,
}

/// Frame statistics from mpv (spec 13 HUD and the playback benchmark).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct FrameDrops {
    /// Frames dropped by the video output / sync (`frame-drop-count`).
    pub vo: i64,
    /// Frames dropped by the decoder (`decoder-frame-drop-count`).
    pub decoder: i64,
    /// Frames shown late (`vo-delayed-frame-count`).
    pub delayed: i64,
    /// Frames whose display time was off (`mistimed-frame-count`).
    pub mistimed: i64,
}

struct Shared {
    events: AtomicBool,
    frame: AtomicBool,
    wake: Waker,
}

/// Owns the GL symbol loader for mpv's lifetime.
struct ProcLoader(Box<dyn Fn(&str) -> *const c_void>);

// Observed property ids (reply_userdata).
const OBS_TIME_POS: u64 = 1;
const OBS_DURATION: u64 = 2;
const OBS_PAUSE: u64 = 3;
const OBS_HWDEC: u64 = 4;
const OBS_VIDEO_CODEC: u64 = 5;
const OBS_AUDIO_CODEC: u64 = 6;
const OBS_WIDTH: u64 = 7;
const OBS_HEIGHT: u64 = 8;
const OBS_FPS: u64 = 9;
const OBS_DROP_VO: u64 = 10;
const OBS_DROP_DECODER: u64 = 11;
const OBS_DELAYED: u64 = 12;
const OBS_MISTIMED: u64 = 13;

const OBSERVED: [(u64, &str, c_int); 13] = [
    (OBS_TIME_POS, "time-pos", MPV_FORMAT_DOUBLE),
    (OBS_DURATION, "duration", MPV_FORMAT_DOUBLE),
    (OBS_PAUSE, "pause", MPV_FORMAT_FLAG),
    (OBS_HWDEC, "hwdec-current", MPV_FORMAT_STRING),
    (OBS_VIDEO_CODEC, "video-format", MPV_FORMAT_STRING),
    (OBS_AUDIO_CODEC, "audio-codec-name", MPV_FORMAT_STRING),
    (OBS_WIDTH, "width", MPV_FORMAT_INT64),
    (OBS_HEIGHT, "height", MPV_FORMAT_INT64),
    (OBS_FPS, "container-fps", MPV_FORMAT_DOUBLE),
    (OBS_DROP_VO, "frame-drop-count", MPV_FORMAT_INT64),
    (OBS_DROP_DECODER, "decoder-frame-drop-count", MPV_FORMAT_INT64),
    (OBS_DELAYED, "vo-delayed-frame-count", MPV_FORMAT_INT64),
    (OBS_MISTIMED, "mistimed-frame-count", MPV_FORMAT_INT64),
];

/// mpv's built-in scripts and UI features, disabled explicitly
/// (`load-scripts=no` only covers user scripts). Names vary across mpv
/// versions, so unknown ones are ignored.
const BUILTINS_OFF: [&str; 9] = [
    "load-stats-overlay",
    "load-osd-console",
    "load-console",
    "load-select",
    "load-commands",
    "load-positioning",
    "load-context-menu",
    "load-auto-profiles",
    "ytdl",
];

pub struct Player {
    mpv: *mut mpv_handle,
    render: *mut mpv_render_context,
    shared: Box<Shared>,
    _loader: Box<ProcLoader>,
    // mpv may keep pointers to these for the render context's lifetime.
    _drm_params: Box<mpv_opengl_drm_params_v2>,
    _render_node: Option<std::fs::File>,
    pub state: PlaybackState,
}

fn cstr(s: &str) -> CString {
    CString::new(s).unwrap_or_default()
}

fn err(code: c_int) -> String {
    // SAFETY: mpv_error_string returns a static string for any code.
    unsafe { CStr::from_ptr(mpv_error_string(code)).to_string_lossy().into_owned() }
}

/// SAFETY contract: `d` is the `Shared` boxed by `Player`, which outlives
/// the mpv handle and render context that call this.
unsafe extern "C" fn on_wakeup(d: *mut c_void) {
    // SAFETY: see function contract.
    let s = unsafe { &*(d as *const Shared) };
    s.events.store(true, Ordering::Release);
    (s.wake)();
}

/// SAFETY contract: as [`on_wakeup`].
unsafe extern "C" fn on_render_update(d: *mut c_void) {
    // SAFETY: see function contract.
    let s = unsafe { &*(d as *const Shared) };
    s.frame.store(true, Ordering::Release);
    (s.wake)();
}

/// SAFETY contract: `ctx` is the boxed `ProcLoader` owned by `Player`;
/// `name` is a NUL-terminated string from mpv.
unsafe extern "C" fn get_proc_address(ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    // SAFETY: see function contract.
    let (loader, name) = unsafe { (&*(ctx as *const ProcLoader), CStr::from_ptr(name)) };
    match name.to_str() {
        Ok(n) => (loader.0)(n) as *mut c_void,
        Err(_) => std::ptr::null_mut(),
    }
}

/// libmpv client API version as (major, minor).
pub fn client_api_version() -> (u32, u32) {
    // SAFETY: no preconditions.
    let v = unsafe { mpv_client_api_version() } as u64;
    ((v >> 16) as u32, (v & 0xffff) as u32)
}

impl Player {
    /// Creates mpv and its GL renderer on the current GL context (render
    /// thread). `load` resolves GL/EGL symbols; `wake` must be cheap and
    /// thread-safe.
    pub fn new(
        load: impl Fn(&str) -> *const c_void + 'static,
        wake: Waker,
        opts: &PlayerOptions,
    ) -> Result<Self, String> {
        // SAFETY: mpv_create has no preconditions; null means failure.
        let mpv = unsafe { mpv_create() };
        if mpv.is_null() {
            return Err("mpv_create failed".into());
        }
        let shared = Box::new(Shared {
            events: AtomicBool::new(false),
            frame: AtomicBool::new(false),
            wake,
        });
        let loader = Box::new(ProcLoader(Box::new(load)));
        let render_node = match &opts.render_node {
            Some(path) => match std::fs::File::open(path) {
                Ok(f) => Some(f),
                Err(e) => {
                    tracing::warn!("cannot open {} for VA-API interop: {e}", path.display());
                    None
                }
            },
            None => None,
        };
        let drm_params = Box::new(mpv_opengl_drm_params_v2 {
            fd: -1,
            crtc_id: 0,
            connector_id: 0,
            atomic_request_ptr: std::ptr::null_mut(),
            render_fd: render_node.as_ref().map_or(-1, |f| {
                use std::os::fd::AsRawFd;
                f.as_raw_fd()
            }),
        });
        let mut player = Self {
            mpv,
            render: std::ptr::null_mut(),
            shared,
            _loader: loader,
            _drm_params: drm_params,
            _render_node: render_node,
            state: PlaybackState::default(),
        };

        // Spec 7.1: configured through the API only; no config files,
        // scripts, OSC or default key bindings. The app draws its own OSD.
        let options: [(&str, &str); 19] = [
            ("config", "no"),
            ("load-scripts", "no"),
            ("osc", "no"),
            ("input-default-bindings", "no"),
            ("input-vo-keyboard", "no"),
            ("terminal", "no"),
            ("vo", "libmpv"),
            ("hwdec", &opts.hwdec),
            ("gpu-hwdec-interop", &default_hwdec_interop()),
            ("video-sync", &opts.video_sync),
            ("interpolation", "no"),
            ("cache", "yes"),
            ("demuxer-max-bytes", &opts.cache_max_bytes),
            ("demuxer-max-back-bytes", &opts.cache_max_back_bytes),
            ("sub-auto", "no"),
            ("keep-open", "no"),
            ("idle", "yes"),
            ("audio-display", "no"),
            // A missing audio device (TV or receiver off, HDMI unplugged)
            // must not stop video (spec 12).
            ("audio-fallback-to-null", "yes"),
        ];
        for (k, v) in options {
            player.set_option(k, v)?;
        }
        for k in BUILTINS_OFF {
            if let Err(e) = player.set_option(k, "no") {
                tracing::debug!("{e} (not in this mpv version)");
            }
        }

        // SAFETY: handle valid; options set before initialize as required.
        let rc = unsafe { mpv_initialize(mpv) };
        if rc < 0 {
            return Err(format!("mpv_initialize: {}", err(rc)));
        }
        let level = cstr(&opts.log_level);
        // SAFETY: handle valid; level is NUL-terminated.
        unsafe { mpv_request_log_messages(mpv, level.as_ptr()) };
        for (id, name, fmt) in OBSERVED {
            let n = cstr(name);
            // SAFETY: handle valid; name NUL-terminated (mpv copies it).
            unsafe { mpv_observe_property(mpv, id, n.as_ptr(), fmt) };
        }

        let mut init = mpv_opengl_init_params {
            get_proc_address: Some(get_proc_address),
            get_proc_address_ctx: &*player._loader as *const ProcLoader as *mut c_void,
        };
        let mut advanced: c_int = 1;
        let mut params = vec![
            mpv_render_param {
                type_: MPV_RENDER_PARAM_API_TYPE,
                data: MPV_RENDER_API_TYPE_OPENGL.as_ptr() as *mut c_void,
            },
            mpv_render_param {
                type_: MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                data: &mut init as *mut _ as *mut c_void,
            },
            // Advanced control: mpv may block in render() until the frame's
            // display time and expects report_swap; required for
            // display-resample timing.
            mpv_render_param {
                type_: MPV_RENDER_PARAM_ADVANCED_CONTROL,
                data: &mut advanced as *mut _ as *mut c_void,
            },
        ];
        // Render node for VA-API interop (see PlayerOptions::render_node).
        if player._drm_params.render_fd >= 0 {
            params.push(mpv_render_param {
                type_: MPV_RENDER_PARAM_DRM_DISPLAY_V2,
                data: &*player._drm_params as *const _ as *mut c_void,
            });
        }
        params
            .push(mpv_render_param { type_: MPV_RENDER_PARAM_INVALID, data: std::ptr::null_mut() });
        let mut render: *mut mpv_render_context = std::ptr::null_mut();
        // SAFETY: GL context current on this thread (caller contract);
        // params are valid for the call; the loader box outlives the context.
        let rc = unsafe { mpv_render_context_create(&mut render, mpv, params.as_mut_ptr()) };
        if rc < 0 {
            return Err(format!("mpv render context: {}", err(rc)));
        }
        player.render = render;

        let d = &*player.shared as *const Shared as *mut c_void;
        // SAFETY: `shared` is boxed and outlives both registrations; the
        // callbacks only touch atomics and the waker.
        unsafe {
            mpv_set_wakeup_callback(mpv, Some(on_wakeup), d);
            mpv_render_context_set_update_callback(render, Some(on_render_update), d);
        }
        Ok(player)
    }

    fn set_option(&mut self, k: &str, v: &str) -> Result<(), String> {
        let (k, v) = (cstr(k), cstr(v));
        // SAFETY: handle valid; strings NUL-terminated, copied by mpv.
        let rc = unsafe { mpv_set_option_string(self.mpv, k.as_ptr(), v.as_ptr()) };
        if rc < 0 {
            return Err(format!(
                "mpv option {}={}: {}",
                k.to_string_lossy(),
                v.to_string_lossy(),
                err(rc)
            ));
        }
        Ok(())
    }

    /// Queues an mpv command (e.g. `["loadfile", url]`) without waiting;
    /// results arrive as events.
    pub fn command(&mut self, args: &[&str]) -> Result<(), String> {
        let owned: Vec<CString> = args.iter().map(|a| cstr(a)).collect();
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        // SAFETY: NULL-terminated array of NUL-terminated strings that
        // outlive the call (mpv copies them); never waits for the core.
        let rc = unsafe { mpv_command_async(self.mpv, 0, ptrs.as_mut_ptr()) };
        if rc < 0 {
            return Err(format!("mpv {:?}: {}", args.first(), err(rc)));
        }
        Ok(())
    }

    pub fn load(&mut self, url: &str) -> Result<(), String> {
        self.state = PlaybackState::default();
        self.command(&["loadfile", url])
    }

    pub fn toggle_pause(&mut self) {
        let _ = self.command(&["cycle", "pause"]);
    }

    pub fn seek_relative(&mut self, secs: f64) {
        let s = format!("{secs}");
        let _ = self.command(&["seek", &s, "relative"]);
    }

    pub fn stop(&mut self) {
        let _ = self.command(&["stop"]);
    }

    /// Latest dropped/late frame counters (from observed properties).
    pub fn frame_drops(&self) -> FrameDrops {
        self.state.drops
    }

    /// Drains mpv's event queue into `out` and updates [`Self::state`].
    /// Cheap when nothing is pending (one atomic load).
    pub fn poll_events(&mut self, out: &mut Vec<PlayerEvent>) {
        if !self.shared.events.swap(false, Ordering::AcqRel) {
            return;
        }
        loop {
            // SAFETY: handle valid; timeout 0 never blocks. The returned
            // event is owned by mpv and valid until the next wait_event.
            let ev = unsafe { &*mpv_wait_event(self.mpv, 0.0) };
            match ev.event_id {
                MPV_EVENT_NONE => break,
                MPV_EVENT_SHUTDOWN => out.push(PlayerEvent::Shutdown),
                MPV_EVENT_LOG_MESSAGE => {
                    // SAFETY: data points to mpv_event_log_message for this id.
                    let m = unsafe { &*(ev.data as *const mpv_event_log_message) };
                    // SAFETY: strings are NUL-terminated and valid until the
                    // next wait_event.
                    let (prefix, level, text) = unsafe {
                        (CStr::from_ptr(m.prefix), CStr::from_ptr(m.level), CStr::from_ptr(m.text))
                    };
                    let text = text.to_string_lossy();
                    let text = text.trim_end();
                    let prefix = prefix.to_string_lossy();
                    match level.to_bytes() {
                        b"fatal" | b"error" => tracing::error!(target: "mpv", "[{prefix}] {text}"),
                        b"warn" => tracing::warn!(target: "mpv", "[{prefix}] {text}"),
                        b"info" => tracing::info!(target: "mpv", "[{prefix}] {text}"),
                        _ => tracing::debug!(target: "mpv", "[{prefix}] {text}"),
                    }
                }
                MPV_EVENT_START_FILE => self.state.file_loaded = false,
                MPV_EVENT_COMMAND_REPLY if ev.error < 0 => {
                    tracing::warn!("mpv command failed: {}", err(ev.error));
                }
                MPV_EVENT_FILE_LOADED => {
                    self.state.file_loaded = true;
                    out.push(PlayerEvent::FileLoaded);
                }
                MPV_EVENT_VIDEO_RECONFIG => out.push(PlayerEvent::VideoReconfig),
                MPV_EVENT_PLAYBACK_RESTART => out.push(PlayerEvent::PlaybackRestart),
                MPV_EVENT_END_FILE => {
                    // SAFETY: data points to mpv_event_end_file for this id.
                    let e = unsafe { &*(ev.data as *const mpv_event_end_file) };
                    let reason = match e.reason {
                        MPV_END_FILE_REASON_EOF => EndReason::Eof,
                        MPV_END_FILE_REASON_STOP => EndReason::Stop,
                        MPV_END_FILE_REASON_QUIT => EndReason::Quit,
                        MPV_END_FILE_REASON_ERROR => EndReason::Error,
                        _ => EndReason::Other,
                    };
                    let error = (reason == EndReason::Error).then(|| err(e.error));
                    self.state.file_loaded = false;
                    out.push(PlayerEvent::EndFile { reason, error });
                }
                MPV_EVENT_PROPERTY_CHANGE => {
                    // SAFETY: data points to mpv_event_property for this id.
                    let p = unsafe { &*(ev.data as *const mpv_event_property) };
                    if p.format == MPV_FORMAT_NONE || p.data.is_null() {
                        continue;
                    }
                    // SAFETY: `data` matches the format each id was observed
                    // with (OBSERVED); strings are `char*` valid until the
                    // next wait_event.
                    unsafe {
                        let f64v = || *(p.data as *const f64);
                        let i64v = || *(p.data as *const i64);
                        let strv = || {
                            let s = *(p.data as *const *const c_char);
                            if s.is_null() {
                                String::new()
                            } else {
                                CStr::from_ptr(s).to_string_lossy().into_owned()
                            }
                        };
                        let st = &mut self.state;
                        match ev.reply_userdata {
                            OBS_TIME_POS => st.time_pos = f64v(),
                            OBS_DURATION => st.duration = f64v(),
                            OBS_PAUSE => st.paused = *(p.data as *const c_int) != 0,
                            OBS_HWDEC => st.hwdec = strv(),
                            OBS_VIDEO_CODEC => st.video_codec = strv(),
                            OBS_AUDIO_CODEC => st.audio_codec = strv(),
                            OBS_WIDTH => st.width = i64v(),
                            OBS_HEIGHT => st.height = i64v(),
                            OBS_FPS => st.fps = f64v(),
                            OBS_DROP_VO => st.drops.vo = i64v(),
                            OBS_DROP_DECODER => st.drops.decoder = i64v(),
                            OBS_DELAYED => st.drops.delayed = i64v(),
                            OBS_MISTIMED => st.drops.mistimed = i64v(),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// True if mpv has a new frame to show (consumes the update signal).
    pub fn frame_ready(&mut self) -> bool {
        if !self.shared.frame.swap(false, Ordering::AcqRel) {
            return false;
        }
        // SAFETY: render context valid; called on the render thread.
        let flags = unsafe { mpv_render_context_update(self.render) };
        flags & MPV_RENDER_UPDATE_FRAME != 0
    }

    /// Renders the current video frame into framebuffer 0. The caller must
    /// have put GL state at defaults (see `Gl::reset_to_defaults`) and must
    /// invalidate other renderers' GL state cache afterwards (spec 6.3).
    pub fn render(&mut self, fb: (u32, u32)) {
        let mut fbo =
            mpv_opengl_fbo { fbo: 0, w: fb.0 as c_int, h: fb.1 as c_int, internal_format: 0 };
        // The default framebuffer is bottom-up.
        let mut flip: c_int = 1;
        let mut params = [
            mpv_render_param {
                type_: MPV_RENDER_PARAM_OPENGL_FBO,
                data: &mut fbo as *mut _ as *mut c_void,
            },
            mpv_render_param {
                type_: MPV_RENDER_PARAM_FLIP_Y,
                data: &mut flip as *mut _ as *mut c_void,
            },
            mpv_render_param { type_: MPV_RENDER_PARAM_INVALID, data: std::ptr::null_mut() },
        ];
        // SAFETY: render context valid; GL context current; params valid
        // for the call.
        let rc = unsafe { mpv_render_context_render(self.render, params.as_mut_ptr()) };
        if rc < 0 {
            tracing::warn!("mpv render: {}", err(rc));
        }
    }

    /// Tells mpv the frame was presented (after swap), for timing.
    pub fn report_swap(&mut self) {
        // SAFETY: render context valid.
        unsafe { mpv_render_context_report_swap(self.render) };
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // SAFETY: the render context must be freed before the handle, on
        // the thread with the GL context current; both pointers are owned
        // by this Player. The callbacks' data (`shared`) is dropped after.
        unsafe {
            if !self.render.is_null() {
                mpv_render_context_free(self.render);
                self.render = std::ptr::null_mut();
            }
            mpv_terminate_destroy(self.mpv);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_version_is_v2() {
        let (major, minor) = client_api_version();
        assert_eq!(major, 2, "unexpected libmpv client API {major}.{minor}");
    }

    #[test]
    fn software_decode_detection() {
        let mut s = PlaybackState::default();
        assert!(!s.software_decode(), "nothing loaded yet");
        s.file_loaded = true;
        s.hwdec = "no".into();
        assert!(s.software_decode());
        s.hwdec = "vaapi".into();
        assert!(!s.software_decode());
    }
}
