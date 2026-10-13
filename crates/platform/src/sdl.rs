//! SDL3 backend. Production uses the KMSDRM video driver (no display server);
//! dev mode lets SDL pick Wayland/X11 so the same code runs in a window.

use std::ffi::c_void;
use std::time::Duration;

use sdl3::event::{Event, WindowEvent};
use sdl3::keyboard::{Keycode, Mod};
use sdl3::video::{GLContext, GLProfile, SwapInterval, Window};

use crate::gl::Gl;
use crate::pad::{Analog, AnalogState};
use crate::{Key, ModeInfo, Modifiers, PadButton, Platform, PlatformEvent, PlatformInfo};

#[derive(Clone, Debug, Default)]
pub struct SdlOptions {
    /// Run in a desktop window (Wayland/X11/cage) instead of KMSDRM.
    pub dev_window: bool,
    /// Request double instead of triple buffering on KMSDRM.
    pub double_buffer: bool,
    /// Disable vsync (benchmarking only).
    pub no_vsync: bool,
    /// Dev window size in pixels (default 1280x720).
    pub window_size: Option<(u32, u32)>,
    /// HDMI-CEC remote input; `None` disables it.
    pub cec: Option<crate::cec::CecOptions>,
}

/// An open gamepad and its analog-to-button state.
struct Pad {
    id: sdl3::joystick::JoystickId,
    _pad: sdl3::gamepad::Gamepad,
    analog: AnalogState,
}

/// Gamepad hotplug and input. Separate from the window so it can be tested
/// with SDL's virtual gamepads.
pub(crate) struct Gamepads {
    sub: sdl3::GamepadSubsystem,
    pads: Vec<Pad>,
}

impl Gamepads {
    pub(crate) fn new(sub: sdl3::GamepadSubsystem) -> Self {
        Self { sub, pads: Vec::new() }
    }

    /// Handles gamepad events; returns the event back if it is not one.
    pub(crate) fn handle(&mut self, ev: Event, out: &mut Vec<PlatformEvent>) -> Option<Event> {
        match ev {
            Event::GamepadAdded { which, .. } => {
                if self.pads.iter().any(|p| p.id == which) {
                    return None;
                }
                match self.sub.open(which) {
                    Ok(pad) => {
                        let name = pad.name().unwrap_or_else(|| "unknown".into());
                        tracing::info!(name, "gamepad connected");
                        self.pads.push(Pad {
                            id: which,
                            _pad: pad,
                            analog: AnalogState::default(),
                        });
                    }
                    Err(e) => tracing::warn!("cannot open gamepad: {e}"),
                }
            }
            Event::GamepadRemoved { which, .. } => {
                if let Some(i) = self.pads.iter().position(|p| p.id == which) {
                    self.pads.swap_remove(i);
                    tracing::info!("gamepad disconnected");
                }
            }
            Event::GamepadButtonDown { button, .. } => {
                if let Some(button) = map_pad_button(button) {
                    out.push(PlatformEvent::PadDown { button });
                }
            }
            Event::GamepadButtonUp { button, .. } => {
                if let Some(button) = map_pad_button(button) {
                    out.push(PlatformEvent::PadUp { button });
                }
            }
            Event::GamepadAxisMotion { which, axis, value, .. } => {
                let (axis, v) = map_analog(axis, value)?;
                let pad = self.pads.iter_mut().find(|p| p.id == which)?;
                for (button, down) in pad.analog.update(axis, v).into_iter().flatten() {
                    out.push(if down {
                        PlatformEvent::PadDown { button }
                    } else {
                        PlatformEvent::PadUp { button }
                    });
                }
            }
            other => return Some(other),
        }
        None
    }
}

