//! Shared pieces of the browsing pages (spec 10.3: keep the widget set
//! tiny): poster cards, buttons, the backdrop crossfade, and `RowList`, a
//! vertical list of horizontally scrolling card rows used by Home, Detail
//! and Search. Pages combine `RowList`'s card nodes with their own buttons
//! in one spatial focus graph (D27).

use std::time::{Duration, Instant};

use jellyfin_api::endpoints::{ImageType, image_path};
use jellyroll_render::{Color, Point, Rect, Renderer, TextStyle, Weight};

use super::model::{Art, Card, Row, Shape};
use super::textures::Textures;
use super::theme::{ACCENT, BG, BG_RGB, TEXT, TEXT_DIM};
use crate::anim::Tween;
use crate::focus::{Dir, FocusGraph, NodeId};
use crate::net::images::blurhash_size;
use crate::net::store::fnv1a;
use crate::net::{Finish, ImageJob, ImageSource};

pub const RADIUS: f32 = 8.0;
pub const GAP: f32 = 22.0;
pub const LABEL_H: f32 = 44.0;
pub const TITLE_GAP: f32 = 10.0;
pub const TEXT_H: f32 = 50.0;
pub const ROW_GAP: f32 = 24.0;
pub const FOCUS_SCALE: f32 = 0.06;
pub const FOCUS_ANIM: Duration = Duration::from_millis(150);
pub const SCROLL_ANIM: Duration = Duration::from_millis(220);
const BACKDROP_FADE: Duration = Duration::from_millis(300);
/// Extra cards past the right edge to load ahead (spec 9: prefetch).
const PREFETCH_COLS: usize = 3;

const LABEL: TextStyle = TextStyle::new(26.0, Weight::Bold);

/// Node ids at or above this are `RowList` cards; pages use smaller ids for
/// their own elements.
pub const CARD_NODE: NodeId = 1 << 30;

pub fn card_node(r: usize, c: usize) -> NodeId {
    CARD_NODE | ((r as u32) << 14) | c as u32
}

pub fn card_of(id: NodeId) -> Option<(usize, usize)> {
    (id >= CARD_NODE).then_some((((id & !CARD_NODE) >> 14) as usize, (id & 0x3fff) as usize))
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Viewport {
    /// Logical width (height is always 1080).
    pub lw: f32,
    pub safe: f32,
    /// Output pixels per logical pixel.
    pub scale: f32,
}

impl Viewport {
    pub fn sx(&self) -> f32 {
        self.lw * self.safe
    }

    pub fn sy(&self) -> f32 {
        1080.0 * self.safe
    }

    pub fn px(&self, logical: f32) -> u32 {
        (logical * self.scale).round().max(1.0) as u32
    }

    /// Backdrops are decoded at half the output size (soft anyway).
    pub fn backdrop_px(&self) -> (u32, u32) {
        (self.px(self.lw / 2.0), self.px(540.0))
    }
}

/// Texture keys for one card at the current output scale.
#[derive(Clone, Copy, Default, Debug)]
pub struct CardKeys {
    pub image: Option<u64>,
    pub blur: Option<u64>,
    pub backdrop: Option<u64>,
}

fn art_key(a: &Art, w: u32, h: u32, finish: &[u8]) -> u64 {
    fnv1a(&[
        a.item_id.as_bytes(),
        a.kind.as_str().as_bytes(),
        a.tag.as_bytes(),
        &w.to_le_bytes(),
        &h.to_le_bytes(),
        finish,
    ])
}

fn blur_key(hash: &str, w: u32, h: u32) -> u64 {
    fnv1a(&[b"blurhash", hash.as_bytes(), &w.to_le_bytes(), &h.to_le_bytes()])
}

pub fn card_keys(card: &Card, shape: Shape, vp: &Viewport) -> CardKeys {
    let (w, h) = shape.size();
    let (bhw, bhh) = blurhash_size(w, h);
    let (bw, bh) = vp.backdrop_px();
    CardKeys {
        image: card.image.as_ref().map(|a| art_key(a, vp.px(w), vp.px(h), b"card")),
        blur: card
            .image
            .as_ref()
            .and_then(|a| a.blurhash.as_deref())
            .map(|h| blur_key(h, bhw, bhh)),
        backdrop: card.backdrop.as_ref().map(|a| art_key(a, bw, bh, b"backdrop")),
    }
}

pub fn backdrop_key(art: &Art, vp: &Viewport) -> u64 {
    let (bw, bh) = vp.backdrop_px();
    art_key(art, bw, bh, b"backdrop")
}

/// Collects image jobs, most important first, skipping what is loaded,
/// queued, failed, or already listed.
pub struct Wants<'a> {
    pub jobs: Vec<ImageJob>,
    tex: &'a Textures,
    vp: Viewport,
}

