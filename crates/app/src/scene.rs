//! M2 render benchmark scene (spec 15, M2 exit criteria: a 4K scene of 60
//! cards plus text at 60 fps). A home-screen-like layout: full-bleed
//! backdrop with crossfade and gradient scrims, four rows of fifteen poster
//! cards with baked shadows, rounded artwork and ellipsized titles, a
//! focus scale-up, and Latin/Japanese/Cyrillic text.
//!
//! Navigation (M4) goes through the spatial focus graph: a nav bar of pills
//! above the rows. Select on a pill picks a section; Select on a card shows
//! its title, so every input source can be checked for Select.
//!
//! The data is fake and the strings are fixtures; the real home screen and
//! string table arrive in M5/M6. In bench mode the focus walks the grid and
//! the rows sway, so every frame moves everything.

use std::time::{Duration, Instant};

use jellyroll_render::{Color, ImageId, Point, Rect, Renderer, TextStyle, Weight};

use crate::anim::Tween;
use crate::focus::{Dir, FocusGraph, NodeId};
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

const NAV: [&str; 6] = ["Home", "映画", "テレビ番組", "Фильмы", "Search", "Settings"];
/// Focus ids: cards are 0..CARDS, nav pills follow.
const NAV_ID: usize = CARDS;
const PILL_W: f32 = 150.0;
const PILL_H: f32 = 36.0;
const PILL_GAP: f32 = 12.0;
const PILL_Y: f32 = 56.0;

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

fn card_title(i: usize) -> &'static str {
    TITLES[(i * 7 + i / COLS) % TITLES.len()]
}

