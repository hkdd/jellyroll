//! M2 render benchmark scene (spec 15, M2 exit criteria: a 4K scene of 60
//! cards plus text at 60 fps). A home-screen-like layout: full-bleed
//! backdrop with crossfade and gradient scrims, four rows of fifteen poster
//! cards with baked shadows, rounded artwork and ellipsized titles, a
//! focus scale-up, and Latin/Japanese/Cyrillic text.
//!
//! The data is fake and the strings are fixtures; the real home screen and
//! string table arrive in M5/M6. In bench mode the focus walks the grid and
//! the rows sway, so every frame moves everything.

use std::time::{Duration, Instant};

use jellyroll_render::{Color, ImageId, Point, Rect, Renderer, TextStyle, Weight};

use crate::anim::Tween;
use crate::input::InputAction;
use crate::posters;

pub const ROWS: usize = 4;
pub const COLS: usize = 15;
const CARDS: usize = ROWS * COLS;
const BACKDROPS: usize = 4;

const CARD_W: f32 = 100.0;
const CARD_H: f32 = 150.0;
const CARD_GAP: f32 = 16.0;
const CARD_RADIUS: f32 = 8.0;
const ROW_LABEL_H: f32 = 38.0;
const TITLE_GAP: f32 = 6.0;
const TITLE_H: f32 = 22.0;
const ROW_GAP: f32 = 12.0;
const ROW_H: f32 = ROW_LABEL_H + CARD_H + TITLE_GAP + TITLE_H + ROW_GAP;
const HEADER_H: f32 = 100.0;
const FOCUS_SCALE: f32 = 0.08;

const FOCUS_ANIM: Duration = Duration::from_millis(150);
const BACKDROP_FADE: Duration = Duration::from_millis(250);
/// Bench mode: how often the focus advances.
const BENCH_STEP: Duration = Duration::from_millis(120);
/// Per-frame upload budget for artwork (spec 9: uploads never cause hitches).
pub const UPLOAD_BUDGET: Duration = Duration::from_millis(2);

/// Fraction of each edge kept clear for TV overscan (spec 10.2).
pub const DEFAULT_SAFE_AREA: f32 = 0.025;

const ROW_LABELS: [&str; ROWS] =
    ["Continue Watching", "Next Up", "Latest Movies", "最近追加された映画"];

const TITLES: [&str; 20] = [
    "The Long Voyage Home",
    "千と千尋の神隠し",
    "Midnight in the Archive",
    "Северный ветер",
    "A Very Long Title That Will Certainly Need An Ellipsis",
    "Arrival",
    "君の名は。",
    "Glass Onion",
    "The Quiet Earth",
    "Тихий Дон",
    "Paprika",
    "Dune: Part Two",
    "もののけ姫",
    "Moon",
    "Everything Everywhere All at Once",
    "Heat",
    "Solaris",
    "Spirited Away",
    "Le Samouraï",
    "Ikiru",
];

const TEXT: Color = Color::rgb(235, 235, 240);
const TEXT_DIM: Color = Color::rgb(170, 175, 190);
const NOTICE: Color = Color::rgb(240, 180, 60);
const BG_RGB: [u8; 3] = [12, 14, 20];
const BG: Color = Color::rgb(BG_RGB[0], BG_RGB[1], BG_RGB[2]);

pub struct Scene {
    focus: usize,
    prev_focus: usize,
    focus_anim: Tween,
    backdrop: usize,
    prev_backdrop: usize,
    fade: Tween,
    posters: Vec<Option<ImageId>>,
    backdrops: Vec<Option<ImageId>>,
    pending: usize,
    pixels: Vec<u8>,
    /// Continuous animation for pacing benchmarks.
    pub bench: bool,
    bench_next: Instant,
    start: Instant,
}

impl Scene {
    pub fn new(now: Instant) -> Self {
        Self {
            focus: 0,
            prev_focus: 0,
            focus_anim: Tween::settled(1.0, now),
            backdrop: 0,
            prev_backdrop: 0,
            fade: Tween::settled(1.0, now),
            posters: vec![None; CARDS],
            backdrops: vec![None; BACKDROPS],
            pending: 0,
            pixels: Vec::new(),
            bench: false,
            bench_next: now,
            start: now,
        }
    }

    pub fn uploads_pending(&self) -> bool {
        self.pending < CARDS + BACKDROPS
    }

