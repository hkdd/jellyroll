//! "Debug info" on an item page (D83): says where the report will go,
//! collects it on Write, then says where it went (or why it couldn't).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use jellyroll_render::{Color, Point, Rect, Renderer, TextStyle, Weight};

use super::strings as s;
use super::theme::{TEXT, TEXT_DIM};
use super::widgets;
use crate::debug_report::Outcome as Written;
use crate::input::InputAction;

/// How often a running report is checked.
const POLL: Duration = Duration::from_millis(200);

enum State {
    Confirm,
    /// Waiting for the server's side (the fetch `req`).
    Fetching,
    /// The report is being collected and written.
    Writing(Written),
    Done(Result<PathBuf, String>),
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Stay,
    /// Write was chosen: fetch the server's side.
    Write,
    Close,
}

pub struct DebugDialog {
    pub item_id: String,
    pub title: String,
    pub path: PathBuf,
    /// The server fetch, while Fetching.
    pub req: Option<u64>,
    state: State,
    /// 0: Write, 1: Cancel (Confirm only).
    focus: usize,
}

impl DebugDialog {
    pub fn new(item_id: String, title: String, path: PathBuf) -> Self {
        Self { item_id, title, path, req: None, state: State::Confirm, focus: 0 }
    }

    pub fn fetching(&mut self, req: u64) {
        self.req = Some(req);
        self.state = State::Fetching;
    }

    pub fn writing(&mut self, w: Written) {
        self.req = None;
        self.state = State::Writing(w);
    }

    pub fn failed(&mut self, e: String) {
        self.req = None;
        self.state = State::Done(Err(e));
    }

    pub fn on_action(&mut self, a: InputAction) -> Outcome {
        match (&self.state, a) {
            (State::Confirm, InputAction::Left | InputAction::Up) => self.focus = 0,
            (State::Confirm, InputAction::Right | InputAction::Down) => self.focus = 1,
            (State::Confirm, InputAction::Select) if self.focus == 0 => return Outcome::Write,
            (State::Done(_), InputAction::Select) | (_, InputAction::Back) => {
                return Outcome::Close;
            }
            (State::Confirm, InputAction::Select) => return Outcome::Close,
            _ => {}
        }
        Outcome::Stay
    }

    /// Picks up a finished report. True if the dialog changed.
    pub fn update(&mut self) -> bool {
        let State::Writing(w) = &self.state else { return false };
        let Some(r) = w.get() else { return false };
        self.state = State::Done(r.clone());
        true
    }

    pub fn next_deadline(&self, now: Instant) -> Option<Instant> {
        matches!(self.state, State::Fetching | State::Writing(_)).then(|| now + POLL)
    }