impl<'a> Wants<'a> {
    pub fn new(tex: &'a Textures, vp: Viewport) -> Self {
        Self { jobs: Vec::new(), tex, vp }
    }

    fn fresh(&self, key: u64) -> bool {
        !self.tex.known(key) && !self.jobs.iter().any(|j| j.key == key)
    }

    pub fn backdrop(&mut self, art: Option<&Art>) {
        let Some(a) = art else { return };
        let key = backdrop_key(a, &self.vp);
        if self.fresh(key) {
            let (w, h) = self.vp.backdrop_px();
            self.jobs.push(ImageJob {
                key,
                source: ImageSource::Server(image_path(
                    &a.item_id,
                    ImageType::Backdrop,
                    &a.tag,
                    w,
                    h,
                )),
                w,
                h,
                finish: Finish::Backdrop { bg: BG_RGB },
            });
        }
    }

    pub fn blur(&mut self, card: &Card, shape: Shape, keys: &CardKeys) {
        let (Some(key), Some(hash)) =
            (keys.blur, card.image.as_ref().and_then(|a| a.blurhash.as_ref()))
        else {
            return;
        };
        if self.fresh(key) {
            let (w, h) = shape.size();
            let (bw, bh) = blurhash_size(w, h);
            self.jobs.push(ImageJob {
                key,
                source: ImageSource::BlurHash(hash.clone()),
                w: bw,
                h: bh,
                finish: Finish::Card { radius: 0.0 },
            });
        }
    }

    pub fn image(&mut self, card: &Card, shape: Shape, keys: &CardKeys) {
        let (Some(key), Some(a)) = (keys.image, card.image.as_ref()) else { return };
        if self.fresh(key) {
            let (w, h) = shape.size();
            let (pw, ph) = (self.vp.px(w), self.vp.px(h));
            self.jobs.push(ImageJob {
                key,
                source: ImageSource::Server(image_path(&a.item_id, a.kind, &a.tag, pw, ph)),
                w: pw,
                h: ph,
                finish: Finish::Card { radius: RADIUS * self.vp.scale },
            });
        }
    }
}

/// A poster/thumbnail card with its title below. `focus` is 0..1 (the
/// focus animation); `base` is the unscaled card rect.
#[allow(clippy::too_many_arguments)]
pub fn draw_card<R: Renderer>(
    r: &mut R,
    tex: &mut Textures,
    base: Rect,
    focus: f32,
    card: &Card,
    keys: &CardKeys,
    focused: bool,
    text: bool,
) {
    let s = 1.0 + FOCUS_SCALE * focus;
    let (w, h) = (base.w, base.h);
    let rect = Rect::new(base.x - w * (s - 1.0) / 2.0, base.y - h * (s - 1.0) / 2.0, w * s, h * s);
    r.shadow(rect, RADIUS, 14.0, Color::rgba(0, 0, 0, 150));
    if let Some(img) = keys.image.and_then(|k| tex.get(k)) {
        r.image(img, rect, 0.0, 1.0);
    } else if let Some(img) = keys.blur.and_then(|k| tex.get(k)) {
        r.image(img, rect, RADIUS, 1.0);
    } else {
        r.fill_rrect(rect, RADIUS, Color::rgba(255, 255, 255, 22));
        if card.image.is_none() {
            // No artwork: the title inside the card.
            let st = TextStyle::new(20.0, Weight::Bold).max_width(rect.w - 20.0);
            r.text(&card.title, &st, Point::new(rect.x + 10.0, rect.y + 10.0), TEXT_DIM);
        }
    }
    if let Some(p) = card.progress {
        let track = Rect::new(rect.x + 10.0, rect.bottom() - 14.0, rect.w - 20.0, 5.0);
        r.fill_rrect(track, 2.5, Color::rgba(0, 0, 0, 160));
        r.fill_rrect(Rect::new(track.x, track.y, track.w * p, track.h), 2.5, ACCENT);
    }
    if focus > 0.01 {
        r.stroke_rrect(rect.inset(-3.0), RADIUS + 3.0, 3.0, Color::WHITE.with_alpha(focus));
    }
    if text {
        let title_style = TextStyle::new(19.0, Weight::Regular).max_width(w);
        let sub_style = TextStyle::new(16.0, Weight::Regular).max_width(w);
        let ty = rect.bottom().max(base.bottom()) + 3.0 * focus.max(0.0) + TITLE_GAP;
        r.text(
            &card.title,
            &title_style,
            Point::new(base.x, ty),
            if focused { TEXT } else { TEXT_DIM },
        );
        if !card.subtitle.is_empty() {
            r.text(&card.subtitle, &sub_style, Point::new(base.x, ty + 26.0), TEXT_DIM);
        }
    }
}

