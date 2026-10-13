//! Library grid (spec 10.1): every item of a library as a poster grid, with
//! a sort/filter bar, a genre picker and an A–Z jump column. Items load in
//! pages of 100 as you approach them (spec 8: pagination everywhere, with
//! prefetch); only rows near the viewport are drawn or kept in the focus
//! logic, so 10,000 items cost the same as 100 (spec 6.5: virtualization).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use jellyfin_api::dto::CollectionType;
use jellyfin_api::endpoints::ItemsQuery;
use jellyroll_render::{Color, Point, Rect, Renderer, TextStyle, Weight};

use super::model::{self, Card, Kind, Shape};
use super::pages::{Ctx, Open};
use super::strings as s;
use super::textures::Textures;
use super::theme::{BG, NOTICE, TEXT, TEXT_DIM};
use super::widgets::{self, GAP, TEXT_H, TITLE_GAP, Viewport, Wants};
use crate::anim::Tween;
use crate::input::InputAction;
use crate::net::{Fetch, Fetched};

const PAGE: u32 = 100;
const GRID_TOP: f32 = 150.0;
const BAR_Y: f32 = 70.0;
const BAR_H: f32 = 44.0;
const LETTERS_W: f32 = 44.0;
const LETTERS: &str = "#ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const SORTS: [(&str, &str, bool); 4] = [
    (s::SORT_NAME, "SortName", false),
    (s::SORT_ADDED, "DateCreated,SortName", true),
    (s::SORT_YEAR, "ProductionYear,SortName", true),
    (s::SORT_RATING, "CommunityRating,SortName", true),
];
const SCROLL_ANIM: Duration = Duration::from_millis(180);

#[derive(Copy, Clone, Debug, PartialEq)]
enum Focus {
    Grid(usize),
    Bar(usize),
    Letters(usize),
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum BarItem {
    Sort,
    Unwatched,
    Favorites,
    Genre,
}

const BAR: [BarItem; 4] = [BarItem::Sort, BarItem::Unwatched, BarItem::Favorites, BarItem::Genre];

/// The genre list overlay.
struct Picker {
    options: Vec<String>,
    focus: usize,
}

pub struct LibraryView {
    id: String,
    title: String,
    shape: Shape,
    include_types: Option<String>,
    sort: usize,
    unwatched: bool,
    favorites: bool,
    genre: Option<String>,
    /// Requested pages (by page number) of the current query.
    pages: HashMap<u32, u64>,
    total: Option<u32>,
    items: Vec<Option<Card>>,
    failed: Option<String>,
    focus: Focus,
    last_grid: usize,
    prev_grid: Option<usize>,
    anim: Tween,
    v_scroll: Tween,
    picker: Option<Picker>,
    genres_req: Option<u64>,
    letter_req: Option<u64>,
    bar_rects: Vec<Rect>,
    /// "1234 items" and "Genre: Drama", formatted when they change (not
    /// per frame).
    count_label: String,
    genre_label: String,
    vp: Viewport,
}

/// Item types a library's grid lists (recursive listings otherwise include
/// seasons, episodes, extras...).
fn include_types(c: Option<CollectionType>) -> Option<String> {
    Some(
        match c {
            Some(CollectionType::Movies) => "Movie",
            Some(CollectionType::Tvshows) => "Series",
            Some(CollectionType::Music) => "MusicAlbum",
            Some(CollectionType::Musicvideos) => "MusicVideo",
            Some(CollectionType::Boxsets) => "BoxSet",
            Some(CollectionType::Homevideos | CollectionType::Photos) => "Video,Photo,PhotoAlbum",
            Some(CollectionType::Playlists) => "Playlist",
            _ => "Movie,Series",
        }
        .into(),
    )
}

impl LibraryView {
    pub fn new(
        id: String,
        title: String,
        collection: Option<CollectionType>,
        vp: Viewport,
        now: Instant,
        ctx: &mut Ctx,
    ) -> Self {
        let mut v = Self {
            id,
            title,
            shape: model::library_shape(collection),
            include_types: include_types(collection),
            sort: 0,
            unwatched: false,
            favorites: false,
            genre: None,
            pages: HashMap::new(),
            total: None,
            items: Vec::new(),
            failed: None,
            focus: Focus::Grid(0),
            last_grid: 0,
            prev_grid: None,
            anim: Tween::settled(1.0, now),
            v_scroll: Tween::settled(0.0, now),
            picker: None,
            genres_req: None,
            letter_req: None,
            bar_rects: Vec::new(),
            count_label: String::new(),
            genre_label: s::GENRE_ALL.into(),
            vp,
        };
        v.load(ctx);
        v
    }

