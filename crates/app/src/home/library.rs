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

use super::blade::{self, Blade, Row};
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

/// Which list the blade shows.
enum Pick {
    /// "Reverse order", then the sorts.
    Sort,
    /// The genre of each row; None for "Clear all".
    Genre(Vec<Option<String>>),
}

pub struct LibraryView {
    id: String,
    title: String,
    shape: Shape,
    include_types: Option<String>,
    sort: usize,
    unwatched: bool,
    favorites: bool,
    /// Selected genres (items in any of them are shown).
    genres: Vec<String>,
    /// The sort's order reversed.
    reverse: bool,
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
    picker: Option<(Pick, Blade)>,
    genres_req: Option<u64>,
    letter_req: Option<u64>,
    bar_rects: Vec<Rect>,
    /// "1234 items" and "Genre: Drama", formatted when they change (not
    /// per frame).
    count_label: String,
    genre_label: String,
    /// The Sort button's label (kept so drawing doesn't allocate).
    sort_label: String,
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
            genres: Vec::new(),
            reverse: false,
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
            genre_label: widgets::list_label(s::GENRE_ALL),
            sort_label: widgets::list_label(&s::fill(s::SORT_LABEL, &[SORTS[0].0])),
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
            descending: desc != self.reverse,
            unplayed: self.unwatched,
            favorites: self.favorites,
            genres: self.genres.clone(),
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
        if let Some((_, b)) = &mut self.picker {
            let outcome = b.on_action(a);
            if outcome == blade::Outcome::Stay {
                return true;
            }
            let Some((pick, b)) = self.picker.take() else { return true };
            let (sort, reverse, genres) = match (pick, outcome) {
                (Pick::Sort, blade::Outcome::Picked(i)) => {
                    (i.saturating_sub(1).min(SORTS.len() - 1), b.is_checked(0), self.genres.clone())
                }
                (Pick::Sort, _) => (self.sort, b.is_checked(0), self.genres.clone()),
                // "Clear all".
                (Pick::Genre(_), blade::Outcome::Picked(_)) => {
                    (self.sort, self.reverse, Vec::new())
                }
                (Pick::Genre(rows), _) => {
                    let mut g: Vec<String> = rows
                        .into_iter()
                        .enumerate()
                        .filter(|(k, _)| b.is_checked(*k))
                        .filter_map(|(_, name)| name)
                        .collect();
                    g.sort();
                    (self.sort, self.reverse, g)
                }
            };
            if (sort, reverse, &genres) != (self.sort, self.reverse, &self.genres) {
                self.sort = sort;
                self.reverse = reverse;
                self.genres = genres;
                self.update_labels();
                self.reset(ctx, now);
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
            (Focus::Grid(_), InputAction::Right) if self.sort == 0 && !self.reverse => {
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
                    let mut rows = vec![Row::check(s::REVERSE_ORDER, self.reverse)];
                    rows.extend(SORTS.iter().map(|s| Row::choice(s.0)));
                    let current = self.sort + 1;
                    let b = Blade::with_rows(s::SORT_TITLE, rows, Some(current), current, now)
                        .divider_after(0);
                    self.picker = Some((Pick::Sort, b));
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
                // Selected genres first (easy to untick), "Clear all" above
                // them when there are several; then the rest.
                let mut rows = Vec::new();
                let mut names_by_row = Vec::new();
                if self.genres.len() > 1 {
                    rows.push(Row::action(s::CLEAR_ALL));
                    names_by_row.push(None);
                }
                for g in &self.genres {
                    rows.push(Row::check(g.clone(), true));
                    names_by_row.push(Some(g.clone()));
                }
                let selected_end = rows.len();
                for n in names.iter().filter(|n| !self.genres.contains(n)) {
                    rows.push(Row::check(n.clone(), false));
                    names_by_row.push(Some(n.clone()));
                }
                let mut b = Blade::with_rows(s::GENRE_TITLE, rows, None, 0, Instant::now());
                if selected_end > 0 {
                    b = b.divider_after(selected_end - 1);
                }
                self.picker = Some((Pick::Genre(names_by_row), b));
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
        self.anim.is_animating(now)
            || self.v_scroll.is_animating(now)
            || self.picker.as_ref().is_some_and(|(_, b)| b.is_animating(now))
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
                let w = match b {
                    BarItem::Unwatched | BarItem::Favorites => {
                        widgets::toggle_width(r, self.bar_label(b))
                    }
                    _ => widgets::button_width(r, self.bar_label(b)).max(150.0),
                };
                self.bar_rects.push(Rect::new(x, sy + BAR_Y, w, BAR_H));
                x += w + 12.0;
            }
        }
        for (i, b) in BAR.iter().enumerate() {
            let focused = self.focus == Focus::Bar(i) && self.picker.is_none();
            let rect = self.bar_rects[i];
            match b {
                BarItem::Unwatched => {
                    widgets::draw_toggle(r, rect, self.bar_label(*b), self.unwatched, focused)
                }
                BarItem::Favorites => {
                    widgets::draw_toggle(r, rect, self.bar_label(*b), self.favorites, focused)
                }
                BarItem::Genre => widgets::draw_button(
                    r,
                    rect,
                    self.bar_label(*b),
                    focused,
                    !self.genres.is_empty(),
                ),
                BarItem::Sort => widgets::draw_button(r, rect, self.bar_label(*b), focused, false),
            }
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
                let msg = if self.unwatched || self.favorites || !self.genres.is_empty() {
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
        if self.sort == 0 && !self.reverse && self.total.unwrap_or(0) > 0 {
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

        // Sort or genre list.
        if let Some((_, b)) = &self.picker {
            b.draw(r, now, self.vp.safe);
        }
    }

    /// The Sort and Genre button labels (kept, so drawing doesn't allocate).
    fn update_labels(&mut self) {
        let sort = SORTS[self.sort].0;
        let fmt = if self.reverse { s::SORT_REVERSED } else { s::SORT_LABEL };
        self.sort_label = widgets::list_label(&s::fill(fmt, &[sort]));
        self.genre_label = widgets::list_label(&match self.genres.as_slice() {
            [] => s::GENRE_ALL.to_string(),
            [g] => format!("{}{g}", s::GENRE_PREFIX),
            [g, rest @ ..] => s::fill(s::GENRES_MORE, &[g, &rest.len().to_string()]),
        });
        self.bar_rects.clear();
    }

    fn bar_label(&self, b: BarItem) -> &str {
        match b {
            BarItem::Sort => &self.sort_label,
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

    fn library(next: &mut u64) -> LibraryView {
        let mut ctx = Ctx::new(next);
        let t = Instant::now();
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
        v
    }

    fn press(v: &mut LibraryView, a: InputAction, next: &mut u64) -> Vec<ItemsQuery> {
        let mut ctx = Ctx::new(next);
        v.on_action(a, Instant::now(), &mut ctx);
        fetches(&ctx).into_iter().map(|f| f.1).collect()
    }

    fn open_genres(v: &mut LibraryView, next: &mut u64) {
        v.focus = Focus::Bar(3);
        let mut ctx = Ctx::new(next);
        v.on_action(InputAction::Select, Instant::now(), &mut ctx);
        let Some(crate::net::Cmd::Fetch { id, req: Fetch::Genres(_) }) = ctx.cmds.pop() else {
            panic!("genre list request")
        };
        let names = ["Comedy", "Drama", "Horror", "Thriller"].map(String::from).to_vec();
        v.on_fetched(id, &Fetched::Names(names), Instant::now(), &mut ctx);
        assert!(v.picker.is_some());
    }

    #[test]
    fn genres_multi_select() {
        let mut next = 0;
        let mut v = library(&mut next);
        open_genres(&mut v, &mut next);
        // Tick Drama and Thriller; nothing reloads until the list closes.
        assert!(press(&mut v, InputAction::Down, &mut next).is_empty());
        press(&mut v, InputAction::Select, &mut next);
        press(&mut v, InputAction::Down, &mut next);
        press(&mut v, InputAction::Down, &mut next);
        assert!(press(&mut v, InputAction::Select, &mut next).is_empty());
        let q = press(&mut v, InputAction::Back, &mut next);
        assert_eq!(q.last().unwrap().genres, ["Drama", "Thriller"]);
        assert_eq!(v.genre_label, widgets::list_label("Genres: Drama +1"));
        // Reopened: "Clear all", then the selected genres, then the rest.
        open_genres(&mut v, &mut next);
        let (_, b) = v.picker.as_ref().unwrap();
        assert!(b.is_checked(1) && b.is_checked(2) && !b.is_checked(3));
        let q = press(&mut v, InputAction::Select, &mut next);
        assert!(q.last().unwrap().genres.is_empty(), "Clear all");
        assert!(v.picker.is_none());
        // One selected: no "Clear all"; unticking it clears the filter.
        open_genres(&mut v, &mut next);
        press(&mut v, InputAction::Select, &mut next);
        press(&mut v, InputAction::Back, &mut next);
        assert_eq!(v.genres, ["Comedy"]);
        open_genres(&mut v, &mut next);
        press(&mut v, InputAction::Select, &mut next);
        let q = press(&mut v, InputAction::Back, &mut next);
        assert!(q.last().unwrap().genres.is_empty());
        // Closing without a change reloads nothing.
        open_genres(&mut v, &mut next);
        assert!(press(&mut v, InputAction::Back, &mut next).is_empty());
    }

    #[test]
    fn sort_list_with_reverse() {
        let mut next = 0;
        let mut v = library(&mut next);
        v.focus = Focus::Bar(0);
        press(&mut v, InputAction::Select, &mut next);
        // Opens on the current sort (Name, row 1); row 0 is Reverse order.
        press(&mut v, InputAction::Up, &mut next);
        assert!(press(&mut v, InputAction::Select, &mut next).is_empty(), "a toggle");
        let q = press(&mut v, InputAction::Back, &mut next);
        assert!(q.last().unwrap().descending, "name, reversed");
        assert_eq!(v.sort_label, widgets::list_label("Sort: Name, reversed"));
        // No A-Z column on a reversed name sort.
        v.focus_grid(v.cols() - 1, Instant::now(), &mut Ctx::new(&mut next));
        press(&mut v, InputAction::Right, &mut next);
        assert!(!matches!(v.focus, Focus::Letters(_)));
        // Pick Year: newest first by default, reversed: oldest first.
        v.focus = Focus::Bar(0);
        press(&mut v, InputAction::Select, &mut next);
        press(&mut v, InputAction::Down, &mut next);
        press(&mut v, InputAction::Down, &mut next);
        let q = press(&mut v, InputAction::Select, &mut next);
        let q = q.last().unwrap();
        assert_eq!(q.sort_by.as_deref(), Some("ProductionYear,SortName"));
        assert!(!q.descending);
    }
}