pub struct SdlPlatform {
    // Drop order matters: context before window before subsystems.
    _gl_ctx: GLContext,
    window: Window,
    video: sdl3::VideoSubsystem,
    gamepads: Option<Gamepads>,
    cec: Option<(crate::cec::Cec, std::sync::mpsc::Receiver<PlatformEvent>)>,
    pump: sdl3::EventPump,
    _sdl: sdl3::Sdl,
    gl: Gl,
    info: PlatformInfo,
    /// SDL user event type used by the waker.
    wake_type: u32,
}

fn mode_info(m: &sdl3::video::DisplayMode) -> ModeInfo {
    ModeInfo {
        width: m.w,
        height: m.h,
        refresh_hz: m.refresh_rate,
        refresh_num: m.refresh_rate_numerator,
        refresh_den: m.refresh_rate_denominator,
    }
}

fn sdl_version() -> String {
    let v = sdl3::version::version();
    format!("{}.{}.{}", v.major, v.minor, v.patch)
}

fn init(opts: &SdlOptions) -> Result<(sdl3::Sdl, sdl3::VideoSubsystem), String> {
    if !opts.dev_window {
        // Normal priority: an explicit SDL_VIDEO_DRIVER env var still wins,
        // which is how dev setups (e.g. under cage) override it.
        sdl3::hint::set("SDL_VIDEO_DRIVER", "kmsdrm");
    }
    if opts.double_buffer {
        sdl3::hint::set("SDL_VIDEO_DOUBLE_BUFFER", "1");
    }
    // We own the whole screen; never let SDL blank it.
    sdl3::hint::set("SDL_VIDEO_ALLOW_SCREENSAVER", "0");
    let sdl = sdl3::init().map_err(|e| format!("SDL_Init: {e}"))?;
    let video = sdl.video().map_err(|e| format!("SDL video init: {e}"))?;
    Ok((sdl, video))
}

