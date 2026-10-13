//! `--print-diagnostics` (spec 11): everything useful for support over SSH.
//! Later milestones add mpv/hwdec, audio and CEC details.

use std::process::ExitCode;

use jellyroll_platform::{Platform, SdlOptions, SdlPlatform};

fn read_trim(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn list_dir(path: &str, prefix: &str) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(path)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with(prefix))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

pub fn print(opts: &SdlOptions) -> ExitCode {
    println!("jellyroll: {}", env!("CARGO_PKG_VERSION"));
    println!("kernel: {}", read_trim("/proc/sys/kernel/osrelease").unwrap_or_default());
    println!("dri_devices: {}", list_dir("/dev/dri", "").join(", "));
    println!("cec_devices: {}", list_dir("/dev", "cec").join(", "));
    for var in [
        "XDG_SESSION_TYPE",
        "XDG_SEAT",
        "XDG_VTNR",
        "WAYLAND_DISPLAY",
        "DISPLAY",
        "SDL_VIDEO_DRIVER",
    ] {
        if let Ok(v) = std::env::var(var) {
            println!("env.{var}: {v}");
        }
    }
    if let Err(e) = jellyroll_platform::print_sdl_diagnostics(opts) {
        println!("sdl.error: {e}");
    }
    // Creating a context needs DRM master on KMSDRM; if the service is
    // running this fails, which is itself useful to report.
    match SdlPlatform::new(opts) {
        Ok(p) => {
            let i = p.info();
            println!("gl.vendor: {}", i.gl_vendor);
            println!("gl.renderer: {}", i.gl_renderer);
            println!("gl.version: {}", i.gl_version);
            println!("gl.glsl: {}", i.glsl_version);
            println!("gl.es: {}", i.is_gles);
            println!("gl.timer_queries: {}", i.gpu_timer_queries);
            println!("gl.swap_interval: {}", i.swap_interval);
            let (w, h) = p.framebuffer_size();
            println!("framebuffer: {w}x{h} @ {:.3} Hz", p.refresh_hz());
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!("gl.error: {e}");
            ExitCode::FAILURE
        }
    }
}
