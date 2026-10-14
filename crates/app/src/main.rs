//! jellyroll: boot-to-app Jellyfin HTPC client. See docs/ and the README.

#![forbid(unsafe_code)]

mod anim;
mod args;
mod audio;
mod bench_scroll;
mod config;
mod diagnostics;
mod focus;
mod frame_loop;
mod home;
mod hud;
mod input;
mod input_access;
mod logging;
mod net;
mod osd;
mod perf;
mod playback;
mod posters;
mod power;
mod refresh;
mod scene;
mod screensaver;
mod soak;
mod systemd;
mod ui_prefs;

use std::process::ExitCode;
use std::time::Instant;

use jellyroll_platform::{Platform, SdlOptions, SdlPlatform};

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
    let mut loaded = match opts.config.clone().or_else(config::default_path) {
        Some(path) => config::load(&path),
        None => config::Loaded {
            config: config::Config::default(),
            path: Default::default(),
            warnings: vec!["no $HOME or $XDG_CONFIG_HOME; using default config".into()],
        },
    };
    let _trace = logging::init(&loaded.config.log.level, opts.trace.as_deref());
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
        window_size: opts.window,
        cec: loaded.config.input.cec.then(|| jellyroll_platform::cec::CecOptions {
            device: (!loaded.config.input.cec_device.is_empty())
                .then(|| loaded.config.input.cec_device.clone().into()),
            wake_tv: loaded.config.input.cec_wake_tv,
            ..Default::default()
        }),
    };
    if opts.print_diagnostics {
        return diagnostics::print(&sdl_opts);
    }

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        dev = opts.dev,
        ms = process_start.elapsed().as_millis() as u64,
        "starting"
    );

    // On KMSDRM, SDL reads input straight from /dev/input. Without access the
    // screen works but nothing responds, so say so instead of hanging there.
    // In --dev the compositor delivers input, so the check does not apply.
    let mut notice = frame_loop::config_notice(loaded.warnings.len());
    let t_scan = std::time::Instant::now();
    let access = input_access::scan(std::path::Path::new("/dev/input"), true);
    tracing::info!(
        ms = process_start.elapsed().as_millis() as u64,
        scan_ms = t_scan.elapsed().as_millis() as u64,
        devices = access.devices,
        "input access checked"
    );
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
    // A resolution chosen in Settings wins over config.toml (benchmarks
    // use the config alone).
    if opts.bench.is_none()
        && let Some(r) =
            net::store::data_dir().and_then(|d| ui_prefs::load(&ui_prefs::path(&d)).ui_resolution)
    {
        loaded.config.display.ui_resolution = r;
    }
    let ui_res = loaded.config.display.ui_resolution;
    let render_scale = ui_res.render_scale(platform.framebuffer_size().1);
    tracing::info!(?ui_res, render_scale, "UI resolution");
    let skia_opts = jellyroll_render::skia::SkiaOptions { render_scale, ..Default::default() };
    tracing::info!(ms = process_start.elapsed().as_millis() as u64, "platform ready");
    let mut renderer = match jellyroll_render::skia::SkiaRenderer::new(
        |name| platform.gl_proc_address(name),
        skia_opts,
    ) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("renderer init failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(ms = process_start.elapsed().as_millis() as u64, "renderer ready");
    if opts.bench_upload {
        let info = platform.info();
        println!("gl.renderer: {}", info.gl_renderer);
        // Poster and placeholder sizes at 4K, then a 1080p backdrop.
        for (w, h) in [(320, 480), (32, 48), (1920, 1080)] {
            println!("{w}x{h} ({} KB), mean of 60:", w * h * 4 / 1024);
            let s0 = jellyroll_render::Renderer::stats(&renderer);
            for (name, op, flush) in renderer.bench_uploads(w, h, 60) {
                println!("  {name}: call {op:.3} ms, flush {flush:.3} ms");
            }
            let s1 = jellyroll_render::Renderer::stats(&renderer);
            let n = (s1.uploads - s0.uploads).max(1) as f64 * 1e6;
            println!(
                "  D (both) breakdown, ms: get texture {:.3}, write {:.3}",
                (s1.upload_texture_ns - s0.upload_texture_ns) as f64 / n,
                (s1.upload_write_ns - s0.upload_write_ns) as f64 / n
            );
        }
        return ExitCode::SUCCESS;
    }
    let c = &loaded.config;
    let mut bench_dir = None;
    let net = if opts.soak.is_some() {
        let mock = jellyroll_mock_server::MockConfig {
            movies: 2000,
            media: opts.soak_media.clone(),
            media_secs: 600,
            ..Default::default()
        };
        match bench_scroll::setup_with("soak", mock) {
            Ok((cfg, dir)) => {
                bench_dir = Some(dir);
                cfg
            }
            Err(e) => {
                tracing::error!("{e}");
                return ExitCode::FAILURE;
            }
        }
    } else if let Some(args::Bench::Scroll(_)) = opts.bench {
        match bench_scroll::setup() {
            Ok((cfg, dir)) => {
                bench_dir = Some(dir);
                cfg
            }
            Err(e) => {
                tracing::error!("{e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        net::NetConfig {
            server_url: c.server.url.clone(),
            ca_file: (!c.server.ca_file.is_empty()).then(|| c.server.ca_file.clone().into()),
            image_cache_bytes: c.cache.images_mb << 20,
            data_dir: net::store::data_dir(),
            cache_dir: net::store::cache_dir(),
        }
    };
    let exit = frame_loop::run(
        &mut platform,
        &mut renderer,
        &opts,
        &loaded,
        Some(net),
        &notice,
        process_start,
    );
    if let Some(dir) = bench_dir {
        let _ = std::fs::remove_dir_all(dir);
    }
    match exit {
        frame_loop::Exit::Quit | frame_loop::Exit::BenchPassed => ExitCode::SUCCESS,
        frame_loop::Exit::BenchFailed | frame_loop::Exit::Error => ExitCode::FAILURE,
    }
}
