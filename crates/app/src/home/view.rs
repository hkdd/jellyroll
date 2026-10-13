//! The home screen (spec 10.1): the focused item's backdrop and details in
//! a hero area, rows of cards below. Focus moves spatially (D27); the
//! focused row scrolls to a fixed line and each row scrolls sideways to
//! keep its focused card in view. Artwork arrives asynchronously:
//! BlurHash placeholder first, then the real image (spec 8).

use std::time::{Duration, Instant};

use jellyfin_api::endpoints::{ImageType, image_path};
use jellyroll_render::{Color, Point, Rect, Renderer, TextStyle, Weight};

use super::model::{Art, HomeData};
use super::textures::Textures;
use super::theme::{ACCENT, BG, BG_RGB, NOTICE, TEXT, TEXT_DIM};
use crate::anim::Tween;
use crate::focus::{Dir, FocusGraph, NodeId};
use crate::input::InputAction;
use crate::net::images::blurhash_size;
use crate::net::store::fnv1a;
use crate::net::{Finish, ImageJob, ImageSource};

/// Logical y of the focused row's label.
const ROWS_TOP: f32 = 540.0;
const LABEL_H: f32 = 44.0;
const GAP: f32 = 22.0;
const TITLE_GAP: f32 = 10.0;
const TEXT_H: f32 = 50.0;
const ROW_GAP: f32 = 24.0;
const RADIUS: f32 = 8.0;
const FOCUS_SCALE: f32 = 0.06;
const FOCUS_ANIM: Duration = Duration::from_millis(150);
const SCROLL_ANIM: Duration = Duration::from_millis(220);
const BACKDROP_FADE: Duration = Duration::from_millis(300);
/// Extra cards past the right edge to load ahead (spec 9: prefetch).
const PREFETCH_COLS: usize = 3;

const HERO_TITLE: TextStyle = TextStyle::new(52.0, Weight::Bold).max_width(1100.0);
const HERO_SUB: TextStyle = TextStyle::new(26.0, Weight::Regular).max_width(1100.0);
const HERO_META: TextStyle = TextStyle::new(22.0, Weight::Regular).max_width(1100.0);
const HERO_OVERVIEW: TextStyle = TextStyle::new(22.0, Weight::Regular).max_width(1100.0);
const LABEL: TextStyle = TextStyle::new(26.0, Weight::Bold);
const HEADER: TextStyle = TextStyle::new(18.0, Weight::Regular);

/// Texture keys for one card at the current output scale.
#[derive(Clone, Copy, Default)]
struct CardKeys {
    image: Option<u64>,
    blur: Option<u64>,
    backdrop: Option<u64>,
}

#[derive(Copy, Clone, PartialEq)]
struct Viewport {
    /// Logical width (height is always 1080).
    lw: f32,
    safe: f32,
    /// Output pixels per logical pixel.
    scale: f32,
}

pub struct HomeView {
    pub data: HomeData,
    /// Shown from cache while the server is unreachable.
    pub offline: bool,
    vp: Viewport,
    keys: Vec<Vec<CardKeys>>,
    row_tops: Vec<f32>,
    graph: FocusGraph,
    focus: (usize, usize),
    prev_focus: (usize, usize),
    focus_anim: Tween,
    v_scroll: Tween,
    h_scroll: Vec<Tween>,
    backdrop_shown: Option<u64>,
    backdrop_prev: Option<u64>,
    fade: Tween,
    picked: Option<(usize, usize)>,
}