impl SdlPlatform {
    pub fn new(opts: &SdlOptions) -> Result<Self, String> {
        let (sdl, video) = init(opts)?;
        let driver = video.current_video_driver().to_string();
        tracing::info!(driver, sdl = %sdl_version(), "SDL video initialized");

        let display = video.get_primary_display().map_err(|e| e.to_string())?;
        let desktop = display.get_mode().map_err(|e| e.to_string())?;

        // Prefer desktop GL 3.3 core (timer queries are core there); fall back
        // to GLES 3.0, which every Mesa/KMS driver provides.
        let attempts: [(GLProfile, u8, u8); 2] = [(GLProfile::Core, 3, 3), (GLProfile::GLES, 3, 0)];
        let mut last_err = String::new();
        for (profile, major, minor) in attempts {
            let attr = video.gl_attr();
            attr.set_context_profile(profile);
            attr.set_context_version(major, minor);
            attr.set_double_buffer(true);
            attr.set_depth_size(0);
            attr.set_stencil_size(8); // Skia (M2) wants a stencil buffer.
            attr.set_red_size(8);
            attr.set_green_size(8);
            attr.set_blue_size(8);

            let mut builder = if opts.dev_window {
                let (w, h) = opts.window_size.unwrap_or((1280, 720));
                let mut b = video.window("jellyroll (dev)", w, h);
                b.resizable();
                b
            } else {
                let mut b = video.window("jellyroll", desktop.w as u32, desktop.h as u32);
                b.fullscreen();
                b
            };
            let window = match builder.opengl().build() {
                Ok(w) => w,
                Err(e) => {
                    last_err = format!("window ({profile:?} {major}.{minor}): {e}");
                    tracing::warn!("{last_err}");
                    continue;
                }
            };
            let ctx = match window.gl_create_context() {
                Ok(c) => c,
                Err(e) => {
                    last_err = format!("GL context ({profile:?} {major}.{minor}): {e}");
                    tracing::warn!("{last_err}");
                    continue;
                }
            };
            window.gl_make_current(&ctx).map_err(|e| e.to_string())?;

            let is_gles = profile == GLProfile::GLES;
            let loader = |name: &str| -> *const c_void {
                match video.gl_get_proc_address(name) {
                    Some(f) => f as *const c_void,
                    None => std::ptr::null(),
                }
            };
            let has_ext = |name: &str| video.gl_extension_supported(name);
            let gl = Gl::load(&loader, is_gles, &has_ext)?;

            let interval =
                if opts.no_vsync { SwapInterval::Immediate } else { SwapInterval::VSync };
            if let Err(e) = video.gl_set_swap_interval(interval) {
                tracing::warn!("set swap interval: {e}");
            }
            let swap_interval = match video.gl_get_swap_interval() {
                Ok(SwapInterval::Immediate) => 0,
                Ok(SwapInterval::VSync) => 1,
                Ok(SwapInterval::LateSwapTearing) => -1,
                Err(_) => i32::MIN,
            };

            sdl.mouse().show_cursor(false);

            let info = PlatformInfo {
                backend: "sdl3",
                video_driver: driver,
                sdl_version: sdl_version(),
                gl_vendor: gl.vendor(),
                gl_renderer: gl.renderer(),
                gl_version: gl.version(),
                glsl_version: gl.glsl_version(),
                is_gles,
                gpu_timer_queries: gl.has_timer_queries(),
                swap_interval,
            };
            tracing::info!(
                vendor = %info.gl_vendor,
                renderer = %info.gl_renderer,
                version = %info.gl_version,
                timer_queries = info.gpu_timer_queries,
                swap_interval,
                "GL context ready"
            );
            let pump = sdl.event_pump().map_err(|e| e.to_string())?;
            // SAFETY: SDL is initialized; registers one user event type.
            let wake_type = unsafe { sdl3::sys::events::SDL_RegisterEvents(1) };
            if wake_type == 0 {
                return Err("SDL_RegisterEvents failed".into());
            }
            // Gamepads are optional: a missing subsystem only disables them.
            let gamepads = match sdl.gamepad() {
                Ok(g) => Some(Gamepads::new(g)),
                Err(e) => {
                    tracing::warn!("gamepad support unavailable: {e}");
                    None
                }
            };
            let mut platform = Self {
                _gl_ctx: ctx,
                window,
                video,
                gamepads,
                cec: None,
                pump,
                _sdl: sdl,
                gl,
                info,
                wake_type,
            };
            if let Some(cec_opts) = opts.cec.clone() {
                let (tx, rx) = std::sync::mpsc::channel();
                match crate::cec::start(cec_opts, tx, platform.waker()) {
                    Ok(cec) => {
                        tracing::info!(
                            device = %cec.info.device,
                            driver = %cec.info.driver,
                            "HDMI-CEC enabled"
                        );
                        platform.cec = Some((cec, rx));
                    }
                    Err(e) => tracing::info!("HDMI-CEC not available: {e}"),
                }
            }
            return Ok(platform);
        }
        Err(format!("no usable GL context: {last_err}"))
    }
}

fn map_key(k: Keycode) -> Option<Key> {
    use Keycode as K;
    Some(match k {
        K::Up => Key::Up,
        K::Down => Key::Down,
        K::Left => Key::Left,
        K::Right => Key::Right,
        K::Return | K::KpEnter => Key::Enter,
        K::Escape => Key::Escape,
        K::Backspace => Key::Backspace,
        K::Space => Key::Space,
        K::Tab => Key::Tab,
        K::Home | K::AcHome => Key::Home,
        K::Menu | K::Application => Key::Menu,
        K::AcSearch => Key::Search,
        K::AcBack => Key::Back,
        K::F1 => Key::F(1),
        K::F2 => Key::F(2),
        K::F3 => Key::F(3),
        K::F4 => Key::F(4),
        K::F5 => Key::F(5),
        K::F6 => Key::F(6),
        K::F7 => Key::F(7),
        K::F8 => Key::F(8),
        K::F9 => Key::F(9),
        K::F10 => Key::F(10),
        K::F11 => Key::F(11),
        K::F12 => Key::F(12),
        K::MediaPlayPause => Key::MediaPlayPause,
        K::MediaPlay => Key::MediaPlay,
        K::MediaPause => Key::MediaPause,
        K::MediaStop => Key::MediaStop,
        K::MediaNextTrack => Key::MediaNext,
        K::MediaPreviousTrack => Key::MediaPrev,
        K::MediaFastForward => Key::MediaFastForward,
        K::MediaRewind => Key::MediaRewind,
        K::VolumeUp => Key::VolumeUp,
        K::VolumeDown => Key::VolumeDown,
        K::Mute => Key::Mute,
        other => {
            let code = other.to_ll().0;
            // Printable ASCII keycodes equal their character.
            if (0x20..0x7f).contains(&code) {
                Key::Char(code as u8 as char)
            } else {
                return None;
            }
        }
    })
}

