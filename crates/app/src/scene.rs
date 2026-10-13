//! M1 test pattern: a focusable tile grid with an animated focus frame, a
//! safe-area outline, and (for pacing tests) a constantly sweeping bar that
//! makes judder and tearing visible. Replaced by the real UI from M2/M6.

use std::time::{Duration, Instant};

use crate::anim::Tween;
use crate::draw::{Color, RectBatch, rgb};
use crate::input::InputAction;

const COLS: i32 = 6;
const ROWS: i32 = 2;
const FOCUS_ANIM: Duration = Duration::from_millis(150);
const SWEEP_PERIOD: Duration = Duration::from_secs(2);

const BG: Color = rgb(16, 20, 32);
const SAFE: Color = rgb(40, 48, 70);
const FOCUS: Color = rgb(240, 240, 240);
const SWEEP: Color = rgb(250, 250, 250);
const HEADER: Color = rgb(200, 205, 220);
const NOTICE: Color = rgb(240, 180, 60);

pub struct Scene {
    focus: (i32, i32),
    fx: Tween,
    fy: Tween,
    selected: [bool; (COLS * ROWS) as usize],
    /// Continuous animation for pacing benchmarks.
    pub sweep: bool,
    start: Instant,
}

/// Pixel geometry for one frame.
pub struct Layout {
    pub scale: f32,
    pub safe_x: i32,
    pub safe_y: i32,
    pub safe_w: i32,
    pub safe_h: i32,
}

impl Layout {
    /// Logical units: 1080 logical px = output height (spec 6.4).
    /// `safe_area`: fraction of each edge kept clear for TV overscan
    /// (spec 10.2, from config).
    pub fn new(w: u32, h: u32, safe_area: f32) -> Self {
        let scale = h as f32 / 1080.0;
        let safe_x = (w as f32 * safe_area) as i32;
        let safe_y = (h as f32 * safe_area) as i32;
        Self { scale, safe_x, safe_y, safe_w: w as i32 - 2 * safe_x, safe_h: h as i32 - 2 * safe_y }
    }
}

fn tile_color(i: usize, selected: bool) -> Color {
    // Muted hue wheel; selected tiles brighten.
    const PALETTE: [Color; 6] = [
        rgb(70, 90, 160),
        rgb(60, 130, 140),
        rgb(80, 140, 90),
        rgb(150, 130, 70),
        rgb(150, 80, 80),
        rgb(120, 80, 150),
    ];
    let c = PALETTE[i % PALETTE.len()];
    if selected { [c[0] * 1.5, c[1] * 1.5, c[2] * 1.5, 1.0] } else { c }
}

impl Scene {
    pub fn new(now: Instant) -> Self {
        Self {
            focus: (0, 0),
            fx: Tween::settled(0.0, now),
            fy: Tween::settled(0.0, now),
            selected: Default::default(),
            sweep: false,
            start: now,
        }
    }

    /// Applies an action; returns true if anything visible changed.
    pub fn on_action(&mut self, a: InputAction, now: Instant) -> bool {
        let (mut c, mut r) = self.focus;
        match a {
            InputAction::Left => c -= 1,
            InputAction::Right => c += 1,
            InputAction::Up => r -= 1,
            InputAction::Down => r += 1,
            InputAction::Select => {
                let i = (r * COLS + c) as usize;
                self.selected[i] = !self.selected[i];
                return true;
            }
            _ => return false,
        }
        let next = (c.clamp(0, COLS - 1), r.clamp(0, ROWS - 1));
        if next == self.focus {
            return false;
        }
        self.focus = next;
        self.fx.retarget(next.0 as f32, now, FOCUS_ANIM);
        self.fy.retarget(next.1 as f32, now, FOCUS_ANIM);
        true
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        self.sweep || self.fx.is_animating(now) || self.fy.is_animating(now)
    }