fn node(r: usize, c: usize) -> NodeId {
    ((r as u32) << 16) | c as u32
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

impl HomeView {
    pub fn new(data: HomeData, lw: f32, safe: f32, scale: f32, now: Instant) -> Self {
        let mut v = Self {
            data,
            offline: false,
            vp: Viewport { lw, safe, scale },
            keys: Vec::new(),
            row_tops: Vec::new(),
            graph: FocusGraph::new(),
            focus: (0, 0),
            prev_focus: (0, 0),
            focus_anim: Tween::settled(1.0, now),
            v_scroll: Tween::settled(0.0, now),
            h_scroll: Vec::new(),
            backdrop_shown: None,
            backdrop_prev: None,
            fade: Tween::settled(1.0, now),
            picked: None,
        };
        v.h_scroll = vec![Tween::settled(0.0, now); v.data.rows.len()];
        v.relayout(now);
        v
    }

    /// Fresh data from the server: keep focus on the same item (or the
    /// same position) and each row's scroll, so a refresh never jumps
    /// (spec 9: diff-update without resetting focus or scroll).
    pub fn set_data(&mut self, data: HomeData, now: Instant) {
        let old_key = self.data.rows.get(self.focus.0).map(|r| r.key.clone());
        let old_id = self.focused_card().map(|c| c.id.clone());
        let old_scroll: Vec<(String, f32)> = self
            .data
            .rows
            .iter()
            .zip(&self.h_scroll)
            .map(|(r, t)| (r.key.clone(), t.value(now)))
            .collect();
        self.data = data;
        self.h_scroll = self
            .data
            .rows
            .iter()
            .map(|r| {
                let v = old_scroll.iter().find(|(k, _)| *k == r.key).map_or(0.0, |s| s.1);
                Tween::settled(v, now)
            })
            .collect();
        let row = old_key
            .and_then(|k| self.data.rows.iter().position(|r| r.key == k))
            .unwrap_or(self.focus.0)
            .min(self.data.rows.len().saturating_sub(1));
        let col = self
            .data
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
        self.picked = None;
        self.relayout(now);
    }

    pub fn set_viewport(&mut self, lw: f32, safe: f32, scale: f32, now: Instant) -> bool {
        let vp = Viewport { lw, safe, scale };
        if vp == self.vp {
            return false;
        }
        self.vp = vp;
        self.relayout(now);
        true
    }

    fn sx(&self) -> f32 {
        self.vp.lw * self.vp.safe
    }

    fn px(&self, logical: f32) -> u32 {
        (logical * self.vp.scale).round().max(1.0) as u32
    }

    fn row_h(&self, r: usize) -> f32 {
        let (_, h) = self.data.rows[r].shape.size();
        LABEL_H + h + TITLE_GAP + TEXT_H + ROW_GAP
    }

    /// Recomputes texture keys, row positions, scroll targets and the
    /// focus graph after data or viewport changes.
    fn relayout(&mut self, now: Instant) {
        let (bw, bh) = (self.px(self.vp.lw / 2.0), self.px(540.0));
        self.keys = self
            .data
            .rows
            .iter()
            .map(|row| {
                let (w, h) = row.shape.size();
                let (pw, ph) = (self.px(w), self.px(h));
                let (bhw, bhh) = blurhash_size(w, h);
                row.cards
                    .iter()
                    .map(|c| CardKeys {
                        image: c.image.as_ref().map(|a| art_key(a, pw, ph, b"card")),
                        blur: c
                            .image
                            .as_ref()
                            .and_then(|a| a.blurhash.as_deref())
                            .map(|h| blur_key(h, bhw, bhh)),
                        backdrop: c.backdrop.as_ref().map(|a| art_key(a, bw, bh, b"backdrop")),
                    })
                    .collect()
            })
            .collect();
        let mut y = 0.0;
        self.row_tops = (0..self.data.rows.len())
            .map(|r| {
                let top = y;
                y += self.row_h(r);
                top
            })
            .collect();
        self.scroll_to_focus(now, false);
        self.rebuild_graph();
    }

    fn h_target(&self, r: usize, c: usize) -> f32 {
        let (w, _) = self.data.rows[r].shape.size();
        let step = w + GAP;
        let visible = self.vp.lw - 2.0 * self.sx();
        let content = self.data.rows[r].cards.len() as f32 * step - GAP;
        // Keep one card visible to the left of the focused one.
        let want = (c as f32 * step - step).max(0.0);
        want.min((content - visible).max(0.0))
    }

    fn scroll_to_focus(&mut self, now: Instant, animate: bool) {
        let (r, c) = self.focus;
        if r >= self.data.rows.len() {
            return;
        }
        let d = if animate { SCROLL_ANIM } else { Duration::ZERO };
        self.v_scroll.retarget(self.row_tops[r], now, d);
        let h = self.h_target(r, c);
        if let Some(t) = self.h_scroll.get_mut(r) {
            t.retarget(h, now, d);
        }
    }

    /// Card rects at the scroll targets (screen space), so spatial search
    /// matches what the user will see.
    fn rebuild_graph(&mut self) {
        let sx = self.sx();
        let v = self.row_tops.get(self.focus.0).copied().unwrap_or(0.0);
        let rows = &self.data.rows;
        let tops = &self.row_tops;
        let hs = &self.h_scroll;
        let nodes = rows.iter().enumerate().flat_map(|(r, row)| {
            let (w, h) = row.shape.size();
            let y = ROWS_TOP + tops[r] - v + LABEL_H;
            let off = hs[r].target();
            (0..row.cards.len())
                .map(move |c| (node(r, c), Rect::new(sx + c as f32 * (w + GAP) - off, y, w, h)))
        });
        self.graph.set_nodes(nodes);
        self.graph.focus(node(self.focus.0, self.focus.1));
    }

    pub fn focused_card(&self) -> Option<&super::model::Card> {
        self.data.rows.get(self.focus.0)?.cards.get(self.focus.1)
    }

    pub fn on_action(&mut self, a: InputAction, now: Instant) -> bool {
        if let Some(dir) = Dir::from_action(a) {
            let Some(id) = self.graph.navigate(dir) else { return false };
            self.prev_focus = self.focus;
            self.focus = ((id >> 16) as usize, (id & 0xffff) as usize);
            self.focus_anim = Tween::settled(0.0, now);
            self.focus_anim.retarget(1.0, now, FOCUS_ANIM);
            self.scroll_to_focus(now, true);
            self.rebuild_graph();
            return true;
        }
        if a == InputAction::Select && self.focused_card().is_some() {
            let changed = self.picked != Some(self.focus);
            self.picked = Some(self.focus);
            return changed;
        }
        false
    }

    /// Starts the backdrop crossfade once the focused item's backdrop is
    /// loaded. Returns true if that needs a redraw.
    pub fn update(&mut self, now: Instant, tex: &Textures) -> bool {
        let want =
            self.keys.get(self.focus.0).and_then(|r| r.get(self.focus.1)).and_then(|k| k.backdrop);
        match want {
            Some(k) if Some(k) != self.backdrop_shown && tex.loaded(k) => {
                self.backdrop_prev = self.backdrop_shown;
                self.backdrop_shown = Some(k);
                self.fade = Tween::settled(0.0, now);
                self.fade.retarget(1.0, now, BACKDROP_FADE);
                true
            }
            _ => false,
        }
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        self.focus_anim.is_animating(now)
            || self.v_scroll.is_animating(now)
            || self.fade.is_animating(now)
            || self.h_scroll.get(self.focus.0).is_some_and(|t| t.is_animating(now))
            || self.h_scroll.get(self.prev_focus.0).is_some_and(|t| t.is_animating(now))
    }

    fn visible_cols(&self, r: usize) -> std::ops::Range<usize> {
        let (w, _) = self.data.rows[r].shape.size();
        let step = w + GAP;
        let off = self.h_scroll[r].target();
        let first = (off / step).floor().max(0.0) as usize;
        let n = ((self.vp.lw - 2.0 * self.sx()) / step).ceil() as usize + PREFETCH_COLS;
        first..(first + n).min(self.data.rows[r].cards.len())
    }

    /// Images to load, most important first: the focused backdrop, every
    /// visible placeholder, the focused row's art, nearby rows, then
    /// neighbors' backdrops. Only what isn't loaded, queued or failed.
    pub fn wants(&self, tex: &Textures) -> Vec<ImageJob> {
        let mut jobs: Vec<ImageJob> = Vec::new();
        let push =
            |jobs: &mut Vec<ImageJob>, key: Option<u64>, make: &dyn Fn() -> Option<ImageJob>| {
                if let Some(k) = key
                    && !tex.known(k)
                    && !jobs.iter().any(|j| j.key == k)
                    && let Some(j) = make()
                {
                    jobs.push(j);
                }
            };
        let (fr, fc) = self.focus;
        let (bw, bh) = (self.px(self.vp.lw / 2.0), self.px(540.0));
        let backdrop_job = |r: usize, c: usize| {
            let k = self.keys.get(r)?.get(c)?.backdrop?;
            let a = self.data.rows[r].cards[c].backdrop.as_ref()?;
            Some(ImageJob {
                key: k,
                source: ImageSource::Server(image_path(
                    &a.item_id,
                    ImageType::Backdrop,
                    &a.tag,
                    bw,
                    bh,
                )),
                w: bw,
                h: bh,
                finish: Finish::Backdrop { bg: BG_RGB },
            })
        };
        if fr < self.data.rows.len() {
            push(&mut jobs, self.keys[fr].get(fc).and_then(|k| k.backdrop), &|| {
                backdrop_job(fr, fc)
            });
        }
        let rows: Vec<usize> = [fr, fr + 1, fr + 2, fr.wrapping_sub(1)]
            .into_iter()
            .filter(|&r| r < self.data.rows.len())
            .collect();
        for &r in &rows {
            let (w, h) = self.data.rows[r].shape.size();
            let (bhw, bhh) = blurhash_size(w, h);
            for c in self.visible_cols(r) {
                let card = &self.data.rows[r].cards[c];
                push(&mut jobs, self.keys[r][c].blur, &|| {
                    Some(ImageJob {
                        key: self.keys[r][c].blur?,
                        source: ImageSource::BlurHash(card.image.as_ref()?.blurhash.clone()?),
                        w: bhw,
                        h: bhh,
                        finish: Finish::Card { radius: 0.0 },
                    })
                });
            }
        }
        for &r in &rows {
            let (w, h) = self.data.rows[r].shape.size();
            let (pw, ph) = (self.px(w), self.px(h));
            for c in self.visible_cols(r) {
                let card = &self.data.rows[r].cards[c];
                push(&mut jobs, self.keys[r][c].image, &|| {
                    let a = card.image.as_ref()?;
                    Some(ImageJob {
                        key: self.keys[r][c].image?,
                        source: ImageSource::Server(image_path(&a.item_id, a.kind, &a.tag, pw, ph)),
                        w: pw,
                        h: ph,
                        finish: Finish::Card { radius: RADIUS * self.vp.scale },
                    })
                });
            }
        }
        if fr < self.data.rows.len() {
            for c in [fc + 1, fc.wrapping_sub(1), fc + 2] {
                if c < self.data.rows[fr].cards.len() {
                    push(&mut jobs, self.keys[fr][c].backdrop, &|| backdrop_job(fr, c));
                }
            }
        }
        jobs
    }

    fn card_scale(&self, rc: (usize, usize), now: Instant) -> f32 {
        let t = self.focus_anim.value(now);
        if rc == self.focus {
            1.0 + FOCUS_SCALE * t
        } else if rc == self.prev_focus {
            1.0 + FOCUS_SCALE * (1.0 - t)
        } else {
            1.0
        }
    }

    pub fn draw<R: Renderer>(&self, r: &mut R, now: Instant, tex: &mut Textures, notice: &str) {
        let (lw, lh) = r.logical_size();
        let (sx, sy) = (self.sx(), 1080.0 * self.vp.safe);
        let screen = Rect::new(0.0, 0.0, lw, lh);

        // Backdrop crossfade; the scrim is baked into the texture.
        let fade = self.fade.value(now);
        let cur = self.backdrop_shown.and_then(|k| tex.get(k));
        let prev = if fade < 1.0 { self.backdrop_prev.and_then(|k| tex.get(k)) } else { None };
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

        // Header.
        r.text("Jellyroll", &TextStyle::new(30.0, Weight::Bold), Point::new(sx, sy), TEXT);
        let who = &self.data.user_name;
        let m = r.measure_text(who, &HEADER);
        r.text(who, &HEADER, Point::new(lw - sx - m.width, sy + 8.0), TEXT_DIM);
        if self.offline {
            let msg = "Offline · showing saved data";
            let st = TextStyle::new(18.0, Weight::Bold);
            let mo = r.measure_text(msg, &st);
            let x = lw - sx - m.width - mo.width - 40.0;
            r.fill_rrect(
                Rect::new(x - 12.0, sy + 2.0, mo.width + 24.0, mo.height + 12.0),
                8.0,
                Color::rgba(0, 0, 0, 140),
            );
            r.text(msg, &st, Point::new(x, sy + 8.0), NOTICE);
        }

        // Hero: the focused item.
        if let Some(card) = self.focused_card() {
            let mut y = sy + 110.0;
            r.text(&card.title, &HERO_TITLE, Point::new(sx, y), TEXT);
            y += 70.0;
            // Movies: the subtitle is the year, already first in the meta line.
            if !card.subtitle.is_empty() && !card.meta.starts_with(card.subtitle.as_str()) {
                r.text(&card.subtitle, &HERO_SUB, Point::new(sx, y), TEXT);
                y += 40.0;
            }
            if !card.meta.is_empty() {
                r.text(&card.meta, &HERO_META, Point::new(sx, y), TEXT_DIM);
                y += 38.0;
            }
            if !card.overview.is_empty() {
                r.text(&card.overview, &HERO_OVERVIEW, Point::new(sx, y), TEXT_DIM);
            }
        }

        // Rows, clipped below the hero.
        let clip_top = ROWS_TOP - 12.0;
        r.push_clip(Rect::new(0.0, clip_top, lw, lh - clip_top), 0.0);
        let v = self.v_scroll.value(now);
        for (ri, row) in self.data.rows.iter().enumerate() {
            let y = ROWS_TOP + self.row_tops[ri] - v;
            if y > lh {
                break;
            }
            if y + self.row_h(ri) < clip_top {
                continue;
            }
            r.text(&row.title, &LABEL, Point::new(sx, y), TEXT);
            let (w, h) = row.shape.size();
            let off = self.h_scroll[ri].value(now);
            let title_style = TextStyle::new(19.0, Weight::Regular).max_width(w);
            let sub_style = TextStyle::new(16.0, Weight::Regular).max_width(w);
            let first = ((off - w) / (w + GAP)).floor().max(0.0) as usize;
            for (ci, card) in row.cards.iter().enumerate().skip(first) {
                let x = sx + ci as f32 * (w + GAP) - off;
                if x > lw {
                    break;
                }
                let base = Rect::new(x, y + LABEL_H, w, h);
                let s = self.card_scale((ri, ci), now);
                let rect = Rect::new(
                    base.x - w * (s - 1.0) / 2.0,
                    base.y - h * (s - 1.0) / 2.0,
                    w * s,
                    h * s,
                );
                r.shadow(rect, RADIUS, 14.0, Color::rgba(0, 0, 0, 150));
                let keys = self.keys[ri][ci];
                if let Some(img) = keys.image.and_then(|k| tex.get(k)) {
                    r.image(img, rect, 0.0, 1.0);
                } else if let Some(img) = keys.blur.and_then(|k| tex.get(k)) {
                    r.image(img, rect, RADIUS, 1.0);
                } else {
                    r.fill_rrect(rect, RADIUS, Color::rgba(255, 255, 255, 22));
                }
                if let Some(p) = card.progress {
                    let track = Rect::new(rect.x + 10.0, rect.bottom() - 14.0, rect.w - 20.0, 5.0);
                    r.fill_rrect(track, 2.5, Color::rgba(0, 0, 0, 160));
                    r.fill_rrect(Rect::new(track.x, track.y, track.w * p, track.h), 2.5, ACCENT);
                }
                let focus = (s - 1.0) / FOCUS_SCALE;
                if focus > 0.01 {
                    r.stroke_rrect(
                        rect.inset(-3.0),
                        RADIUS + 3.0,
                        3.0,
                        Color::WHITE.with_alpha(focus),
                    );
                }
                let focused = (ri, ci) == self.focus;
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
        r.pop();

        if let Some((pr, pc)) = self.picked
            && let Some(c) = self.data.rows.get(pr).and_then(|row| row.cards.get(pc))
        {
            let st = TextStyle::new(20.0, Weight::Regular).max_width(lw / 2.0);
            let m = r.measure_text(&c.title, &st);
            r.text(&c.title, &st, Point::new(lw - sx - m.width, sy + 40.0), ACCENT);
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
    use crate::home::model::{Card, Row, Shape};

    fn card(id: &str) -> Card {
        Card {
            id: id.into(),
            title: id.into(),
            subtitle: String::new(),
            meta: String::new(),
            overview: String::new(),
            image: Some(Art {
                item_id: id.into(),
                kind: ImageType::Primary,
                tag: "t".into(),
                blurhash: Some("00000N".into()),
            }),
            backdrop: Some(Art {
                item_id: id.into(),
                kind: ImageType::Backdrop,
                tag: "b".into(),
                blurhash: None,
            }),
            progress: None,
        }
    }

    fn data(rows: &[(&str, Shape, usize)]) -> HomeData {
        HomeData {
            server_name: "s".into(),
            user_name: "u".into(),
            rows: rows
                .iter()
                .map(|(k, shape, n)| Row {
                    key: (*k).into(),
                    title: (*k).into(),
                    shape: *shape,
                    cards: (0..*n).map(|i| card(&format!("{k}{i}"))).collect(),
                })
                .collect(),
        }
    }

    #[test]
    fn navigates_rows_and_scrolls() {
        let t = Instant::now();
        let mut v = HomeView::new(
            data(&[("a", Shape::Landscape, 20), ("b", Shape::Poster, 30)]),
            1920.0,
            0.025,
            2.0,
            t,
        );
        assert!(!v.on_action(InputAction::Up, t), "top row");
        for _ in 0..8 {
            assert!(v.on_action(InputAction::Right, t));
        }
        assert_eq!(v.focus, (0, 8));
        assert!(v.h_scroll[0].target() > 0.0, "row scrolled to keep focus visible");
        assert!(v.on_action(InputAction::Down, t));
        assert_eq!(v.focus.0, 1);
        assert_eq!(v.v_scroll.target(), v.row_tops[1], "focused row moves to the rows line");
        assert!(!v.on_action(InputAction::Down, t), "bottom row");
        // The card picked below is the one under the focused landscape card.
        assert!(v.focus.1 > 0);
    }

    #[test]
    fn refresh_keeps_focus_on_the_same_item() {
        let t = Instant::now();
        let mut v = HomeView::new(
            data(&[("a", Shape::Landscape, 5), ("b", Shape::Poster, 10)]),
            1920.0,
            0.025,
            1.0,
            t,
        );
        v.on_action(InputAction::Down, t);
        for _ in 0..4 {
            v.on_action(InputAction::Right, t);
        }
        let id = v.focused_card().unwrap().id.clone();
        // New data: an extra row on top and a new first card in row "b".
        let mut d = data(&[
            ("new", Shape::Poster, 3),
            ("a", Shape::Landscape, 5),
            ("b", Shape::Poster, 10),
        ]);
        d.rows[2].cards.insert(0, card("fresh"));
        v.set_data(d, t);
        assert_eq!(v.focused_card().unwrap().id, id);
        assert_eq!(v.focus, (2, 5));
    }

    #[test]
    fn wants_put_visible_work_first_and_skip_known() {
        let t = Instant::now();
        let v = HomeView::new(
            data(&[
                ("a", Shape::Poster, 40),
                ("b", Shape::Poster, 40),
                ("c", Shape::Poster, 40),
                ("d", Shape::Poster, 40),
            ]),
            1920.0,
            0.025,
            2.0,
            t,
        );
        let mut tex = Textures::new(1 << 30);
        let w = v.wants(&tex);
        assert!(matches!(w[0].finish, Finish::Backdrop { .. }), "focused backdrop first");
        assert!(matches!(w[1].source, ImageSource::BlurHash(_)), "then placeholders");
        let posters: Vec<_> = w
            .iter()
            .filter(|j| matches!(j.finish, Finish::Card { radius } if radius > 0.0))
            .collect();
        assert_eq!((posters[0].w, posters[0].h), (320, 480), "poster at 2x output scale");
        // Rows a, b, c (focused + 2 below), visible columns + prefetch only.
        let cols = v.visible_cols(0).len();
        assert!(cols < 40);
        assert_eq!(posters.len(), 3 * cols);
        tex.queue(w[0].key, 1, 1, vec![0; 4]);
        assert!(
            v.wants(&tex).iter().all(|j| j.key != w[0].key),
            "queued images aren't re-requested"
        );
    }
}
