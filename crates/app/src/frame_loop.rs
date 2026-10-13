//! The render loop (spec 4, 6.2). Single thread owns the GL context.
//!
//! Redraw only when dirty: input, a running animation, a HUD refresh, or an
//! expose/resize. Otherwise block in `wait_events` until the next deadline
//! (key repeat, HUD refresh, watchdog ping, benchmark end) with no page flip.
//!
//! With `--play`, mpv renders the video each vsync while playing (its
//! `display-resample` timing needs that), then Skia draws the OSD on top
//! only when something is visible (spec 6.3). mpv wakes the loop from its
//! own threads through the platform waker.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use jellyroll_platform::alloc_counter;
use jellyroll_platform::{GpuTimer, Platform, PlatformEvent};
use jellyroll_render::Renderer;

use crate::args::{Bench, Options};
use crate::home::HomeApp;
use crate::hud::{CPU_BUDGET_MS, GPU_BUDGET_MS, Hud};
use crate::input::{self, Input, InputAction, KeyRepeat};
use crate::net::{NetConfig, NetHandle};
use crate::perf::{FrameStats, is_missed_vsync, ms};
use crate::playback::{Outcome, Playback};
use crate::scene::{Scene, UPLOAD_BUDGET};
use crate::systemd::Systemd;

/// What the loop shows when not playing: the Jellyfin home screen, or the
/// synthetic demo scene (`--demo`, render benchmarks).
enum View {
    Demo(Box<Scene>),
    Home(Box<HomeApp>),
}

impl View {
    fn is_animating(&self, now: Instant) -> bool {
        match self {
            View::Demo(s) => s.is_animating(now),
            View::Home(h) => h.is_animating(now),
        }
    }

    /// Startup work done (artwork uploaded): steady state may begin.
    fn settled(&self) -> bool {
        match self {
            View::Demo(s) => !s.uploads_pending(),
            View::Home(h) => h.settled(),
        }
    }

    fn on_action(&mut self, a: InputAction, now: Instant) -> bool {
        match self {
            View::Demo(s) => s.on_action(a, now),
            View::Home(h) => h.on_action(a, now),
        }
    }

    fn load_work(&self) -> u64 {
        match self {
            View::Home(h) => h.load_work(),
            View::Demo(_) => 0,
        }
    }

    fn wants_text(&self) -> bool {
        matches!(self, View::Home(h) if h.wants_text())
    }

    fn next_deadline(&self) -> Option<Instant> {
        match self {
            View::Home(h) => h.next_deadline(),
            View::Demo(_) => None,
        }
    }

    /// Per-iteration update; returns true if a redraw is needed.
    fn update(&mut self, now: Instant) -> bool {
        match self {
            View::Demo(s) => {
                s.update(now);
                false
            }
            View::Home(h) => h.update(now),
        }
    }
}

/// Minimum time before steady state (allocation checks and benchmark
/// windows). Steady state also waits for initial artwork uploads to finish.
const WARMUP: Duration = Duration::from_secs(1);
/// Upper bound on a single wait so a lost wakeup cannot stall us forever.
const MAX_WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, PartialEq, Eq)]
pub enum Exit {
    Quit,
    BenchPassed,
    BenchFailed,
    /// Playback could not start or ended with an error.
    Error,
}

/// Baselines captured when the benchmark window starts.
#[derive(Default)]
struct BenchBase {
    process_cpu: Option<Duration>,
    thread_cpu: Option<Duration>,
    frames: u64,
    drops: jellyroll_video::FrameDrops,
}

/// Bench mode toggles the OSD on this period to catch overlay hitches.
const BENCH_OSD_PERIOD: Duration = Duration::from_secs(2);

