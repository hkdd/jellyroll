//! Item detail page (spec 10.1): backdrop, title, metadata line, codec
//! badges, overview, play/resume, watched and favorite toggles; seasons
//! and episodes for shows; cast; similar items.
//!
//! Badges for video the GPU can't decode are red with a plain explanation
//! (D19: say clearly when this hardware can't play something).

use std::time::Instant;

use jellyroll_render::{Color, Point, Rect, Renderer, TextStyle, Weight};
use jellyroll_video::va::DecodeCaps;

use super::blade::{self, Blade};
use super::model::{Art, Card, Detail, Kind, Row, Shape};
use super::pages::{Ctx, Open};
use super::status::wrap;
use super::strings as s;
use super::textures::Textures;
use super::theme::{NOTICE, TEXT, TEXT_DIM};
use super::widgets::{self, Backdrop, RowList, Step, Viewport, Wants};
use crate::anim::Tween;
use crate::focus::{Dir, FocusGraph, NodeId};
use crate::input::InputAction;
use crate::net::{Changed, Fetch, Fetched, TrackChoice};

const ROWS_TOP: f32 = 700.0;
const BUTTONS_Y: f32 = 590.0;
const BUTTON_H: f32 = 52.0;
/// The overview's node in the focus graph (buttons are 0.., cards higher).
const OVERVIEW_NODE: NodeId = 1 << 20;
const EXPAND_IN: std::time::Duration = std::time::Duration::from_millis(180);
const EXPAND_OUT: std::time::Duration = std::time::Duration::from_millis(120);
const TEXT_W: f32 = 1150.0;

const TITLE: TextStyle = TextStyle::new(56.0, Weight::Bold).max_width(TEXT_W);
const SUB: TextStyle = TextStyle::new(28.0, Weight::Regular).max_width(TEXT_W);
const META: TextStyle = TextStyle::new(22.0, Weight::Regular).max_width(TEXT_W);
const BADGE: TextStyle = TextStyle::new(18.0, Weight::Bold);
const BODY: TextStyle = TextStyle::new(23.0, Weight::Regular);
const WARN: TextStyle = TextStyle::new(19.0, Weight::Bold).max_width(TEXT_W);

