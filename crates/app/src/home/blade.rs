//! The list picker: a panel that slides in from the right edge over a
//! dimmed page. Every choice among more than two values uses it (D53), so
//! a long list (audio outputs, genres) never has to be cycled through.
//! On/off settings are toggle buttons instead (`widgets::draw_toggle`).
//!
//! Up/Down move (and scroll), Select picks, Back closes without a change.
//! The current value is marked with an accent bar and focused on open.

use std::time::{Duration, Instant};

use jellyroll_render::{Color, Point, Rect, Renderer, TextStyle, Weight};

use super::theme::{ACCENT, BG, TEXT, TEXT_DIM};
use crate::anim::Tween;
use crate::input::InputAction;

const WIDTH: f32 = 560.0;
const ITEM_H: f32 = 56.0;
const SLIDE: Duration = Duration::from_millis(180);

#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// Still open.
    Stay,
    /// Picked option `i`; the blade is done.
    Picked(usize),
    /// Closed without a change.
    Closed,
}

pub struct Blade {
    title: String,
    options: Vec<String>,
    /// The current value, marked.
    current: Option<usize>,
    focus: usize,
    slide: Tween,
}

impl Blade {
    pub fn new(title: &str, options: Vec<String>, current: Option<usize>, now: Instant) -> Self {
        let mut slide = Tween::settled(0.0, now);
        slide.retarget(1.0, now, SLIDE);
        let current = current.filter(|c| *c < options.len());
        Self { title: title.into(), focus: current.unwrap_or(0), current, options, slide }
    }

    pub fn on_action(&mut self, a: InputAction) -> Outcome {
        let n = self.options.len();
        match a {
            InputAction::Up if self.focus > 0 => self.focus -= 1,
            // Wrap at the ends: from the top, Up reaches the bottom.
            InputAction::Up if n > 0 => self.focus = n - 1,
            InputAction::Down if self.focus + 1 < n => self.focus += 1,
            InputAction::Down => self.focus = 0,
            InputAction::Select if n > 0 => return Outcome::Picked(self.focus),
            InputAction::Back | InputAction::Left => return Outcome::Closed,
            _ => {}
        }
        Outcome::Stay
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        self.slide.is_animating(now)
    }

    pub fn draw<R: Renderer>(&self, r: &mut R, now: Instant, safe: f32) {
        let (lw, lh) = r.logical_size();
        let t = self.slide.value(now);
        r.fill_rect(Rect::new(0.0, 0.0, lw, lh), Color::rgba(0, 0, 0, (150.0 * t) as u8));
        let sy = lh * safe;
        let sx = lw * safe;
        let x = lw - (WIDTH + sx) * t;
        let panel = Rect::new(x, sy, WIDTH, lh - 2.0 * sy);
        r.fill_rrect(panel, 16.0, Color::rgba(28, 30, 38, 250));
        let title = TextStyle::new(28.0, Weight::Bold).max_width(WIDTH - 60.0);
        r.text(&self.title, &title, Point::new(x + 30.0, sy + 26.0), TEXT_DIM);
        let top = sy + 84.0;
        let visible = (((panel.bottom() - 20.0 - top) / ITEM_H) as usize).max(1);
        let n = self.options.len();
        let first = self.focus.saturating_sub(visible / 2).min(n.saturating_sub(visible));
        let st = TextStyle::new(24.0, Weight::Regular).max_width(WIDTH - 80.0);
        for (k, opt) in self.options.iter().enumerate().skip(first).take(visible) {
            let y = top + (k - first) as f32 * ITEM_H;
            let rect = Rect::new(x + 16.0, y, WIDTH - 32.0, ITEM_H - 6.0);
            let focused = k == self.focus;
            if focused {
                r.fill_rrect(rect, 10.0, Color::WHITE);
            }
            if Some(k) == self.current {
                r.fill_rect(Rect::new(rect.x + 6.0, rect.y + 10.0, 4.0, rect.h - 20.0), ACCENT);
            }
            r.text(
                opt,
                &st,
                Point::new(rect.x + 22.0, rect.y + 10.0),
                if focused { BG } else { TEXT },
            );
        }
        // A long list: a scrollbar shows where in it the view is.
        if n > visible {
            let track_h = visible as f32 * ITEM_H - 6.0;
            let h = (track_h * visible as f32 / n as f32).max(24.0);
            let y = top + (track_h - h) * first as f32 / (n - visible) as f32;
            r.fill_rrect(Rect::new(panel.right() - 10.0, y, 4.0, h), 2.0, TEXT_DIM);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blade(n: usize, current: Option<usize>) -> Blade {
        let opts = (0..n).map(|i| format!("Option {i}")).collect();
        Blade::new("Pick", opts, current, Instant::now())
    }

    #[test]
    fn opens_on_the_current_value_and_picks() {
        let mut b = blade(12, Some(7));
        assert_eq!(b.focus, 7);
        b.on_action(InputAction::Down);
        assert_eq!(b.on_action(InputAction::Select), Outcome::Picked(8));
    }

    #[test]
    fn wraps_and_closes() {
        let mut b = blade(3, None);
        assert_eq!(b.focus, 0);
        b.on_action(InputAction::Up);
        assert_eq!(b.focus, 2, "Up from the top reaches the bottom");
        b.on_action(InputAction::Down);
        assert_eq!(b.focus, 0);
        assert_eq!(b.on_action(InputAction::Back), Outcome::Closed);
        assert_eq!(b.on_action(InputAction::Left), Outcome::Closed);
    }

    #[test]
    fn empty_list_is_harmless() {
        let mut b = blade(0, Some(3));
        assert_eq!(b.on_action(InputAction::Select), Outcome::Stay);
        assert_eq!(b.on_action(InputAction::Down), Outcome::Stay);
    }
}
