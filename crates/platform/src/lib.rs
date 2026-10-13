//! Platform boundary: display, GL context, input events, vsync/swap.
//!
//! This crate (together with the future `video` crate) is the only place
//! `unsafe` is permitted. Everything above it talks to [`Platform`], so the
//! SDL3 KMSDRM backend can later be swapped for a native DRM/GBM/EGL backend
//! (spec 5.3) without touching the app.
//!
//! The app is generic over `P: Platform` (static dispatch in the render path).

pub mod alloc_counter;
pub mod gl;
mod sdl;

use std::time::Duration;

pub use gl::{Gl, GpuTimer};
pub use sdl::{SdlOptions, SdlPlatform, print_sdl_diagnostics};

/// Logical keys the app cares about. Platform keycodes are mapped here so the
/// app never sees SDL types.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Escape,
    Backspace,
    Space,
    Tab,
    Home,
    Menu,
    Search,
    Back,
    F(u8),
    Char(char),
    MediaPlayPause,
    MediaPlay,
    MediaPause,
    MediaStop,
    MediaNext,
    MediaPrev,
    MediaFastForward,
    MediaRewind,
    VolumeUp,
    VolumeDown,
    Mute,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum PlatformEvent {
    Quit,
    /// `repeat` is the OS auto-repeat flag. The app ignores OS repeats and
    /// runs its own repeat timer (spec 5.4) so every input source behaves
    /// the same.
    KeyDown {
        key: Key,
        mods: Modifiers,
        repeat: bool,
    },
    KeyUp {
        key: Key,
    },
    /// Framebuffer size changed (pixels).
    Resized {
        width: u32,
        height: u32,
    },
    /// Contents lost/need repaint (e.g. VT switch back, window uncovered).
    Exposed,
    /// Mode or connector changed; refresh rate may differ.
    DisplayChanged,
    /// Another thread asked the render loop to wake up (see
    /// [`Platform::waker`]), e.g. mpv has events or a new video frame.
    Wake,
}

#[derive(Clone, Debug)]
pub struct ModeInfo {
    pub width: i32,
    pub height: i32,
    pub refresh_hz: f32,
    pub refresh_num: i32,
    pub refresh_den: i32,
}

#[derive(Clone, Debug, Default)]
pub struct PlatformInfo {
    pub backend: &'static str,
    pub video_driver: String,
    pub sdl_version: String,
    pub gl_vendor: String,
    pub gl_renderer: String,
    pub gl_version: String,
    pub glsl_version: String,
    pub is_gles: bool,
    pub gpu_timer_queries: bool,
    pub swap_interval: i32,
}

pub trait Platform {
    fn gl(&self) -> &Gl;
    /// Resolves a GL/EGL symbol for the current context (null if missing).
    /// Used by Skia (and later mpv) to load their own entry points.
    fn gl_proc_address(&self, name: &str) -> *const std::ffi::c_void;
    /// An owned GL/EGL symbol resolver usable for the life of the process
    /// (mpv keeps its loader).
    fn gl_loader(&self) -> Box<dyn Fn(&str) -> *const std::ffi::c_void>;
    fn info(&self) -> &PlatformInfo;
    /// Drawable size in pixels.
    fn framebuffer_size(&self) -> (u32, u32);
    /// Refresh rate of the current output mode (Hz), 0.0 if unknown.
    fn refresh_hz(&self) -> f32;
    fn current_mode(&self) -> Option<ModeInfo>;
    fn display_modes(&self) -> Vec<ModeInfo>;
    /// Appends all pending events without blocking.
    fn poll_events(&mut self, out: &mut Vec<PlatformEvent>);
    /// Blocks until at least one event arrives or `timeout` elapses, then
    /// drains the queue into `out`.
    fn wait_events(&mut self, timeout: Duration, out: &mut Vec<PlatformEvent>);
    /// Presents the back buffer. With swap interval 1 this paces to vblank.
    fn swap(&mut self);
    /// A thread-safe handle that makes `wait_events` return with
    /// [`PlatformEvent::Wake`]. Cheap to call from any thread, including
    /// C callbacks; it only queues an event.
    fn waker(&self) -> std::sync::Arc<dyn Fn() + Send + Sync>;
}
