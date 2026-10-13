//! Search (spec 10.1, 8): an on-screen keyboard (or a physical keyboard)
//! and results in rows by type. The server is asked 300 ms after the last
//! keystroke (spec 8: debounced search), and answers for older text are
//! dropped.

use std::time::{Duration, Instant};

use jellyfin_api::endpoints::ItemsQuery;
use jellyroll_render::{Point, Rect, Renderer, TextStyle, Weight};

use super::model::{Card, Kind, Row, Shape};
use super::osk::{self, Osk};
use super::pages::{Ctx, Open};
use super::strings as s;
use super::textures::Textures;
use super::theme::{BG, TEXT, TEXT_DIM};
use super::widgets::{self, RowList, Viewport, Wants};
use crate::focus::{Dir, FocusGraph, NodeId};
use crate::input::InputAction;
use crate::net::{Fetch, Fetched};

const DEBOUNCE: Duration = Duration::from_millis(300);
const ROWS_TOP: f32 = 560.0;
const MAX_LEN: usize = 60;
/// (title, item types, card shape)
const KINDS: [(&str, &str, Shape); 3] = [
    (s::SEARCH_MOVIES, "Movie", Shape::Poster),
    (s::SEARCH_SHOWS, "Series", Shape::Poster),
    (s::SEARCH_EPISODES, "Episode", Shape::Landscape),
];

pub struct SearchView {
    vp: Viewport,
    text: String,
    osk: Osk,
    /// Focused key; None = a result card.
    key_focus: Option<usize>,
    rows: RowList,
    graph: FocusGraph,
    due: Option<Instant>,
    reqs: [Option<u64>; 3],
    results: [Vec<Card>; 3],
    searched: String,
}

impl SearchView {
    pub fn new(vp: Viewport, now: Instant) -> Self {
        let mut v = Self {
            osk: Osk::new(vp.sx(), vp.sy() + 160.0, 64.0, 56.0, 8.0, &osk::ROWS_LETTERS),
            vp,
            text: String::new(),
            key_focus: Some(0),
            rows: RowList::new(Vec::new(), ROWS_TOP, &vp, now),
            graph: FocusGraph::new(),
            due: None,
            reqs: [None; 3],
            results: Default::default(),
            searched: String::new(),
        };
        v.rows.active = false;
        v.rebuild_graph();
        v
    }

    pub fn set_viewport(&mut self, vp: Viewport, now: Instant) -> bool {
        if vp == self.vp {
            return false;
        }
        self.vp = vp;
        self.osk = Osk::new(vp.sx(), vp.sy() + 160.0, 64.0, 56.0, 8.0, &osk::ROWS_LETTERS);
        self.rows.layout(&vp, now);
        self.rebuild_graph();
        true
    }

    fn rebuild_graph(&mut self) {
        let mut nodes = self.rows.nodes(&self.vp);
        nodes.extend(self.osk.keys.iter().enumerate().map(|(i, k)| (i as NodeId, k.1)));
        self.graph.set_nodes(nodes);
        let id = match self.key_focus {
            Some(i) => i as NodeId,
            None => widgets::card_node(self.rows.focus.0, self.rows.focus.1),
        };
        self.graph.focus(id);
    }

    fn changed(&mut self, now: Instant) {
        self.due = Some(now + DEBOUNCE);
    }

    fn search(&mut self, ctx: &mut Ctx) {
        self.due = None;
        let term = self.text.trim().to_string();
        if term == self.searched {
            return;
        }
        self.searched = term.clone();
        if term.is_empty() {
            self.reqs = [None; 3];
            self.results = Default::default();
            return;
        }
        for (i, (_, types, shape)) in KINDS.iter().enumerate() {
            let query = ItemsQuery {
                include_types: Some((*types).into()),
                search: Some(term.clone()),
                limit: 24,
                ..Default::default()
            };
            self.reqs[i] = Some(ctx.fetch(Fetch::Items { query, shape: *shape }));
        }
    }