fn map_pad_button(b: sdl3::gamepad::Button) -> Option<PadButton> {
    use sdl3::gamepad::Button as B;
    Some(match b {
        B::South => PadButton::South,
        B::East => PadButton::East,
        B::West => PadButton::West,
        B::North => PadButton::North,
        B::Back => PadButton::Back,
        B::Guide => PadButton::Guide,
        B::Start => PadButton::Start,
        B::LeftShoulder => PadButton::LeftShoulder,
        B::RightShoulder => PadButton::RightShoulder,
        B::DPadUp => PadButton::DPadUp,
        B::DPadDown => PadButton::DPadDown,
        B::DPadLeft => PadButton::DPadLeft,
        B::DPadRight => PadButton::DPadRight,
        _ => return None,
    })
}

fn map_analog(a: sdl3::gamepad::Axis, v: i16) -> Option<(Analog, f32)> {
    use sdl3::gamepad::Axis as A;
    let n = (v as f32 / 32767.0).clamp(-1.0, 1.0);
    Some(match a {
        A::LeftX => (Analog::LeftX, n),
        A::LeftY => (Analog::LeftY, n),
        A::TriggerLeft => (Analog::LeftTrigger, n),
        A::TriggerRight => (Analog::RightTrigger, n),
        _ => return None,
    })
}

impl SdlPlatform {
    /// Converts one SDL event, updating gamepad state.
    fn handle(&mut self, ev: Event, out: &mut Vec<PlatformEvent>) {
        let ev = match self.gamepads.as_mut() {
            Some(g) => match g.handle(ev, out) {
                Some(ev) => ev,
                None => return,
            },
            None => ev,
        };
        if let Some(e) = convert(ev, &self.window, self.wake_type) {
            out.push(e);
        }
    }
}

fn map_mods(m: Mod) -> Modifiers {
    Modifiers {
        ctrl: m.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD),
        shift: m.intersects(Mod::LSHIFTMOD | Mod::RSHIFTMOD),
        alt: m.intersects(Mod::LALTMOD | Mod::RALTMOD),
    }
}

fn convert(ev: Event, window: &Window, wake_type: u32) -> Option<PlatformEvent> {
    Some(match ev {
        Event::User { type_, .. } if type_ == wake_type => PlatformEvent::Wake,
        Event::Quit { .. } => PlatformEvent::Quit,
        Event::KeyDown { keycode: Some(k), keymod, repeat, .. } => {
            PlatformEvent::KeyDown { key: map_key(k)?, mods: map_mods(keymod), repeat }
        }
        Event::KeyUp { keycode: Some(k), .. } => PlatformEvent::KeyUp { key: map_key(k)? },
        Event::Window { win_event, .. } => match win_event {
            WindowEvent::CloseRequested => PlatformEvent::Quit,
            WindowEvent::PixelSizeChanged(..) | WindowEvent::Resized(..) => {
                let (width, height) = window.size_in_pixels();
                PlatformEvent::Resized { width, height }
            }
            WindowEvent::Exposed | WindowEvent::Shown | WindowEvent::Restored => {
                PlatformEvent::Exposed
            }
            WindowEvent::DisplayChanged(_) => PlatformEvent::DisplayChanged,
            _ => return None,
        },
        Event::Display { .. } => PlatformEvent::DisplayChanged,
        _ => return None,
    })
}

