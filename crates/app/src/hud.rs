//! Perf HUD (spec 13): frame timing, render-cache stats and a CPU
//! frame-time graph, drawn through the Renderer. Toggle with F3.

use std::fmt::Write;
use std::time::{Duration, Instant};

use jellyroll_platform::PlatformInfo;
use jellyroll_render::{Color, Point, Rect, RenderStats, Renderer, TextStyle, Weight};

use crate::perf::{FrameStats, HISTORY};

const LINES: usize = 8;
const GRAPH_SAMPLES: usize = 180;
const GRAPH_MAX_MS: f32 = 20.0;
pub const REFRESH: Duration = Duration::from_millis(500);

const BG: Color = Color::rgba(8, 8, 12, 225);
const FG: Color = Color::rgb(230, 230, 230);
const GOOD: Color = Color::rgb(80, 200, 120);
const WARN: Color = Color::rgb(230, 190, 60);
const BAD: Color = Color::rgb(230, 70, 70);
const BUDGET_LINE: Color = Color::rgb(70, 140, 230);
const PERIOD_LINE: Color = Color::rgb(120, 120, 120);

/// CPU frame budget from spec 2 (p99 < 4 ms).
pub const CPU_BUDGET_MS: f32 = 4.0;
/// GPU frame budget for UI-only frames from spec 2 (p99 < 6 ms at 4K).
pub const GPU_BUDGET_MS: f32 = 6.0;

/// Fixed-capacity text line; `core::fmt` into it does not allocate.
pub struct Line<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> Default for Line<N> {
    fn default() -> Self {
        Self { buf: [0; N], len: 0 }
    }
}

impl<const N: usize> Line<N> {
    pub fn clear(&mut self) {
        self.len = 0;
    }
    pub fn as_str(&self) -> &str {
        // Only whole &str fragments are appended, so this is valid UTF-8.
        std::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl<const N: usize> std::fmt::Write for Line<N> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        // Truncate on a char boundary rather than failing.
        let mut take = s.len().min(N - self.len);
        while !s.is_char_boundary(take) {
            take -= 1;
        }
        self.buf[self.len..self.len + take].copy_from_slice(&s.as_bytes()[..take]);
        self.len += take;
        Ok(())
    }
}

pub struct Hud {
    pub visible: bool,
    lines: [Line<96>; LINES],
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
        render: &RenderStats,
        info: &PlatformInfo,
        refresh_hz: f32,
        fb: (u32, u32),
    ) {
        self.next_refresh = Some(now + REFRESH);
        let cpu = stats.cpu_ms.percentiles(&mut self.scratch);
        let gpu = stats.gpu_ms.percentiles(&mut self.scratch);
        let present = stats.present_ms.percentiles(&mut self.scratch);
        let rss_mb = crate::perf::rss_bytes().unwrap_or(0) as f64 / (1024.0 * 1024.0);
        let lookups = render.text_cache_hits + render.text_cache_misses;
        let hit_pct = if lookups == 0 {
            100.0
        } else {
            100.0 * render.text_cache_hits as f64 / lookups as f64
        };
        for l in &mut self.lines {
            l.clear();
        }
        let [l0, l1, l2, l3, l4, l5, l6, l7] = &mut self.lines;
        let _ = write!(l0, "{}×{} @ {:.2} Hz  {}", fb.0, fb.1, refresh_hz, info.video_driver);
        let _ = write!(l1, "CPU ms  p50 {:5.2}  p99 {:5.2}  max {:5.2}", cpu.p50, cpu.p99, cpu.max);
        if info.gpu_timer_queries {
            let _ =
                write!(l2, "GPU ms  p50 {:5.2}  p99 {:5.2}  max {:5.2}", gpu.p50, gpu.p99, gpu.max);
        } else {
            let _ = write!(l2, "GPU ms  n/a (no timer queries)");
        }
        let _ = write!(
            l3,
            "present p50 {:5.2}  p99 {:5.2}  missed {}",
            present.p50, present.p99, stats.missed_vsyncs
        );
        let _ = write!(
            l4,
            "frames {}  alloc/frame {}  alloc frames {}",
            stats.frames, stats.allocs_last_frame, stats.allocating_frames
        );
        let _ = write!(
            l5,
            "textures {:.1} MB ({})  shadows {}",
            render.texture_bytes as f64 / (1024.0 * 1024.0),
            render.images,
            render.shadow_cache_entries
        );
        let _ = write!(
            l6,
            "text cache {} entries, {hit_pct:.1}% hits  RSS {rss_mb:.0} MB",
            render.text_cache_entries
        );
        let _ = write!(l7, "{}", info.gl_renderer);
    }

    /// Draws anchored to the top-right corner at (`right`, `y`), logical px.
    pub fn draw<R: Renderer>(&self, r: &mut R, stats: &FrameStats, right: f32, y: f32) {
        if !self.visible {
            return;
        }
        let style = TextStyle::new(15.0, Weight::Regular);
        let pad = 12.0;
        let line_h = 21.0;
        let bar_w = 2.0;
        let graph_w = GRAPH_SAMPLES as f32 * bar_w;
        let graph_h = 80.0;
        let w = 520.0_f32.max(graph_w + 2.0 * pad);
        let h = LINES as f32 * line_h + graph_h + 3.0 * pad;
        let x = right - w;
        r.fill_rrect(Rect::new(x, y, w, h), 8.0, BG);

        let mut ty = y + pad;
        for l in &self.lines {
            r.text(l.as_str(), &style, Point::new(x + pad, ty), FG);
            ty += line_h;
        }

        // CPU frame-time graph, newest on the right.
        let gx = x + pad;
        let gy = ty + pad;
        let ms_to_px = graph_h / GRAPH_MAX_MS;
        let n = stats.cpu_ms.len();
        let skip = n.saturating_sub(GRAPH_SAMPLES);
        let offset = GRAPH_SAMPLES.saturating_sub(n) as f32;
        for (i, v) in stats.cpu_ms.iter().skip(skip).enumerate() {
            let bh = (v.min(GRAPH_MAX_MS) * ms_to_px).max(1.0);
            let c = if v < CPU_BUDGET_MS {
                GOOD
            } else if v < 2.0 * CPU_BUDGET_MS {
                WARN
            } else {
                BAD
            };
            r.fill_rect(
                Rect::new(gx + (offset + i as f32) * bar_w, gy + graph_h - bh, bar_w, bh),
                c,
            );
        }
        for (ms, c) in [(CPU_BUDGET_MS, BUDGET_LINE), (1000.0 / 60.0, PERIOD_LINE)] {
            r.fill_rect(Rect::new(gx, gy + graph_h - ms * ms_to_px, graph_w, 1.0), c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_truncates_on_char_boundary() {
        let mut l: Line<4> = Line::default();
        write!(l, "abcdef").unwrap();
        assert_eq!(l.as_str(), "abcd");
        let mut l: Line<4> = Line::default();
        write!(l, "a×b").unwrap(); // '×' is 2 bytes
        assert_eq!(l.as_str(), "a×b");
        let mut l: Line<2> = Line::default();
        write!(l, "a×").unwrap();
        assert_eq!(l.as_str(), "a");
    }
}
