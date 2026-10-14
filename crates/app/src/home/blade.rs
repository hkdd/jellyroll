//! The list picker: a panel that slides in from the right edge over a
//! dimmed page. Every choice among more than two values uses it (D53), so
//! a long list (audio outputs, genres) never has to be cycled through.
//! On/off settings are toggle buttons instead (`widgets::draw_toggle`).
//!
//! Rows are choices (Select picks one and closes), checkboxes (Select
//! ticks or unticks and the list stays open: multi-select, D55) or actions
//! (Select runs it and closes, e.g. "Clear all"). Up/Down move (wrapping,
//! scrolling), Back or Left closes; the caller then reads the checkboxes.
//! The current choice is marked with an accent bar.

use std::time::{Duration, Instant};

use jellyroll_render::{Color, Point, Rect, Renderer, TextStyle, Weight};

use super::theme::{ACCENT, BG, TEXT, TEXT_DIM};
use crate::anim::Tween;
use crate::input::InputAction;

const WIDTH: f32 = 560.0;
const ITEM_H: f32 = 56.0;
/// Extra space for the line between groups of rows.
const DIVIDER_H: f32 = 14.0;
const SLIDE: Duration = Duration::from_millis(180);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Choice,
    Check(bool),
    Action,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub label: String,
    pub kind: Kind,
}

impl Row {
    pub fn choice(label: impl Into<String>) -> Self {
        Self { label: label.into(), kind: Kind::Choice }
    }
    pub fn check(label: impl Into<String>, on: bool) -> Self {
        Self { label: label.into(), kind: Kind::Check(on) }
    }
    pub fn action(label: impl Into<String>) -> Self {
        Self { label: label.into(), kind: Kind::Action }
    }
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// Still open.
    Stay,
    /// Picked choice or action `i`; the blade is done.
    Picked(usize),
    /// Closed (Back/Left); checkboxes keep what was ticked.
    Closed,
}

pub struct Blade {
    title: String,
    rows: Vec<Row>,
    /// The current choice, marked.
    current: Option<usize>,
    /// A line is drawn after these rows (between groups).
    dividers: Vec<usize>,
    focus: usize,
    slide: Tween,
}

impl Blade {
    /// A plain list of choices, opened on `current`.
    pub fn new(title: &str, options: Vec<String>, current: Option<usize>, now: Instant) -> Self {
        let rows = options.into_iter().map(Row::choice).collect();
        Self::with_rows(title, rows, current, current.unwrap_or(0), now)
    }

    /// Any rows; focus starts on `focus`.
    pub fn with_rows(
        title: &str,
        rows: Vec<Row>,
        current: Option<usize>,
        focus: usize,
        now: Instant,
    ) -> Self {
        let mut slide = Tween::settled(0.0, now);
        slide.retarget(1.0, now, SLIDE);
        let current = current.filter(|c| *c < rows.len());
        let focus = focus.min(rows.len().saturating_sub(1));
        Self { title: title.into(), rows, current, dividers: Vec::new(), focus, slide }
    }

    /// Draws a line after row `i`.
    pub fn divider_after(mut self, i: usize) -> Self {
        self.dividers.push(i);
        self
    }

    pub fn is_checked(&self, i: usize) -> bool {
        matches!(self.rows.get(i).map(|r| r.kind), Some(Kind::Check(true)))
    }