    fn query(&self) -> ItemsQuery {
        let (_, sort_by, desc) = SORTS[self.sort];
        ItemsQuery {
            parent_id: Some(self.id.clone()),
            include_types: self.include_types.clone(),
            sort_by: Some(sort_by.into()),
            descending: desc,
            unplayed: self.unwatched,
            favorites: self.favorites,
            genre: self.genre.clone(),
            ..Default::default()
        }
    }

    /// Starts over with the current sort/filters.
    fn reset(&mut self, ctx: &mut Ctx, now: Instant) {
        self.pages.clear();
        self.total = None;
        self.items.clear();
        self.failed = None;
        self.last_grid = 0;
        self.v_scroll = Tween::settled(0.0, now);
        self.load(ctx);
    }

    fn cell(&self) -> (f32, f32) {
        let (w, h) = self.shape.size();
        (w + GAP, h + TITLE_GAP + TEXT_H + 16.0)
    }

    fn cols(&self) -> usize {
        let avail = self.vp.lw - 2.0 * self.vp.sx() - LETTERS_W - 20.0;
        (((avail + GAP) / self.cell().0).floor() as usize).max(1)
    }

    fn visible_rows(&self) -> std::ops::Range<usize> {
        let (_, ch) = self.cell();
        let first = (self.v_scroll.target() / ch).floor().max(0.0) as usize;
        let n = ((1080.0 - self.vp.sy() - GRID_TOP) / ch).ceil() as usize + 1;
        first..first + n
    }

    /// Requests pages around the visible rows (and the first page).
    fn load(&mut self, ctx: &mut Ctx) {
        let cols = self.cols();
        let rows = self.visible_rows();
        let lo = rows.start.saturating_sub(2) * cols;
        let hi = (rows.end + 4) * cols;
        let total = self.total.unwrap_or(PAGE);
        for page in (lo as u32 / PAGE)..=((hi as u32).min(total.max(1) - 1) / PAGE) {
            if !self.pages.contains_key(&page) && page * PAGE < total.max(1) {
                let query = ItemsQuery { start: page * PAGE, limit: PAGE, ..self.query() };
                let id = ctx.fetch(Fetch::Items { query, shape: self.shape });
                self.pages.insert(page, id);
            }
        }
    }

    pub fn set_viewport(&mut self, vp: Viewport, _now: Instant) -> bool {
        if vp == self.vp {
            return false;
        }
        self.vp = vp;
        true
    }

    fn scroll_to(&mut self, i: usize, now: Instant) {
        let (_, ch) = self.cell();
        let row = i / self.cols();
        let target = (row as f32 - 1.0).max(0.0) * ch;
        self.v_scroll.retarget(target, now, SCROLL_ANIM);
    }

    fn focus_grid(&mut self, i: usize, now: Instant, ctx: &mut Ctx) {
        self.prev_grid = match self.focus {
            Focus::Grid(p) => Some(p),
            _ => None,
        };
        self.focus = Focus::Grid(i);
        self.last_grid = i;
        self.anim = Tween::settled(0.0, now);
        self.anim.retarget(1.0, now, widgets::FOCUS_ANIM);
        self.scroll_to(i, now);
        self.load(ctx);
    }

    fn leave_grid(&mut self, to: Focus, now: Instant) {
        if let Focus::Grid(p) = self.focus {
            self.prev_grid = Some(p);
            self.anim = Tween::settled(0.0, now);
            self.anim.retarget(1.0, now, widgets::FOCUS_ANIM);
        }
        self.focus = to;
    }

    fn letter_index(&self) -> usize {
        // The letter of the focused item, for entering the A–Z column.
        let c = self
            .items
            .get(self.last_grid)
            .and_then(|c| c.as_ref())
            .and_then(|c| c.title.chars().next())
            .map(|c| c.to_ascii_uppercase())
            .unwrap_or('#');
        LETTERS.find(c).unwrap_or(0)
    }

    /// The first page has arrived (for `--bench-scroll`).
    pub fn loaded(&self) -> bool {
        self.total.is_some()
    }