    pub fn draw(
        &self,
        b: &mut RectBatch,
        fb: (u32, u32),
        l: &Layout,
        now: Instant,
        header: &str,
        notice: &str,
    ) {
        let (w, h) = (fb.0 as i32, fb.1 as i32);
        let s = l.scale;
        b.rect(0, 0, w, h, BG);
        b.frame(l.safe_x, l.safe_y, l.safe_w, l.safe_h, (2.0 * s).max(1.0) as i32, SAFE);

        let text_px = (4.0 * s).round().max(1.0) as i32;
        let text_x = l.safe_x + (40.0 * s) as i32;
        b.text(text_x, l.safe_y + (40.0 * s) as i32, text_px, HEADER, header);

        // Poster-like 2:3 tiles, centered in the safe area.
        let gap = (32.0 * s) as i32;
        let tile_w = (220.0 * s) as i32;
        let tile_h = tile_w * 3 / 2;
        let grid_w = COLS * tile_w + (COLS - 1) * gap;
        let grid_h = ROWS * tile_h + (ROWS - 1) * gap;
        let ox = l.safe_x + (l.safe_w - grid_w) / 2;
        let oy = l.safe_y + (l.safe_h - grid_h) / 2;

        // Focus frame sits behind the tiles and glides between them.
        let fx = self.fx.value(now);
        let fy = self.fy.value(now);
        let border = (8.0 * s) as i32;
        b.rect(
            ox + (fx * (tile_w + gap) as f32) as i32 - border,
            oy + (fy * (tile_h + gap) as f32) as i32 - border,
            tile_w + 2 * border,
            tile_h + 2 * border,
            FOCUS,
        );
        for r in 0..ROWS {
            for c in 0..COLS {
                let i = (r * COLS + c) as usize;
                b.rect(
                    ox + c * (tile_w + gap),
                    oy + r * (tile_h + gap),
                    tile_w,
                    tile_h,
                    tile_color(i, self.selected[i]),
                );
            }
        }

        if self.sweep {
            let period = SWEEP_PERIOD.as_secs_f32();
            let t = now.saturating_duration_since(self.start).as_secs_f32() % period / period;
            let bar_w = (24.0 * s) as i32;
            let x = (t * (w - bar_w) as f32) as i32;
            b.rect(x, 0, bar_w, h, SWEEP);
        }

        // Drawn last so nothing covers it; bottom-left of the safe area.
        if !notice.is_empty() {
            let y = l.safe_y + l.safe_h - (40.0 * s) as i32 - 5 * text_px;
            b.text(text_x, y, text_px, NOTICE, notice);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_moves_and_clamps() {
        let t0 = Instant::now();
        let mut s = Scene::new(t0);
        assert!(!s.on_action(InputAction::Left, t0));
        assert!(!s.on_action(InputAction::Up, t0));
        assert!(s.on_action(InputAction::Right, t0));
        assert!(s.is_animating(t0 + Duration::from_millis(10)));
        assert!(!s.is_animating(t0 + Duration::from_millis(200)));
        for _ in 0..10 {
            s.on_action(InputAction::Right, t0);
        }
        assert_eq!(s.focus, (COLS - 1, 0));
        assert!(s.on_action(InputAction::Down, t0));
        assert!(!s.on_action(InputAction::Down, t0));
        assert!(s.on_action(InputAction::Select, t0));
        assert!(!s.on_action(InputAction::Info, t0));
    }

    #[test]
    fn layout_scale() {
        let l = Layout::new(3840, 2160, 0.025);
        assert_eq!(l.scale, 2.0);
        assert_eq!(l.safe_x, 96);
        assert_eq!(l.safe_y, 54);
    }

    #[test]
    fn draw_stays_on_screen_at_1080p_and_4k() {
        let t0 = Instant::now();
        let s = Scene::new(t0);
        for (w, h) in [(1920u32, 1080u32), (3840, 2160), (1280, 720)] {
            let mut b = RectBatch::with_capacity(256);
            s.draw(&mut b, (w, h), &Layout::new(w, h, 0.1), t0, "TEST", "NOTICE");
            for (r, _) in b.items() {
                assert!(r.x >= 0 && r.y >= 0, "{r:?}");
                assert!(r.x + r.w <= w as i32 && r.y + r.h <= h as i32, "{r:?} in {w}x{h}");
            }
        }
    }
}