fn earliest(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn make_header(fb: (u32, u32), hz: f32, header: &mut String) {
    header.clear();
    let _ = write!(header, "M2 render benchmark · {}×{} @ {:.2} Hz · F3 HUD", fb.0, fb.1, hz);
}

/// Device facts for the Settings page.
fn about_device<P: Platform>(p: &P, fb: (u32, u32), hz: f32) -> Vec<(String, String)> {
    let i = p.info();
    let (major, minor) = jellyroll_video::client_api_version();
    vec![
        ("Jellyroll".into(), env!("CARGO_PKG_VERSION").into()),
        ("GPU".into(), i.gl_renderer.clone()),
        ("OpenGL".into(), i.gl_version.clone()),
        ("Display".into(), format!("{}×{} @ {:.2} Hz ({})", fb.0, fb.1, hz, i.video_driver)),
        ("libmpv".into(), format!("client API {major}.{minor}")),
    ]
}

/// On-screen notice for config problems (details are in the journal).
pub fn config_notice(warnings: usize) -> String {
    match warnings {
        0 => String::new(),
        1 => "CONFIG: 1 WARNING, SEE JOURNALCTL -U JELLYROLL".into(),
        n => format!("CONFIG: {n} WARNINGS, SEE JOURNALCTL -U JELLYROLL"),
    }
}

/// How long the loop may sleep. Zero means render/poll immediately.
pub fn wait_timeout(
    dirty: bool,
    animating: bool,
    deadline: Option<Instant>,
    now: Instant,
) -> Duration {
    if dirty || animating {
        return Duration::ZERO;
    }
    match deadline {
        Some(t) => t.saturating_duration_since(now).min(MAX_WAIT),
        None => MAX_WAIT,
    }
}

pub fn run<P: Platform, R: Renderer>(
    p: &mut P,
    r: &mut R,
    opts: &Options,
    config: &crate::config::Loaded,
    net: Option<NetConfig>,
    notice: &str,
    process_start: Instant,
) -> Exit {
    let start = Instant::now();
    let mut systemd = Systemd::new(start);
    let mut hud = Hud::new(opts.hud, start);
    let mut repeat = KeyRepeat::new(config.config.input.repeat());
    let safe_area = config.config.display.safe_area;
    let mut stats = FrameStats::default();
    let mut events: Vec<PlatformEvent> = Vec::with_capacity(64);
    let mut gpu_timer = GpuTimer::new(p.gl());

    let mut fb = p.framebuffer_size();
    let mut refresh_hz = p.refresh_hz();
    let mut header = String::new();
    make_header(fb, refresh_hz, &mut header);
    let mut view = match net {
        Some(cfg) if !opts.demo && opts.play.is_none() => {
            let cap = config.config.cache.textures_mb << 20;
            match NetHandle::start(cfg, p.waker()) {
                Ok(h) => {
                    let about = about_device(p, fb, refresh_hz);
                    View::Home(Box::new(HomeApp::new(h, cap, fb, safe_area, about)))
                }
                Err(e) => {
                    tracing::error!("cannot start the network thread: {e}");
                    View::Demo(Box::new(Scene::new(start)))
                }
            }
        }
        _ => View::Demo(Box::new(Scene::new(start))),
    };

    let mut playback = match &opts.play {
        Some(url) => match Playback::start(p, url, start) {
            Ok(pb) => Some(pb),
            Err(e) => {
                tracing::error!("playback: {e}");
                return Exit::Error;
            }
        },
        None => None,
    };
    let mut bench_osd_next = start;
    let mut overlay_shown = false;

    let bench_len = opts.bench.map(|b| match b {
        Bench::Pacing(d) | Bench::Idle(d) | Bench::Play(d) | Bench::Scroll(d) => d,
    });
    // --bench-scroll: drive the UI like a user; measure while scrolling.
    let mut script = matches!(opts.bench, Some(Bench::Scroll(_)))
        .then(|| crate::bench_scroll::Script::new(start));
    // Until steady state the loop must still wake up to notice it.
    let mut bench_end = match script {
        Some(_) => Some(start + crate::bench_scroll::SETUP_TIMEOUT),
        None => bench_len.map(|_| start + WARMUP),
    };
    if let (Some(Bench::Pacing(_)), View::Demo(scene)) = (opts.bench, &mut view) {
        scene.bench = true;
    }
    let mut bench_reset_done = false;
    let mut base = BenchBase::default();

    let mut dirty = true;
    let mut first_frame = true;
    // Browsing the server: playback returns to the UI when it ends
    // (with --play, the app exits instead).
    let browsing = opts.play.is_none() && matches!(view, View::Home(_));
    let mut stop_playback = false;
    let mut last_present: Option<Instant> = None;
    let mut alloc_warned = false;

    loop {
        // ---- wait ------------------------------------------------------
        let now = Instant::now();
        let deadline = earliest(
            earliest(repeat.next_deadline(), hud.next_deadline()),
            earliest(systemd.next_deadline(), bench_end),
        );
        let deadline = earliest(deadline, script.as_ref().and_then(|s| s.next_deadline()));
        let (deadline, animating) = match &playback {
            Some(pb) => (earliest(deadline, pb.osd.next_deadline(now)), pb.playing()),
            None => (earliest(deadline, view.next_deadline()), view.is_animating(now)),
        };
        let timeout = wait_timeout(dirty, animating, deadline, now);
        events.clear();
        if timeout.is_zero() {
            p.poll_events(&mut events);
        } else {
            // Going idle: the next present will not be back-to-back.
            last_present = None;
            p.wait_events(timeout, &mut events);
        }

        // ---- housekeeping (outside the measured frame) ----------------
        let now = Instant::now();
        systemd.tick(now);
        if hud.due(now) {
            hud.refresh(now, &stats, &r.stats(), p.info(), refresh_hz, fb);
            dirty = true;
        }
        let loaded = match &playback {
            Some(pb) => pb.started,
            None => view.settled(),
        };
        let ready = match &script {
            Some(s) => s.measuring(),
            None => now >= start + WARMUP && loaded,
        };
        if !bench_reset_done && ready {
            bench_reset_done = true;
            bench_end = bench_len.map(|d| now + d);
            stats.reset();
            base = BenchBase {
                process_cpu: crate::perf::process_cpu_time(),
                thread_cpu: crate::perf::thread_cpu_time(),
                frames: 0,
                drops: playback.as_ref().map(|pb| pb.player.frame_drops()).unwrap_or_default(),
            };
        }
        if let Some(end) = bench_end
            && !bench_reset_done
            && script.is_some()
            && now >= end
        {
            println!("bench: scroll");
            println!(
                "FAIL: the library grid did not open within {:?}",
                crate::bench_scroll::SETUP_TIMEOUT
            );
            println!("result: FAIL");
            return Exit::BenchFailed;
        }
        if let Some(end) = bench_end
            && bench_reset_done
            && now >= end
        {
            return finish_bench(opts, &stats, &base, p, r, playback.as_ref(), script.as_ref());
        }

        if let View::Home(h) = &mut view {
            dirty |= h.poll(now);
        }

        // ---- input + update (measured) --------------------------------
        let frame_start = Instant::now();
        let allocs_start = alloc_counter::allocations();
        let work_start = view.load_work();
        for ev in &events {
            match *ev {
                PlatformEvent::Quit => {
                    systemd.stopping();
                    return Exit::Quit;
                }
                PlatformEvent::KeyDown {
                    key: jellyroll_platform::Key::F(3),
                    repeat: false,
                    ..
                } => {
                    hud.toggle(now);
                    dirty = true;
                }
                PlatformEvent::CecStandby => {
                    // The TV is going to standby: pause playback (spec 5.4).
                    if let Some(pb) = playback.as_mut()
                        && !pb.player.state.paused
                    {
                        pb.player.toggle_pause();
                    }
                }
                PlatformEvent::Resized { .. } | PlatformEvent::DisplayChanged => {
                    fb = p.framebuffer_size();
                    refresh_hz = p.refresh_hz();
                    make_header(fb, refresh_hz, &mut header);
                    if let View::Home(h) = &mut view {
                        h.set_output(fb, safe_area, now);
                    }
                    tracing::info!(w = fb.0, h = fb.1, refresh_hz, "output changed");
                    repeat.reset();
                    dirty = true;
                }
                PlatformEvent::Exposed => dirty = true,
                PlatformEvent::Wake => {}
                ref other => match input::from_event(other, view.wants_text()) {
                    Some(Input::Release(b)) => repeat.release(b),
                    Some(Input::Typed(c)) => {
                        if playback.is_none() {
                            dirty |= view.on_action(InputAction::TextInput(c), now);
                        }
                    }
                    Some(Input::Press { button, action: Some(a) }) => {
                        repeat.press(button, a, now);
                        if let Some(pb) = playback.as_mut() {
                            match pb.on_action(a, now) {
                                Outcome::Exit if browsing => stop_playback = true,
                                Outcome::Exit => {
                                    systemd.stopping();
                                    return Exit::Quit;
                                }
                                Outcome::Continue { redraw } => dirty |= redraw,
                            }
                            continue;
                        }
                        // The screen gets Back first (e.g. leave a problem
                        // screen); unused Back at the top level quits.
                        let used = view.on_action(a, now);
                        if !used && a == InputAction::Back && opts.allow_exit {
                            systemd.stopping();
                            return Exit::Quit;
                        }
                        dirty |= used;
                    }
                    Some(Input::Press { action: None, .. }) | None => {}
                },
            }
        }
        if let Some(pb) = playback.as_mut() {
            if let Some(a) = repeat.tick(now)
                && let Outcome::Continue { redraw } = pb.on_action(a, now)
            {
                dirty |= redraw;
            }
            dirty |= pb.poll(now);
            if let Some((reason, error)) = &pb.ended
                && browsing
            {
                // Back to the page that started it.
                match error {
                    Some(e) => tracing::warn!("playback ended: {reason:?}: {e}"),
                    None => tracing::info!("playback ended: {reason:?}"),
                }
                stop_playback = true;
            } else if let Some((reason, error)) = &pb.ended {
                systemd.stopping();
                if bench_end.is_some() {
                    let why = error.as_deref().unwrap_or("no error");
                    if bench_reset_done {
                        println!("playback ended early: {reason:?} ({why})");
                        return finish_bench(opts, &stats, &base, p, r, playback.as_ref(), None);
                    }
                    // Never leave a benchmark run without output.
                    println!("bench: play {:?}", opts.play.as_deref().unwrap_or(""));
                    println!("FAIL: playback ended before measurement started: {reason:?} ({why})");
                    println!("result: FAIL");
                    return Exit::BenchFailed;
                }
                return if error.is_some() { Exit::Error } else { Exit::Quit };
            } else if matches!(opts.bench, Some(Bench::Play(_))) && now >= bench_osd_next {
                bench_osd_next = now + BENCH_OSD_PERIOD;
                pb.osd.toggle(now);
                dirty = true;
            }
            // Video advancing: draw every vsync. Otherwise redraw only when
            // the overlay appears or disappears (e.g. auto-hide while paused).
            let overlay = pb.osd.any_visible(now, &pb.player.state);
            dirty |= pb.playing() || overlay != overlay_shown;
            overlay_shown = overlay;
        } else {
            if let Some(a) = repeat.tick(now) {
                dirty |= view.on_action(a, now);
            }
            if let (Some(s), View::Home(h)) = (script.as_mut(), &mut view) {
                let (home, grid) = h.bench_state();
                if let Some(a) = s.tick(now, home, grid) {
                    dirty |= h.on_action(a, now);
                }
            }
            dirty |= view.update(now);
            p.set_text_input(view.wants_text());
            if let View::Home(h) = &mut view
                && let Some(req) = h.take_play()
            {
                let popts = jellyroll_video::PlayerOptions {
                    http_header: Some(format!("Authorization: {}", req.authorization)),
                    start_secs: Some(req.start_secs),
                    ..Default::default()
                };
                match Playback::start_with(p, &req.url, &req.title, popts, now) {
                    Ok(pb) => {
                        playback = Some(pb);
                        overlay_shown = false;
                        repeat.reset();
                        dirty = true;
                    }
                    Err(e) => tracing::error!("playback: {e}"),
                }
            }
            if view.is_animating(now) {
                dirty = true;
            }
        }
        // Starting to load something (requests, image wants) allocates;
        // such a frame is not a steady-state frame (D40).
        let load_allocs = if view.load_work() != work_start {
            alloc_counter::allocations() - allocs_start
        } else {
            0
        };
        if std::mem::take(&mut stop_playback) {
            playback = None;
            // mpv changed GL state; Skia must not trust its cache.
            r.reset_gl_state();
            repeat.reset();
            if let View::Home(h) = &mut view {
                h.playback_finished(now);
            }
            dirty = true;
        }
        if !dirty {
            continue;
        }

        // ---- draw ------------------------------------------------------
        let timing = gpu_timer.as_mut().is_some_and(|t| t.begin(p.gl()));
        let mut hud_allocs = load_allocs;
        let mut loading = load_allocs > 0;
        if let Some(pb) = playback.as_mut() {
            // mpv needs default GL state; Skia must then forget its cache.
            p.gl().reset_to_defaults();
            pb.player.render(fb);
            r.reset_gl_state();
            let submit_start = Instant::now();
            stats.record_ms.push(ms(submit_start - frame_start));
            if (pb.osd.any_visible(now, &pb.player.state) || hud.visible) && r.begin_frame(fb) {
                // OSD text (time) and HUD lines change while playing; keep
                // their shaping out of the steady-state allocation check.
                let a0 = alloc_counter::allocations();
                pb.osd.draw(r, now, &pb.player.state, &pb.drops, safe_area);
                let (lw, lh) = r.logical_size();
                hud.draw(r, &stats, lw * (1.0 - safe_area), lh * safe_area);
                hud_allocs = alloc_counter::allocations() - a0;
                r.end_frame();
            }
            stats.submit_ms.push(ms(submit_start.elapsed()));
        } else if r.begin_frame(fb) {
            match &mut view {
                View::Demo(scene) => {
                    if scene.uploads_pending() {
                        scene.pump_uploads(r, fb.1, UPLOAD_BUDGET);
                    }
                    scene.set_viewport(r.logical_size().0, safe_area);
                    scene.draw(r, now, safe_area, &header, notice);
                }
                View::Home(h) => {
                    // Texture uploads and shaping new text allocate (once
                    // per image or string); keep them out of the
                    // steady-state check like the HUD.
                    let a0 = alloc_counter::allocations();
                    let uploading = !h.settled();
                    let misses = r.stats().text_cache_misses;
                    h.draw(r, now, notice);
                    let shaped = r.stats().text_cache_misses - misses;
                    if bench_reset_done {
                        if h.last_upload_ms > 0.0 {
                            stats.upload_ms.push(h.last_upload_ms);
                        }
                        if shaped > 0 {
                            stats.new_text.push(shaped as f32);
                        }
                    }
                    if uploading || r.stats().text_cache_misses != misses {
                        let n = alloc_counter::allocations() - a0;
                        loading |= n > 0;
                        hud_allocs += n;
                    }
                }
            }
            // HUD text changes every refresh and is re-shaped then; keep its
            // allocations out of the steady-state check.
            let a0 = alloc_counter::allocations();
            let (lw, lh) = r.logical_size();
            hud.draw(r, &stats, lw * (1.0 - safe_area), lh * safe_area);
            hud_allocs += alloc_counter::allocations() - a0;
            let submit_start = Instant::now();
            stats.record_ms.push(ms(submit_start - frame_start));
            r.end_frame();
            stats.submit_ms.push(ms(submit_start.elapsed()));
        }
        if timing && let Some(t) = gpu_timer.as_mut() {
            t.end(p.gl());
        }
        let cpu = frame_start.elapsed();

        // ---- present -----------------------------------------------------
        p.swap();
        let presented = Instant::now();
        if let Some(pb) = playback.as_mut() {
            pb.player.report_swap();
        }
        dirty = false;

        if let Some(prev) = last_present {
            let interval = ms(presented - prev);
            stats.present_ms.push(interval);
            if !opts.no_vsync && is_missed_vsync(interval, refresh_hz) {
                stats.missed_vsyncs += 1;
                tracing::debug!(interval_ms = interval, "missed vsync");
            }
        }
        last_present = Some(presented);
        stats.cpu_ms.push(ms(cpu));
        stats.frames += 1;
        base.frames += 1;
        if let Some(t) = gpu_timer.as_mut() {
            t.collect(p.gl(), |ns| stats.gpu_ms.push(ns as f32 / 1.0e6));
        }

        let allocs = alloc_counter::allocations() - allocs_start - hud_allocs;
        stats.allocs_last_frame = allocs;
        if bench_reset_done {
            if loading {
                stats.loading_cpu_ms.push(ms(cpu));
            } else {
                stats.steady_cpu_ms.push(ms(cpu));
            }
        }
        if loading && bench_reset_done {
            stats.loading_frames += 1;
        } else if allocs > 0 && bench_reset_done {
            stats.allocating_frames += 1;
            if cfg!(debug_assertions) && !alloc_warned {
                alloc_warned = true;
                tracing::warn!(allocs, "steady-state frame allocated (reported once)");
            }
        }

        if first_frame {
            first_frame = false;
            let t = process_start.elapsed();
            tracing::info!(ms = t.as_millis() as u64, "first frame presented");
            systemd.ready(t);
            p.start_input_devices();
        }
    }
}

fn finish_bench<P: Platform, R: Renderer>(
    opts: &Options,
    stats: &FrameStats,
    base: &BenchBase,
    p: &P,
    r: &R,
    playback: Option<&Playback>,
    script: Option<&crate::bench_scroll::Script>,
) -> Exit {
    let mut scratch = Box::new([0.0f32; crate::perf::HISTORY]);
    let info = p.info();
    println!("gl.renderer: {}", info.gl_renderer);
    println!("video_driver: {}", info.video_driver);
    let (w, h) = p.framebuffer_size();
    println!("output: {w}x{h} @ {:.3} Hz", p.refresh_hz());
    let mut pass = true;
    match opts.bench {
        Some(b @ (Bench::Pacing(d) | Bench::Scroll(d))) => {
            let cpu = stats.cpu_ms.percentiles(&mut scratch);
            let gpu = stats.gpu_ms.percentiles(&mut scratch);
            let present = stats.present_ms.percentiles(&mut scratch);
            if matches!(b, Bench::Scroll(_)) {
                println!("bench: scroll {:.1}s", d.as_secs_f32());
                println!("items: {}", crate::bench_scroll::ITEMS);
                println!("presses: {}", script.map_or(0, |s| s.presses));
            } else {
                println!("bench: pacing {:.1}s", d.as_secs_f32());
            }
            println!("frames: {}", stats.frames);
            println!("cpu_ms: p50 {:.3} p99 {:.3} max {:.3}", cpu.p50, cpu.p99, cpu.max);
            let rec = stats.record_ms.percentiles(&mut scratch);
            let sub = stats.submit_ms.percentiles(&mut scratch);
            println!("  record_ms: p50 {:.3} p99 {:.3} max {:.3}", rec.p50, rec.p99, rec.max);
            println!("  submit_ms: p50 {:.3} p99 {:.3} max {:.3}", sub.p50, sub.p99, sub.max);
            if info.gpu_timer_queries {
                println!("gpu_ms: p50 {:.3} p99 {:.3} max {:.3}", gpu.p50, gpu.p99, gpu.max);
            } else {
                println!("gpu_ms: n/a (no timer queries)");
            }
            println!(
                "present_ms: p50 {:.3} p99 {:.3} max {:.3}",
                present.p50, present.p99, present.max
            );
            println!("missed_vsyncs: {}", stats.missed_vsyncs);
            println!("allocating_frames: {}", stats.allocating_frames);
            println!(
                "loading_frames: {} (not steady state; not counted above)",
                stats.loading_frames
            );
            let mut pct = |ring: &crate::perf::Ring<{ crate::perf::HISTORY }>| {
                let q = ring.percentiles(&mut scratch);
                format!("p50 {:.3} p99 {:.3} max {:.3}", q.p50, q.p99, q.max)
            };
            println!("  cpu_ms, steady frames: {}", pct(&stats.steady_cpu_ms));
            println!("  cpu_ms, loading frames: {}", pct(&stats.loading_cpu_ms));
            println!("  upload_ms, frames that uploaded: {}", pct(&stats.upload_ms));
            println!("  strings shaped, frames that shaped: {}", pct(&stats.new_text));
            let rs = r.stats();
            println!(
                "render: textures {:.1} MB ({} images), text cache {} entries ({} hits / {} misses), shadows {}",
                rs.texture_bytes as f64 / (1024.0 * 1024.0),
                rs.images,
                rs.text_cache_entries,
                rs.text_cache_hits,
                rs.text_cache_misses,
                rs.shadow_cache_entries
            );
            println!(
                "skia cache: {:.1} of {:.1} MB",
                rs.gpu_cache_bytes as f64 / (1024.0 * 1024.0),
                rs.gpu_cache_limit as f64 / (1024.0 * 1024.0)
            );
            let mut check = |ok: bool, what: &str| {
                if !ok {
                    println!("FAIL: {what}");
                    pass = false;
                }
            };
            check(cpu.p99 < CPU_BUDGET_MS, "cpu p99 >= 4 ms");
            check(!info.gpu_timer_queries || gpu.p99 < GPU_BUDGET_MS, "gpu p99 >= 6 ms");
            check(opts.no_vsync || stats.missed_vsyncs == 0, "missed vsyncs");
            check(stats.allocating_frames == 0, "steady-state frames allocated");
            check(stats.frames > 0, "no frames rendered");
            check(script.is_none_or(|s| s.presses > 0), "never scrolled");
        }
        Some(Bench::Play(d)) => {
            let Some(pb) = playback else { return Exit::Error };
            let s = &pb.player.state;
            let drops = pb.player.frame_drops();
            let pct = |a: Option<Duration>, b: Option<Duration>| {
                a.zip(b).map(|(a, b)| 100.0 * b.saturating_sub(a).as_secs_f64() / d.as_secs_f64())
            };
            let process = pct(base.process_cpu, crate::perf::process_cpu_time());
            let render = pct(base.thread_cpu, crate::perf::thread_cpu_time());
            let present = stats.present_ms.percentiles(&mut scratch);
            let cpu = stats.cpu_ms.percentiles(&mut scratch);
            println!("bench: play {:.1}s", d.as_secs_f32());
            println!(
                "video: {} {}x{} @ {:.3} fps, audio {}",
                s.video_codec, s.width, s.height, s.fps, s.audio_codec
            );
            let hw = if s.software_decode() { "SOFTWARE" } else { s.hwdec.as_str() };
            println!("decoder: {hw}");
            println!("frames_presented: {}", stats.frames);
            println!(
                "present_ms: p50 {:.3} p99 {:.3} max {:.3}",
                present.p50, present.p99, present.max
            );
            // Presents follow the video's frame cadence (e.g. ~41.7 ms for
            // 24p on a 60 Hz output) until refresh-rate matching (M8), so
            // vsync "misses" are not a playback metric; mpv's dropped,
            // delayed and mistimed counters below are.
            println!(
                "display_vsyncs_skipped: {} (expected when video fps != refresh)",
                stats.missed_vsyncs
            );
            // Includes time mpv blocks until a frame's display time.
            println!(
                "render_thread_frame_ms (incl. waiting): p50 {:.3} p99 {:.3}",
                cpu.p50, cpu.p99
            );
            println!(
                "mpv_dropped: vo {} decoder {} (delayed {} mistimed {})",
                drops.vo - base.drops.vo,
                drops.decoder - base.drops.decoder,
                drops.delayed - base.drops.delayed,
                drops.mistimed - base.drops.mistimed
            );
            let fmt = |v: Option<f64>| v.map_or("unknown".into(), |v| format!("{v:.1}"));
            println!("cpu_percent_of_one_core: process {} (incl. decode, audio)", fmt(process));
            println!("cpu_percent_of_one_core: render thread {}", fmt(render));
            println!("allocating_frames: {}", stats.allocating_frames);
            let mut check = |ok: bool, what: &str| {
                if !ok {
                    println!("FAIL: {what}");
                    pass = false;
                }
            };
            check(!s.software_decode(), "software decoding (no hardware decoder for this video)");
            check(drops.vo - base.drops.vo == 0, "frames dropped by video output");
            check(drops.decoder - base.drops.decoder == 0, "frames dropped by decoder");
            check(drops.delayed - base.drops.delayed == 0, "frames shown late");
            check(stats.allocating_frames == 0, "steady-state frames allocated");
            check(stats.frames > 0, "no frames presented");
        }
        Some(Bench::Idle(d)) => {
            let frames = base.frames;
            let cpu = base
                .process_cpu
                .zip(crate::perf::process_cpu_time())
                .map(|(a, b)| b.saturating_sub(a));
            let pct = cpu.map(|c| 100.0 * c.as_secs_f64() / d.as_secs_f64());
            println!("bench: idle {:.1}s", d.as_secs_f32());
            println!("frames_rendered: {frames}");
            match pct {
                Some(p) => println!("cpu_percent_of_one_core: {p:.3}"),
                None => println!("cpu_percent_of_one_core: unknown"),
            }
            if frames != 0 {
                println!("FAIL: redrew while idle");
                pass = false;
            }
            if pct.is_some_and(|p| p >= 1.0) {
                println!("FAIL: idle cpu >= 1%");
                pass = false;
            }
        }
        None => {}
    }
    println!("result: {}", if pass { "PASS" } else { "FAIL" });
    if pass { Exit::BenchPassed } else { Exit::BenchFailed }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_waits_until_deadline() {
        let now = Instant::now();
        assert_eq!(wait_timeout(true, false, None, now), Duration::ZERO);
        assert_eq!(wait_timeout(false, true, None, now), Duration::ZERO);
        assert_eq!(wait_timeout(false, false, None, now), MAX_WAIT);
        let d = Duration::from_millis(250);
        assert_eq!(wait_timeout(false, false, Some(now + d), now), d);
        // Overdue deadline: don't sleep.
        assert_eq!(wait_timeout(false, false, Some(now), now + d), Duration::ZERO);
    }

    #[test]
    fn earliest_deadline() {
        let t = Instant::now();
        let later = t + Duration::from_secs(1);
        assert_eq!(earliest(Some(later), Some(t)), Some(t));
        assert_eq!(earliest(None, Some(t)), Some(t));
        assert_eq!(earliest(None, None), None);
    }
}