    pub fn on_action(&mut self, a: InputAction, now: Instant, ctx: &mut Ctx) -> bool {
        if let Some(p) = &mut self.picker {
            match a {
                InputAction::Up => p.focus = p.focus.saturating_sub(1),
                InputAction::Down => p.focus = (p.focus + 1).min(p.options.len() - 1),
                InputAction::Select => {
                    self.genre = (p.focus > 0).then(|| p.options[p.focus].clone());
                    self.genre_label = match &self.genre {
                        Some(g) => format!("{}{g}", s::GENRE_PREFIX),
                        None => s::GENRE_ALL.into(),
                    };
                    self.bar_rects.clear();
                    self.picker = None;
                    self.reset(ctx, now);
                }
                InputAction::Back => self.picker = None,
                _ => return false,
            }
            return true;
        }
        let total = self.total.unwrap_or(0) as usize;
        let cols = self.cols();
        match (self.focus, a) {
            (Focus::Grid(i), InputAction::Left) if i % cols > 0 => self.focus_grid(i - 1, now, ctx),
            (Focus::Grid(i), InputAction::Right) if i % cols < cols - 1 && i + 1 < total => {
                self.focus_grid(i + 1, now, ctx)
            }
            (Focus::Grid(_), InputAction::Right) if self.sort == 0 => {
                let l = self.letter_index();
                self.leave_grid(Focus::Letters(l), now);
            }
            (Focus::Grid(i), InputAction::Up) if i >= cols => self.focus_grid(i - cols, now, ctx),
            (Focus::Grid(i), InputAction::Up) => {
                let col = i % cols;
                self.leave_grid(Focus::Bar(col.min(BAR.len() - 1)), now);
            }
            (Focus::Grid(i), InputAction::Down) if i + cols < total => {
                self.focus_grid(i + cols, now, ctx)
            }
            (Focus::Grid(i), InputAction::Down) if i / cols < (total.saturating_sub(1)) / cols => {
                self.focus_grid(total - 1, now, ctx)
            }
            (Focus::Grid(i), InputAction::Select) => {
                let Some(Some(card)) = self.items.get(i) else { return false };
                ctx.open = match card.kind {
                    Kind::Person | Kind::Library => return false,
                    _ => Some(Open::Detail { id: card.id.clone() }),
                };
            }
            (Focus::Bar(b), InputAction::Left) if b > 0 => self.focus = Focus::Bar(b - 1),
            (Focus::Bar(b), InputAction::Right) if b + 1 < BAR.len() => {
                self.focus = Focus::Bar(b + 1)
            }
            (Focus::Bar(_), InputAction::Down) if total > 0 => {
                let i = self.visible_rows().start * cols;
                self.focus_grid(i.min(total - 1), now, ctx);
            }
            (Focus::Bar(b), InputAction::Select) => match BAR[b] {
                BarItem::Sort => {
                    self.sort = (self.sort + 1) % SORTS.len();
                    self.reset(ctx, now);
                }
                BarItem::Unwatched => {
                    self.unwatched = !self.unwatched;
                    self.reset(ctx, now);
                }
                BarItem::Favorites => {
                    self.favorites = !self.favorites;
                    self.reset(ctx, now);
                }
                BarItem::Genre => {
                    self.genres_req = Some(ctx.fetch(Fetch::Genres(self.id.clone())));
                }
            },
            (Focus::Letters(l), InputAction::Up) if l > 0 => self.focus = Focus::Letters(l - 1),
            (Focus::Letters(l), InputAction::Down) if l + 1 < LETTERS.len() => {
                self.focus = Focus::Letters(l + 1)
            }
            (Focus::Letters(_), InputAction::Left) if total > 0 => {
                let i = self.last_grid.min(total - 1);
                self.focus_grid(i, now, ctx);
            }
            (Focus::Letters(l), InputAction::Select) => {
                if l == 0 {
                    if total > 0 {
                        self.focus_grid(0, now, ctx);
                    }
                } else {
                    let letter = LETTERS.as_bytes()[l] as char;
                    self.letter_req =
                        Some(ctx.fetch(Fetch::LetterIndex { query: self.query(), letter }));
                }
            }
            _ => return false,
        }
        true
    }

