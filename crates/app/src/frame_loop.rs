//! The render loop (spec 4, 6.2). Single thread owns the GL context.
//!
//! Redraw only when dirty: input, a running animation, a HUD refresh, or an
//! expose/resize. Otherwise block in `wait_events` until the next deadline
//! (key repeat, HUD refresh, watchdog ping, benchmark end) with no page flip.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use jellyroll_platform::alloc_counter;
use jellyroll_platform::{GpuTimer, Platform, PlatformEvent};

use crate::args::{Bench, Options};
use crate::draw::RectBatch;
use crate::hud::{CPU_BUDGET_MS, GPU_BUDGET_MS, Hud};
use crate::input::{InputAction, KeyRepeat, map_key};
use crate::perf::{FrameStats, is_missed_vsync, ms};
use crate::scene::{Layout, Scene};
use crate::systemd::Systemd;

/// Frames before steady state (allocation checks and benchmark windows).
const WARMUP: Duration = Duration::from_secs(1);
/// Upper bound on a single wait so a lost wakeup cannot stall us forever.
const MAX_WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, PartialEq, Eq)]
pub enum Exit {
    Quit,
    BenchPassed,
    BenchFailed,
}

fn earliest(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn make_header(fb: (u32, u32), hz: f32, header: &mut String) {
    header.clear();
    let _ = write!(header, "JELLYROLL M1   {}X{} @ {:.2}   F3 HUD", fb.0, fb.1, hz);
}

/// On-screen notice for config problems (details are in the journal).
fn config_notice(warnings: usize) -> String {
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

pub fn run<P: Platform>(
    p: &mut P,
    opts: &Options,
    config: &crate::config::Loaded,
    process_start: Instant,
) -> Exit {
    let start = Instant::now();
    let mut systemd = Systemd::new(start);
    let mut scene = Scene::new(start);
    let mut hud = Hud::new(opts.hud, start);
    let mut repeat = KeyRepeat::new(config.config.input.repeat());
    let safe_area = config.config.display.safe_area;
    let notice = config_notice(config.warnings.len());
    let mut stats = FrameStats::default();
    let mut batch = RectBatch::with_capacity(4096);
    let mut events: Vec<PlatformEvent> = Vec::with_capacity(64);
    let mut gpu_timer = GpuTimer::new(p.gl());

    let mut fb = p.framebuffer_size();
    let mut refresh_hz = p.refresh_hz();
    let mut layout = Layout::new(fb.0, fb.1, safe_area);
    let mut header = String::new();
    make_header(fb, refresh_hz, &mut header);

    let bench_end = opts.bench.map(|b| match b {
        Bench::Pacing(d) | Bench::Idle(d) => start + WARMUP + d,
    });
    if let Some(Bench::Pacing(_)) = opts.bench {
        scene.sweep = true;
    }
    let mut bench_reset_done = false;
    let mut idle_cpu_at_warmup = None;
    let mut frames_at_warmup = 0;

    let mut dirty = true;
    let mut first_frame = true;
    let mut last_present: Option<Instant> = None;
    let mut alloc_warned = false;

    loop {
        // ---- wait ------------------------------------------------------
        let now = Instant::now();
        let deadline = earliest(
            earliest(repeat.next_deadline(), hud.next_deadline()),
            earliest(systemd.next_deadline(), bench_end),
        );
        let timeout = wait_timeout(dirty, scene.is_animating(now), deadline, now);
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
            hud.refresh(now, &stats, p.info(), refresh_hz, fb);
            dirty = true;
        }
        if !bench_reset_done && now >= start + WARMUP {
            bench_reset_done = true;
            stats.reset();
            idle_cpu_at_warmup = crate::perf::process_cpu_time();
            frames_at_warmup = 0;
        }
        if let Some(end) = bench_end
            && now >= end
        {
            return finish_bench(opts, &stats, idle_cpu_at_warmup, frames_at_warmup, p);
        }

        // ---- input + update (measured) --------------------------------
        let frame_start = Instant::now();
        let allocs_start = alloc_counter::allocations();
        for ev in &events {
            match *ev {
                PlatformEvent::Quit => {
                    systemd.stopping();
                    return Exit::Quit;
                }
                PlatformEvent::KeyDown { key, mods, repeat: os_repeat } => {
                    if os_repeat {
                        continue; // in-app repeat only (spec 5.4)
                    }
                    if key == jellyroll_platform::Key::F(3) {
                        hud.toggle(now);
                        dirty = true;
                        continue;
                    }
                    if let Some(a) = map_key(key, mods) {
                        repeat.press(key, a, now);
                        if a == InputAction::Back && opts.allow_exit {
                            systemd.stopping();
                            return Exit::Quit;
                        }
                        dirty |= scene.on_action(a, now);
                    }
                }
                PlatformEvent::KeyUp { key } => repeat.release(key),
                PlatformEvent::Resized { .. } | PlatformEvent::DisplayChanged => {
                    fb = p.framebuffer_size();
                    refresh_hz = p.refresh_hz();
                    layout = Layout::new(fb.0, fb.1, safe_area);
                    make_header(fb, refresh_hz, &mut header);
                    tracing::info!(w = fb.0, h = fb.1, refresh_hz, "output changed");
                    repeat.reset();
                    dirty = true;
                }
                PlatformEvent::Exposed => dirty = true,
            }
        }
        if let Some(a) = repeat.tick(now) {
            dirty |= scene.on_action(a, now);
        }
        if scene.is_animating(now) {
            dirty = true;
        }
        if !dirty {
            continue;
        }

        // ---- draw ------------------------------------------------------
        let gl = p.gl();
        gl.bind_default_framebuffer();
        gl.viewport(0, 0, fb.0 as i32, fb.1 as i32);
        let timing = gpu_timer.as_mut().is_some_and(|t| t.begin(gl));
        batch.clear();
        scene.draw(&mut batch, fb, &layout, now, &header, &notice);
        hud.draw(&mut batch, &stats, layout.safe_x + layout.safe_w, layout.safe_y, layout.scale);
        batch.flush(gl, fb.1 as i32);
        if timing && let Some(t) = gpu_timer.as_mut() {
            t.end(gl);
        }
        let cpu = frame_start.elapsed();

        // ---- present -----------------------------------------------------
        p.swap();
        let presented = Instant::now();
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
        frames_at_warmup += 1;
        if let Some(t) = gpu_timer.as_mut() {
            t.collect(p.gl(), |ns| stats.gpu_ms.push(ns as f32 / 1.0e6));
        }

        let allocs = alloc_counter::allocations() - allocs_start;
        stats.allocs_last_frame = allocs;
        if allocs > 0 && bench_reset_done {
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
        }
    }
}

fn finish_bench<P: Platform>(
    opts: &Options,
    stats: &FrameStats,
    cpu_at_warmup: Option<Duration>,
    frames: u64,
    p: &P,
) -> Exit {
    let mut scratch = Box::new([0.0f32; crate::perf::HISTORY]);
    let info = p.info();
    println!("gl.renderer: {}", info.gl_renderer);
    println!("video_driver: {}", info.video_driver);
    let (w, h) = p.framebuffer_size();
    println!("output: {w}x{h} @ {:.3} Hz", p.refresh_hz());
    let mut pass = true;
    match opts.bench {
        Some(Bench::Pacing(d)) => {
            let cpu = stats.cpu_ms.percentiles(&mut scratch);
            let gpu = stats.gpu_ms.percentiles(&mut scratch);
            let present = stats.present_ms.percentiles(&mut scratch);
            println!("bench: pacing {:.1}s", d.as_secs_f32());
            println!("frames: {}", stats.frames);
            println!("cpu_ms: p50 {:.3} p99 {:.3} max {:.3}", cpu.p50, cpu.p99, cpu.max);
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
        }
        Some(Bench::Idle(d)) => {
            let cpu = cpu_at_warmup
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