    /// Generates and uploads artwork within `budget`, decoded at on-screen
    /// size for the current output (spec 9). Returns true if anything was
    /// uploaded.
    pub fn pump_uploads<R: Renderer>(&mut self, r: &mut R, fb_h: u32, budget: Duration) -> bool {
        let start = Instant::now();
        let scale = fb_h as f32 / 1080.0;
        let mut any = false;
        while self.uploads_pending() && start.elapsed() < budget {
            let i = self.pending;
            if i < BACKDROPS {
                // Backdrops are softly upscaled; half resolution is plenty.
                let (w, h) = ((960.0 * scale) as u32, (540.0 * scale) as u32);
                posters::backdrop(i, w, h, &mut self.pixels);
                posters::apply_scrim(&mut self.pixels, w, h, BG_RGB, 0.92, 0.95);
                self.backdrops[i] = r.create_image(w, h, &self.pixels);
            } else {
                let c = i - BACKDROPS;
                let (w, h) = ((CARD_W * scale).round() as u32, (CARD_H * scale).round() as u32);
                posters::poster(c, w, h, &mut self.pixels);
                self.posters[c] = r.create_image(w, h, &self.pixels);
            }
            self.pending += 1;
            any = true;
        }
        if !self.uploads_pending() {
            self.pixels = Vec::new();
        }
        any
    }

    fn set_focus(&mut self, next: usize, now: Instant) -> bool {
        if next == self.focus {
            return false;
        }
        self.prev_focus = self.focus;
        self.focus = next;
        self.focus_anim = Tween::settled(0.0, now);
        self.focus_anim.retarget(1.0, now, FOCUS_ANIM);
        let row_backdrop = (next / COLS + next % COLS) % BACKDROPS;
        if row_backdrop != self.backdrop {
            self.prev_backdrop = self.backdrop;
            self.backdrop = row_backdrop;
            self.fade = Tween::settled(0.0, now);
            self.fade.retarget(1.0, now, BACKDROP_FADE);
        }
        true
    }

    /// Applies an action; returns true if anything visible changed.
    pub fn on_action(&mut self, a: InputAction, now: Instant) -> bool {
        let (r, c) = (self.focus / COLS, self.focus % COLS);
        let next = match a {
            InputAction::Left => (r, c.saturating_sub(1)),
            InputAction::Right => (r, (c + 1).min(COLS - 1)),
            InputAction::Up => (r.saturating_sub(1), c),
            InputAction::Down => ((r + 1).min(ROWS - 1), c),
            _ => return false,
        };
        self.set_focus(next.0 * COLS + next.1, now)
    }

    /// Advances bench-mode animation; call once per frame.
    pub fn update(&mut self, now: Instant) {
        if self.bench && now >= self.bench_next {
            self.bench_next = now + BENCH_STEP;
            self.set_focus((self.focus + 1) % CARDS, now);
        }
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        self.bench
            || self.uploads_pending()
            || self.focus_anim.is_animating(now)
            || self.fade.is_animating(now)
    }

    fn card_scale(&self, i: usize, now: Instant) -> f32 {
        let t = self.focus_anim.value(now);
        if i == self.focus {
            1.0 + FOCUS_SCALE * t
        } else if i == self.prev_focus {
            1.0 + FOCUS_SCALE * (1.0 - t)
        } else {
            1.0
        }
    }

