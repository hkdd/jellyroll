//! Perf HUD (spec 13), M1 edition: numbers + CPU frame-time graph drawn with
//! the stopgap rect batch. Toggle with F3.

use std::fmt::Write;
use std::time::{Duration, Instant};

use jf_platform::PlatformInfo;

use crate::draw::{Color, LINE_HEIGHT, Line, RectBatch, rgb};
use crate::perf::{FrameStats, HISTORY};

const LINES: usize = 7;
const GRAPH_SAMPLES: usize = 180;
const GRAPH_MAX_MS: f32 = 20.0;
pub const REFRESH: Duration = Duration::from_millis(500);

const BG: Color = rgb(10, 10, 14);
const FG: Color = rgb(230, 230, 230);
const GOOD: Color = rgb(80, 200, 120);
const WARN: Color = rgb(230, 190, 60);
const BAD: Color = rgb(230, 70, 70);
const BUDGET_LINE: Color = rgb(70, 140, 230);
const PERIOD_LINE: Color = rgb(120, 120, 120);

/// CPU frame budget from spec 2 (p99 < 4 ms).
pub const CPU_BUDGET_MS: f32 = 4.0;
/// GPU frame budget for UI-only frames from spec 2 (p99 < 6 ms at 4K).
pub const GPU_BUDGET_MS: f32 = 6.0;

pub struct Hud {
    pub visible: bool,
    lines: [Line<72>; LINES],
    next_refresh: Option<Instant>,
    scratch: Box<[f32; HISTORY]>,
}

impl Hud {
    pub fn new(visible: bool, now: Instant) -> Self {
        Self {
            visible,
            lines: Default::default(),
            next_refresh: visible.then_some(now),
            scratch: Box::new([0.0; HISTORY]),
        }
    }

    pub fn toggle(&mut self, now: Instant) {
        self.visible = !self.visible;
        self.next_refresh = self.visible.then_some(now);
    }

    /// When the HUD next needs a repaint (only while visible).
    pub fn next_deadline(&self) -> Option<Instant> {
        if self.visible { self.next_refresh } else { None }
    }

    pub fn due(&self, now: Instant) -> bool {
        self.next_deadline().is_some_and(|t| now >= t)
    }

    /// Rebuilds the text lines. Called at most every `REFRESH`, outside the
    /// measured frame (it reads /proc).
    pub fn refresh(
        &mut self,
        now: Instant,
        stats: &FrameStats,
        info: &PlatformInfo,
        refresh_hz: f32,
        fb: (u32, u32),
    ) {
        self.next_refresh = Some(now + REFRESH);
        let cpu = stats.cpu_ms.percentiles(&mut self.scratch);
        let gpu = stats.gpu_ms.percentiles(&mut self.scratch);
        let present = stats.present_ms.percentiles(&mut self.scratch);
        let rss_mb = crate::perf::rss_bytes().unwrap_or(0) as f64 / (1024.0 * 1024.0);
        for l in &mut self.lines {
            l.clear();
        }
        let [l0, l1, l2, l3, l4, l5, l6] = &mut self.lines;
        let _ = write!(l0, "{}X{} @ {:.2} HZ  {}", fb.0, fb.1, refresh_hz, info.video_driver);
        let _ = write!(l1, "CPU MS  P50 {:5.2}  P99 {:5.2}  MAX {:5.2}", cpu.p50, cpu.p99, cpu.max);
        if info.gpu_timer_queries {
            let _ =
                write!(l2, "GPU MS  P50 {:5.2}  P99 {:5.2}  MAX {:5.2}", gpu.p50, gpu.p99, gpu.max);
        } else {
            let _ = write!(l2, "GPU MS  N/A (NO TIMER QUERIES)");
        }
        let _ = write!(
            l3,
            "PRESENT P50 {:5.2}  P99 {:5.2}  MISSED {}",
            present.p50, present.p99, stats.missed_vsyncs
        );
        let _ = write!(
            l4,
            "FRAMES {}  ALLOC/FRAME {}  ALLOC FRAMES {}",
            stats.frames, stats.allocs_last_frame, stats.allocating_frames
        );
        let _ = write!(l5, "RSS {rss_mb:.1} MB  SWAP INTERVAL {}", info.swap_interval);
        let _ = write!(l6, "{}", info.gl_renderer);
    }

    /// Draws anchored to the top-right corner at (`right`, `y`).
    pub fn draw(&self, b: &mut RectBatch, stats: &FrameStats, right: i32, y: i32, scale: f32) {
        if !self.visible {
            return;
        }
        let px = ((3.0 * scale).round() as i32).max(2);
        let pad = 4 * px;
        let line_h = LINE_HEIGHT * px;
        let bar_w = ((2.0 * scale).round() as i32).max(1);
        let graph_w = GRAPH_SAMPLES as i32 * bar_w;
        let graph_h = (120.0 * scale) as i32;
        let text_w = self.lines.iter().map(|l| l.as_str().len() as i32).max().unwrap_or(0)
            * crate::draw::GLYPH_ADVANCE
            * px;
        let w = text_w.max(graph_w) + 2 * pad;
        let h = LINES as i32 * line_h + graph_h + 3 * pad;
        let x = (right - w).max(0);
        b.rect(x, y, w, h, BG);

        let mut ty = y + pad;
        for l in &self.lines {
            b.text(x + pad, ty, px, FG, l.as_str());
            ty += line_h;
        }

        // CPU frame-time graph, newest on the right.
        let gx = x + pad;
        let gy = ty + pad;
        let ms_to_px = graph_h as f32 / GRAPH_MAX_MS;
        let n = stats.cpu_ms.len();
        let skip = n.saturating_sub(GRAPH_SAMPLES);
        let offset = GRAPH_SAMPLES.saturating_sub(n) as i32;
        for (i, v) in stats.cpu_ms.iter().skip(skip).enumerate() {
            let bh = ((v.min(GRAPH_MAX_MS) * ms_to_px).ceil() as i32).max(1);
            let c = if v < CPU_BUDGET_MS {
                GOOD
            } else if v < 2.0 * CPU_BUDGET_MS {
                WARN
            } else {
                BAD
            };
            b.rect(gx + (offset + i as i32) * bar_w, gy + graph_h - bh, bar_w, bh, c);
        }
        let line = |b: &mut RectBatch, ms: f32, c: Color| {
            let ly = gy + graph_h - (ms * ms_to_px) as i32;
            b.rect(gx, ly, graph_w, 1.max(scale as i32), c);
        };
        line(b, CPU_BUDGET_MS, BUDGET_LINE);
        line(b, 1000.0 / 60.0, PERIOD_LINE);
    }
}
