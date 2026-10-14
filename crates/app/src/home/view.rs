//! The home screen (spec 10.1): a nav bar (Home, each library, Search,
//! Settings), the focused item's backdrop and details in a hero area, and
//! rows of cards below. Focus moves spatially over the nav pills and cards
//! (D27). Select opens the focused card (library grid or detail page).

use std::time::Instant;

use jellyroll_render::{Color, Point, Rect, Renderer, TextStyle, Weight};

use super::model::{HomeData, Kind};
use super::pages::{Ctx, Open};
use super::strings as s;
use super::textures::Textures;
use super::theme::{NOTICE, TEXT, TEXT_DIM};
use super::widgets::{self, Backdrop, RowList, Step, Viewport, Wants};
use crate::focus::{Dir, FocusGraph, NodeId};
use crate::input::InputAction;

/// Logical y of the focused row's label.
const ROWS_TOP: f32 = 560.0;
const NAV_H: f32 = 44.0;
const NAV_GAP: f32 = 12.0;

const HERO_TITLE: TextStyle = TextStyle::new(52.0, Weight::Bold).max_width(1100.0);
const HERO_SUB: TextStyle = TextStyle::new(26.0, Weight::Regular).max_width(1100.0);
const HERO_META: TextStyle = TextStyle::new(22.0, Weight::Regular).max_width(1100.0);
const HERO_OVERVIEW: TextStyle = TextStyle::new(22.0, Weight::Regular).max_width(1100.0);
const HEADER: TextStyle = TextStyle::new(18.0, Weight::Regular);

struct NavItem {
    label: String,
    open: Option<Open>,
    rect: Rect,
}

pub struct HomeView {
    pub data: HomeData,
    /// Shown from cache while the server is unreachable.
    pub offline: bool,
    vp: Viewport,
    rows: RowList,
    nav: Vec<NavItem>,
    /// Nav laid out for these labels (measured on the first draw).
    nav_ready: bool,
    nav_focus: Option<usize>,
    graph: FocusGraph,
    backdrop: Backdrop,
}

impl HomeView {
    pub fn new(data: HomeData, vp: Viewport, now: Instant) -> Self {
        let rows = RowList::new(data.rows.clone(), ROWS_TOP, &vp, now);
        let mut v = Self {
            data,
            offline: false,
            vp,
            rows,
            nav: Vec::new(),
            nav_ready: false,
            nav_focus: None,
            graph: FocusGraph::new(),
            backdrop: Backdrop::new(now),
        };
        v.build_nav();
        v.rebuild_graph();
        v
    }

    /// Fresh data: rows keep focus and scroll (see `RowList::set_rows`).
    pub fn set_data(&mut self, data: HomeData, now: Instant) {
        self.rows.set_rows(data.rows.clone(), &self.vp, now);
        self.data = data;
        let focus = self.nav_focus.map(|i| self.nav[i].label.clone());
        self.build_nav();
        self.nav_focus = focus.and_then(|l| self.nav.iter().position(|n| n.label == l));
        if self.nav_focus.is_none() {
            self.rows.active = true;
        }
        self.rebuild_graph();
    }

    pub fn set_viewport(&mut self, vp: Viewport, now: Instant) -> bool {
        if vp == self.vp {
            return false;
        }
        self.vp = vp;
        self.rows.layout(&vp, now);
        self.nav_ready = false;
        self.rebuild_graph();
        true
    }

    /// Nav items: Home, each library (from the My Media row), Search,
    /// Settings. Positions are set on the first draw (text widths).
    fn build_nav(&mut self) {
        let mut nav =
            vec![NavItem { label: s::NAV_HOME.into(), open: None, rect: Rect::default() }];
        if let Some(views) = self.data.rows.iter().find(|r| r.key == "views") {
            for c in &views.cards {
                nav.push(NavItem {
                    label: c.title.clone(),
                    open: Some(Open::Library {
                        id: c.id.clone(),
                        title: c.title.clone(),
                        collection: c.collection,
                    }),
                    rect: Rect::default(),
                });
            }
        }
        nav.push(NavItem {
            label: s::NAV_SEARCH.into(),
            open: Some(Open::Search),
            rect: Rect::default(),
        });
        nav.push(NavItem {
            label: s::NAV_SETTINGS.into(),
            open: Some(Open::Settings),
            rect: Rect::default(),
        });
        self.nav = nav;
        self.nav_ready = false;
    }

