//! jellyroll: boot-to-app Jellyfin HTPC client. See docs/ and the README.

#![forbid(unsafe_code)]

mod anim;
mod args;
mod diagnostics;
mod draw;
mod frame_loop;
mod hud;
mod input;
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
    let opts = match args::parse(std::env::args().skip(1)) {
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
    logging::init();
    logging::install_panic_hook();

    let sdl_opts = SdlOptions {
        dev_window: opts.dev,
        double_buffer: opts.double_buffer,
        no_vsync: opts.no_vsync,
    };
    if opts.print_diagnostics {
        return diagnostics::print(&sdl_opts);
    }

    tracing::info!(version = env!("CARGO_PKG_VERSION"), dev = opts.dev, "starting");
    let mut platform = match SdlPlatform::new(&sdl_opts) {
        Ok(p) => p,
        Err(e) => {
            // Without a display there is nothing to show an error screen on;
            // exit non-zero so systemd restarts us (e.g. TV not yet on).
            tracing::error!("platform init failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    match frame_loop::run(&mut platform, &opts, process_start) {
        frame_loop::Exit::Quit | frame_loop::Exit::BenchPassed => ExitCode::SUCCESS,
        frame_loop::Exit::BenchFailed => ExitCode::FAILURE,
    }
}