#[derive(Copy, Clone, Debug, PartialEq)]
enum Action {
    Play {
        start_ticks: i64,
    },
    /// Choose the audio track or subtitles before Play.
    Audio,
    Subtitles,
    Watched,
    Favorite,
    /// Write a debug report (D83).
    Debug,
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum Pick {
    Audio,
    Subtitles,
}

struct Button {
    action: Action,
    label: String,
    rect: Rect,
}

pub struct DetailView {
    id: String,
    vp: Viewport,
    req: Option<u64>,
    detail: Option<Detail>,
    failed: Option<String>,
    backdrop: Backdrop,
    buttons: Vec<Button>,
    buttons_ready: bool,
    /// Focused button; None = a card in the rows.
    button_focus: Option<usize>,
    rows: RowList,
    graph: FocusGraph,
    seasons: Vec<Card>,
    episodes: Vec<Card>,
    cast: Vec<Card>,
    similar: Vec<Card>,
    season_title: String,
    seasons_req: Option<u64>,
    episodes_req: Option<u64>,
    /// The season whose episodes show (series, season), to load again
    /// when they change on the server.
    episodes_of: Option<(String, String)>,
    /// A season was selected: focus its episodes when they arrive (they
    /// are below the fold, so otherwise nothing visible changes).
    focus_episodes: bool,
    similar_req: Option<u64>,
    /// A person's movies and shows, or what a playlist/collection holds.
    contents: Vec<Card>,
    contents_req: Option<u64>,
    toggle_req: Option<u64>,
    overview: Option<Vec<String>>,
    /// The overview didn't fit in its lines: it can take focus, and then
    /// shows in full over the page (D57).
    overview_cut: bool,
    /// The whole overview, wrapped (for the expanded view).
    overview_full: Vec<String>,
    /// Where the overview is on the page (its focus-graph node).
    overview_rect: Rect,
    overview_focus: bool,
    /// Where focus came to the overview from: Down goes back there.
    overview_from: NodeId,
    /// 0 collapsed .. 1 expanded.
    expand: Tween,
    /// Decode warning, filled once the detail and the GPU caps are known.
    warning: Option<String>,
    unsupported: Vec<bool>,
    /// Tracks chosen here for Play (None: the server's default).
    tracks: TrackChoice,
    picker: Option<(Pick, Blade)>,
}

/// A track label short enough for a button: its first part ("English"
/// of "English - Dolby Digital - 5.1 - Default").
fn short_label(label: &str) -> &str {
    label.split(" - ").next().unwrap_or(label)
}

fn ticks_to_hms(ticks: i64) -> String {
    let s = (ticks / 10_000_000).max(0);
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

impl DetailView {
    pub fn new(id: String, vp: Viewport, now: Instant, ctx: &mut Ctx) -> Self {
        let req = Some(ctx.fetch(Fetch::Detail(id.clone())));
        Self {
            id,
            vp,
            req,
            detail: None,
            failed: None,
            backdrop: Backdrop::new(now),
            buttons: Vec::new(),
            buttons_ready: false,
            button_focus: Some(0),
            rows: RowList::new(Vec::new(), ROWS_TOP, &vp, now),
            graph: FocusGraph::new(),
            seasons: Vec::new(),
            episodes: Vec::new(),
            cast: Vec::new(),
            similar: Vec::new(),
            season_title: s::EPISODES.into(),
            seasons_req: None,
            episodes_req: None,
            episodes_of: None,
            focus_episodes: false,
            similar_req: None,
            contents: Vec::new(),
            contents_req: None,
            toggle_req: None,
            overview: None,
            overview_cut: false,
            overview_full: Vec::new(),
            overview_rect: Rect::default(),
            overview_focus: false,
            overview_from: 0,
            expand: Tween::settled(0.0, now),
            warning: None,
            unsupported: Vec::new(),
            tracks: TrackChoice::default(),
            picker: None,
        }
    }

    /// The audio track Play will use (chosen, else the server's default).
    fn audio_track(&self) -> Option<i32> {
        let d = self.detail.as_ref()?;
        self.tracks.audio.filter(|a| d.audio.iter().any(|t| t.0 == *a)).or(d.default_audio)
    }

    /// The subtitle track Play will use; None: off.
    fn subtitle_track(&self) -> Option<i32> {
        let d = self.detail.as_ref()?;
        match self.tracks.subtitle {
            Some(-1) => None,
            Some(i) if d.subtitles.iter().any(|t| t.0 == i) => Some(i),
            _ => d.default_subtitle,
        }
    }

    fn open_picker(&mut self, pick: Pick, now: Instant) {
        let Some(d) = &self.detail else { return };
        let b = match pick {
            Pick::Audio => {
                let labels = d.audio.iter().map(|t| t.1.clone()).collect();
                let cur = d.audio.iter().position(|t| Some(t.0) == self.audio_track());
                Blade::new(s::AUDIO, labels, cur, now)
            }
            Pick::Subtitles => {
                let mut labels = vec![s::SUBTITLES_OFF.to_string()];
                labels.extend(d.subtitles.iter().map(|t| t.1.clone()));
                let cur = match self.subtitle_track() {
                    None => 0,
                    Some(i) => d.subtitles.iter().position(|t| t.0 == i).map_or(0, |p| p + 1),
                };
                Blade::new(s::SUBTITLES, labels, Some(cur), now)
            }
        };
        self.picker = Some((pick, b));
    }

    fn picked(&mut self, pick: Pick, i: usize) {
        let Some(d) = &self.detail else { return };
        match pick {
            Pick::Audio => self.tracks.audio = d.audio.get(i).map(|t| t.0),
            Pick::Subtitles if i == 0 => self.tracks.subtitle = Some(-1),
            Pick::Subtitles => self.tracks.subtitle = d.subtitles.get(i - 1).map(|t| t.0),
        }
        self.set_buttons();
    }

    /// Back from playback: reload (resume point, watched state).
    /// The server is back: load the page again if it failed.
    pub fn retry(&mut self, ctx: &mut Ctx) -> bool {
        if self.failed.take().is_none() {
            return false;
        }
        self.reload(ctx);
        true
    }

    pub fn reload(&mut self, ctx: &mut Ctx) {
        self.req = Some(ctx.fetch(Fetch::Detail(self.id.clone())));
    }

    fn fetch_episodes(&mut self, series: String, season: String, ctx: &mut Ctx) {
        self.episodes_of = Some((series.clone(), season.clone()));
        self.episodes_req = Some(ctx.fetch(Fetch::Episodes { series, season }));
    }

    /// Something changed on the server (live updates): load again what
    /// shows it (watched marks, resume point, favorite). Focus stays.
    pub fn on_changed(&mut self, c: &Changed, ctx: &mut Ctx) -> bool {
        let shown = |cards: &[Card]| {
            cards.iter().any(|k| c.items.contains(&crate::net::socket::norm_id(&k.id)))
        };
        let me = c.items.contains(&crate::net::socket::norm_id(&self.id));
        let lists = [&self.seasons, &self.episodes, &self.contents, &self.similar];
        if !(c.library || me || lists.iter().any(|l| shown(l))) {
            return false;
        }
        self.reload(ctx);
        if let Some((series, season)) = self.episodes_of.clone()
            && self.episodes_req.is_none()
        {
            self.fetch_episodes(series, season, ctx);
        }
        if self.contents_req.is_none()
            && let Some(d) = &self.detail
        {
            let id = d.card.id.clone();
            self.contents_req = match d.card.kind {
                Kind::Person => Some(ctx.fetch(Fetch::Filmography(id))),
                Kind::Playlist => Some(ctx.fetch(Fetch::Contents { id, playlist: true })),
                Kind::Folder => Some(ctx.fetch(Fetch::Contents { id, playlist: false })),
                _ => None,
            };
        }
        true
    }

    pub fn set_viewport(&mut self, vp: Viewport, now: Instant) -> bool {
        if vp == self.vp {
            return false;
        }
        self.vp = vp;
        self.rows.layout(&vp, now);
        self.buttons_ready = false;
        self.rebuild_graph();
        true
    }

    fn set_buttons(&mut self) {
        let Some(d) = &self.detail else { return };
        let mut b = Vec::new();
        let playable = matches!(d.card.kind, Kind::Movie | Kind::Episode | Kind::Other);
        let watchable = d.card.kind != Kind::Person;
        if playable {
            if d.resume_ticks > 0 {
                let label = format!("{}{}", s::RESUME_PREFIX, ticks_to_hms(d.resume_ticks));
                b.push((Action::Play { start_ticks: d.resume_ticks }, label));
                b.push((Action::Play { start_ticks: 0 }, s::PLAY_FROM_START.into()));
            } else {
                b.push((Action::Play { start_ticks: 0 }, s::PLAY.into()));
            }
            // Track choices: only when there is a choice to make.
            if d.audio.len() > 1 {
                let cur = self.audio_track().and_then(|a| d.audio.iter().find(|t| t.0 == a));
                let name = cur.map_or("", |t| short_label(&t.1));
                b.push((Action::Audio, widgets::list_label(&format!("{}: {name}", s::AUDIO))));
            }
            if !d.subtitles.is_empty() {
                let cur = self.subtitle_track().and_then(|i| d.subtitles.iter().find(|t| t.0 == i));
                let name = cur.map_or(s::SUBTITLES_OFF, |t| short_label(&t.1));
                b.push((
                    Action::Subtitles,
                    widgets::list_label(&format!("{}: {name}", s::SUBTITLES)),
                ));
            }
        }
        if watchable {
            b.push((
                Action::Watched,
                if d.played { s::MARK_UNWATCHED } else { s::MARK_WATCHED }.into(),
            ));
        }
        b.push((
            Action::Favorite,
            if d.favorite { s::REMOVE_FAVORITE } else { s::ADD_FAVORITE }.into(),
        ));
        if playable {
            b.push((Action::Debug, s::DEBUG_INFO.into()));
        }
        self.buttons = b
            .into_iter()
            .map(|(action, label)| Button { action, label, rect: Rect::default() })
            .collect();
        self.buttons_ready = false;
        if let Some(f) = self.button_focus {
            self.button_focus = Some(f.min(self.buttons.len() - 1));
        }
    }

    fn layout_buttons<R: Renderer>(&mut self, r: &mut R) {
        if self.buttons_ready {
            return;
        }
        let mut x = self.vp.sx();
        for b in &mut self.buttons {
            let w = widgets::button_width(r, &b.label).max(170.0);
            b.rect = Rect::new(x, BUTTONS_Y, w, BUTTON_H);
            x += w + 14.0;
        }
        self.buttons_ready = true;
        self.rebuild_graph();
    }

    fn set_rows(&mut self, now: Instant) {
        let mut rows = Vec::new();
        if !self.contents.is_empty() {
            let title = match self.detail.as_ref().map(|d| d.card.kind) {
                Some(Kind::Person) => s::FILMOGRAPHY,
                Some(Kind::Playlist) => s::IN_PLAYLIST,
                _ => s::IN_COLLECTION,
            };
            rows.push(Row {
                key: "contents".into(),
                title: title.into(),
                shape: Shape::Poster,
                cards: self.contents.clone(),
            });
        }
        if !self.seasons.is_empty() {
            rows.push(Row {
                key: "seasons".into(),
                title: s::SEASONS.into(),
                shape: Shape::Poster,
                cards: self.seasons.clone(),
            });
        }
        if !self.episodes.is_empty() {
            rows.push(Row {
                key: "episodes".into(),
                title: self.season_title.clone(),
                shape: Shape::Landscape,
                cards: self.episodes.clone(),
            });
        }
        if !self.cast.is_empty() {
            rows.push(Row {
                key: "cast".into(),
                title: s::CAST.into(),
                shape: Shape::Poster,
                cards: self.cast.clone(),
            });
        }
        if !self.similar.is_empty() {
            rows.push(Row {
                key: "similar".into(),
                title: s::MORE_LIKE_THIS.into(),
                shape: Shape::Poster,
                cards: self.similar.clone(),
            });
        }
        let active = self.rows.active && self.button_focus.is_none();
        self.rows.set_rows(rows, &self.vp, now);
        self.rows.active = active;
        // Rows arrive one at a time; until the user is in them, the first
        // row is where Down goes (not wherever the first answer was).
        if !active {
            self.rows.reset_focus(&self.vp, now);
        }
        self.rebuild_graph();
    }

    /// Focus the episodes row: the episode in progress, else the first.
    fn focus_episode_row(&mut self, now: Instant) {
        let Some(r) = self.rows.rows.iter().position(|r| r.key == "episodes") else { return };
        let c = self.episodes.iter().position(|e| e.progress.is_some()).unwrap_or(0);
        self.rows.take_focus((r, c), &self.vp, now);
        self.rebuild_graph();
    }

    fn rebuild_graph(&mut self) {
        let mut nodes = self.rows.nodes(&self.vp);
        if self.buttons_ready {
            nodes.extend(self.buttons.iter().enumerate().map(|(i, b)| (i as NodeId, b.rect)));
        }
        if self.overview_cut && self.overview_rect.w > 0.0 {
            nodes.push((OVERVIEW_NODE, self.overview_rect));
        }
        self.graph.set_nodes(nodes);
        self.graph.set_override(OVERVIEW_NODE, Dir::Down, self.overview_from);
        let id = match self.button_focus {
            _ if self.overview_focus => OVERVIEW_NODE,
            Some(i) => i as NodeId,
            None => widgets::card_node(self.rows.focus.0, self.rows.focus.1),
        };
        self.graph.focus(id);
    }

    pub fn on_action(&mut self, a: InputAction, now: Instant, ctx: &mut Ctx) -> bool {
        if let Some((pick, b)) = &mut self.picker {
            let pick = *pick;
            match b.on_action(a) {
                blade::Outcome::Stay => {}
                blade::Outcome::Picked(i) => {
                    self.picker = None;
                    self.picked(pick, i);
                }
                blade::Outcome::Closed => self.picker = None,
            }
            return true;
        }
        if let Some(dir) = Dir::from_action(a) {
            let from = self.graph.focused();
            let id = match self.rows.step(dir, &self.vp) {
                Step::To(id) => id,
                Step::Stay => return true,
                Step::Free => match self.graph.navigate(dir) {
                    Some(id) => id,
                    None => return false,
                },
            };
            let to_overview = id == OVERVIEW_NODE;
            if to_overview && let Some(f) = from {
                self.overview_from = f;
            }
            if to_overview != self.overview_focus {
                self.overview_focus = to_overview;
                let d = if to_overview { EXPAND_IN } else { EXPAND_OUT };
                self.expand.retarget(f32::from(u8::from(to_overview)), now, d);
            }
            match widgets::card_of(id) {
                _ if to_overview => {
                    self.button_focus = None;
                    self.rows.release_focus(now);
                }
                Some(rc) => {
                    self.button_focus = None;
                    self.rows.take_focus(rc, &self.vp, now);
                }
                None => {
                    self.button_focus = Some(id as usize);
                    self.rows.release_focus(now);
                }
            }
            self.rebuild_graph();
            return true;
        }
        if a != InputAction::Select {
            return false;
        }
        if self.overview_focus {
            return true;
        }
        let Some(d) = &self.detail else { return false };
        if let Some(i) = self.button_focus {
            let Some(b) = self.buttons.get(i) else { return false };
            match b.action {
                Action::Play { start_ticks } => {
                    ctx.open = Some(Open::Play {
                        id: d.card.id.clone(),
                        media_source: d.media_source_id.clone(),
                        start_ticks,
                        title: d.card.title.clone(),
                        tracks: self.tracks,
                    });
                }
                Action::Audio => self.open_picker(Pick::Audio, now),
                Action::Subtitles => self.open_picker(Pick::Subtitles, now),
                Action::Watched => {
                    self.toggle_req =
                        Some(ctx.fetch(Fetch::SetPlayed { id: self.id.clone(), on: !d.played }));
                }
                Action::Favorite => {
                    self.toggle_req = Some(
                        ctx.fetch(Fetch::SetFavorite { id: self.id.clone(), on: !d.favorite }),
                    );
                }
                Action::Debug => {
                    let title = match d.card.kind {
                        Kind::Episode if !d.card.subtitle.is_empty() => {
                            format!("{} {}", d.card.title, d.card.subtitle)
                        }
                        _ => d.card.title.clone(),
                    };
                    ctx.open = Some(Open::Debug { id: d.card.id.clone(), title });
                }
            }
            return true;
        }
        let row_key = self.rows.rows.get(self.rows.focus.0).map(|r| r.key.clone());
        let Some(card) = self.rows.focused_card() else { return false };
        match (row_key.as_deref(), card.kind) {
            // A season: show its episodes here rather than opening a page.
            (Some("seasons"), _) => {
                if let Some(series) = d.series_id.clone().or_else(|| Some(self.id.clone())) {
                    self.season_title = card.title.clone();
                    self.fetch_episodes(series, card.id.clone(), ctx);
                    self.focus_episodes = true;
                }
            }
            (_, Kind::Library) => return false,
            _ => ctx.open = Some(Open::Detail { id: card.id.clone() }),
        }
        true
    }

    pub fn on_fetched(&mut self, id: u64, result: &Fetched, now: Instant, ctx: &mut Ctx) -> bool {
        if self.req == Some(id) {
            self.req = None;
            match result {
                Fetched::Detail(d) => {
                    let d = (**d).clone();
                    let first_load = self.detail.is_none();
                    self.cast = d.people.clone();
                    if first_load {
                        match d.card.kind {
                            Kind::Series => {
                                self.seasons_req =
                                    Some(ctx.fetch(Fetch::Seasons(d.card.id.clone())));
                            }
                            Kind::Season => {
                                if let Some(series) = d.series_id.clone() {
                                    self.season_title = d.card.title.clone();
                                    self.fetch_episodes(series, d.card.id.clone(), ctx);
                                }
                            }
                            _ => {}
                        }
                        let id = d.card.id.clone();
                        self.contents_req = match d.card.kind {
                            Kind::Person => Some(ctx.fetch(Fetch::Filmography(id))),
                            Kind::Playlist => {
                                Some(ctx.fetch(Fetch::Contents { id, playlist: true }))
                            }
                            Kind::Folder => {
                                Some(ctx.fetch(Fetch::Contents { id, playlist: false }))
                            }
                            _ => None,
                        };
                        if matches!(d.card.kind, Kind::Movie | Kind::Series) {
                            self.similar_req = Some(ctx.fetch(Fetch::Similar(d.card.id.clone())));
                        }
                    }
                    self.detail = Some(d);
                    self.overview = None;
                    self.warning = None;
                    self.set_buttons();
                    self.set_rows(now);
                }
                Fetched::Failed(e) => self.failed = Some(e.clone()),
                _ => {}
            }
            return true;
        }
        if self.seasons_req == Some(id) {
            self.seasons_req = None;
            if let Fetched::Cards(c) = result {
                self.seasons = c.clone();
                // Episodes of the first season with unwatched episodes
                // (else the first season).
                if let Some(first) =
                    self.seasons.iter().find(|c| c.progress.is_some()).or(self.seasons.first())
                {
                    self.season_title = first.title.clone();
                    let (series, season) = (self.id.clone(), first.id.clone());
                    self.fetch_episodes(series, season, ctx);
                }
                self.set_rows(now);
            }
            return true;
        }
        if self.episodes_req == Some(id) {
            self.episodes_req = None;
            if let Fetched::Cards(c) = result {
                self.episodes = c.clone();
                self.set_rows(now);
                // Unless the user moved off the seasons meanwhile.
                let on_seasons = self.button_focus.is_none()
                    && self.rows.rows.get(self.rows.focus.0).is_some_and(|r| r.key == "seasons");
                if std::mem::take(&mut self.focus_episodes) && on_seasons {
                    self.focus_episode_row(now);
                }
            }
            return true;
        }
        if self.contents_req == Some(id) {
            self.contents_req = None;
            if let Fetched::Cards(c) = result {
                self.contents = c.clone();
                self.set_rows(now);
            }
            return true;
        }
        if self.similar_req == Some(id) {
            self.similar_req = None;
            if let Fetched::Cards(c) = result {
                self.similar = c.clone();
                self.set_rows(now);
            }
            return true;
        }
        if self.toggle_req == Some(id) {
            self.toggle_req = None;
            if let (Fetched::UserData { favorite, played }, Some(d)) = (result, &mut self.detail) {
                d.favorite = *favorite;
                d.played = *played;
                self.set_buttons();
                ctx.send(crate::net::Cmd::Refresh);
            }
            return true;
        }
        false
    }

    /// The item's backdrop, else (a playlist or folder, which seldom have
    /// one of their own) the first of its contents that has one.
    fn backdrop_art(&self) -> Option<&Art> {
        let d = self.detail.as_ref()?;
        d.card.backdrop.as_ref().or_else(|| self.contents.iter().find_map(|c| c.backdrop.as_ref()))
    }

    pub fn update(&mut self, now: Instant, tex: &Textures) -> bool {
        let want = self.backdrop_art().map(|a| widgets::backdrop_key(a, &self.vp));
        self.backdrop.update(want, now, tex)
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        self.rows.is_animating(now)
            || self.backdrop.is_animating(now)
            || self.expand.is_animating(now)
            || self.picker.as_ref().is_some_and(|(_, b)| b.is_animating(now))
    }

    pub fn wants(&self, w: &mut Wants) {
        w.backdrop(self.backdrop_art());
        self.rows.wants(w, &self.vp, false);
    }

    pub fn draw<R: Renderer>(
        &mut self,
        r: &mut R,
        now: Instant,
        tex: &mut Textures,
        caps: Option<&DecodeCaps>,
    ) {
        // The overview first: whether it fits decides if it takes focus.
        if self.overview.is_none()
            && let Some(d) = &self.detail
        {
            let mut lines = wrap(r, &d.card.overview, &BODY, TEXT_W);
            self.overview_full = lines.clone();
            if lines.len() > 3 {
                lines.truncate(3);
                if let Some(l) = lines.last_mut() {
                    l.push('…');
                }
                self.overview_cut = true;
            }
            self.overview = Some(lines);
        }
        self.layout_buttons(r);
        let (lw, lh) = r.logical_size();
        let (sx, sy) = (self.vp.sx(), self.vp.sy());
        self.backdrop.draw(r, now, tex);
        let Some(d) = &self.detail else {
            let msg = self.failed.as_deref().unwrap_or(s::LOADING);
            r.text(
                msg,
                &TextStyle::new(26.0, Weight::Regular).max_width(lw - 2.0 * sx),
                Point::new(sx, sy + 40.0),
                TEXT_DIM,
            );
            return;
        };
        if self.warning.is_none() {
            self.unsupported = d
                .badges
                .iter()
                .map(|b| b.decode.is_some_and(|c| caps.is_some_and(|k| !k.supports(c))))
                .collect();
            self.warning = Some(
                d.badges
                    .iter()
                    .zip(&self.unsupported)
                    .find(|(_, u)| **u)
                    .map(|(b, _)| s::fill(s::NO_HW_DECODE, &[&b.text]))
                    .unwrap_or_default(),
            );
        }

        let mut y = sy + 50.0;
        r.text(&d.card.title, &TITLE, Point::new(sx, y), TEXT);
        y += 78.0;
        let is_ep = d.card.kind == Kind::Episode;
        if is_ep && !d.card.subtitle.is_empty() {
            r.text(&d.card.subtitle, &SUB, Point::new(sx, y), TEXT);
            y += 42.0;
        }
        if !d.card.meta.is_empty() || d.played {
            let m = r.text(&d.card.meta, &META, Point::new(sx, y), TEXT_DIM);
            // Watched: the cards' check mark, here too (D74).
            if d.played {
                let x = if d.card.meta.is_empty() { sx } else { sx + m.width + 18.0 };
                widgets::draw_check(r, Rect::new(x, y - 1.0, 26.0, 26.0));
                r.text(s::WATCHED, &META, Point::new(x + 34.0, y), TEXT_DIM);
            }
            y += 40.0;
        }
        if !d.badges.is_empty() {
            let mut x = sx;
            for (b, bad) in d.badges.iter().zip(&self.unsupported) {
                let m = r.measure_text(&b.text, &BADGE);
                let rect = Rect::new(x, y, m.width + 24.0, 32.0);
                let bg =
                    if *bad { Color::rgb(190, 40, 40) } else { Color::rgba(255, 255, 255, 40) };
                r.fill_rrect(rect, 6.0, bg);
                r.text(&b.text, &BADGE, Point::new(rect.x + 12.0, rect.y + 5.0), TEXT);
                x += rect.w + 10.0;
            }
            y += 44.0;
        }
        if let Some(w) = self.warning.as_deref().filter(|w| !w.is_empty()) {
            r.text(w, &WARN, Point::new(sx, y), NOTICE);
            y += 34.0;
        }
        if !d.genres.is_empty() {
            r.text(&d.genres, &META, Point::new(sx, y), TEXT_DIM);
            y += 36.0;
        }
        if !d.tagline.is_empty() {
            r.text(&d.tagline, &META, Point::new(sx, y), TEXT);
            y += 36.0;
        }
        let overview_top = y;
        if let Some(lines) = &self.overview {
            for l in lines {
                if y > BUTTONS_Y - 34.0 {
                    break;
                }
                r.text(l, &BODY, Point::new(sx, y), TEXT);
                y += 32.0;
            }
        }
        let overview_rect =
            Rect::new(sx - 14.0, overview_top - 4.0, TEXT_W + 28.0, y - overview_top + 8.0);

        for (i, b) in self.buttons.iter().enumerate() {
            widgets::draw_button(r, b.rect, &b.label, self.button_focus == Some(i), false);
        }
        self.rows.draw(r, now, tex, &self.vp);
        if self.overview_cut && overview_rect != self.overview_rect {
            self.overview_rect = overview_rect;
            self.rebuild_graph();
        }
        self.draw_overview_focus(r, now, overview_top, lh - sy);
        if let Some((_, b)) = &self.picker {
            b.draw(r, now, self.vp.safe);
        }
    }

    /// The focused overview: the whole text, grown down over the page.
    fn draw_overview_focus<R: Renderer>(&self, r: &mut R, now: Instant, top: f32, bottom: f32) {
        let t = self.expand.value(now);
        if !self.overview_cut || t <= 0.001 {
            return;
        }
        let base = self.overview_rect;
        let full_h = self.overview_full.len() as f32 * 32.0 + 20.0;
        let h = (base.h + (full_h - base.h) * t).min(bottom - base.y);
        let rect = Rect::new(base.x, base.y, base.w, h);
        r.fill_rrect(rect, 12.0, Color::rgba(14, 16, 22, (236.0 * t) as u8));
        r.push_clip(rect, 12.0);
        let mut y = top;
        for l in &self.overview_full {
            if y > rect.bottom() {
                break;
            }
            r.text(l, &BODY, Point::new(base.x + 14.0, y), TEXT);
            y += 32.0;
        }
        r.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::home::model::{Badge, Card};

    const VP: Viewport = Viewport { lw: 1920.0, safe: 0.025, scale: 1.0 };

    fn card(id: &str, kind: Kind) -> Card {
        Card {
            id: id.into(),
            kind,
            collection: None,
            title: id.into(),
            subtitle: String::new(),
            meta: String::new(),
            overview: String::new(),
            image: None,
            backdrop: None,
            progress: None,
            played: false,
        }
    }

    fn detail(kind: Kind, resume: i64) -> Detail {
        Detail {
            card: card("item", kind),
            tagline: String::new(),
            genres: String::new(),
            badges: vec![Badge {
                text: "HEVC 10-bit".into(),
                decode: Some(jellyroll_video::va::Codec::Hevc10),
            }],
            played: false,
            favorite: false,
            resume_ticks: resume,
            people: vec![card("actor", Kind::Person)],
            series_id: None,
            season_id: None,
            media_source_id: "item".into(),
            audio: Vec::new(),
            subtitles: Vec::new(),
            default_audio: None,
            default_subtitle: None,
        }
    }

    fn take_fetch(ctx: &mut Ctx) -> (u64, Fetch) {
        match ctx.cmds.remove(0) {
            crate::net::Cmd::Fetch { id, req } => (id, req),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn movie_resume_and_toggles() {
        let t = Instant::now();
        let mut next = 0;
        let mut ctx = Ctx::new(&mut next);
        let mut v = DetailView::new("item".into(), VP, t, &mut ctx);
        let (id, req) = take_fetch(&mut ctx);
        assert_eq!(req, Fetch::Detail("item".into()));
        v.on_fetched(
            id,
            &Fetched::Detail(Box::new(detail(Kind::Movie, 6_000_000_000))),
            t,
            &mut ctx,
        );
        let labels: Vec<&str> = v.buttons.iter().map(|b| b.label.as_str()).collect();
        assert_eq!(
            labels,
            ["Resume 10:00", "From the start", "Mark watched", "Add to favorites", s::DEBUG_INFO]
        );
        assert!(matches!(take_fetch(&mut ctx).1, Fetch::Similar(_)));
        // Debug info opens the report dialog for this item (D83).
        v.button_focus = Some(4);
        v.on_action(InputAction::Select, t, &mut ctx);
        assert!(matches!(ctx.open.take(), Some(Open::Debug { ref id, .. }) if *id == v.id));
        v.button_focus = Some(0);
        // Select on Resume plays from the resume point.
        v.on_action(InputAction::Select, t, &mut ctx);
        assert!(matches!(ctx.open, Some(Open::Play { start_ticks: 6_000_000_000, .. })));
        // Favorite toggle round trip.
        v.button_focus = Some(3);
        let mut ctx = Ctx::new(&mut next);
        v.on_action(InputAction::Select, t, &mut ctx);
        let (tid, req) = take_fetch(&mut ctx);
        assert_eq!(req, Fetch::SetFavorite { id: "item".into(), on: true });
        v.on_fetched(tid, &Fetched::UserData { favorite: true, played: false }, t, &mut ctx);
        assert_eq!(v.buttons[3].label, "Remove from favorites");
    }

    #[test]
    fn series_loads_seasons_then_episodes() {
        let t = Instant::now();
        let mut next = 0;
        let mut ctx = Ctx::new(&mut next);
        let mut v = DetailView::new("show".into(), VP, t, &mut ctx);
        let (id, _) = take_fetch(&mut ctx);
        let mut d = detail(Kind::Series, 0);
        d.card.id = "show".into();
        v.on_fetched(id, &Fetched::Detail(Box::new(d)), t, &mut ctx);
        assert!(
            v.buttons.iter().all(|b| !matches!(b.action, Action::Play { .. })),
            "no Play on a series"
        );
        let (sid, req) = take_fetch(&mut ctx);
        assert_eq!(req, Fetch::Seasons("show".into()));
        v.on_fetched(
            sid,
            &Fetched::Cards(vec![card("s1", Kind::Season), card("s2", Kind::Season)]),
            t,
            &mut ctx,
        );
        let (eid, req) = ctx
            .cmds
            .iter()
            .rev()
            .find_map(|c| match c {
                crate::net::Cmd::Fetch { id, req: r @ Fetch::Episodes { .. } } => {
                    Some((*id, r.clone()))
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(req, Fetch::Episodes { series: "show".into(), season: "s1".into() });
        v.on_fetched(eid, &Fetched::Cards(vec![card("e1", Kind::Episode)]), t, &mut ctx);
        let keys: Vec<&str> = v.rows.rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["seasons", "episodes", "cast"]);
        // Cast arrived first; focus (where Down from the buttons goes) is
        // still the first row, Seasons.
        assert_eq!(v.rows.focus, (0, 0));

        // Right to Season 2 (buttons are not laid out here, so focus is
        // on the rows), Select: its episodes load and focus moves to them.
        v.on_action(InputAction::Right, t, &mut ctx);
        assert_eq!(v.rows.focus, (0, 1));
        assert!(v.on_action(InputAction::Select, t, &mut ctx));
        let Some(crate::net::Cmd::Fetch { id: eid, req }) = ctx.cmds.pop() else { panic!() };
        assert_eq!(req, Fetch::Episodes { series: "show".into(), season: "s2".into() });
        let eps = vec![card("e5", Kind::Episode), card("e6", Kind::Episode)];
        v.on_fetched(eid, &Fetched::Cards(eps), t, &mut ctx);
        assert_eq!(v.rows.focus, (1, 0), "first episode of season 2");
        assert_eq!(v.rows.rows[1].title, "s2");
    }

    fn contents_fetch(ctx: &Ctx) -> Option<Fetch> {
        ctx.cmds.iter().find_map(|c| match c {
            crate::net::Cmd::Fetch {
                req: r @ (Fetch::Filmography(_) | Fetch::Contents { .. }),
                ..
            } => Some(r.clone()),
            _ => None,
        })
    }

    #[test]
    fn person_page_lists_their_work() {
        let t = Instant::now();
        let mut next = 0;
        let mut ctx = Ctx::new(&mut next);
        let mut v = DetailView::new("actor".into(), VP, t, &mut ctx);
        let (id, _) = take_fetch(&mut ctx);
        let mut d = detail(Kind::Person, 0);
        d.card.id = "actor".into();
        v.on_fetched(id, &Fetched::Detail(Box::new(d)), t, &mut ctx);
        assert_eq!(contents_fetch(&ctx), Some(Fetch::Filmography("actor".into())));
        let labels: Vec<&str> = v.buttons.iter().map(|b| b.label.as_str()).collect();
        assert_eq!(labels, [s::ADD_FAVORITE], "no Play or Mark watched on a person");
        let Some(crate::net::Cmd::Fetch { id, .. }) = ctx
            .cmds
            .iter()
            .find(|c| matches!(c, crate::net::Cmd::Fetch { req: Fetch::Filmography(_), .. }))
        else {
            panic!()
        };
        let id = *id;
        v.on_fetched(id, &Fetched::Cards(vec![card("m1", Kind::Movie)]), t, &mut ctx);
        assert_eq!(v.rows.rows[0].key, "contents");
        assert_eq!(v.rows.rows[0].title, s::FILMOGRAPHY);
    }

    #[test]
    fn playlists_and_collections_list_their_items() {
        for (kind, playlist) in [(Kind::Playlist, true), (Kind::Folder, false)] {
            let t = Instant::now();
            let mut next = 0;
            let mut ctx = Ctx::new(&mut next);
            let mut v = DetailView::new("pl".into(), VP, t, &mut ctx);
            let (id, _) = take_fetch(&mut ctx);
            let mut d = detail(kind, 0);
            d.card.id = "pl".into();
            v.on_fetched(id, &Fetched::Detail(Box::new(d)), t, &mut ctx);
            assert_eq!(contents_fetch(&ctx), Some(Fetch::Contents { id: "pl".into(), playlist }));
            assert!(v.buttons.iter().all(|b| !matches!(b.action, Action::Play { .. })));
        }
    }

    #[test]
    fn a_playlist_without_a_backdrop_borrows_one_from_its_items() {
        let t = Instant::now();
        let mut next = 0;
        let mut ctx = Ctx::new(&mut next);
        let mut v = DetailView::new("pl".into(), VP, t, &mut ctx);
        let (id, _) = take_fetch(&mut ctx);
        let mut d = detail(Kind::Playlist, 0);
        d.card.id = "pl".into();
        d.card.backdrop = None;
        v.on_fetched(id, &Fetched::Detail(Box::new(d)), t, &mut ctx);
        assert!(v.backdrop_art().is_none());
        let Some(crate::net::Cmd::Fetch { id, .. }) = ctx
            .cmds
            .iter()
            .find(|c| matches!(c, crate::net::Cmd::Fetch { req: Fetch::Contents { .. }, .. }))
        else {
            panic!()
        };
        let id = *id;
        let art = Art {
            item_id: "m2".into(),
            kind: jellyfin_api::endpoints::ImageType::Backdrop,
            tag: "b2".into(),
            blurhash: None,
        };
        let mut with = card("m2", Kind::Movie);
        with.backdrop = Some(art.clone());
        let mut without = card("m1", Kind::Movie);
        without.backdrop = None;
        v.on_fetched(id, &Fetched::Cards(vec![without, with]), t, &mut ctx);
        assert_eq!(v.backdrop_art(), Some(&art), "the first item that has one");
    }

    #[test]
    fn a_cut_overview_takes_focus_and_expands() {
        let t = Instant::now();
        let mut next = 0;
        let mut ctx = Ctx::new(&mut next);
        let mut v = DetailView::new("item".into(), VP, t, &mut ctx);
        let (id, _) = take_fetch(&mut ctx);
        v.on_fetched(id, &Fetched::Detail(Box::new(detail(Kind::Movie, 0))), t, &mut ctx);
        // As drawing finds it: cut, above the buttons.
        v.overview_cut = true;
        v.overview_rect = Rect::new(34.0, 300.0, 1178.0, 108.0);
        for (i, b) in v.buttons.iter_mut().enumerate() {
            b.rect = Rect::new(48.0 + i as f32 * 220.0, BUTTONS_Y, 200.0, BUTTON_H);
        }
        v.buttons_ready = true;
        v.button_focus = Some(0);
        v.rebuild_graph();
        assert!(v.on_action(InputAction::Up, t, &mut ctx));
        assert!(v.overview_focus);
        assert_eq!(v.button_focus, None);
        assert!(v.is_animating(t), "grows open");
        assert!(v.on_action(InputAction::Select, t, &mut ctx), "Select does nothing there");
        assert!(ctx.open.is_none());
        v.on_action(InputAction::Down, t, &mut ctx);
        assert!(!v.overview_focus);
        assert_eq!(v.button_focus, Some(0));
    }

    #[test]
    fn server_changes_reload_the_page_and_its_episodes() {
        let t = Instant::now();
        let mut next = 0;
        let mut ctx = Ctx::new(&mut next);
        let mut v = DetailView::new("show".into(), VP, t, &mut ctx);
        let (did, _) = take_fetch(&mut ctx);
        let mut d = detail(Kind::Series, 0);
        d.card.id = "show".into();
        v.on_fetched(did, &Fetched::Detail(Box::new(d)), t, &mut ctx);
        let seasons = ctx
            .cmds
            .iter()
            .find_map(|c| match c {
                crate::net::Cmd::Fetch { id, req: Fetch::Seasons(_) } => Some(*id),
                _ => None,
            })
            .unwrap();
        v.on_fetched(seasons, &Fetched::Cards(vec![card("s1", Kind::Season)]), t, &mut ctx);
        let eps = ctx
            .cmds
            .iter()
            .rev()
            .find_map(|c| match c {
                crate::net::Cmd::Fetch { id, req: Fetch::Episodes { .. } } => Some(*id),
                _ => None,
            })
            .unwrap();
        let e1 = card("E1-ab", Kind::Episode);
        v.on_fetched(eps, &Fetched::Cards(vec![e1]), t, &mut ctx);

        let changed = |items: &[&str]| Changed {
            items: items.iter().map(|s| s.to_string()).collect(),
            library: false,
        };
        let mut ctx = Ctx::new(&mut next);
        assert!(!v.on_changed(&changed(&["other"]), &mut ctx));
        assert!(ctx.cmds.is_empty());
        // An episode watched elsewhere (ids compare without dashes).
        assert!(v.on_changed(&changed(&["e1ab"]), &mut ctx));
        let reqs: Vec<Fetch> = ctx
            .cmds
            .iter()
            .filter_map(|c| match c {
                crate::net::Cmd::Fetch { req, .. } => Some(req.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            reqs,
            [
                Fetch::Detail("show".into()),
                Fetch::Episodes { series: "show".into(), season: "s1".into() }
            ]
        );
    }

    #[test]
    fn tracks_are_chosen_before_play() {
        let t = Instant::now();
        let mut next = 0;
        let mut ctx = Ctx::new(&mut next);
        let mut v = DetailView::new("item".into(), VP, t, &mut ctx);
        let (did, _) = take_fetch(&mut ctx);
        let mut d = detail(Kind::Movie, 0);
        d.audio =
            vec![(1, "English - AAC - Stereo - Default".into()), (2, "Commentary - AAC".into())];
        d.subtitles = vec![(3, "English - SUBRIP".into()), (4, "Français - SRT".into())];
        d.default_audio = Some(1);
        v.on_fetched(did, &Fetched::Detail(Box::new(d)), t, &mut ctx);
        let labels: Vec<&str> = v.buttons.iter().map(|b| b.label.as_str()).collect();
        assert_eq!(labels[1], "Audio: English  ›");
        assert_eq!(labels[2], "Subtitles: Off  ›");
        // Audio: pick the commentary.
        v.button_focus = Some(1);
        assert!(v.on_action(InputAction::Select, t, &mut ctx));
        assert!(v.picker.is_some());
        v.on_action(InputAction::Down, t, &mut ctx);
        v.on_action(InputAction::Select, t, &mut ctx);
        assert!(v.picker.is_none());
        assert_eq!(v.buttons[1].label, "Audio: Commentary  ›");
        // Subtitles: the list starts with Off; pick French.
        v.button_focus = Some(2);
        v.on_action(InputAction::Select, t, &mut ctx);
        v.on_action(InputAction::Down, t, &mut ctx);
        v.on_action(InputAction::Down, t, &mut ctx);
        v.on_action(InputAction::Select, t, &mut ctx);
        assert_eq!(v.buttons[2].label, "Subtitles: Français  ›");
        // Back closes the list without changing anything.
        v.on_action(InputAction::Select, t, &mut ctx);
        v.on_action(InputAction::Up, t, &mut ctx);
        assert!(v.on_action(InputAction::Back, t, &mut ctx));
        assert!(v.picker.is_none());
        assert_eq!(v.buttons[2].label, "Subtitles: Français  ›");
        // Play carries the choice.
        v.button_focus = Some(0);
        v.on_action(InputAction::Select, t, &mut ctx);
        let Some(Open::Play { tracks, .. }) = &ctx.open else { panic!("{:?}", ctx.open) };
        assert_eq!(*tracks, TrackChoice { audio: Some(2), subtitle: Some(4) });
    }
}
