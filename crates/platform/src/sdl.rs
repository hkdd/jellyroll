//! SDL3 backend. Production uses the KMSDRM video driver (no display server);
//! dev mode lets SDL pick Wayland/X11 so the same code runs in a window.

use std::ffi::c_void;
use std::time::Duration;

use sdl3::event::{Event, WindowEvent};
use sdl3::keyboard::{Keycode, Mod};
use sdl3::video::{GLContext, GLProfile, SwapInterval, Window};

use crate::gl::Gl;
use crate::{Key, ModeInfo, Modifiers, Platform, PlatformEvent, PlatformInfo};

#[derive(Clone, Debug, Default)]
pub struct SdlOptions {
    /// Run in a desktop window (Wayland/X11/cage) instead of KMSDRM.
    pub dev_window: bool,
    /// Request double instead of triple buffering on KMSDRM.
    pub double_buffer: bool,
    /// Disable vsync (benchmarking only).
    pub no_vsync: bool,
}

pub struct SdlPlatform {
    // Drop order matters: context before window before subsystems.
    _gl_ctx: GLContext,
    window: Window,
    _video: sdl3::VideoSubsystem,
    pump: sdl3::EventPump,
    _sdl: sdl3::Sdl,
    gl: Gl,
    info: PlatformInfo,
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
                let mut b = video.window("jf-htpc (dev)", 1280, 720);
                b.resizable();
                b
            } else {
                let mut b = video.window("jf-htpc", desktop.w as u32, desktop.h as u32);
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
            return Ok(Self { _gl_ctx: ctx, window, _video: video, pump, _sdl: sdl, gl, info });
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

fn map_mods(m: Mod) -> Modifiers {
    Modifiers {
        ctrl: m.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD),
        shift: m.intersects(Mod::LSHIFTMOD | Mod::RSHIFTMOD),
        alt: m.intersects(Mod::LALTMOD | Mod::RALTMOD),
    }
}

fn convert(ev: Event, window: &Window) -> Option<PlatformEvent> {
    Some(match ev {
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
            if let Some(e) = convert(ev, &self.window) {
                out.push(e);
            }
        }
    }

    fn wait_events(&mut self, timeout: Duration, out: &mut Vec<PlatformEvent>) {
        if let Some(ev) = self.pump.wait_event_timeout(timeout)
            && let Some(e) = convert(ev, &self.window)
        {
            out.push(e);
        }
        self.poll_events(out);
    }

    fn swap(&mut self) {
        self.window.gl_swap_window();
    }
}

/// `--print-diagnostics` part that does not need a window: compiled-in
/// drivers and every display's modes.
pub fn print_sdl_diagnostics(opts: &SdlOptions) -> Result<(), String> {
    println!("sdl.version: {}", sdl_version());
    let drivers: Vec<&str> = sdl3::video::drivers().collect();
    println!("sdl.video_drivers: {}", drivers.join(", "));
    let (_sdl, video) = init(opts)?;
    println!("sdl.current_video_driver: {}", video.current_video_driver());
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