    fn set_rows(&mut self, now: Instant) {
        let rows = KINDS
            .iter()
            .zip(&self.results)
            .filter(|(_, cards)| !cards.is_empty())
            .map(|((title, _, shape), cards)| Row {
                key: (*title).into(),
                title: (*title).into(),
                shape: *shape,
                cards: cards.clone(),
            })
            .collect();
        let active = self.rows.active;
        self.rows.set_rows(rows, &self.vp, now);
        self.rows.active = active && !self.rows.is_empty();
        if self.rows.is_empty() && self.key_focus.is_none() {
            self.key_focus = Some(0);
        }
        self.rebuild_graph();
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.due
    }

    /// Fires the debounced search.
    pub fn update(&mut self, now: Instant, ctx: &mut Ctx) -> bool {
        if self.due.is_some_and(|d| now >= d) {
            self.search(ctx);
            return true;
        }
        false
    }

    pub fn on_action(&mut self, a: InputAction, now: Instant, ctx: &mut Ctx) -> bool {
        if let Some(dir) = Dir::from_action(a) {
            let step = self.rows.step(dir, &self.vp);
            let Some(id) = step.or_else(|| self.graph.navigate(dir)) else { return false };
            match widgets::card_of(id) {
                Some(rc) => {
                    self.key_focus = None;
                    self.rows.take_focus(rc, &self.vp, now);
                }
                None => {
                    self.key_focus = Some(id as usize);
                    self.rows.release_focus(now);
                }
            }
            self.rebuild_graph();
            return true;
        }
        match a {
            InputAction::TextInput(c) => {
                if self.text.chars().count() < MAX_LEN {
                    self.text.push(c);
                    self.changed(now);
                }
                true
            }
            InputAction::Backspace => {
                let changed = self.text.pop().is_some();
                if changed {
                    self.changed(now);
                }
                changed
            }
            // Enter: to the results if there are any (typing on a
            // keyboard, the results are many presses below the keys).
            InputAction::Submit if !self.rows.is_empty() && self.due.is_none() => {
                self.key_focus = None;
                self.rows.take_focus((0, 0), &self.vp, now);
                self.rebuild_graph();
                true
            }
            InputAction::Submit => {
                self.search(ctx);
                true
            }
            InputAction::Select => match self.key_focus {
                Some(i) => {
                    let key = self.osk.keys[i].0;
                    if self.osk.apply(key, &mut self.text, MAX_LEN) {
                        self.changed(now);
                    }
                    true
                }
                None => match self.rows.focused_card() {
                    Some(c) if !matches!(c.kind, Kind::Person | Kind::Library) => {
                        ctx.open = Some(Open::Detail { id: c.id.clone() });
                        true
                    }
                    _ => false,
                },
            },
            _ => false,
        }
    }

    pub fn on_fetched(&mut self, id: u64, result: &Fetched, now: Instant) -> bool {
        let Some(i) = self.reqs.iter().position(|r| *r == Some(id)) else { return false };
        self.reqs[i] = None;
        self.results[i] = match result {
            Fetched::Page { cards, .. } => cards.clone(),
            _ => Vec::new(),
        };
        self.set_rows(now);
        true
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        self.rows.is_animating(now)
    }

    pub fn wants(&self, w: &mut Wants) {
        self.rows.wants(w, &self.vp, false);
    }