    pub fn on_action(&mut self, a: InputAction) -> Outcome {
        let n = self.rows.len();
        match a {
            InputAction::Up if self.focus > 0 => self.focus -= 1,
            // Wrap at the ends: from the top, Up reaches the bottom.
            InputAction::Up if n > 0 => self.focus = n - 1,
            InputAction::Down if self.focus + 1 < n => self.focus += 1,
            InputAction::Down => self.focus = 0,
            InputAction::Select if n > 0 => match &mut self.rows[self.focus].kind {
                Kind::Check(on) => *on = !*on,
                Kind::Choice | Kind::Action => return Outcome::Picked(self.focus),
            },
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
        let room = panel.bottom() - 20.0 - top - DIVIDER_H * self.dividers.len() as f32;
        let visible = ((room / ITEM_H) as usize).max(1);
        let n = self.rows.len();
        let first = self.focus.saturating_sub(visible / 2).min(n.saturating_sub(visible));
        let regular = TextStyle::new(24.0, Weight::Regular).max_width(WIDTH - 130.0);
        let bold = TextStyle::new(24.0, Weight::Bold).max_width(WIDTH - 130.0);
        let mut y = top;
        for (k, row) in self.rows.iter().enumerate().skip(first).take(visible) {
            let rect = Rect::new(x + 16.0, y, WIDTH - 32.0, ITEM_H - 6.0);
            let focused = k == self.focus;
            if focused {
                r.fill_rrect(rect, 10.0, Color::WHITE);
            }
            if Some(k) == self.current {
                r.fill_rect(Rect::new(rect.x + 6.0, rect.y + 10.0, 4.0, rect.h - 20.0), ACCENT);
            }
            let fg = if focused { BG } else { TEXT };
            let mut tx = rect.x + 22.0;
            if let Kind::Check(on) = row.kind {
                // A checkbox: filled with a white centre when ticked.
                let b = Rect::new(tx, rect.y + (rect.h - 26.0) / 2.0, 26.0, 26.0);
                let edge = if focused { Color::rgba(0, 0, 0, 90) } else { TEXT_DIM };
                r.fill_rrect(b, 6.0, if on { ACCENT } else { edge });
                if on {
                    r.fill_rrect(Rect::new(b.x + 8.0, b.y + 8.0, 10.0, 10.0), 3.0, Color::WHITE);
                } else {
                    let inner = if focused { Color::WHITE } else { Color::rgba(28, 30, 38, 255) };
                    r.fill_rrect(Rect::new(b.x + 3.0, b.y + 3.0, 20.0, 20.0), 4.0, inner);
                }
                tx += 42.0;
            }
            let style = if row.kind == Kind::Action { &bold } else { &regular };
            r.text(&row.label, style, Point::new(tx, rect.y + 10.0), fg);
            y += ITEM_H;
            if self.dividers.contains(&k) && k + 1 < n {
                let ly = y - 6.0 + DIVIDER_H / 2.0;
                r.fill_rect(Rect::new(x + 30.0, ly, WIDTH - 60.0, 1.0), TEXT_DIM.with_alpha(0.4));
                y += DIVIDER_H;
            }
        }
        // A long list: a scrollbar shows where in it the view is.
        if n > visible {
            let track_h = y - top - 6.0;
            let h = (track_h * visible as f32 / n as f32).max(24.0);
            let sy = top + (track_h - h) * first as f32 / (n - visible) as f32;
            r.fill_rrect(Rect::new(panel.right() - 10.0, sy, 4.0, h), 2.0, TEXT_DIM);
        }
    }
}

/// Long text (a full description) in the same panel from the right:
/// Up/Down scroll, Back/Left/Select close.
pub struct TextPanel {
    title: String,
    text: String,
    /// Wrapped on the first draw (needs the renderer).
    lines: Vec<String>,
    first: usize,
    /// Lines that fit (known after drawing).
    visible: usize,
    slide: Tween,
}

const LINE_H: f32 = 36.0;

impl TextPanel {
    pub fn new(title: &str, text: &str, now: Instant) -> Self {
        let mut slide = Tween::settled(0.0, now);
        slide.retarget(1.0, now, SLIDE);
        Self {
            title: title.into(),
            text: text.into(),
            lines: Vec::new(),
            first: 0,
            visible: 1,
            slide,
        }
    }

    /// False when it should close.
    pub fn on_action(&mut self, a: InputAction) -> bool {
        let max_first = self.lines.len().saturating_sub(self.visible);
        match a {
            InputAction::Down => self.first = (self.first + 3).min(max_first),
            InputAction::Up => self.first = self.first.saturating_sub(3),
            InputAction::Back | InputAction::Left | InputAction::Select => return false,
            _ => {}
        }
        true
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        self.slide.is_animating(now)
    }

    pub fn draw<R: Renderer>(&mut self, r: &mut R, now: Instant, safe: f32) {
        const TEXT_W: f32 = 820.0;
        let w = TEXT_W + 60.0;
        let (lw, lh) = r.logical_size();
        let t = self.slide.value(now);
        r.fill_rect(Rect::new(0.0, 0.0, lw, lh), Color::rgba(0, 0, 0, (150.0 * t) as u8));
        let (sx, sy) = (lw * safe, lh * safe);
        let x = lw - (w + sx) * t;
        let panel = Rect::new(x, sy, w, lh - 2.0 * sy);
        r.fill_rrect(panel, 16.0, Color::rgba(28, 30, 38, 250));
        let title = TextStyle::new(28.0, Weight::Bold).max_width(w - 60.0);
        r.text(&self.title, &title, Point::new(x + 30.0, sy + 26.0), TEXT_DIM);
        let body = TextStyle::new(24.0, Weight::Regular);
        if self.lines.is_empty() && !self.text.is_empty() {
            self.lines = super::status::wrap(r, &self.text, &body, TEXT_W);
        }
        let top = sy + 84.0;
        self.visible = (((panel.bottom() - 20.0 - top) / LINE_H) as usize).max(1);
        for (k, l) in self.lines.iter().enumerate().skip(self.first).take(self.visible) {
            let y = top + (k - self.first) as f32 * LINE_H;
            r.text(l, &body, Point::new(x + 30.0, y), TEXT);
        }
        let n = self.lines.len();
        if n > self.visible {
            let track_h = self.visible as f32 * LINE_H;
            let h = (track_h * self.visible as f32 / n as f32).max(24.0);
            let max_first = (n - self.visible) as f32;
            let y = top + (track_h - h) * self.first.min(n - self.visible) as f32 / max_first;
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

    #[test]
    fn checkboxes_stay_open_actions_close() {
        let rows = vec![Row::action("Clear all"), Row::check("A", true), Row::check("B", false)];
        let mut b = Blade::with_rows("Genre", rows, None, 2, Instant::now());
        assert_eq!(b.on_action(InputAction::Select), Outcome::Stay);
        assert!(b.is_checked(2));
        b.on_action(InputAction::Up);
        b.on_action(InputAction::Select);
        assert!(!b.is_checked(1));
        b.on_action(InputAction::Up);
        assert_eq!(b.on_action(InputAction::Select), Outcome::Picked(0));
    }
}
