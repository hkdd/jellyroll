//! Platform boundary: display, GL context, input events, vsync/swap.
//!
//! This crate (together with the future `video` crate) is the only place
//! `unsafe` is permitted. Everything above it talks to [`Platform`], so the
//! SDL3 KMSDRM backend can later be swapped for a native DRM/GBM/EGL backend
//! (spec 5.3) without touching the app.
//!
//! The app is generic over `P: Platform` (static dispatch in the render path).

pub mod alloc_counter;
pub mod cec;
mod console;
pub mod drm_hdr;
pub mod gl;
pub mod hdmi_audio;
mod hotplug;
pub mod pad;
mod sdl;

use std::time::Duration;

pub use gl::{Gl, GpuTimer};
pub use sdl::{SdlOptions, SdlPlatform, print_sdl_diagnostics};

/// Logical keys the app cares about. Platform keycodes are mapped here so the
/// app never sees SDL types.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
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

/// Gamepad buttons by position (SDL's layout: South is the bottom face
/// button, A on Xbox pads). The left stick and the triggers are turned
/// into buttons by [`pad::AnalogState`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum PadButton {
    South,
    East,
    West,
    North,
    Back,
    Guide,
    Start,
    LeftShoulder,
    RightShoulder,
    LeftTrigger,
    RightTrigger,
    DPadUp,
    DPadDown,
    DPadLeft,
    DPadRight,
    StickUp,
    StickDown,
    StickLeft,
    StickRight,
}

/// HDMI-CEC remote keys ("UI command" codes, CEC 1.4 / linux/cec.h).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum CecKey {
    Select,
    Up,
    Down,
    Left,
    Right,
    RootMenu,
    SetupMenu,
    ContentsMenu,
    Back,
    Number(u8),
    Enter,
    ChannelUp,
    ChannelDown,
    Info,
    Guide,
    VolumeUp,
    VolumeDown,
    Mute,
    Play,
    Stop,
    Pause,
    Rewind,
    FastForward,
    SkipForward,
    SkipBackward,
    PlayPause,
    Blue,
    Red,
    Green,
    Yellow,
    Other(u8),
}

impl CecKey {
    pub fn from_ui_command(code: u8) -> Self {
        use CecKey as K;
        match code {
            0x00 => K::Select,
            0x01 => K::Up,
            0x02 => K::Down,
            0x03 => K::Left,
            0x04 => K::Right,
            0x09 => K::RootMenu,
            0x0a => K::SetupMenu,
            0x0b => K::ContentsMenu,
            0x0d => K::Back,
            0x20..=0x29 => K::Number(code - 0x20),
            0x2b => K::Enter,
            0x30 => K::ChannelUp,
            0x31 => K::ChannelDown,
            0x35 => K::Info,
            0x53 => K::Guide,
            0x41 => K::VolumeUp,
            0x42 => K::VolumeDown,
            0x43 => K::Mute,
            0x44 | 0x60 => K::Play,
            0x45 | 0x64 => K::Stop,
            0x46 => K::Pause,
            0x48 => K::Rewind,
            0x49 => K::FastForward,
            0x4b => K::SkipForward,
            0x4c => K::SkipBackward,
            0x61 => K::PlayPause,
            0x71 => K::Blue,
            0x72 => K::Red,
            0x73 => K::Green,
            0x74 => K::Yellow,
            other => K::Other(other),
        }
    }
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
    /// A typed character, with the keyboard layout and Shift applied. Only
    /// sent while text input is on ([`Platform::set_text_input`]).
    Text {
        ch: char,
    },
    PadDown {
        button: PadButton,
    },
    PadUp {
        button: PadButton,
    },
    /// CEC key pressed. Repeated "pressed" messages while held are folded
    /// into one press; release comes from "released" or a hold timeout.
    CecDown {
        key: CecKey,
    },
    CecUp {
        key: CecKey,
    },
    /// The TV asked all devices to go to standby (spec 5.4: pause playback).
    CecStandby,
    /// The display was disconnected (TV off, cable out) or is back (spec
    /// 5.2). Not sent in a desktop window.
    OutputConnected {
        connected: bool,
    },
    /// An HDMI audio sink (TV, receiver) went away, came back or
    /// renegotiated; an open audio stream may need reopening.
    AudioSinkChanged,
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
    /// The output mode now: one set with [`Platform::set_mode`], else the
    /// desktop mode.
    fn current_mode(&self) -> Option<ModeInfo>;
    /// The mode the output had at startup (the TV's preferred mode).
    fn desktop_mode(&self) -> Option<ModeInfo>;
    /// Switches the output to `mode` (one of [`Platform::display_modes`]),
    /// or back to the desktop mode with None. Takes effect at the next
    /// swap. Fails in a desktop window.
    fn set_mode(&mut self, mode: Option<&ModeInfo>) -> Result<(), String>;
    /// Sets the current mode again: a full modeset at the next swap, for a
    /// display that came back (spec 5.2: re-modeset and continue).
    fn reapply_mode(&mut self) -> Result<(), String>;
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

    /// Starts gamepad and HDMI-CEC support. Called once, after the first
    /// frame, so their setup never delays it (spec 2: first frame < 500 ms).
    fn start_input_devices(&mut self);

    /// Turns typed-character events ([`PlatformEvent::Text`]) on or off,
    /// for text fields.
    fn set_text_input(&mut self, on: bool);

    /// HDR10 passthrough (M9): `Some` sends the metadata to the TV and
    /// switches it into HDR; `None` back to SDR. The TV re-syncs either
    /// way. Only on KMSDRM with a connector that supports it.
    fn set_hdr(&mut self, meta: Option<&drm_hdr::HdrMetadata>) -> Result<(), String> {
        match meta {
            Some(_) => Err("HDR passthrough needs the KMSDRM backend".into()),
            None => Ok(()),
        }
    }

    /// What the connected TV says it supports; None if not known.
    fn hdr_caps(&self) -> Option<drm_hdr::HdrCaps> {
        None
    }
}