    fn layout_nav<R: Renderer>(&mut self, r: &mut R) {
        if self.nav_ready {
            return;
        }
        let mut x = self.vp.sx() + 190.0;
        let y = self.vp.sy() - 4.0;
        for item in &mut self.nav {
            let w = widgets::button_width(r, &item.label);
            item.rect = Rect::new(x, y, w, NAV_H);
            x += w + NAV_GAP;
        }
        self.nav_ready = true;
        self.rebuild_graph();
    }

    fn rebuild_graph(&mut self) {
        let mut nodes = self.rows.nodes(&self.vp);
        if self.nav_ready {
            nodes.extend(self.nav.iter().enumerate().map(|(i, n)| (i as NodeId, n.rect)));
        }
        self.graph.set_nodes(nodes);
        let id = match self.nav_focus {
            Some(i) => i as NodeId,
            None => widgets::card_node(self.rows.focus.0, self.rows.focus.1),
        };
        self.graph.focus(id);
    }

    /// Home pressed on the home screen: back to the top, on the nav bar's
    /// Home tab, rows scrolled to the start.
    pub fn go_to_top(&mut self, now: Instant) {
        self.rows.reset_focus(&self.vp, now);
        self.rows.release_focus(now);
        self.nav_focus = Some(0);
        self.rebuild_graph();
    }

    pub fn on_action(&mut self, a: InputAction, now: Instant, ctx: &mut Ctx) -> bool {
        if let Some(dir) = Dir::from_action(a) {
            let id = match self.rows.step(dir, &self.vp) {
                Step::To(id) => id,
                Step::Stay => return true,
                Step::Free => match self.graph.navigate(dir) {
                    Some(id) => id,
                    None => return false,
                },
            };
            match widgets::card_of(id) {
                Some(rc) => {
                    self.nav_focus = None;
                    self.rows.take_focus(rc, &self.vp, now);
                }
                None => {
                    self.nav_focus = Some(id as usize);
                    self.rows.release_focus(now);
                }
            }
            self.rebuild_graph();
            return true;
        }
        if a != InputAction::Select {
            return false;
        }
        if let Some(i) = self.nav_focus {
            ctx.open = self.nav[i].open.clone();
            return ctx.open.is_some();
        }
        let Some(card) = self.rows.focused_card() else { return false };
        ctx.open = match card.kind {
            Kind::Library => Some(Open::Library {
                id: card.id.clone(),
                title: card.title.clone(),
                collection: card.collection,
            }),
            _ => Some(Open::Detail { id: card.id.clone() }),
        };
        ctx.open.is_some()
    }

    /// Backdrop crossfade once the focused card's backdrop is loaded.
    pub fn update(&mut self, now: Instant, tex: &Textures) -> bool {
        let want = self.rows.focused_keys().and_then(|k| k.backdrop);
        self.backdrop.update(want, now, tex)
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        self.rows.is_animating(now) || self.backdrop.is_animating(now)
    }

    /// The focused backdrop first, then rows (see `RowList::wants`).
    pub fn wants(&self, w: &mut Wants) {
        w.backdrop(self.rows.focused_card().and_then(|c| c.backdrop.as_ref()));
        self.rows.wants(w, &self.vp, true);
    }