pub struct Scene {
    graph: FocusGraph,
    /// Layout the graph was built for: (logical width, safe area).
    layout: (f32, f32),
    /// Selected nav section.
    nav: usize,
    /// Last card Select was pressed on.
    picked: Option<usize>,
    /// Previously focused node, for the focus-out animation.
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
        let mut s = Self {
            graph: FocusGraph::new(),
            layout: (0.0, 0.0),
            nav: 0,
            picked: None,
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
        };
        s.set_viewport(1920.0, DEFAULT_SAFE_AREA);
        s
    }

    fn card_rect(row: usize, col: usize, sx: f32, sy: f32) -> Rect {
        let y = sy + HEADER_H + row as f32 * ROW_H + ROW_LABEL_H;
        Rect::new(sx + col as f32 * (CARD_W + CARD_GAP), y, CARD_W, CARD_H)
    }

    fn pill_rect(k: usize, sx: f32, sy: f32) -> Rect {
        Rect::new(sx + k as f32 * (PILL_W + PILL_GAP), sy + PILL_Y, PILL_W, PILL_H)
    }

    /// Rebuilds the focus graph when the logical width or safe area
    /// changes. Focus is kept.
    pub fn set_viewport(&mut self, logical_w: f32, safe_area: f32) {
        if self.layout == (logical_w, safe_area) {
            return;
        }
        self.layout = (logical_w, safe_area);
        let (sx, sy) = (logical_w * safe_area, 1080.0 * safe_area);
        let cards = (0..CARDS).map(|i| {
            let (r, c) = (i / COLS, i % COLS);
            (i as NodeId, Self::card_rect(r, c, sx, sy))
        });
        let pills = (0..NAV.len()).map(|k| ((NAV_ID + k) as NodeId, Self::pill_rect(k, sx, sy)));
        self.graph.set_nodes(cards.chain(pills));
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
                posters::round_corners(&mut self.pixels, w, h, CARD_RADIUS * scale);
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

    /// The focused node: the graph is the single source of truth.
    fn focus(&self) -> usize {
        self.graph.focused().unwrap_or(0) as usize
    }

    fn is_focused(&self, i: usize) -> bool {
        self.graph.is_focused(i as NodeId)
    }

    fn set_focus(&mut self, next: usize, now: Instant) -> bool {
        let prev = self.focus();
        if !self.graph.focus(next as NodeId) {
            return false;
        }
        self.focus_moved(prev, next, now);
        true
    }

    /// Starts the focus and backdrop animations for a move.
    fn focus_moved(&mut self, prev: usize, next: usize, now: Instant) {
        self.prev_focus = prev;
        self.focus_anim = Tween::settled(0.0, now);
        self.focus_anim.retarget(1.0, now, FOCUS_ANIM);
        let row_backdrop = (next / COLS + next % COLS) % BACKDROPS;
        if next < CARDS && row_backdrop != self.backdrop {
            self.prev_backdrop = self.backdrop;
            self.backdrop = row_backdrop;
            self.fade = Tween::settled(0.0, now);
            self.fade.retarget(1.0, now, BACKDROP_FADE);
        }
    }

    /// Applies an action; returns true if anything visible changed.
    pub fn on_action(&mut self, a: InputAction, now: Instant) -> bool {
        if let Some(dir) = Dir::from_action(a) {
            let prev = self.focus();
            let Some(next) = self.graph.navigate(dir) else { return false };
            self.focus_moved(prev, next as usize, now);
            return true;
        }
        let focus = self.focus();
        match a {
            InputAction::Select if focus >= NAV_ID => {
                let changed = focus - NAV_ID != self.nav;
                self.nav = focus - NAV_ID;
                changed
            }
            InputAction::Select => {
                let changed = self.picked != Some(focus);
                self.picked = Some(focus);
                changed
            }
            _ => false,
        }
    }

    /// Advances bench-mode animation; call once per frame.
    pub fn update(&mut self, now: Instant) {
        if self.bench && now >= self.bench_next {
            self.bench_next = now + BENCH_STEP;
            self.set_focus((self.focus() + 1) % CARDS, now);
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
        if self.is_focused(i) {
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
        // Backdrop crossfade: the outgoing backdrop at full opacity under the
        // incoming one. Backdrops are opaque and cover the screen, so no
        // clear is needed once one is loaded (saves a full-screen pass).
        // The legibility scrims are baked into the textures.
        let fade = self.fade.value(now);
        let prev = if fade < 1.0 { self.backdrops[self.prev_backdrop] } else { None };
        let cur = self.backdrops[self.backdrop];
        match (prev, cur) {
            (Some(p), Some(c)) => {
                r.image(p, screen, 0.0, 1.0);
                r.image(c, screen, 0.0, fade);
            }
            (None, Some(c)) if fade >= 1.0 => r.image(c, screen, 0.0, 1.0),
            _ => {
                r.clear(BG);
                if let Some(c) = cur {
                    r.image(c, screen, 0.0, fade);
                }
            }
        }

        r.text("Jellyroll", &TextStyle::new(44.0, Weight::Bold), Point::new(sx, sy), TEXT);
        let pill_style = TextStyle::new(20.0, Weight::Bold).max_width(PILL_W - 16.0);
        for (k, label) in NAV.iter().enumerate() {
            let rect = Self::pill_rect(k, sx, sy);
            let focused = self.is_focused(NAV_ID + k);
            let (bg, fg) = if focused {
                (Color::WHITE, BG)
            } else if k == self.nav {
                (Color::rgba(255, 255, 255, 60), TEXT)
            } else {
                (Color::rgba(255, 255, 255, 18), TEXT_DIM)
            };
            r.fill_rrect(rect, PILL_H / 2.0, bg);
            let m = r.measure_text(label, &pill_style);
            let at = Point::new(
                (rect.x + (rect.w - m.width) / 2.0).round(),
                (rect.y + (rect.h - m.height) / 2.0).round(),
            );
            r.text(label, &pill_style, at, fg);
        }
        r.text(
            header,
            &TextStyle::new(16.0, Weight::Regular),
            Point::new(sx + 260.0, sy + 18.0),
            TEXT_DIM,
        );

        if let Some(i) = self.picked {
            let x = Self::pill_rect(NAV.len(), sx, sy).x + 12.0;
            let style = TextStyle::new(20.0, Weight::Regular).max_width(lw - sx - x);
            let mut line = crate::hud::Line::<96>::default();
            let _ =
                std::fmt::Write::write_fmt(&mut line, format_args!("Selected: {}", card_title(i)));
            r.text(line.as_str(), &style, Point::new(x, sy + PILL_Y + 6.0), TEXT);
        }

        let t = now.saturating_duration_since(self.start).as_secs_f32();
        let title_style = TextStyle::new(16.0, Weight::Regular).max_width(CARD_W);
        let label_style = TextStyle::new(22.0, Weight::Bold);
        for (row, label) in ROW_LABELS.iter().enumerate() {
            let y = sy + HEADER_H + row as f32 * ROW_H;
            r.text(label, &label_style, Point::new(sx, y), TEXT);
            let sway = if self.bench { (t * 1.6 + row as f32).sin() * 60.0 } else { 0.0 };
            for col in 0..COLS {
                let i = row * COLS + col;
                let mut base = Self::card_rect(row, col, sx, sy);
                base.x += sway;
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
                    // Corners are baked into the texture: a plain image draw.
                    Some(img) => r.image(img, card, 0.0, 1.0),
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
                let title = card_title(i);
                let color = if self.is_focused(i) { TEXT } else { TEXT_DIM };
                // The title slides down with the focused card's growth and
                // border so they never overlap.
                let ty = card.bottom().max(base.bottom()) + 3.0 * focus.max(0.0) + TITLE_GAP;
                r.text(title, &title_style, Point::new(base.x, ty), color);
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
        assert!(s.on_action(InputAction::Right, t0));
        assert_eq!(s.focus(), 1);
        assert!(s.focus_anim.is_animating(t0 + Duration::from_millis(10)));
        for _ in 0..30 {
            s.on_action(InputAction::Right, t0);
        }
        assert_eq!(s.focus(), COLS - 1);
        for _ in 0..10 {
            s.on_action(InputAction::Down, t0);
        }
        assert_eq!(s.focus(), CARDS - 1);
        assert!(!s.on_action(InputAction::Down, t0), "bottom edge");
    }

    #[test]
    fn nav_bar_and_select() {
        let t0 = Instant::now();
        let mut s = Scene::new(t0);
        assert!(s.on_action(InputAction::Select, t0), "select on a card picks it");
        assert_eq!(s.picked, Some(0));
        assert!(!s.on_action(InputAction::Select, t0), "same card again: nothing new");
        // Up from the first card reaches the nav bar; Select picks a section.
        assert!(s.on_action(InputAction::Up, t0));
        assert_eq!(s.focus(), NAV_ID);
        assert!(!s.on_action(InputAction::Up, t0), "top edge");
        s.on_action(InputAction::Right, t0);
        assert!(s.on_action(InputAction::Select, t0));
        assert_eq!(s.nav, 1);
        // Down goes to the card under the pill.
        s.on_action(InputAction::Down, t0);
        assert_eq!(s.focus(), 2);
    }

    #[test]
    fn moves_straight_without_row_memory() {
        let t0 = Instant::now();
        let mut s = Scene::new(t0);
        for _ in 0..5 {
            s.on_action(InputAction::Right, t0);
        }
        s.on_action(InputAction::Down, t0);
        s.on_action(InputAction::Left, t0);
        s.on_action(InputAction::Left, t0);
        s.on_action(InputAction::Up, t0);
        assert_eq!(s.focus(), 3, "straight up from column 3");
    }

    #[test]
    fn relayout_keeps_focus() {
        let t0 = Instant::now();
        let mut s = Scene::new(t0);
        s.on_action(InputAction::Down, t0);
        s.set_viewport(2560.0, 0.05);
        assert_eq!(s.graph.focused(), Some(COLS as NodeId));
        assert!(s.on_action(InputAction::Right, t0));
        assert_eq!(s.focus(), COLS + 1);
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
