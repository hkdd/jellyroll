//! Player OSD for the M3 harness (spec 7.3, minimal): title, position,
//! progress bar and pause state at the bottom; a diagnostics panel (Info);
//! and a persistent warning badge when video is decoded in software, which
//! must never happen silently (spec 7.1). Auto-hides; any input shows it.
//! The full player UX (tracks, trickplay, skip, next-up) is M7.

use std::fmt::Write;
use std::time::{Duration, Instant};

use jellyroll_render::{Color, Gradient, Point, Rect, Renderer, TextStyle, Weight};
use jellyroll_video::{FrameDrops, PlaybackState};

use crate::hud::Line;

pub const AUTO_HIDE: Duration = Duration::from_secs(4);

const TEXT: Color = Color::rgb(240, 240, 245);
const DIM: Color = Color::rgb(180, 185, 200);
const TRACK: Color = Color::rgba(255, 255, 255, 60);
const FILL: Color = Color::rgb(0, 164, 220);
const PANEL: Color = Color::rgba(10, 10, 14, 210);
const WARN_BG: Color = Color::rgb(190, 40, 40);

pub struct Osd {
    hide_at: Option<Instant>,
    pub diagnostics: bool,
    title: String,
    time: Line<48>,
    time_secs: (i64, i64),
}

/// `h:mm:ss` or `m:ss`.
pub fn fmt_time(out: &mut impl Write, secs: f64) {
    let s = secs.max(0.0) as i64;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    let _ = if h > 0 { write!(out, "{h}:{m:02}:{s:02}") } else { write!(out, "{m}:{s:02}") };
}

impl Osd {
    pub fn new(title: &str, now: Instant) -> Self {
        Self {
            hide_at: Some(now + AUTO_HIDE),
            diagnostics: false,
            title: title.to_string(),
            time: Line::default(),
            time_secs: (-1, -1),
        }
    }

    pub fn show(&mut self, now: Instant) {
        self.hide_at = Some(now + AUTO_HIDE);
    }

    pub fn hide(&mut self) {
        self.hide_at = None;
    }

    pub fn toggle(&mut self, now: Instant) {
        if self.bar_visible(now) { self.hide() } else { self.show(now) }
    }

    /// Bottom bar visible (paused keeps it up).
    pub fn bar_visible_with(&self, now: Instant, paused: bool) -> bool {
        paused || self.bar_visible(now)
    }

    fn bar_visible(&self, now: Instant) -> bool {
        self.hide_at.is_some_and(|t| now < t)
    }

    /// When the bar hides, so the loop can wake to redraw without it.
    pub fn next_deadline(&self, now: Instant) -> Option<Instant> {
        self.hide_at.filter(|&t| t > now)
    }

    /// Anything to draw over the video this frame?
    pub fn any_visible(&self, now: Instant, s: &PlaybackState) -> bool {
        self.bar_visible_with(now, s.paused) || self.diagnostics || s.software_decode()
    }

    pub fn draw<R: Renderer>(
        &mut self,
        r: &mut R,
        now: Instant,
        s: &PlaybackState,
        drops: &FrameDrops,
        safe_area: f32,
    ) {
        let (lw, lh) = r.logical_size();
        let (sx, sy) = (lw * safe_area, lh * safe_area);

        if s.software_decode() {
            let style = TextStyle::new(20.0, Weight::Bold);
            let msg = "Software decoding: this device has no hardware decoder for this video";
            let m = r.measure_text(msg, &style);
            let rect = Rect::new(lw - sx - m.width - 32.0, sy, m.width + 32.0, m.height + 16.0);
            r.fill_rrect(rect, 8.0, WARN_BG);
            r.text(msg, &style, Point::new(rect.x + 16.0, rect.y + 8.0), TEXT);
        }

        if self.diagnostics {
            self.draw_diagnostics(r, s, drops, sx, sy);
        }

        if !self.bar_visible_with(now, s.paused) {
            return;
        }
        let bar_h = lh * 0.22;
        r.fill_gradient(
            Rect::new(0.0, lh - bar_h, lw, bar_h),
            0.0,
            &Gradient {
                from: Color::BLACK.with_alpha(0.0),
                to: Color::BLACK.with_alpha(0.85),
                vertical: true,
            },
        );
        let bottom = lh - sy;
        let title_style = TextStyle::new(30.0, Weight::Bold).max_width(lw - 2.0 * sx);
        r.text(&self.title, &title_style, Point::new(sx, bottom - 110.0), TEXT);

        // Re-format the time only when the displayed second changes.
        let secs = (s.time_pos as i64, s.duration as i64);
        if secs != self.time_secs {
            self.time_secs = secs;
            self.time.clear();
            fmt_time(&mut self.time, s.time_pos);
            let _ = write!(self.time, " / ");
            fmt_time(&mut self.time, s.duration);
            if s.paused {
                let _ = write!(self.time, "   Paused");
            }
        }
        r.text(
            self.time.as_str(),
            &TextStyle::new(22.0, Weight::Regular),
            Point::new(sx, bottom - 64.0),
            DIM,
        );

        let track = Rect::new(sx, bottom - 24.0, lw - 2.0 * sx, 8.0);
        r.fill_rrect(track, 4.0, TRACK);
        let frac =
            if s.duration > 0.0 { (s.time_pos / s.duration).clamp(0.0, 1.0) as f32 } else { 0.0 };
        if frac > 0.0 {
            r.fill_rrect(Rect::new(track.x, track.y, track.w * frac, track.h), 4.0, FILL);
        }
    }