    pub fn draw<R: Renderer>(&self, r: &mut R) {
        let (lw, lh) = r.logical_size();
        r.fill_rect(Rect::new(0.0, 0.0, lw, lh), Color::rgba(0, 0, 0, 170));
        let w = 980.0;
        let body = TextStyle::new(24.0, Weight::Regular);
        let mono = TextStyle::new(22.0, Weight::Bold);
        let path = self.path.display().to_string();
        let (lines, path_lines, buttons): (Vec<String>, Vec<String>, Vec<&str>) = match &self.state
        {
            State::Confirm => (
                vec![s::fill(s::DEBUG_CONFIRM, &[&self.title]), s::DEBUG_TAKES.into()],
                vec![path],
                vec![s::DEBUG_WRITE, s::CANCEL],
            ),
            State::Fetching | State::Writing(_) => {
                (vec![s::DEBUG_WORKING.into()], vec![path], vec![])
            }
            State::Done(Ok(p)) => (
                vec![s::DEBUG_DONE.into()],
                vec![p.display().to_string(), s::DEBUG_READ_HINT.into()],
                vec![s::OK],
            ),
            State::Done(Err(e)) => (vec![s::fill(s::DEBUG_FAILED, &[e])], vec![], vec![s::OK]),
        };
        let wrap = |r: &mut R, v: &[String], st: &TextStyle| -> Vec<String> {
            v.iter().flat_map(|l| super::status::wrap(r, l, st, w - 80.0)).collect()
        };
        let lines = wrap(r, &lines, &body);
        let path_lines: Vec<String> =
            path_lines.iter().flat_map(|l| wrap_path(r, l, &mono, w - 80.0)).collect();
        let h = 116.0
            + lines.len() as f32 * 34.0
            + 16.0
            + path_lines.len() as f32 * 32.0
            + 24.0
            + if buttons.is_empty() { 0.0 } else { 72.0 };
        let panel = Rect::new((lw - w) / 2.0, (lh - h) / 2.0, w, h);
        r.fill_rrect(panel, 18.0, Color::rgba(24, 27, 36, 250));
        let title = TextStyle::new(32.0, Weight::Bold).max_width(w - 80.0);
        r.text(s::DEBUG_TITLE, &title, Point::new(panel.x + 40.0, panel.y + 36.0), TEXT);
        let mut y = panel.y + 100.0;
        for l in &lines {
            r.text(l, &body, Point::new(panel.x + 40.0, y), TEXT_DIM);
            y += 34.0;
        }
        y += 16.0;
        for l in &path_lines {
            r.text(l, &mono, Point::new(panel.x + 40.0, y), TEXT);
            y += 32.0;
        }
        y += 24.0;
        let mut x = panel.x + 40.0;
        for (i, b) in buttons.iter().enumerate() {
            let bw = widgets::button_width(r, b).max(200.0);
            let focused = match self.state {
                State::Confirm => i == self.focus,
                _ => true,
            };
            widgets::draw_button(r, Rect::new(x, y, bw, 52.0), b, focused, false);
            x += bw + 20.0;
        }
    }
}

/// Breaks a path (no spaces) into lines that fit `max_w`, after a `/` where
/// possible, else anywhere.
fn wrap_path<R: Renderer>(r: &mut R, text: &str, style: &TextStyle, max_w: f32) -> Vec<String> {
    let fits = |r: &mut R, s: &str| r.measure_text(s, style).width <= max_w;
    let mut lines = Vec::new();
    let mut line = String::new();
    for part in text.split_inclusive('/') {
        let joined = format!("{line}{part}");
        if fits(r, &joined) {
            line = joined;
            continue;
        }
        if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        // A part too long on its own: break it by characters.
        for c in part.chars() {
            line.push(c);
            if !fits(r, &line) {
                line.pop();
                lines.push(std::mem::take(&mut line));
                line.push(c);
            }
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, OnceLock};

    fn dialog() -> DebugDialog {
        DebugDialog::new("id".into(), "Heat".into(), "/d/report.txt".into())
    }

    #[test]
    fn write_or_cancel() {
        let mut d = dialog();
        assert_eq!(d.on_action(InputAction::Select), Outcome::Write, "starts on Write");
        d.on_action(InputAction::Right);
        assert_eq!(d.on_action(InputAction::Select), Outcome::Close);
        assert_eq!(dialog().on_action(InputAction::Back), Outcome::Close);
    }

    #[test]
    fn shows_the_result_when_written() {
        let mut d = dialog();
        d.fetching(7);
        assert_eq!(d.req, Some(7));
        assert_eq!(d.on_action(InputAction::Select), Outcome::Stay, "busy");
        let w: Written = Arc::new(OnceLock::new());
        d.writing(w.clone());
        assert!(!d.update());
        assert!(d.next_deadline(Instant::now()).is_some());
        let _ = w.set(Ok("/d/report.txt".into()));
        assert!(d.update());
        assert!(d.next_deadline(Instant::now()).is_none());
        assert_eq!(d.on_action(InputAction::Select), Outcome::Close);
    }

    #[test]
    fn a_failure_is_shown() {
        let mut d = dialog();
        d.fetching(1);
        d.failed("Can't reach the server".into());
        assert!(matches!(d.state, State::Done(Err(_))));
    }
}