impl Platform for SdlPlatform {
    fn gl(&self) -> &Gl {
        &self.gl
    }

    fn gl_loader(&self) -> Box<dyn Fn(&str) -> *const c_void> {
        Box::new(|name: &str| {
            let Ok(name) = std::ffi::CString::new(name) else { return std::ptr::null() };
            // SAFETY: SDL video is initialized for the process lifetime of
            // the platform; SDL_GL_GetProcAddress accepts any name.
            match unsafe { sdl3::sys::video::SDL_GL_GetProcAddress(name.as_ptr()) } {
                Some(f) => f as *const c_void,
                None => std::ptr::null(),
            }
        })
    }

    fn gl_proc_address(&self, name: &str) -> *const c_void {
        match self.video.gl_get_proc_address(name) {
            Some(f) => f as *const c_void,
            None => std::ptr::null(),
        }
    }

    fn info(&self) -> &PlatformInfo {
        &self.info
    }

    fn framebuffer_size(&self) -> (u32, u32) {
        self.window.size_in_pixels()
    }

    fn refresh_hz(&self) -> f32 {
        self.current_mode().map(|m| m.refresh_hz).unwrap_or(0.0)
    }

    fn current_mode(&self) -> Option<ModeInfo> {
        let display = self.window.get_display().ok()?;
        display.get_mode().ok().map(|m| mode_info(&m))
    }

    fn display_modes(&self) -> Vec<ModeInfo> {
        let Ok(display) = self.window.get_display() else { return Vec::new() };
        display
            .get_fullscreen_modes()
            .map(|v| v.iter().map(mode_info).collect())
            .unwrap_or_default()
    }

    fn poll_events(&mut self, out: &mut Vec<PlatformEvent>) {
        while let Some(ev) = self.pump.poll_event() {
            self.handle(ev, out);
        }
        if let Some((_, rx)) = &self.cec {
            out.extend(rx.try_iter());
        }
    }

    fn wait_events(&mut self, timeout: Duration, out: &mut Vec<PlatformEvent>) {
        if let Some(ev) = self.pump.wait_event_timeout(timeout) {
            self.handle(ev, out);
        }
        self.poll_events(out);
    }

    fn swap(&mut self) {
        self.window.gl_swap_window();
    }

    fn waker(&self) -> std::sync::Arc<dyn Fn() + Send + Sync> {
        let wake_type = self.wake_type;
        std::sync::Arc::new(move || {
            // SAFETY: SDL_PushEvent is documented thread-safe; the event is
            // fully initialized (zeroed, then type set) and copied by SDL.
            unsafe {
                let mut ev: sdl3::sys::events::SDL_Event = std::mem::zeroed();
                ev.r#type = wake_type;
                sdl3::sys::events::SDL_PushEvent(&mut ev);
            }
        })
    }
}