pub const BUTTON: TextStyle = TextStyle::new(24.0, Weight::Bold);

/// A rounded button or nav pill. `selected` marks the current choice
/// (e.g. the page you are on) when it doesn't have focus.
pub fn draw_button<R: Renderer>(r: &mut R, rect: Rect, label: &str, focused: bool, selected: bool) {
    let (bg, fg) = if focused {
        (Color::WHITE, BG)
    } else if selected {
        (Color::rgba(255, 255, 255, 60), TEXT)
    } else {
        (Color::rgba(255, 255, 255, 26), TEXT)
    };
    r.fill_rrect(rect, rect.h / 2.0, bg);
    let style = BUTTON.max_width(rect.w - 24.0);
    let m = r.measure_text(label, &style);
    let p = Point::new(
        (rect.x + (rect.w - m.width) / 2.0).round(),
        (rect.y + (rect.h - m.height) / 2.0).round(),
    );
    r.text(label, &style, p, fg);
}

/// Button width that fits `label`.
pub fn button_width<R: Renderer>(r: &mut R, label: &str) -> f32 {
    r.measure_text(label, &BUTTON).width + 56.0
}

/// Full-screen backdrop with a crossfade when it changes.
pub struct Backdrop {
    shown: Option<u64>,
    prev: Option<u64>,
    fade: Tween,
}

impl Backdrop {
    pub fn new(now: Instant) -> Self {
        Self { shown: None, prev: None, fade: Tween::settled(1.0, now) }
    }

    /// Starts the crossfade once `want` is loaded; true if a redraw is due.
    pub fn update(&mut self, want: Option<u64>, now: Instant, tex: &Textures) -> bool {
        match want {
            Some(k) if Some(k) != self.shown && tex.loaded(k) => {
                self.prev = self.shown;
                self.shown = Some(k);
                self.fade = Tween::settled(0.0, now);
                self.fade.retarget(1.0, now, BACKDROP_FADE);
                true
            }
            _ => false,
        }
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        self.fade.is_animating(now)
    }

    /// Draws the backdrop (or clears to the background color).
    pub fn draw<R: Renderer>(&self, r: &mut R, now: Instant, tex: &mut Textures) {
        let (lw, lh) = r.logical_size();
        let screen = Rect::new(0.0, 0.0, lw, lh);
        let fade = self.fade.value(now);
        let cur = self.shown.and_then(|k| tex.get(k));
        let prev = if fade < 1.0 { self.prev.and_then(|k| tex.get(k)) } else { None };
        match (prev, cur) {
            (Some(p), Some(c)) => {
                r.image(p, screen, 0.0, 1.0);
                r.image(c, screen, 0.0, fade);
            }
            (None, Some(c)) if fade >= 1.0 => r.image(c, screen, 0.0, 1.0),
            (_, c) => {
                r.clear(BG);
                if let Some(c) = c {
                    r.image(c, screen, 0.0, fade);
                }
            }
        }
    }
}

/// Rows of cards that scroll: the focused row moves to `top`, each row
/// scrolls sideways to keep its focused card in view, rows above `top`
/// slide under the clip.
pub struct RowList {
    pub rows: Vec<Row>,
    keys: Vec<Vec<CardKeys>>,
    tops: Vec<f32>,
    h_scroll: Vec<Tween>,
    v_scroll: Tween,
    pub focus: (usize, usize),
    prev_focus: (usize, usize),
    anim: Tween,
    /// Whether focus is on a card (vs. a page element elsewhere).
    pub active: bool,
    /// Logical y of the focused row's label.
    pub top: f32,
}