    pub fn on_fetched(&mut self, id: u64, result: &Fetched, now: Instant, ctx: &mut Ctx) -> bool {
        if self.genres_req == Some(id) {
            self.genres_req = None;
            if let Fetched::Names(names) = result {
                let mut options = vec![s::GENRE_ALL.to_string()];
                options.extend(names.iter().cloned());
                let focus = self
                    .genre
                    .as_ref()
                    .and_then(|g| options.iter().position(|o| o == g))
                    .unwrap_or(0);
                self.picker = Some(Picker { options, focus });
                return true;
            }
            return false;
        }
        if self.letter_req == Some(id) {
            self.letter_req = None;
            if let (Fetched::Index(n), Some(total)) = (result, self.total)
                && total > 0
            {
                self.focus_grid((*n as usize).min(total as usize - 1), now, ctx);
                return true;
            }
            return false;
        }
        let Some(page) = self.pages.iter().find(|(_, r)| **r == id).map(|(p, _)| *p) else {
            return false;
        };
        match result {
            Fetched::Page { start, total, cards } => {
                if self.total != Some(*total) {
                    self.total = Some(*total);
                    self.count_label = format!("{total} {}", s::ITEMS);
                    self.items.resize(*total as usize, None);
                    self.load(ctx);
                }
                for (i, c) in cards.iter().enumerate() {
                    if let Some(slot) = self.items.get_mut(*start as usize + i) {
                        *slot = Some(c.clone());
                    }
                }
                if self.focus == Focus::Grid(0) && *total == 0 {
                    self.focus = Focus::Bar(0);
                }
                true
            }
            Fetched::Failed(e) => {
                self.pages.remove(&page);
                self.failed = Some(e.clone());
                true
            }
            _ => false,
        }
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        self.anim.is_animating(now) || self.v_scroll.is_animating(now)
    }

    fn cell_rect(&self, i: usize, v: f32) -> Rect {
        let (cw, ch) = self.cell();
        let (w, h) = self.shape.size();
        let cols = self.cols();
        let x = self.vp.sx() + (i % cols) as f32 * cw;
        let y = self.vp.sy() + GRID_TOP + (i / cols) as f32 * ch - v;
        Rect::new(x, y, w, h)
    }

    pub fn wants(&self, w: &mut Wants) {
        let cols = self.cols();
        let rows = self.visible_rows();
        let range = (rows.start * cols)..((rows.end + 1) * cols).min(self.items.len());
        // No backdrop: the grid doesn't show one, and at 4K each is a
        // 1920x1080 upload per step.
        for c in self.items[range.clone()].iter().flatten() {
            w.blur(c, self.shape, &widgets::card_keys(c, self.shape, &self.vp));
        }
        for c in self.items[range].iter().flatten() {
            w.image(c, self.shape, &widgets::card_keys(c, self.shape, &self.vp));
        }
    }

    fn focus_of(&self, i: usize, now: Instant) -> f32 {
        let t = self.anim.value(now);
        match self.focus {
            Focus::Grid(f) if f == i => t,
            _ if self.prev_grid == Some(i) => 1.0 - t,
            _ => 0.0,
        }
    }

