//! Full-screen states before the home screen exists (spec 8: never a
//! blank screen): searching, connecting, the Quick Connect code, and
//! problems with a clear explanation and a retry.

use jellyroll_render::{Color, Point, Rect, Renderer, TextStyle, Weight};

use super::theme::{ACCENT, BG, TEXT, TEXT_DIM};
use crate::net::Status;

/// Greedy word wrap to `max_w`. Text is shaped once per status change,
/// not per frame.
pub fn wrap<R: Renderer>(r: &mut R, text: &str, style: &TextStyle, max_w: f32) -> Vec<String> {
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split_whitespace() {
            let candidate =
                if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
            if !line.is_empty() && r.measure_text(&candidate, style).width > max_w {
                lines.push(std::mem::replace(&mut line, word.to_string()));
            } else {
                line = candidate;
            }
        }
        lines.push(line);
    }
    lines
}

pub struct StatusView {
    pub status: Status,
    title: String,
    detail: String,
    /// Quick Connect code with spaced digits.
    code: Option<String>,
    hint: &'static str,
    /// Wrapped detail lines, computed on first draw (needs the renderer).
    lines: Option<Vec<String>>,
}

const TITLE: TextStyle = TextStyle::new(44.0, Weight::Bold);
const BODY: TextStyle = TextStyle::new(24.0, Weight::Regular);
const CODE: TextStyle = TextStyle::new(110.0, Weight::Bold);
const MAX_W: f32 = 1100.0;

impl StatusView {
    pub fn new(status: Status) -> Self {
        let (title, detail, code, hint) = match &status {
            Status::ServerEntry { .. } => (String::new(), String::new(), None, ""),
            Status::Connecting { server } => {
                (format!("Connecting to {server}…"), String::new(), None, "")
            }
            Status::QuickConnect { server, url, code } => (
                format!("Sign in to {server}"),
                // jellyfin-web: user menu (profile picture, top right) →
                // Quick Connect, route #/quickconnect.
                format!(
                    "In Jellyfin on a phone or computer, open the menu under your profile \
                     picture (top right) and choose Quick Connect, or go to \
                     {url}/web/#/quickconnect, then enter this code."
                ),
                Some(
                    code.chars().flat_map(|c| [c, ' ']).collect::<String>().trim_end().to_string(),
                ),
                "Select: new code   ·   Back: use a different server",
            ),
            Status::Problem { title, detail } => (
                title.clone(),
                detail.clone(),
                None,
                "Select: retry   ·   Back: choose another server",
            ),
        };
        Self { status, title, detail, code, hint, lines: None }
    }

    pub fn can_change_server(&self) -> bool {
        matches!(self.status, Status::Problem { .. } | Status::QuickConnect { .. })
    }

    pub fn retryable(&self) -> bool {
        matches!(self.status, Status::Problem { .. } | Status::QuickConnect { .. })
    }

    pub fn draw<R: Renderer>(&mut self, r: &mut R, safe_area: f32, notice: &str) {
        let (lw, lh) = r.logical_size();
        r.clear(BG);
        let (sx, sy) = (lw * safe_area, lh * safe_area);
        r.text("Jellyroll", &TextStyle::new(32.0, Weight::Bold), Point::new(sx, sy), TEXT);

        if self.lines.is_none() {
            let detail = std::mem::take(&mut self.detail);
            self.lines =
                Some(if detail.is_empty() { Vec::new() } else { wrap(r, &detail, &BODY, MAX_W) });
        }
        let lines = self.lines.as_deref().unwrap_or_default();
        let (title, code, hint) = (self.title.as_str(), self.code.as_deref(), self.hint);

        // Vertically center the block.
        let code_h = if code.is_some() { 170.0 } else { 0.0 };
        let block_h =
            64.0 + code_h + lines.len() as f32 * 34.0 + if hint.is_empty() { 0.0 } else { 70.0 };
        let mut y = ((lh - block_h) / 2.0).max(sy + 60.0);
        let center = |r: &mut R, s: &str, style: &TextStyle, y: f32, c: Color| {
            let m = r.measure_text(s, style);
            r.text(s, style, Point::new(((lw - m.width) / 2.0).round(), y), c);
        };
        center(r, title, &TITLE.max_width(lw - 2.0 * sx), y, TEXT);
        y += 72.0;
        if let Some(code) = code {
            // Spaced digits read better from the couch.
            let m = r.measure_text(code, &CODE);
            let w = m.width + 80.0;
            let rect = Rect::new(((lw - w) / 2.0).round(), y, w, 150.0);
            r.fill_rrect(rect, 18.0, Color::rgba(255, 255, 255, 18));
            r.stroke_rrect(rect, 18.0, 2.0, ACCENT);
            r.text(code, &CODE, Point::new(rect.x + 40.0, rect.y + 2.0), TEXT);
            y += code_h;
        }
        for l in lines {
            center(r, l, &BODY, y, TEXT_DIM);
            y += 34.0;
        }
        if !hint.is_empty() {
            y += 30.0;
            center(r, hint, &TextStyle::new(20.0, Weight::Regular), y, TEXT_DIM);
        }
        if !notice.is_empty() {
            r.text(
                notice,
                &TextStyle::new(20.0, Weight::Bold),
                Point::new(sx, lh - sy - 30.0),
                super::theme::NOTICE,
            );
        }
    }
}