/// What a direction does from the focused card (`RowList::step`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Step {
    /// Focus moves to this node.
    To(NodeId),
    /// Focus stays where it is.
    Stay,
    /// Not the rows' call: the page's focus graph decides.
    Free,
}

impl RowList {
    pub fn new(rows: Vec<Row>, top: f32, vp: &Viewport, now: Instant) -> Self {
        let mut l = Self {
            h_scroll: vec![Tween::settled(0.0, now); rows.len()],
            rows,
            keys: Vec::new(),
            tops: Vec::new(),
            v_scroll: Tween::settled(0.0, now),
            focus: (0, 0),
            prev_focus: (0, 0),
            anim: Tween::settled(1.0, now),
            active: true,
            top,
        };
        l.layout(vp, now);
        l
    }

    /// New rows: keep focus on the same item (or position) and each row's
    /// scroll (spec 9: diff-update without resetting focus or scroll).
    pub fn set_rows(&mut self, rows: Vec<Row>, vp: &Viewport, now: Instant) {
        let old_key = self.rows.get(self.focus.0).map(|r| r.key.clone());
        let old_id = self.focused_card().map(|c| c.id.clone());
        let old_scroll: Vec<(String, f32)> = self
            .rows
            .iter()
            .zip(&self.h_scroll)
            .map(|(r, t)| (r.key.clone(), t.value(now)))
            .collect();
        self.rows = rows;
        self.h_scroll = self
            .rows
            .iter()
            .map(|r| {
                Tween::settled(
                    old_scroll.iter().find(|(k, _)| *k == r.key).map_or(0.0, |s| s.1),
                    now,
                )
            })
            .collect();
        let row = old_key
            .and_then(|k| self.rows.iter().position(|r| r.key == k))
            .unwrap_or(self.focus.0)
            .min(self.rows.len().saturating_sub(1));
        let col = self
            .rows
            .get(row)
            .map(|r| {
                old_id
                    .and_then(|id| r.cards.iter().position(|c| c.id == id))
                    .unwrap_or(self.focus.1)
                    .min(r.cards.len().saturating_sub(1))
            })
            .unwrap_or(0);
        self.focus = (row, col);
        self.prev_focus = self.focus;
        self.layout(vp, now);
    }

    /// Recomputes keys and positions after rows or the viewport change.
    pub fn layout(&mut self, vp: &Viewport, now: Instant) {
        self.keys = self
            .rows
            .iter()
            .map(|row| row.cards.iter().map(|c| card_keys(c, row.shape, vp)).collect())
            .collect();
        let mut y = 0.0;
        self.tops = self
            .rows
            .iter()
            .map(|row| {
                let top = y;
                y += row_height(row.shape);
                top
            })
            .collect();
        self.scroll_to_focus(vp, now, false);
    }

    pub fn is_empty(&self) -> bool {
        self.rows.iter().all(|r| r.cards.is_empty())
    }

    pub fn focused_card(&self) -> Option<&Card> {
        self.rows.get(self.focus.0)?.cards.get(self.focus.1)
    }

    pub fn focused_keys(&self) -> Option<&CardKeys> {
        self.keys.get(self.focus.0)?.get(self.focus.1)
    }

    fn h_target(&self, vp: &Viewport, r: usize, c: usize) -> f32 {
        let (w, _) = self.rows[r].shape.size();
        let step = w + GAP;
        let visible = vp.lw - 2.0 * vp.sx();
        let content = self.rows[r].cards.len() as f32 * step - GAP;
        // Keep one card visible to the left of the focused one.
        let want = (c as f32 * step - step).max(0.0);
        want.min((content - visible).max(0.0))
    }

    fn scroll_to_focus(&mut self, vp: &Viewport, now: Instant, animate: bool) {
        let (r, c) = self.focus;
        if r >= self.rows.len() {
            return;
        }
        let d = if animate { SCROLL_ANIM } else { Duration::ZERO };
        self.v_scroll.retarget(self.tops[r], now, d);
        let h = self.h_target(vp, r, c);
        if let Some(t) = self.h_scroll.get_mut(r) {
            t.retarget(h, now, d);
        }
    }