    pub fn draw<R: Renderer>(&mut self, r: &mut R, now: Instant, tex: &mut Textures) {
        let (lw, _) = r.logical_size();
        let (sx, sy) = (self.vp.sx(), self.vp.sy());
        r.clear(BG);
        r.text(s::SEARCH_TITLE, &TextStyle::new(40.0, Weight::Bold), Point::new(sx, sy), TEXT);
        let field = Rect::new(sx, sy + 70.0, 64.0 * 9.0 + 8.0 * 8.0, 70.0);
        osk::draw_field(r, field, &self.text, s::SEARCH_HINT, true, false);
        self.osk.draw(r, self.key_focus);
        let hint_x = field.right() + 60.0;
        let pending = self.reqs.iter().any(|r| r.is_some()) || self.due.is_some();
        let msg = if self.text.trim().is_empty() {
            s::SEARCH_HINT
        } else if pending {
            s::LOADING
        } else if self.rows.is_empty() {
            s::SEARCH_NONE
        } else {
            ""
        };
        if !msg.is_empty() {
            let st = TextStyle::new(24.0, Weight::Regular).max_width(lw - hint_x - sx);
            r.text(msg, &st, Point::new(hint_x, sy + 90.0), TEXT_DIM);
        }
        self.rows.draw(r, now, tex, &self.vp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debounced_search_by_type() {
        let t = Instant::now();
        let vp = Viewport { lw: 1920.0, safe: 0.025, scale: 1.0 };
        let mut v = SearchView::new(vp, t);
        let mut next = 0;
        let mut ctx = Ctx::new(&mut next);
        for c in "dune".chars() {
            v.on_action(InputAction::TextInput(c), t, &mut ctx);
        }
        assert!(ctx.cmds.is_empty(), "nothing before the debounce");
        assert!(!v.update(t + Duration::from_millis(100), &mut ctx));
        assert!(v.update(t + DEBOUNCE, &mut ctx));
        assert_eq!(ctx.cmds.len(), 3, "movies, shows, episodes");
        let ids: Vec<u64> = v.reqs.iter().flatten().copied().collect();
        // An answer arrives: a Movies row.
        let card = Card {
            id: "m".into(),
            kind: Kind::Movie,
            collection: None,
            title: "Dune".into(),
            subtitle: String::new(),
            meta: String::new(),
            overview: String::new(),
            image: None,
            backdrop: None,
            progress: None,
        };
        v.on_fetched(ids[0], &Fetched::Page { start: 0, total: 1, cards: vec![card] }, t);
        assert_eq!(v.rows.rows.len(), 1);
        // Enter jumps to the results.
        v.on_action(InputAction::Submit, t, &mut ctx);
        assert!(v.rows.active && v.key_focus.is_none());
        // The on-screen keyboard types too.
        v.key_focus = Some(0);
        v.on_action(InputAction::Select, t, &mut ctx);
        assert_eq!(v.text, "dunea");
    }

    #[test]
    fn up_from_a_lower_row_goes_to_the_row_above() {
        let t = Instant::now();
        let vp = Viewport { lw: 1920.0, safe: 0.025, scale: 1.0 };
        let mut v = SearchView::new(vp, t);
        let mut next = 0;
        let mut ctx = Ctx::new(&mut next);
        let card = |id: &str| Card {
            id: id.into(),
            kind: Kind::Movie,
            collection: None,
            title: id.into(),
            subtitle: String::new(),
            meta: String::new(),
            overview: String::new(),
            image: None,
            backdrop: None,
            progress: None,
        };
        for c in "x".chars() {
            v.on_action(InputAction::TextInput(c), t, &mut ctx);
        }
        v.update(t + DEBOUNCE, &mut ctx);
        let ids: Vec<u64> = v.reqs.iter().flatten().copied().collect();
        for (i, id) in ids.iter().enumerate() {
            let cards = (0..5).map(|n| card(&format!("{i}-{n}"))).collect();
            v.on_fetched(*id, &Fetched::Page { start: 0, total: 5, cards }, t);
        }
        assert_eq!(v.rows.rows.len(), 3);
        v.on_action(InputAction::Submit, t, &mut ctx);
        v.on_action(InputAction::Down, t, &mut ctx);
        v.on_action(InputAction::Down, t, &mut ctx);
        assert_eq!(v.rows.focus.0, 2);
        v.on_action(InputAction::Up, t, &mut ctx);
        assert_eq!((v.rows.focus.0, v.key_focus), (1, None), "row above, not the keys");
        v.on_action(InputAction::Up, t, &mut ctx);
        assert_eq!(v.rows.focus.0, 0);
        v.on_action(InputAction::Up, t, &mut ctx);
        assert!(v.key_focus.is_some(), "from the first row, up to the keyboard");
    }
}