/// `--print-diagnostics` part that does not need a window: compiled-in
/// drivers and every display's modes.
pub fn print_sdl_diagnostics(opts: &SdlOptions) -> Result<(), String> {
    println!("sdl.version: {}", sdl_version());
    let drivers: Vec<&str> = sdl3::video::drivers().collect();
    println!("sdl.video_drivers: {}", drivers.join(", "));
    let (sdl, video) = init(opts)?;
    println!("sdl.current_video_driver: {}", video.current_video_driver());
    match sdl.gamepad() {
        Ok(g) => {
            let ids = g.gamepads().unwrap_or_default();
            println!("gamepads: {}", ids.len());
            for id in ids {
                let name = g.name_for_id(id).unwrap_or_else(|_| "unknown".into());
                println!("  gamepad: {name}");
            }
        }
        Err(e) => println!("gamepads.error: {e}"),
    }
    let displays = video.displays().map_err(|e| e.to_string())?;
    for d in displays {
        let name = d.get_name().unwrap_or_default();
        println!("display: {name}");
        if let Ok(m) = d.get_mode() {
            println!(
                "  current: {}x{} @ {:.3} Hz ({}/{})",
                m.w, m.h, m.refresh_rate, m.refresh_rate_numerator, m.refresh_rate_denominator
            );
        }
        if let Ok(modes) = d.get_fullscreen_modes() {
            for m in modes {
                println!(
                    "  mode: {}x{} @ {:.3} Hz ({}/{})",
                    m.w, m.h, m.refresh_rate, m.refresh_rate_numerator, m.refresh_rate_denominator
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drives the real SDL gamepad path with a virtual gamepad: hotplug,
    /// a face button, and the left stick acting as a D-pad.
    #[test]
    fn virtual_gamepad_end_to_end() {
        let sdl = sdl3::init().expect("SDL_Init");
        let mut pads = Gamepads::new(sdl.gamepad().expect("gamepad subsystem"));
        let mut pump = sdl.event_pump().expect("event pump");

        let name = c"jellyroll test pad";
        let mut desc = sdl3::sys::joystick::SDL_VirtualJoystickDesc::new();
        desc.r#type = sdl3::sys::joystick::SDL_JOYSTICK_TYPE_GAMEPAD.0 as u16;
        desc.naxes = 6;
        desc.nbuttons = 15;
        desc.name = name.as_ptr();
        // SAFETY: desc is fully initialized (SDL_INIT_INTERFACE via new());
        // the name outlives the joystick.
        let id = unsafe { sdl3::sys::joystick::SDL_AttachVirtualJoystick(&desc) };
        assert_ne!(id.0, 0, "attach virtual joystick");
        // SAFETY: id is a valid joystick instance; opened for setting state.
        let joy = unsafe { sdl3::sys::joystick::SDL_OpenJoystick(id) };
        assert!(!joy.is_null());

        let mut out = Vec::new();
        let mut pump_all = |pads: &mut Gamepads, out: &mut Vec<PlatformEvent>| {
            for _ in 0..10 {
                // SAFETY: SDL initialized; processes virtual joystick state.
                unsafe { sdl3::sys::joystick::SDL_UpdateJoysticks() };
                while let Some(ev) = pump.poll_event() {
                    let _ = pads.handle(ev, out);
                }
            }
        };
        pump_all(&mut pads, &mut out);
        assert_eq!(pads.pads.len(), 1, "virtual gamepad was opened on hotplug");

        // SAFETY: joy is open; button 0 is SDL_GAMEPAD_BUTTON_SOUTH and axis
        // 0 is SDL_GAMEPAD_AXIS_LEFTX for virtual gamepads.
        unsafe {
            sdl3::sys::joystick::SDL_SetJoystickVirtualButton(joy, 0, true);
        }
        pump_all(&mut pads, &mut out);
        unsafe {
            sdl3::sys::joystick::SDL_SetJoystickVirtualButton(joy, 0, false);
        }
        pump_all(&mut pads, &mut out);
        unsafe {
            sdl3::sys::joystick::SDL_SetJoystickVirtualAxis(joy, 0, 30000);
        }
        pump_all(&mut pads, &mut out);
        assert_eq!(
            out,
            vec![
                PlatformEvent::PadDown { button: PadButton::South },
                PlatformEvent::PadUp { button: PadButton::South },
                PlatformEvent::PadDown { button: PadButton::StickRight },
            ]
        );
        // SAFETY: closes what we opened.
        unsafe {
            sdl3::sys::joystick::SDL_CloseJoystick(joy);
            sdl3::sys::joystick::SDL_DetachVirtualJoystick(id);
        }
    }
}