    /// Card rects at the scroll targets, as focus-graph nodes.
    pub fn nodes(&self, vp: &Viewport) -> Vec<(NodeId, Rect)> {
        let sx = vp.sx();
        let v = self.tops.get(self.focus.0).copied().unwrap_or(0.0);
        let mut out = Vec::new();
        for (r, row) in self.rows.iter().enumerate() {
            let (w, h) = row.shape.size();
            let y = self.top + self.tops[r] - v + LABEL_H;
            let off = self.h_scroll[r].target();
            for c in 0..row.cards.len() {
                out.push((card_node(r, c), Rect::new(sx + c as f32 * (w + GAP) - off, y, w, h)));
            }
        }
        out
    }

    /// Where a direction goes from the focused card when the rows decide
    /// it. Left and Right stay in the row, and Right from the last card
    /// wraps to the first; the whole-page graph would pick the nearest
    /// card of another row. Up from a lower row is the row above: the rows
    /// above the focused one are scrolled up under whatever sits over the
    /// rows (a keyboard, buttons), so the graph could pick that instead.
    pub fn step(&self, dir: Dir, vp: &Viewport) -> Step {
        let (row, col) = self.focus;
        let len = self.rows.get(row).map_or(0, |r| r.cards.len());
        if !self.active || len == 0 {
            return Step::Free;
        }
        match dir {
            Dir::Right if col + 1 < len => Step::To(card_node(row, col + 1)),
            Dir::Right if len > 1 => Step::To(card_node(row, 0)),
            Dir::Left if col > 0 => Step::To(card_node(row, col - 1)),
            Dir::Left | Dir::Right => Step::Stay,
            Dir::Up if row > 0 => {
                let mut g = FocusGraph::new();
                let nodes = self
                    .nodes(vp)
                    .into_iter()
                    .filter(|(id, _)| card_of(*id).is_some_and(|(r, _)| r == row || r + 1 == row));
                g.set_nodes(nodes);
                g.focus(card_node(row, col));
                g.navigate(dir).map_or(Step::Free, Step::To)
            }
            _ => Step::Free,
        }
    }

    /// Back to the first card, scrolled to the top (while focus is
    /// elsewhere, e.g. new search results).
    pub fn reset_focus(&mut self, vp: &Viewport, now: Instant) {
        self.focus = (0, 0);
        self.prev_focus = self.focus;
        self.scroll_to_focus(vp, now, false);
    }

    /// Focus moved to a card.
    pub fn take_focus(&mut self, rc: (usize, usize), vp: &Viewport, now: Instant) {
        self.prev_focus = if self.active { self.focus } else { (usize::MAX, 0) };
        self.focus = rc;
        self.active = true;
        self.anim = Tween::settled(0.0, now);
        self.anim.retarget(1.0, now, FOCUS_ANIM);
        self.scroll_to_focus(vp, now, true);
    }

    /// Focus moved to something outside the rows.
    pub fn release_focus(&mut self, now: Instant) {
        if self.active {
            self.active = false;
            self.prev_focus = self.focus;
            self.anim = Tween::settled(0.0, now);
            self.anim.retarget(1.0, now, FOCUS_ANIM);
        }
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        self.anim.is_animating(now)
            || self.v_scroll.is_animating(now)
            || self.h_scroll.get(self.focus.0).is_some_and(|t| t.is_animating(now))
            || self.h_scroll.get(self.prev_focus.0).is_some_and(|t| t.is_animating(now))
    }

    fn visible_cols(&self, vp: &Viewport, r: usize) -> std::ops::Range<usize> {
        let (w, _) = self.rows[r].shape.size();
        let step = w + GAP;
        let off = self.h_scroll[r].target();
        let first = (off / step).floor().max(0.0) as usize;
        let n = ((vp.lw - 2.0 * vp.sx()) / step).ceil() as usize + PREFETCH_COLS;
        first..(first + n).min(self.rows[r].cards.len())
    }

