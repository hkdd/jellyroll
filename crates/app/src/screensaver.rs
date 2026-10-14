//! Screensaver (D73): after a while without input, while browsing or with
//! a video paused, the screen shows a style (for now The Matrix's falling
//! green characters). Any key wakes it and does nothing else; playing video
//! keeps it away.
//!
//! The rain is drawn at `FRAME` intervals rather than every vsync: smooth
//! enough for falling characters, and light on the GPU.

use std::time::{Duration, Instant};

use jellyroll_render::{Color, Point, Renderer, TextStyle, Weight};

/// Redraw interval while it shows.
const FRAME: Duration = Duration::from_millis(40);
/// Glyph cell size (logical px; the UI is 1080 lines tall).
const CELL: f32 = 30.0;

/// Half-width katakana (as in the film), digits and a few Latin letters.
const GLYPHS: [&str; 52] = [
    "ｦ", "ｧ", "ｨ", "ｩ", "ｪ", "ｫ", "ｬ", "ｭ", "ｮ", "ｯ", "ｱ", "ｲ", "ｳ", "ｴ", "ｵ", "ｶ", "ｷ", "ｸ", "ｹ",
    "ｺ", "ｻ", "ｼ", "ｽ", "ｾ", "ｿ", "ﾀ", "ﾁ", "ﾂ", "ﾃ", "ﾄ", "ﾅ", "ﾆ", "ﾇ", "ﾈ", "ﾉ", "ﾊ", "ﾋ", "ﾌ",
    "ﾍ", "ﾎ", "ﾏ", "ﾐ", "0", "1", "2", "3", "4", "5", "7", "8", "9", "Z",
];

pub struct Screensaver {
    /// None: off.
    after: Option<Duration>,
    idle_since: Instant,
    rain: Option<Rain>,
    next_frame: Instant,
}

impl Screensaver {
    pub fn new(after: Option<Duration>, now: Instant) -> Self {
        Self { after, idle_since: now, rain: None, next_frame: now }
    }

    /// The setting changed.
    pub fn set_after(&mut self, after: Option<Duration>) {
        if after != self.after {
            self.after = after;
            if after.is_none() {
                self.rain = None;
            }
        }
    }

    pub fn active(&self) -> bool {
        self.rain.is_some()
    }

    /// Input arrived. True if it woke the screensaver: the caller drops it.
    pub fn input(&mut self, now: Instant) -> bool {
        self.idle_since = now;
        self.rain.take().is_some()
    }

    /// Not idle (a video is playing): stays away, and goes if showing.
    pub fn keep_awake(&mut self, now: Instant) -> bool {
        self.input(now)
    }

    /// Starts when idle long enough (`eligible`: browsing or paused), and
    /// paces its frames. True when a redraw is due.
    pub fn tick(&mut self, now: Instant, eligible: bool, lw: f32, lh: f32) -> bool {
        match (&self.rain, self.after) {
            (None, Some(after)) if eligible && now >= self.idle_since + after => {
                tracing::info!("screensaver on");
                self.rain = Some(Rain::new(lw, lh, now));
                self.next_frame = now;
                true
            }
            (Some(_), _) if now >= self.next_frame => {
                self.next_frame = now + FRAME;
                true
            }
            _ => false,
        }
    }

    /// When the loop must wake: the next frame, or when it would start.
    pub fn next_deadline(&self) -> Option<Instant> {
        match (&self.rain, self.after) {
            (Some(_), _) => Some(self.next_frame),
            (None, Some(after)) => Some(self.idle_since + after),
            (None, None) => None,
        }
    }

    pub fn draw<R: Renderer>(&mut self, r: &mut R, now: Instant) {
        if let Some(rain) = &mut self.rain {
            rain.draw(r, now);
        }
    }
}

/// xorshift: deterministic, no dependency, plenty for falling characters.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// In [lo, hi).
    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (self.next() % 10_000) as f32 / 10_000.0 * (hi - lo)
    }
}

struct Column {
    /// Head position, in cells from the top.
    head: f32,
    /// Cells per second.
    speed: f32,
    len: f32,
    /// A glyph per row (changes now and then).
    glyphs: Vec<u8>,
}

struct Rain {
    rows: usize,
    drops: Vec<Column>,
    rng: Rng,
    last: Instant,
}