    pub fn draw<R: Renderer>(
        &self,
        r: &mut R,
        now: Instant,
        safe_area: f32,
        header: &str,
        notice: &str,
    ) {
        let (lw, lh) = r.logical_size();
        let screen = Rect::new(0.0, 0.0, lw, lh);
        let (sx, sy) = (lw * safe_area, lh * safe_area);
        r.clear(BG);

        // Backdrop crossfade. The legibility scrims are baked into the
        // backdrop textures, so this is one full-screen pass (two while
        // fading).
        let fade = self.fade.value(now);
        if fade < 1.0
            && let Some(img) = self.backdrops[self.prev_backdrop]
        {
            r.image(img, screen, 0.0, 1.0 - fade);
        }
        if let Some(img) = self.backdrops[self.backdrop] {
            r.image(img, screen, 0.0, fade);
        }

        r.text("Jellyroll", &TextStyle::new(44.0, Weight::Bold), Point::new(sx, sy), TEXT);
        r.text(
            "ホーム · 映画 · テレビ番組 · Фильмы · Search · Settings",
            &TextStyle::new(22.0, Weight::Regular),
            Point::new(sx, sy + 58.0),
            TEXT_DIM,
        );
        r.text(
            header,
            &TextStyle::new(16.0, Weight::Regular),
            Point::new(sx + 260.0, sy + 18.0),
            TEXT_DIM,
        );

        let t = now.saturating_duration_since(self.start).as_secs_f32();
        let title_style = TextStyle::new(16.0, Weight::Regular).max_width(CARD_W);
        let label_style = TextStyle::new(22.0, Weight::Bold);
        for row in 0..ROWS {
            let y = sy + HEADER_H + row as f32 * ROW_H;
            r.text(ROW_LABELS[row], &label_style, Point::new(sx, y), TEXT);
            let sway = if self.bench { (t * 1.6 + row as f32).sin() * 60.0 } else { 0.0 };
            for col in 0..COLS {
                let i = row * COLS + col;
                let x = sx + sway + col as f32 * (CARD_W + CARD_GAP);
                let base = Rect::new(x, y + ROW_LABEL_H, CARD_W, CARD_H);
                // Row virtualization: skip anything off screen.
                if !base.intersects(&screen) {
                    continue;
                }
                let s = self.card_scale(i, now);
                let card = Rect::new(
                    base.x - base.w * (s - 1.0) / 2.0,
                    base.y - base.h * (s - 1.0) / 2.0,
                    base.w * s,
                    base.h * s,
                );
                r.shadow(card, CARD_RADIUS, 14.0, Color::rgba(0, 0, 0, 150));
                match self.posters[i] {
                    Some(img) => r.image(img, card, CARD_RADIUS, 1.0),
                    None => r.fill_rrect(card, CARD_RADIUS, posters::placeholder(i)),
                }
                let focus = (s - 1.0) / FOCUS_SCALE;
                if focus > 0.01 {
                    r.stroke_rrect(
                        card.inset(-3.0),
                        CARD_RADIUS + 3.0,
                        3.0,
                        Color::WHITE.with_alpha(focus),
                    );
                }
                let title = TITLES[(i * 7 + row) % TITLES.len()];
                let color = if i == self.focus { TEXT } else { TEXT_DIM };
                r.text(title, &title_style, Point::new(base.x, base.bottom() + TITLE_GAP), color);
            }
        }

        if !notice.is_empty() {
            r.text(
                notice,
                &TextStyle::new(20.0, Weight::Bold),
                Point::new(sx, lh - sy - 30.0),
                NOTICE,
            );
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
        assert_eq!(s.focus, 1);
        assert!(s.focus_anim.is_animating(t0 + Duration::from_millis(10)));
        for _ in 0..30 {
            s.on_action(InputAction::Right, t0);
        }
        assert_eq!(s.focus, COLS - 1);
        for _ in 0..10 {
            s.on_action(InputAction::Down, t0);
        }
        assert_eq!(s.focus, CARDS - 1);
        assert!(!s.on_action(InputAction::Select, t0));
    }

    #[test]
    fn focus_scale_animates_in_and_out() {
        let t0 = Instant::now();
        let mut s = Scene::new(t0);
        s.on_action(InputAction::Right, t0);
        let mid = t0 + FOCUS_ANIM / 2;
        assert!(s.card_scale(1, mid) > 1.0 && s.card_scale(1, mid) < 1.0 + FOCUS_SCALE);
        assert!(s.card_scale(0, mid) > 1.0);
        let end = t0 + FOCUS_ANIM;
        assert_eq!(s.card_scale(1, end), 1.0 + FOCUS_SCALE);
        assert_eq!(s.card_scale(0, end), 1.0);
        assert_eq!(s.card_scale(5, end), 1.0);
    }

    #[test]
    fn layout_fits_1080_logical() {
        // Header + 4 rows inside the default safe area.
        let bottom = 1080.0 * DEFAULT_SAFE_AREA + HEADER_H + ROWS as f32 * ROW_H;
        assert!(bottom <= 1080.0 * (1.0 - DEFAULT_SAFE_AREA), "{bottom}");
        let right = 1920.0 * DEFAULT_SAFE_AREA + COLS as f32 * (CARD_W + CARD_GAP) - CARD_GAP;
        assert!(right <= 1920.0 * (1.0 - DEFAULT_SAFE_AREA), "{right}");
    }
}