    /// Placeholders for nearby rows, then their artwork, focused row
    /// first; optionally backdrops of the focused card's neighbors.
    pub fn wants(&self, w: &mut Wants, vp: &Viewport, neighbor_backdrops: bool) {
        let (fr, fc) = self.focus;
        let rows: Vec<usize> = [fr, fr + 1, fr + 2, fr.wrapping_sub(1)]
            .into_iter()
            .filter(|&r| r < self.rows.len())
            .collect();
        for &r in &rows {
            for c in self.visible_cols(vp, r) {
                w.blur(&self.rows[r].cards[c], self.rows[r].shape, &self.keys[r][c]);
            }
        }
        for &r in &rows {
            for c in self.visible_cols(vp, r) {
                w.image(&self.rows[r].cards[c], self.rows[r].shape, &self.keys[r][c]);
            }
        }
        if neighbor_backdrops && fr < self.rows.len() {
            for c in [fc + 1, fc.wrapping_sub(1), fc + 2] {
                if let Some(card) = self.rows[fr].cards.get(c) {
                    w.backdrop(card.backdrop.as_ref());
                }
            }
        }
    }

    fn focus_of(&self, rc: (usize, usize), now: Instant) -> f32 {
        let t = self.anim.value(now);
        if self.active && rc == self.focus {
            t
        } else if rc == self.prev_focus {
            1.0 - t
        } else {
            0.0
        }
    }

    pub fn draw<R: Renderer>(&self, r: &mut R, now: Instant, tex: &mut Textures, vp: &Viewport) {
        let (lw, lh) = r.logical_size();
        let sx = vp.sx();
        let clip_top = self.top - 12.0;
        r.push_clip(Rect::new(0.0, clip_top, lw, lh - clip_top), 0.0);
        let v = self.v_scroll.value(now);
        for (ri, row) in self.rows.iter().enumerate() {
            let y = self.top + self.tops[ri] - v;
            if y > lh {
                break;
            }
            if y + row_height(row.shape) < clip_top {
                continue;
            }
            r.text(&row.title, &LABEL, Point::new(sx, y), TEXT);
            let (w, h) = row.shape.size();
            let off = self.h_scroll[ri].value(now);
            let first = ((off - w) / (w + GAP)).floor().max(0.0) as usize;
            for (ci, card) in row.cards.iter().enumerate().skip(first) {
                let x = sx + ci as f32 * (w + GAP) - off;
                if x > lw {
                    break;
                }
                let focused = self.active && (ri, ci) == self.focus;
                draw_card(
                    r,
                    tex,
                    Rect::new(x, y + LABEL_H, w, h),
                    self.focus_of((ri, ci), now),
                    card,
                    &self.keys[ri][ci],
                    focused,
                    true,
                );
            }
        }
        r.pop();
    }
}

pub fn row_height(shape: Shape) -> f32 {
    let (_, h) = shape.size();
    LABEL_H + h + TITLE_GAP + TEXT_H + ROW_GAP
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::home::model::Kind;

    #[test]
    fn card_node_ids_round_trip() {
        assert_eq!(card_of(card_node(3, 1234)), Some((3, 1234)));
        assert_eq!(card_of(7), None);
    }

    fn row(key: &str, n: usize) -> Row {
        let cards = (0..n)
            .map(|i| Card {
                id: format!("{key}{i}"),
                kind: Kind::Movie,
                collection: None,
                title: String::new(),
                subtitle: String::new(),
                meta: String::new(),
                overview: String::new(),
                image: None,
                backdrop: None,
                progress: None,
            })
            .collect();
        Row { key: key.into(), title: key.into(), shape: Shape::Poster, cards }
    }

    #[test]
    fn left_right_stay_in_the_row_and_right_wraps() {
        let vp = Viewport { lw: 1920.0, safe: 0.025, scale: 1.0 };
        let t = Instant::now();
        let mut l = RowList::new(vec![row("a", 3), row("b", 12), row("c", 1)], 0.0, &vp, t);
        l.take_focus((1, 10), &vp, t);
        assert_eq!(l.step(Dir::Right, &vp), Step::To(card_node(1, 11)));
        l.take_focus((1, 11), &vp, t);
        assert_eq!(l.step(Dir::Right, &vp), Step::To(card_node(1, 0)), "wraps to this row");
        assert_eq!(l.step(Dir::Left, &vp), Step::To(card_node(1, 10)));
        l.take_focus((1, 0), &vp, t);
        assert_eq!(l.step(Dir::Left, &vp), Step::Stay, "not into a scrolled row");
        assert_eq!(l.step(Dir::Down, &vp), Step::Free);
        l.take_focus((2, 0), &vp, t);
        assert_eq!(l.step(Dir::Right, &vp), Step::Stay, "a single card");
        l.release_focus(t);
        assert_eq!(l.step(Dir::Right, &vp), Step::Free, "focus is elsewhere");
    }
}