    fn draw_diagnostics<R: Renderer>(
        &self,
        r: &mut R,
        s: &PlaybackState,
        d: &FrameDrops,
        x: f32,
        y: f32,
    ) {
        let style = TextStyle::new(18.0, Weight::Regular);
        let mut lines: [Line<96>; 6] = Default::default();
        let hw =
            if s.software_decode() { "software (no hardware decoder)" } else { s.hwdec.as_str() };
        let _ = write!(
            lines[0],
            "Video  {} {}×{} @ {:.3} fps",
            s.video_codec, s.width, s.height, s.fps
        );
        let _ = write!(lines[1], "Decoder  {hw}");
        let _ = write!(lines[2], "Audio  {}", s.audio_codec);
        let _ = write!(lines[3], "Dropped  vo {}  decoder {}", d.vo, d.decoder);
        let _ = write!(lines[4], "Delayed {}  mistimed {}", d.delayed, d.mistimed);
        let _ = write!(
            lines[5],
            "Buffer  {:.0} s  network {:.1} Mbit/s{}",
            s.cache_secs,
            crate::playback::mbit(s.cache_speed),
            if s.buffering { "  (waiting)" } else { "" }
        );
        let (w, lh) = (620.0, 26.0);
        r.fill_rrect(Rect::new(x, y, w, lh * lines.len() as f32 + 24.0), 8.0, PANEL);
        for (i, l) in lines.iter().enumerate() {
            r.text(l.as_str(), &style, Point::new(x + 12.0, y + 12.0 + i as f32 * lh), TEXT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(secs: f64) -> String {
        let mut s = String::new();
        fmt_time(&mut s, secs);
        s
    }

    #[test]
    fn time_format() {
        assert_eq!(t(0.0), "0:00");
        assert_eq!(t(65.9), "1:05");
        assert_eq!(t(3725.0), "1:02:05");
        assert_eq!(t(-3.0), "0:00");
    }

    #[test]
    fn auto_hide_and_pause() {
        let t0 = Instant::now();
        let mut o = Osd::new("x", t0);
        assert!(o.bar_visible_with(t0, false));
        let later = t0 + AUTO_HIDE + Duration::from_millis(1);
        assert!(!o.bar_visible_with(later, false));
        assert!(o.bar_visible_with(later, true), "paused keeps the bar up");
        assert_eq!(o.next_deadline(later), None);
        o.show(later);
        assert!(o.next_deadline(later).is_some());
        o.toggle(later);
        assert!(!o.bar_visible_with(later, false));
    }

    #[test]
    fn software_decode_forces_overlay() {
        let t0 = Instant::now();
        let mut o = Osd::new("x", t0);
        o.hide();
        let mut s =
            PlaybackState { file_loaded: true, hwdec: "vaapi".into(), ..Default::default() };
        assert!(!o.any_visible(t0, &s));
        s.hwdec = "no".into();
        assert!(o.any_visible(t0, &s));
    }
}