    pub fn draw<R: Renderer>(&mut self, r: &mut R, now: Instant, tex: &mut Textures, notice: &str) {
        self.layout_nav(r);
        let (lw, lh) = r.logical_size();
        let (sx, sy) = (self.vp.sx(), self.vp.sy());
        self.backdrop.draw(r, now, tex);

        // Header: name, nav pills, user (and the offline badge).
        r.text("Jellyroll", &TextStyle::new(30.0, Weight::Bold), Point::new(sx, sy), TEXT);
        for (i, n) in self.nav.iter().enumerate() {
            widgets::draw_button(r, n.rect, &n.label, self.nav_focus == Some(i), i == 0);
        }
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
        if let Some(card) = self.rows.focused_card() {
            let mut y = sy + 120.0;
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

        self.rows.draw(r, now, tex, &self.vp);

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
    use crate::home::model::{Art, Card, Row, Shape};
    use jellyfin_api::endpoints::ImageType;

    fn card(id: &str, kind: Kind) -> Card {
        Card {
            id: id.into(),
            kind,
            collection: None,
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
            played: false,
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
                    cards: (0..*n)
                        .map(|i| {
                            card(
                                &format!("{k}{i}"),
                                if *k == "views" { Kind::Library } else { Kind::Movie },
                            )
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    const VP: Viewport = Viewport { lw: 1920.0, safe: 0.025, scale: 2.0 };

    #[test]
    fn select_opens_the_right_page() {
        let t = Instant::now();
        let mut v = HomeView::new(
            data(&[("views", Shape::Landscape, 2), ("latest", Shape::Poster, 30)]),
            VP,
            t,
        );
        let mut next = 0;
        let mut ctx = Ctx::new(&mut next);
        assert!(v.on_action(InputAction::Select, t, &mut ctx));
        assert!(matches!(ctx.open, Some(Open::Library { ref id, .. }) if id == "views0"));
        let mut ctx = Ctx::new(&mut next);
        v.on_action(InputAction::Down, t, &mut ctx);
        v.on_action(InputAction::Select, t, &mut ctx);
        assert!(matches!(ctx.open, Some(Open::Detail { ref id }) if id == "latest0"));
    }

    #[test]
    fn nav_has_libraries_search_and_settings() {
        let t = Instant::now();
        let v = HomeView::new(data(&[("views", Shape::Landscape, 2)]), VP, t);
        let labels: Vec<&str> = v.nav.iter().map(|n| n.label.as_str()).collect();
        assert_eq!(labels, ["Home", "views0", "views1", "Search", "Settings"]);
    }

    #[test]
    fn refresh_keeps_focus_on_the_same_item() {
        let t = Instant::now();
        let mut v =
            HomeView::new(data(&[("a", Shape::Landscape, 5), ("b", Shape::Poster, 10)]), VP, t);
        let mut next = 0;
        let mut ctx = Ctx::new(&mut next);
        v.on_action(InputAction::Down, t, &mut ctx);
        for _ in 0..4 {
            v.on_action(InputAction::Right, t, &mut ctx);
        }
        let id = v.rows.focused_card().unwrap().id.clone();
        let mut d = data(&[
            ("new", Shape::Poster, 3),
            ("a", Shape::Landscape, 5),
            ("b", Shape::Poster, 10),
        ]);
        d.rows[2].cards.insert(0, card("fresh", Kind::Movie));
        v.set_data(d, t);
        assert_eq!(v.rows.focused_card().unwrap().id, id);
        assert_eq!(v.rows.focus, (2, 5));
    }

    #[test]
    fn wants_put_visible_work_first() {
        let t = Instant::now();
        let v = HomeView::new(
            data(&[
                ("a", Shape::Poster, 40),
                ("b", Shape::Poster, 40),
                ("c", Shape::Poster, 40),
                ("d", Shape::Poster, 40),
            ]),
            VP,
            t,
        );
        let tex = Textures::new(1 << 30);
        let mut w = Wants::new(&tex, VP);
        v.wants(&mut w);
        let jobs = w.jobs;
        assert!(
            matches!(jobs[0].finish, crate::net::Finish::Backdrop { .. }),
            "focused backdrop first"
        );
        assert!(
            matches!(jobs[1].source, crate::net::ImageSource::BlurHash(_)),
            "then placeholders"
        );
        let posters = jobs
            .iter()
            .filter(|j| matches!(j.finish, crate::net::Finish::Card { radius } if radius > 0.0))
            .count();
        assert!(posters > 0 && posters < 3 * 40, "visible columns only: {posters}");
    }
}