    pub fn draw<R: Renderer>(&mut self, r: &mut R, now: Instant, tex: &mut Textures) {
        let (lw, lh) = r.logical_size();
        let (sx, sy) = (self.vp.sx(), self.vp.sy());
        r.clear(BG);

        // Title and count.
        let title = TextStyle::new(40.0, Weight::Bold).max_width(lw / 2.0);
        let m = r.text(&self.title, &title, Point::new(sx, sy), TEXT);
        if self.total.is_some() {
            let p = Point::new(sx + m.width + 24.0, sy + 14.0);
            r.text(&self.count_label, &TextStyle::new(22.0, Weight::Regular), p, TEXT_DIM);
        }

        // Sort/filter bar.
        if self.bar_rects.is_empty() {
            let mut x = sx;
            for b in BAR {
                let w = widgets::button_width(r, self.bar_label(b)).max(150.0);
                self.bar_rects.push(Rect::new(x, sy + BAR_Y, w, BAR_H));
                x += w + 12.0;
            }
        }
        for (i, b) in BAR.iter().enumerate() {
            let selected = match b {
                BarItem::Unwatched => self.unwatched,
                BarItem::Favorites => self.favorites,
                BarItem::Genre => self.genre.is_some(),
                BarItem::Sort => false,
            };
            widgets::draw_button(
                r,
                self.bar_rects[i],
                self.bar_label(*b),
                self.focus == Focus::Bar(i),
                selected,
            );
        }

        // Grid (only rows near the viewport).
        let top = sy + GRID_TOP - 16.0;
        r.push_clip(Rect::new(0.0, top, lw, lh - top), 0.0);
        let v = self.v_scroll.value(now);
        let cols = self.cols();
        let (_, ch) = self.cell();
        let first_row = ((v / ch).floor().max(0.0) as usize).saturating_sub(1);
        let last_row = ((v + lh) / ch).ceil() as usize + 1;
        for i in (first_row * cols)..(last_row * cols).min(self.items.len()) {
            let rect = self.cell_rect(i, v);
            let focused = self.focus == Focus::Grid(i);
            match &self.items[i] {
                Some(card) => {
                    let keys = widgets::card_keys(card, self.shape, &self.vp);
                    widgets::draw_card(
                        r,
                        tex,
                        rect,
                        self.focus_of(i, now),
                        card,
                        &keys,
                        focused,
                        true,
                    );
                }
                None => r.fill_rrect(rect, widgets::RADIUS, Color::rgba(255, 255, 255, 14)),
            }
        }
        r.pop();

        match self.total {
            None if self.failed.is_none() => {
                r.text(
                    s::LOADING,
                    &TextStyle::new(24.0, Weight::Regular),
                    Point::new(sx, sy + GRID_TOP),
                    TEXT_DIM,
                );
            }
            Some(0) => {
                let msg = if self.unwatched || self.favorites || self.genre.is_some() {
                    s::NO_MATCHES
                } else {
                    s::NOTHING_HERE
                };
                r.text(
                    msg,
                    &TextStyle::new(24.0, Weight::Regular),
                    Point::new(sx, sy + GRID_TOP),
                    TEXT_DIM,
                );
            }
            _ => {}
        }
        if let Some(e) = &self.failed {
            let st = TextStyle::new(20.0, Weight::Bold).max_width(lw - 2.0 * sx);
            r.text(e, &st, Point::new(sx, lh - sy - 30.0), NOTICE);
        }

        // A–Z column (name sort only).
        if self.sort == 0 && self.total.unwrap_or(0) > 0 {
            let x = lw - sx - LETTERS_W;
            let step = ((lh - sy * 2.0 - GRID_TOP) / LETTERS.len() as f32).min(32.0);
            let st = TextStyle::new(18.0, Weight::Bold);
            for (i, ch) in LETTERS.char_indices() {
                let y = sy + GRID_TOP + i as f32 * step;
                let focused = self.focus == Focus::Letters(i);
                if focused {
                    r.fill_rrect(Rect::new(x, y - 2.0, LETTERS_W, step), step / 2.0, Color::WHITE);
                }
                let mut b = [0u8; 4];
                let label = ch.encode_utf8(&mut b);
                let m = r.measure_text(label, &st);
                let c = if focused { BG } else { TEXT_DIM };
                r.text(
                    label,
                    &st,
                    Point::new((x + (LETTERS_W - m.width) / 2.0).round(), y + 2.0),
                    c,
                );
            }
        }

        // Genre picker overlay.
        if let Some(p) = &self.picker {
            let w = 520.0;
            let x = lw - sx - w;
            r.fill_rect(Rect::new(0.0, 0.0, lw, lh), Color::rgba(0, 0, 0, 140));
            r.fill_rrect(Rect::new(x, sy, w, lh - 2.0 * sy), 16.0, Color::rgba(28, 30, 38, 250));
            let item_h = 52.0;
            let visible = ((lh - 2.0 * sy - 40.0) / item_h) as usize;
            let first =
                p.focus.saturating_sub(visible / 2).min(p.options.len().saturating_sub(visible));
            let st = TextStyle::new(24.0, Weight::Regular).max_width(w - 60.0);
            for (k, opt) in p.options.iter().enumerate().skip(first).take(visible) {
                let y = sy + 20.0 + (k - first) as f32 * item_h;
                let rect = Rect::new(x + 16.0, y, w - 32.0, item_h - 6.0);
                let focused = k == p.focus;
                if focused {
                    r.fill_rrect(rect, 10.0, Color::WHITE);
                }
                r.text(
                    opt,
                    &st,
                    Point::new(rect.x + 18.0, rect.y + 8.0),
                    if focused { BG } else { TEXT },
                );
            }
        }
    }

