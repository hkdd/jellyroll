//! jellyroll: boot-to-app Jellyfin HTPC client. See docs/ and the README.

#![forbid(unsafe_code)]

mod anim;
mod args;
mod config;
mod diagnostics;
mod draw;
mod frame_loop;
mod hud;
mod input;
mod input_access;
mod logging;
mod perf;
mod scene;
mod systemd;

use std::process::ExitCode;
use std::time::Instant;

use jellyroll_platform::{SdlOptions, SdlPlatform};

#[global_allocator]
static ALLOC: jellyroll_platform::alloc_counter::CountingAlloc =
    jellyroll_platform::alloc_counter::CountingAlloc;

fn main() -> ExitCode {
    let process_start = Instant::now();
    let mut opts = match args::parse(std::env::args().skip(1)) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("jellyroll: {e}");
            return ExitCode::from(2);
        }
    };
    if opts.help {
        print!("{}", args::USAGE);
        return ExitCode::SUCCESS;
    }
    // Load config before logging (it sets the log level); report its
    // warnings once logging is up.
    let loaded = match opts.config.clone().or_else(config::default_path) {
        Some(path) => config::load(&path),
        None => config::Loaded {
            config: config::Config::default(),
            path: Default::default(),
            warnings: vec!["no $HOME or $XDG_CONFIG_HOME; using default config".into()],
        },
    };
    logging::init(&loaded.config.log.level);
    logging::install_panic_hook();
    tracing::info!(path = %loaded.path.display(), "config");
    for w in &loaded.warnings {
        tracing::warn!("config: {w}");
    }
    opts.merge_config(&loaded.config);

    let sdl_opts = SdlOptions {
        dev_window: opts.dev,
        double_buffer: opts.double_buffer,
        no_vsync: opts.no_vsync,
    };
    if opts.print_diagnostics {
        return diagnostics::print(&sdl_opts);
    }

    tracing::info!(version = env!("CARGO_PKG_VERSION"), dev = opts.dev, "starting");

    // On KMSDRM, SDL reads input straight from /dev/input. Without access the
    // screen works but nothing responds, so say so instead of hanging there.
    // In --dev the compositor delivers input, so the check does not apply.
    let mut notice = frame_loop::config_notice(loaded.warnings.len());
    let access = input_access::scan(std::path::Path::new("/dev/input"));
    if !opts.dev && access.blocked() {
        let msg = format!(
            "cannot read any of the {} input devices in /dev/input, so keyboard, \
             remote and gamepad will not work: {}",
            access.devices,
            input_access::REMEDY
        );
        if std::env::var_os("NOTIFY_SOCKET").is_none() {
            // Started by hand: refuse rather than take over the screen with
            // no way to control (or quit) the app.
            tracing::error!("{msg}");
            eprintln!("jellyroll: {msg}");
            return ExitCode::FAILURE;
        }
        // Under systemd: keep running and show the problem on screen.
        tracing::error!("{msg}");
        notice = "NO INPUT ACCESS: ADD THE SERVICE USER TO THE INPUT GROUP".into();
    }

    let mut platform = match SdlPlatform::new(&sdl_opts) {
        Ok(p) => p,
        Err(e) => {
            // Without a display there is nothing to show an error screen on;
            // exit non-zero so systemd restarts us (e.g. TV not yet on).
            tracing::error!("platform init failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    match frame_loop::run(&mut platform, &opts, &loaded, &notice, process_start) {
        frame_loop::Exit::Quit | frame_loop::Exit::BenchPassed => ExitCode::SUCCESS,
        frame_loop::Exit::BenchFailed => ExitCode::FAILURE,
    }
}