impl Rain {
    fn new(lw: f32, lh: f32, now: Instant) -> Self {
        let cols = (lw / CELL).ceil().max(1.0) as usize;
        let rows = (lh / CELL).ceil().max(1.0) as usize;
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9e37_79b9, |d| d.as_nanos() as u64)
            | 1;
        let mut rng = Rng(seed);
        let drops = (0..cols)
            .map(|_| {
                let glyphs = (0..rows).map(|_| (rng.next() % GLYPHS.len() as u64) as u8).collect();
                Column {
                    // Start spread above the screen, so it fills in.
                    head: -rng.range(0.0, rows as f32),
                    speed: rng.range(6.0, 20.0),
                    len: rng.range(6.0, rows as f32 * 0.8),
                    glyphs,
                }
            })
            .collect();
        Self { rows, drops, rng, last: now }
    }

    fn draw<R: Renderer>(&mut self, r: &mut R, now: Instant) {
        let dt = (now - self.last).as_secs_f32().min(0.2);
        self.last = now;
        let rows = self.rows as f32;
        for d in &mut self.drops {
            d.head += d.speed * dt;
            if d.head - d.len > rows {
                d.head = -self.rng.range(0.0, rows * 0.5);
                d.speed = self.rng.range(6.0, 20.0);
                d.len = self.rng.range(6.0, rows * 0.8);
            }
            // A few glyphs change each frame.
            if self.rng.next().is_multiple_of(4) {
                let i = (self.rng.next() % self.rows as u64) as usize;
                d.glyphs[i] = (self.rng.next() % GLYPHS.len() as u64) as u8;
            }
        }
        r.clear(Color::BLACK);
        let style = TextStyle::new(CELL * 0.85, Weight::Regular);
        for (c, d) in self.drops.iter().enumerate() {
            let head = d.head.floor() as i32;
            let tail = (d.head - d.len).floor() as i32;
            for row in tail.max(0)..=head.min(self.rows as i32 - 1) {
                let behind = (head - row) as f32;
                let color = if row == head {
                    Color::rgb(210, 255, 210)
                } else {
                    let a = (1.0 - behind / d.len).clamp(0.0, 1.0);
                    Color::rgb(0, 255, 70).with_alpha(a * 0.9)
                };
                let g = GLYPHS[d.glyphs[row as usize] as usize];
                r.text(g, &style, Point::new(c as f32 * CELL, row as f32 * CELL), color);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_when_idle_and_eligible_and_any_input_wakes_it() {
        let t = Instant::now();
        let mut s = Screensaver::new(Some(Duration::from_secs(300)), t);
        assert_eq!(s.next_deadline(), Some(t + Duration::from_secs(300)));
        assert!(!s.tick(t + Duration::from_secs(299), true, 1920.0, 1080.0));
        assert!(!s.tick(t + Duration::from_secs(301), false, 1920.0, 1080.0), "playing");
        assert!(s.tick(t + Duration::from_secs(301), true, 1920.0, 1080.0));
        assert!(s.active());
        // The waking key is used up; the next isn't.
        let later = t + Duration::from_secs(400);
        assert!(s.input(later));
        assert!(!s.active());
        assert!(!s.input(later));
        assert_eq!(s.next_deadline(), Some(later + Duration::from_secs(300)));
    }

    #[test]
    fn off_never_starts() {
        let t = Instant::now();
        let mut s = Screensaver::new(None, t);
        assert!(!s.tick(t + Duration::from_secs(100_000), true, 1920.0, 1080.0));
        assert_eq!(s.next_deadline(), None);
    }

    #[test]
    fn frames_are_paced() {
        let t = Instant::now();
        let mut s = Screensaver::new(Some(Duration::from_secs(1)), t);
        let on = t + Duration::from_secs(2);
        assert!(s.tick(on, true, 1920.0, 1080.0));
        assert!(s.tick(on, true, 1920.0, 1080.0), "first frame");
        assert!(!s.tick(on + Duration::from_millis(10), true, 1920.0, 1080.0));
        assert!(s.tick(on + FRAME, true, 1920.0, 1080.0));
    }

    #[test]
    fn glyphs_render_with_the_bundled_fonts() {
        // Covered by the font subset (scripts/subset-fonts.sh: U+FF00-FFEF).
        for g in GLYPHS {
            let c = g.chars().next().unwrap() as u32;
            assert!(c < 0x80 || (0xFF00..0xFFF0).contains(&c), "{g}");
        }
    }
}