    fn bar_label(&self, b: BarItem) -> &str {
        match b {
            BarItem::Sort => SORTS[self.sort].0,
            BarItem::Unwatched => s::FILTER_UNWATCHED,
            BarItem::Favorites => s::FILTER_FAVORITES,
            BarItem::Genre => &self.genre_label,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VP: Viewport = Viewport { lw: 1920.0, safe: 0.025, scale: 1.0 };

    fn page(v: &mut LibraryView, id: u64, start: u32, total: u32, n: usize, ctx: &mut Ctx) {
        let cards = (0..n)
            .map(|i| model::Card {
                id: format!("m{}", start as usize + i),
                kind: Kind::Movie,
                collection: None,
                title: format!("Movie {}", start as usize + i),
                subtitle: String::new(),
                meta: String::new(),
                overview: String::new(),
                image: None,
                backdrop: None,
                progress: None,
            })
            .collect();
        v.on_fetched(id, &Fetched::Page { start, total, cards }, Instant::now(), ctx);
    }

    fn fetches(ctx: &Ctx) -> Vec<(u64, ItemsQuery)> {
        ctx.cmds
            .iter()
            .filter_map(|c| match c {
                crate::net::Cmd::Fetch { id, req: Fetch::Items { query, .. } } => {
                    Some((*id, query.clone()))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn pages_load_as_you_scroll_10k() {
        let t = Instant::now();
        let mut next = 0;
        let mut ctx = Ctx::new(&mut next);
        let mut v = LibraryView::new(
            "lib".into(),
            "Movies".into(),
            Some(CollectionType::Movies),
            VP,
            t,
            &mut ctx,
        );
        let first = fetches(&ctx);
        assert_eq!(first.len(), 1, "one page before the total is known");
        assert_eq!(first[0].1.include_types.as_deref(), Some("Movie"));
        let (id0, _) = first[0];
        let mut ctx = Ctx::new(&mut next);
        page(&mut v, id0, 0, 10_000, 100, &mut ctx);
        assert_eq!(v.items.len(), 10_000);
        let cols = v.cols();
        assert!(cols >= 8, "{cols} columns at 1080p");
        // Hold Down: pages load ahead, never everything at once.
        let mut ctx = Ctx::new(&mut next);
        for _ in 0..30 {
            v.on_action(InputAction::Down, t, &mut ctx);
        }
        assert_eq!(v.focus, Focus::Grid(30 * cols));
        let starts: Vec<u32> = fetches(&ctx).iter().map(|f| f.1.start).collect();
        assert!(starts.iter().all(|s| s % PAGE == 0));
        assert!(starts.len() < 10, "{starts:?}");
        assert!(v.pages.len() < 12);
    }

    #[test]
    fn filters_reload_and_letters_jump() {
        let t = Instant::now();
        let mut next = 0;
        let mut ctx = Ctx::new(&mut next);
        let mut v = LibraryView::new(
            "lib".into(),
            "Movies".into(),
            Some(CollectionType::Movies),
            VP,
            t,
            &mut ctx,
        );
        let (id0, _) = fetches(&ctx)[0];
        page(&mut v, id0, 0, 500, 100, &mut ctx);
        // Up from the first row: the bar. Right to Unwatched, Select.
        let mut ctx = Ctx::new(&mut next);
        v.on_action(InputAction::Up, t, &mut ctx);
        assert_eq!(v.focus, Focus::Bar(0));
        v.on_action(InputAction::Right, t, &mut ctx);
        v.on_action(InputAction::Select, t, &mut ctx);
        let f = fetches(&ctx);
        assert!(f.last().unwrap().1.unplayed, "reloads with the filter");
        assert_eq!(v.total, None);
        // Letters: from the grid, Right at the row end enters the column.
        let (id, _) = *f.last().unwrap();
        let mut ctx = Ctx::new(&mut next);
        page(&mut v, id, 0, 500, 100, &mut ctx);
        v.focus_grid(v.cols() - 1, t, &mut ctx);
        v.on_action(InputAction::Right, t, &mut ctx);
        assert!(matches!(v.focus, Focus::Letters(_)));
        v.focus = Focus::Letters(LETTERS.find('M').unwrap());
        let mut ctx = Ctx::new(&mut next);
        v.on_action(InputAction::Select, t, &mut ctx);
        let Some(crate::net::Cmd::Fetch { id, req: Fetch::LetterIndex { letter, .. } }) =
            ctx.cmds.pop()
        else {
            panic!("letter index request")
        };
        assert_eq!(letter, 'M');
        let mut ctx = Ctx::new(&mut next);
        v.on_fetched(id, &Fetched::Index(240), t, &mut ctx);
        assert_eq!(v.focus, Focus::Grid(240));
    }
}
