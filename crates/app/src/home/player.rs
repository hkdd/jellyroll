//! The player (spec 7.3): on-screen display, seeking with trickplay
//! previews, audio and subtitle menus, skip buttons for intros and credits,
//! the next episode with a countdown, chapters, and playback reports to the
//! server (spec 7.2). The frame loop owns mpv (`Playback`); this owns what
//! the user sees and what the server is told.
//!
//! Keys: Left/Right scrub (held: faster), committed after a short pause or
//! with Select; Select also skips a segment, plays the next episode now, or
//! pauses; Down/Menu opens the track menu; Info the details panel;
//! Next/Prev jump between chapters; Back closes, cancels, or stops.

use std::fmt::Write;
use std::time::{Duration, Instant};

use jellyfin_api::playback::{Report, ReportKind};
use jellyroll_render::{Color, Gradient, Point, Rect, Renderer, TextStyle, Weight};
use jellyroll_video::{EndReason, PlaybackState, Player};

use super::PlayRequest;
use super::pages::Ctx;
use super::playinfo::{PlayInfo, SegmentKind, TICKS_PER_SEC, Track};
use super::strings as s;
use super::textures::Textures;
use super::theme::{ACCENT, TEXT, TEXT_DIM};
use crate::hud::Line;
use crate::input::InputAction;
use crate::net::store::fnv1a;
use crate::net::{Cmd, Fetch, Fetched, Finish, ImageJob, ImageSource, PlayFetched, Remote};
use crate::osd::fmt_time;
use crate::playback::Playback;

const OSD_HIDE: Duration = Duration::from_secs(4);
/// Progress report period while playing (spec 7.2: ~10 s).
const REPORT_EVERY: Duration = Duration::from_secs(10);
/// A scrub is committed this long after the last Left/Right.
const SCRUB_COMMIT: Duration = Duration::from_millis(700);
/// Presses closer than this continue a scrub (held keys repeat faster).
const SCRUB_CHAIN: Duration = Duration::from_millis(450);
/// Next-episode countdown (seconds of playback).
const NEXT_COUNTDOWN: f64 = 10.0;
/// Without a credits segment, the countdown starts this long before the end.
const NEXT_BEFORE_END: f64 = 30.0;

const WARN_BG: Color = Color::rgb(190, 40, 40);
const PANEL: Color = Color::rgba(14, 16, 22, 235);
const TRACK_BG: Color = Color::rgba(255, 255, 255, 60);

pub enum Outcome {
    Continue {
        redraw: bool,
    },
    /// Playback should stop (Back or Stop); the stop is already reported.
    Stop,
}

pub enum Tick {
    Continue {
        redraw: bool,
    },
    /// Playback is over; back to the page it started from.
    Finished,
    /// Play this next (the next episode); the current one is reported.
    Next(Box<PlayFetched>),
}

struct Scrub {
    target: f64,
    last: Instant,
    presses: u32,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Row {
    Audio(usize),
    SubOff,
    Sub(usize),
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum NextState {
    /// Not reached yet (or no next episode).
    Waiting,
    /// The card is up; `left` seconds of playback until autoplay.
    Showing { left: f64 },
    /// Back was pressed on the card: finish normally.
    Dismissed,
}

pub struct PlayerUi {
    pub fetched: PlayFetched,
    osd_until: Option<Instant>,
    menu: Option<usize>,
    scrub: Option<Scrub>,
    diagnostics: bool,
    /// Selected server stream indexes.
    audio: Option<i32>,
    subtitle: Option<i32>,
    /// External subtitles added to mpv, in order (their mpv ids follow the
    /// embedded ones).
    externals: Vec<i32>,
    /// Default external subtitle, added once the file is loaded.
    pending_external: Option<i32>,
    started: bool,
    last_report: Instant,
    last_paused: bool,
    reported_stop: bool,
    next_at: Option<f64>,
    next: NextState,
    next_fetch: Option<u64>,
    next_ready: Option<Box<PlayFetched>>,
    /// Play the next episode as soon as it is fetched.
    autoplay: bool,
    last_pos: f64,
    // Text, re-formatted only when the shown value changes.
    time_l: Line<24>,
    time_r: Line<24>,
    time_shown: (i64, i64),
    scrub_l: Line<24>,
    scrub_shown: i64,
    count_l: Line<48>,
    count_shown: i64,
    info: [Line<112>; 6],
}

fn secs_to_ticks(s: f64) -> i64 {
    (s.max(0.0) * TICKS_PER_SEC) as i64
}

impl PlayerUi {
    /// The player for `fetched`, and the request that starts mpv.
    pub fn new(fetched: PlayFetched, now: Instant) -> (Self, PlayRequest) {
        let i = &fetched.info;
        let audio =
            i.default_audio.and_then(|d| i.audio.iter().find(|t| t.index == d)).or(i.audio.first());
        let sub = i.default_subtitle.and_then(|d| i.subtitles.iter().find(|t| t.index == d));
        let req = PlayRequest {
            url: format!("{}{}", fetched.base_url, i.stream_path),
            authorization: fetched.authorization.clone(),
            start_secs: i.start,
            title: if i.subtitle.is_empty() {
                i.title.clone()
            } else {
                format!("{} · {}", i.title, i.subtitle)
            },
            audio_track: Some(
                audio.and_then(|t| t.mpv_id).map_or("auto".into(), |n| n.to_string()),
            ),
            sub_track: Some(sub.and_then(|t| t.mpv_id).map_or("no".into(), |n| n.to_string())),
            fps: i.fps,
            match_refresh: false,
            audio: Default::default(),
        };
        let next_at = i.next.as_ref().map(|_| {
            let credits = i.segments.iter().find(|s| s.kind == SegmentKind::Credits);
            match credits {
                Some(c) => c.start,
                None if i.duration > 4.0 * NEXT_BEFORE_END => i.duration - NEXT_BEFORE_END,
                None => i.duration * 0.9,
            }
        });
        let ui = Self {
            audio: audio.map(|t| t.index),
            subtitle: sub.map(|t| t.index),
            pending_external: sub.filter(|t| t.external.is_some()).map(|t| t.index),
            fetched,
            osd_until: Some(now + OSD_HIDE),
            menu: None,
            scrub: None,
            diagnostics: false,
            externals: Vec::new(),
            started: false,
            last_report: now,
            last_paused: false,
            reported_stop: false,
            next_at,
            next: NextState::Waiting,
            next_fetch: None,
            next_ready: None,
            autoplay: false,
            last_pos: 0.0,
            time_l: Line::default(),
            time_r: Line::default(),
            time_shown: (-1, -1),
            scrub_l: Line::default(),
            scrub_shown: -1,
            count_l: Line::default(),
            count_shown: -1,
            info: Default::default(),
        };
        (ui, req)
    }

    fn info(&self) -> &PlayInfo {
        &self.fetched.info
    }

    fn report(&self, kind: ReportKind, pos: f64, paused: bool) -> Cmd {
        let i = self.info();
        Cmd::Report(Box::new(Report {
            kind,
            item_id: i.item_id.clone(),
            media_source_id: i.media_source_id.clone(),
            play_session_id: i.play_session_id.clone(),
            position_ticks: secs_to_ticks(pos),
            paused,
            audio_stream_index: self.audio,
            subtitle_stream_index: self.subtitle,
            play_method: i.play_method,
        }))
    }

    fn show(&mut self, now: Instant) {
        self.osd_until = Some(now + OSD_HIDE);
    }

    fn osd_visible(&self, now: Instant, st: &PlaybackState) -> bool {
        st.paused
            || self.scrub.is_some()
            || self.menu.is_some()
            || self.osd_until.is_some_and(|t| now < t)
    }

    /// The segment playing now that has a skip button (not credits when
    /// the next-episode card takes their place).
    fn skippable(&self, pos: f64) -> Option<(SegmentKind, f64)> {
        let i = self.info();
        i.segments
            .iter()
            .find(|s| pos >= s.start && pos < s.end - 1.0)
            .filter(|s| !(s.kind == SegmentKind::Credits && i.next.is_some()))
            .map(|s| (s.kind, s.end))
    }

    /// Anything to draw over the video?
    pub fn overlay_visible(&self, now: Instant, st: &PlaybackState) -> bool {
        self.osd_visible(now, st)
            || self.diagnostics
            || st.software_decode()
            || !self.started
            || st.buffering
            || matches!(self.next, NextState::Showing { .. })
            || self.skippable(st.time_pos).is_some()
    }

    /// When the OSD hides or a scrub commits (the loop wakes then).
    pub fn next_deadline(&self, now: Instant) -> Option<Instant> {
        let osd = self.osd_until.filter(|t| *t > now);
        let scrub = self.scrub.as_ref().map(|s| s.last + SCRUB_COMMIT);
        match (osd, scrub) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    // ---- tracks --------------------------------------------------------

    fn embedded_subs(&self) -> i64 {
        self.info().subtitles.iter().filter(|t| t.mpv_id.is_some()).count() as i64
    }

    fn select_sub(&mut self, p: &mut Player, t: Option<&Track>) {
        self.subtitle = t.map(|t| t.index);
        let Some(t) = t else {
            p.set_subtitle(None);
            return;
        };
        if let Some(id) = t.mpv_id {
            p.set_subtitle(Some(id));
        } else if let Some(k) = self.externals.iter().position(|i| *i == t.index) {
            p.set_subtitle(Some(self.embedded_subs() + k as i64 + 1));
        } else if let Some(url) = &t.external {
            let url = format!("{}{url}", self.fetched.base_url);
            p.add_subtitle(&url, &t.label, &t.language);
            self.externals.push(t.index);
        }
    }

    fn rows(&self) -> Vec<Row> {
        let i = self.info();
        let mut v: Vec<Row> = (0..i.audio.len()).map(Row::Audio).collect();
        v.push(Row::SubOff);
        v.extend((0..i.subtitles.len()).map(Row::Sub));
        v
    }

    fn row_selected(&self, r: Row) -> bool {
        let i = self.info();
        match r {
            Row::Audio(n) => self.audio == Some(i.audio[n].index),
            Row::SubOff => self.subtitle.is_none(),
            Row::Sub(n) => self.subtitle == Some(i.subtitles[n].index),
        }
    }

    fn apply_row(&mut self, r: Row, p: &mut Player) {
        match r {
            Row::Audio(n) => {
                let t = &self.fetched.info.audio[n];
                self.audio = Some(t.index);
                if let Some(id) = t.mpv_id {
                    p.set_audio(id);
                }
            }
            Row::SubOff => self.select_sub(p, None),
            Row::Sub(n) => {
                let t = self.fetched.info.subtitles[n].clone();
                self.select_sub(p, Some(&t));
            }
        }
    }

    // ---- input ---------------------------------------------------------

    pub(super) fn stop(&mut self, pb: &mut Playback, ctx: &mut Ctx) -> Outcome {
        let st = &pb.player.state;
        if !self.reported_stop && self.started {
            ctx.send(self.report(ReportKind::Stopped, st.time_pos, st.paused));
        }
        self.reported_stop = true;
        pb.player.stop();
        Outcome::Stop
    }

    fn commit_scrub(&mut self, pb: &mut Playback, ctx: &mut Ctx) {
        if let Some(sc) = self.scrub.take() {
            pb.player.seek_absolute(sc.target);
            if self.started {
                ctx.send(self.report(ReportKind::Progress, sc.target, pb.player.state.paused));
            }
        }
    }

    fn scrub(&mut self, dir: f64, pos: f64, now: Instant) {
        let dur = self.info().duration;
        let sc = match self.scrub.take() {
            Some(mut sc) => {
                sc.presses = if now - sc.last < SCRUB_CHAIN { sc.presses + 1 } else { 1 };
                sc
            }
            None => Scrub { target: pos, last: now, presses: 1 },
        };
        let step = match sc.presses {
            0..=3 => 10.0,
            4..=10 => 30.0,
            _ => 60.0,
        };
        let max = if dur > 0.0 { dur - 1.0 } else { f64::MAX };
        self.scrub =
            Some(Scrub { target: (sc.target + dir * step).clamp(0.0, max), last: now, ..sc });
    }

    fn chapter(&mut self, forward: bool, pb: &mut Playback, ctx: &mut Ctx) {
        let pos = pb.player.state.time_pos;
        let ch = &self.info().chapters;
        let t = if forward {
            ch.iter().map(|c| c.0).find(|t| *t > pos + 1.0)
        } else {
            // Within the first seconds of a chapter: the one before.
            ch.iter().map(|c| c.0).rev().find(|t| *t < pos - 3.0)
        };
        if let Some(t) = t {
            self.scrub = Some(Scrub { target: t, last: Instant::now(), presses: 0 });
            self.commit_scrub(pb, ctx);
        }
    }

    pub fn on_action(
        &mut self,
        a: InputAction,
        pb: &mut Playback,
        now: Instant,
        ctx: &mut Ctx,
    ) -> Outcome {
        use InputAction as A;
        self.show(now);
        if let Some(f) = self.menu {
            let rows = self.rows();
            match a {
                A::Up => self.menu = Some(f.saturating_sub(1)),
                A::Down => self.menu = Some((f + 1).min(rows.len().saturating_sub(1))),
                A::Select => {
                    if let Some(r) = rows.get(f).copied() {
                        self.apply_row(r, &mut pb.player);
                        if self.started {
                            let st = &pb.player.state;
                            ctx.send(self.report(ReportKind::Progress, st.time_pos, st.paused));
                        }
                    }
                }
                A::Back | A::Menu | A::Left | A::Right => self.menu = None,
                A::Stop => return self.stop(pb, ctx),
                _ => {}
            }
            return Outcome::Continue { redraw: true };
        }
        let pos = pb.player.state.time_pos;
        match a {
            A::Left | A::SeekBack => self.scrub(-1.0, pos, now),
            A::Right | A::SeekFwd => self.scrub(1.0, pos, now),
            A::Select if self.scrub.is_some() => self.commit_scrub(pb, ctx),
            A::Select if matches!(self.next, NextState::Showing { .. }) => self.autoplay = true,
            A::Select if self.skippable(pos).is_some() => {
                if let Some((_, end)) = self.skippable(pos) {
                    self.scrub = Some(Scrub { target: end, last: now, presses: 0 });
                    self.commit_scrub(pb, ctx);
                }
            }
            A::Select | A::PlayPause => pb.player.toggle_pause(),
            A::Down | A::Menu => {
                self.commit_scrub(pb, ctx);
                let rows = self.rows();
                let at = rows.iter().position(|r| self.row_selected(*r)).unwrap_or(0);
                self.menu = Some(at);
            }
            A::Info => self.diagnostics = !self.diagnostics,
            A::Next => self.chapter(true, pb, ctx),
            A::Prev => self.chapter(false, pb, ctx),
            A::Back if self.scrub.is_some() => self.scrub = None,
            A::Back if matches!(self.next, NextState::Showing { .. }) => {
                self.next = NextState::Dismissed;
            }
            A::Back | A::Stop => return self.stop(pb, ctx),
            _ => {}
        }
        Outcome::Continue { redraw: true }
    }

    /// Remote control from another Jellyfin app (spec 8). Key presses
    /// come through `on_action`; these are the commands with a value.
    pub fn remote(
        &mut self,
        rc: &Remote,
        pb: &mut Playback,
        now: Instant,
        ctx: &mut Ctx,
    ) -> Outcome {
        self.show(now);
        let row = match rc {
            Remote::SetPaused(want) => {
                if pb.player.state.paused != *want {
                    pb.player.toggle_pause();
                }
                None
            }
            Remote::Seek(secs) => {
                let dur = self.info().duration;
                let max = if dur > 0.0 { dur - 1.0 } else { f64::MAX };
                self.scrub = Some(Scrub { target: secs.clamp(0.0, max), last: now, presses: 0 });
                self.commit_scrub(pb, ctx);
                None
            }
            Remote::Stop => return self.stop(pb, ctx),
            Remote::AudioStream(i) => {
                self.info().audio.iter().position(|t| t.index == *i).map(Row::Audio)
            }
            Remote::SubtitleStream(i) if *i < 0 => Some(Row::SubOff),
            Remote::SubtitleStream(i) => {
                self.info().subtitles.iter().position(|t| t.index == *i).map(Row::Sub)
            }
            _ => None,
        };
        if let Some(r) = row {
            self.apply_row(r, &mut pb.player);
            if self.started {
                let st = &pb.player.state;
                ctx.send(self.report(ReportKind::Progress, st.time_pos, st.paused));
            }
        }
        Outcome::Continue { redraw: true }
    }

    /// A fetch answered (the next episode's play data).
    pub fn on_fetched(&mut self, id: u64, result: &Fetched) -> bool {
        if self.next_fetch != Some(id) {
            return false;
        }
        self.next_fetch = None;
        match result {
            Fetched::Play(p) => self.next_ready = Some(p.clone()),
            _ => {
                tracing::warn!("next episode unavailable: {result:?}");
                self.next = NextState::Dismissed;
            }
        }
        true
    }

    // ---- per frame -----------------------------------------------------

    /// Reports, scrub commits, the next-episode countdown and the end.
    pub fn tick(&mut self, pb: &mut Playback, now: Instant, ctx: &mut Ctx) -> Tick {
        let mut redraw = false;
        let (pos, paused) = (pb.player.state.time_pos, pb.player.state.paused);
        if pb.started && !self.started {
            self.started = true;
            self.last_paused = paused;
            self.last_report = now;
            ctx.send(self.report(ReportKind::Start, pos, paused));
            if let Some(i) = self.pending_external.take() {
                let t = self.info().subtitles.iter().find(|t| t.index == i).cloned();
                self.select_sub(&mut pb.player, t.as_ref());
            }
            redraw = true;
        }
        let due = paused != self.last_paused || now - self.last_report >= REPORT_EVERY;
        if self.started && !self.reported_stop && pb.ended.is_none() && due {
            self.last_paused = paused;
            self.last_report = now;
            ctx.send(self.report(ReportKind::Progress, pos, paused));
        }
        if self.scrub.as_ref().is_some_and(|s| now - s.last >= SCRUB_COMMIT) {
            self.commit_scrub(pb, ctx);
            redraw = true;
        }

        // Next episode: the card, a countdown in playback time, autoplay.
        let advanced = (pos - self.last_pos).clamp(0.0, 1.0);
        self.last_pos = pos;
        match self.next {
            NextState::Waiting if self.started && self.next_at.is_some_and(|t| pos >= t) => {
                // Never promise more seconds than are left.
                let left = NEXT_COUNTDOWN.min(self.info().duration - pos).max(1.0);
                self.next = NextState::Showing { left };
                self.fetch_next(ctx);
                redraw = true;
            }
            NextState::Showing { left } => {
                let left = left - advanced;
                self.next = NextState::Showing { left };
                if left <= 0.0 {
                    self.autoplay = true;
                }
            }
            _ => {}
        }

        if let Some((reason, _)) = &pb.ended {
            let eof = *reason == EndReason::Eof;
            if !self.reported_stop && self.started {
                let at = if eof { self.info().duration.max(pos) } else { pos };
                ctx.send(self.report(ReportKind::Stopped, at, false));
            }
            self.reported_stop = true;
            let has_next = self.info().next.is_some();
            if eof && has_next && self.next != NextState::Dismissed {
                self.autoplay = true;
                self.fetch_next(ctx);
            } else {
                return Tick::Finished;
            }
        }
        if self.autoplay && self.next != NextState::Dismissed {
            if let Some(n) = self.next_ready.take() {
                if !self.reported_stop {
                    ctx.send(self.report(ReportKind::Stopped, pos, paused));
                    self.reported_stop = true;
                }
                return Tick::Next(n);
            }
        } else if self.autoplay && pb.ended.is_some() {
            return Tick::Finished;
        }
        Tick::Continue { redraw }
    }

    fn fetch_next(&mut self, ctx: &mut Ctx) {
        if self.next_fetch.is_some() || self.next_ready.is_some() {
            return;
        }
        if let Some(n) = &self.fetched.info.next {
            self.next_fetch = Some(ctx.fetch(Fetch::Play { id: n.id.clone(), start_ticks: 0 }));
        }
    }

    // ---- trickplay -----------------------------------------------------

    fn sheet_job(&self, sheet: u32) -> Option<ImageJob> {
        let i = self.info();
        let t = i.trickplay.as_ref()?;
        let path = jellyfin_api::playback::trickplay_tile_path(
            &i.item_id,
            t.width,
            sheet,
            &i.media_source_id,
        );
        let (w, h) = (t.width * t.cols, t.height * t.sheet_rows(sheet));
        Some(ImageJob {
            key: fnv1a(&[path.as_bytes(), &w.to_le_bytes(), &h.to_le_bytes(), b"trick"]),
            source: ImageSource::Server(path),
            w,
            h,
            finish: Finish::Card { radius: 0.0 },
        })
    }

    /// Tile sheets wanted now: around the scrub target.
    pub fn wants(&self) -> Vec<ImageJob> {
        let (Some(sc), Some(t)) = (&self.scrub, &self.info().trickplay) else { return Vec::new() };
        let (sheet, _, _) = t.tile(sc.target);
        let last = t.tile(f64::MAX).0;
        let mut v: Vec<ImageJob> = [sheet, sheet + 1, sheet.saturating_sub(1)]
            .iter()
            .filter(|s| **s <= last)
            .filter_map(|s| self.sheet_job(*s))
            .collect();
        v.dedup_by_key(|j| j.key);
        v
    }

    // ---- drawing -------------------------------------------------------

    pub fn draw<R: Renderer>(
        &mut self,
        r: &mut R,
        tex: &mut Textures,
        st: &PlaybackState,
        now: Instant,
        safe_area: f32,
    ) {
        let (lw, lh) = r.logical_size();
        let (sx, sy) = (lw * safe_area, lh * safe_area);

        if st.software_decode() {
            let style = TextStyle::new(20.0, Weight::Bold);
            let m = r.measure_text(s::SOFTWARE_DECODE, &style);
            let rect = Rect::new(lw - sx - m.width - 32.0, sy, m.width + 32.0, m.height + 16.0);
            r.fill_rrect(rect, 8.0, WARN_BG);
            r.text(s::SOFTWARE_DECODE, &style, Point::new(rect.x + 16.0, rect.y + 8.0), TEXT);
        }
        if !self.started || st.buffering {
            let msg = if self.started { s::BUFFERING } else { s::STARTING };
            let style = TextStyle::new(26.0, Weight::Bold);
            let m = r.measure_text(msg, &style);
            let rect = Rect::new((lw - m.width) / 2.0 - 20.0, sy, m.width + 40.0, m.height + 20.0);
            r.fill_rrect(rect, 10.0, PANEL);
            r.text(msg, &style, Point::new(rect.x + 20.0, rect.y + 10.0), TEXT);
        }
        if self.diagnostics {
            self.draw_info(r, st, sx, sy);
        }

        let osd = self.osd_visible(now, st);
        let bar_top = lh - sy - 150.0;
        // Skip button / next-episode card, above the bar's area.
        let corner_y = if osd { bar_top - 90.0 } else { lh - sy - 70.0 };
        if let NextState::Showing { left } = self.next {
            self.draw_next(r, left, lw - sx, corner_y);
        } else if let Some((kind, _)) = self.skippable(st.time_pos) {
            let label = match kind {
                SegmentKind::Intro => s::SKIP_INTRO,
                SegmentKind::Recap => s::SKIP_RECAP,
                SegmentKind::Preview => s::SKIP_PREVIEW,
                SegmentKind::Commercial => s::SKIP_COMMERCIAL,
                SegmentKind::Credits => s::SKIP_CREDITS,
            };
            let w = super::widgets::button_width(r, label).max(200.0);
            let rect = Rect::new(lw - sx - w, corner_y, w, 56.0);
            super::widgets::draw_button(r, rect, label, true, false);
        }

        if !osd {
            return;
        }
        let bar_h = lh * 0.3;
        r.fill_gradient(
            Rect::new(0.0, lh - bar_h, lw, bar_h),
            0.0,
            &Gradient {
                from: Color::BLACK.with_alpha(0.0),
                to: Color::BLACK.with_alpha(0.85),
                vertical: true,
            },
        );
        let i = &self.fetched.info;
        let width = lw - 2.0 * sx;
        r.text(
            &i.title,
            &TextStyle::new(34.0, Weight::Bold).max_width(width),
            Point::new(sx, bar_top),
            TEXT,
        );
        if !i.subtitle.is_empty() {
            r.text(
                &i.subtitle,
                &TextStyle::new(22.0, Weight::Regular).max_width(width),
                Point::new(sx, bar_top + 46.0),
                TEXT_DIM,
            );
        }

        // Seek bar with chapter marks, position and scrub target.
        let track = Rect::new(sx, lh - sy - 58.0, width, 8.0);
        let dur = i.duration.max(st.duration).max(1.0);
        let x_at = |t: f64| track.x + track.w * (t / dur).clamp(0.0, 1.0) as f32;
        r.fill_rrect(track, 4.0, TRACK_BG);
        let played = x_at(st.time_pos) - track.x;
        if played > 0.0 {
            r.fill_rrect(Rect::new(track.x, track.y, played, track.h), 4.0, ACCENT);
        }
        for (t, _) in &i.chapters {
            if *t > 0.5 {
                r.fill_rect(Rect::new(x_at(*t) - 1.0, track.y - 3.0, 2.0, track.h + 6.0), TEXT_DIM);
            }
        }
        let shown = (st.time_pos as i64, (dur - st.time_pos).max(0.0) as i64);
        if shown != self.time_shown {
            self.time_shown = shown;
            self.time_l.clear();
            fmt_time(&mut self.time_l, st.time_pos);
            if st.paused {
                let _ = write!(self.time_l, "   {}", s::PAUSED);
            }
            self.time_r.clear();
            let _ = self.time_r.write_str("-");
            fmt_time(&mut self.time_r, dur - st.time_pos);
        }
        let ts = TextStyle::new(22.0, Weight::Regular);
        r.text(self.time_l.as_str(), &ts, Point::new(sx, track.y + 18.0), TEXT);
        let m = r.measure_text(self.time_r.as_str(), &ts);
        r.text(
            self.time_r.as_str(),
            &ts,
            Point::new(track.right() - m.width, track.y + 18.0),
            TEXT,
        );
        if self.scrub.is_some() {
            self.draw_scrub(r, tex, track, x_at, sy);
        } else {
            let x = x_at(st.time_pos);
            r.fill_rrect(Rect::new(x - 8.0, track.y - 4.0, 16.0, 16.0), 8.0, TEXT);
        }
        r.text(
            s::PLAYER_HINT,
            &TextStyle::new(18.0, Weight::Regular),
            Point::new(sx, track.y + 46.0),
            TEXT_DIM,
        );
        if let Some(f) = self.menu {
            self.draw_menu(r, f, lw, lh, sx, sy);
        }
    }

    fn draw_scrub<R: Renderer>(
        &mut self,
        r: &mut R,
        tex: &mut Textures,
        track: Rect,
        x_at: impl Fn(f64) -> f32,
        sy: f32,
    ) {
        let Some(sc) = &self.scrub else { return };
        let target = sc.target;
        let x = x_at(target);
        r.fill_rrect(Rect::new(x - 10.0, track.y - 6.0, 20.0, 20.0), 10.0, ACCENT);
        if target as i64 != self.scrub_shown {
            self.scrub_shown = target as i64;
            self.scrub_l.clear();
            fmt_time(&mut self.scrub_l, target);
        }
        // Thumbnail (trickplay) above the target, with its time below it.
        let (tw, th) = (320.0, 180.0);
        let left = (x - tw / 2.0).clamp(track.x, track.right() - tw);
        let thumb = Rect::new(left, track.y - th - 70.0, tw, th);
        let mut drew = false;
        if let Some(t) = &self.fetched.info.trickplay {
            let (sheet, col, row) = t.tile(target);
            if let Some(job) = self.sheet_job(sheet)
                && let Some(img) = tex.get(job.key)
            {
                let src = Rect::new(
                    (col * t.width) as f32,
                    (row * t.height) as f32,
                    t.width as f32,
                    t.height as f32,
                );
                r.fill_rrect(thumb.inset(-3.0), 6.0, TEXT);
                r.image_region(img, src, thumb);
                drew = true;
            }
        }
        let ts = TextStyle::new(24.0, Weight::Bold);
        let m = r.measure_text(self.scrub_l.as_str(), &ts);
        let label_y = if drew { thumb.bottom() + 8.0 } else { track.y - 60.0 };
        let lx = (x - m.width / 2.0).clamp(track.x, track.right() - m.width);
        r.fill_rrect(
            Rect::new(lx - 10.0, label_y - 4.0, m.width + 20.0, m.height + 8.0),
            6.0,
            PANEL,
        );
        r.text(self.scrub_l.as_str(), &ts, Point::new(lx, label_y), TEXT);
        let _ = sy;
    }

    fn draw_next<R: Renderer>(&mut self, r: &mut R, left: f64, right: f32, y: f32) {
        let Some(n) = &self.fetched.info.next else { return };
        let secs = left.ceil().max(0.0) as i64;
        if secs != self.count_shown {
            self.count_shown = secs;
            self.count_l.clear();
            let n = secs.to_string();
            let _ = self.count_l.write_str(&s::fill(s::PLAYING_IN, &[&n]));
        }
        let (w, h) = (560.0, 150.0);
        let rect = Rect::new(right - w, y - h + 56.0, w, h);
        r.fill_rrect(rect, 12.0, PANEL);
        let x = rect.x + 24.0;
        r.text(
            s::UP_NEXT,
            &TextStyle::new(20.0, Weight::Bold),
            Point::new(x, rect.y + 16.0),
            ACCENT,
        );
        let line = if n.subtitle.is_empty() { &n.title } else { &n.subtitle };
        r.text(
            line,
            &TextStyle::new(26.0, Weight::Bold).max_width(w - 48.0),
            Point::new(x, rect.y + 46.0),
            TEXT,
        );
        r.text(
            self.count_l.as_str(),
            &TextStyle::new(20.0, Weight::Regular),
            Point::new(x, rect.y + 86.0),
            TEXT_DIM,
        );
        r.text(
            s::PLAY_NOW_HINT,
            &TextStyle::new(16.0, Weight::Regular),
            Point::new(x, rect.y + 116.0),
            TEXT_DIM,
        );
        let frac = (1.0 - left / NEXT_COUNTDOWN).clamp(0.0, 1.0) as f32;
        r.fill_rect(Rect::new(rect.x, rect.bottom() - 4.0, w * frac, 4.0), ACCENT);
    }

    fn draw_menu<R: Renderer>(&self, r: &mut R, focus: usize, lw: f32, lh: f32, sx: f32, sy: f32) {
        let i = &self.fetched.info;
        let w = 620.0;
        // Below the software-decode badge (top right).
        let top = sy + 64.0;
        let panel = Rect::new(lw - sx - w, top, w, lh - top - sy - 170.0);
        r.fill_rrect(panel, 14.0, PANEL);
        let x = panel.x + 28.0;
        let mut y = panel.y + 24.0;
        let head = TextStyle::new(24.0, Weight::Bold);
        let item = TextStyle::new(22.0, Weight::Regular).max_width(w - 80.0);
        let rows = self.rows();
        // Keep the focused row in view: rows are 44 px; scroll by whole rows.
        let visible = ((panel.h - 120.0) / 44.0).max(1.0) as usize;
        let first = focus.saturating_sub(visible.saturating_sub(2));
        let header = |r: &mut R, y: &mut f32, t: &str| {
            r.text(t, &head, Point::new(x, *y), TEXT_DIM);
            *y += 40.0;
        };
        for (n, row) in rows.iter().enumerate().skip(first).take(visible) {
            if n == 0 || (n == first && n > 0) {
                header(
                    r,
                    &mut y,
                    if matches!(row, Row::Audio(_)) { s::AUDIO } else { s::SUBTITLES },
                );
            } else if *row == Row::SubOff {
                y += 10.0;
                header(r, &mut y, s::SUBTITLES);
            }
            let label = match row {
                Row::Audio(k) => i.audio[*k].label.as_str(),
                Row::SubOff => s::SUBTITLES_OFF,
                Row::Sub(k) => i.subtitles[*k].label.as_str(),
            };
            let rect = Rect::new(x - 12.0, y - 6.0, w - 32.0, 40.0);
            if n == focus {
                r.fill_rrect(rect, 8.0, TEXT);
            }
            if self.row_selected(*row) {
                r.fill_rect(Rect::new(rect.x + 4.0, rect.y + 8.0, 4.0, rect.h - 16.0), ACCENT);
            }
            let c = if n == focus { Color::rgb(10, 10, 14) } else { TEXT };
            r.text(label, &item, Point::new(x + 8.0, y), c);
            y += 44.0;
        }
    }

    fn draw_info<R: Renderer>(&mut self, r: &mut R, st: &PlaybackState, x: f32, y: f32) {
        let i = &self.fetched.info;
        let d = &st.drops;
        for l in &mut self.info {
            l.clear();
        }
        let hw = if st.software_decode() { "software (no hardware decoder)" } else { &st.hwdec };
        let _ = write!(
            self.info[0],
            "Video  {} {}×{} @ {:.3} fps",
            st.video_codec, st.width, st.height, st.fps
        );
        let _ = write!(self.info[1], "Decoder  {hw}");
        let _ = write!(self.info[2], "Audio  {}", st.audio_codec);
        let _ = write!(self.info[3], "Method  {:?} (no transcoding)", i.play_method);
        let _ = write!(self.info[4], "Dropped  vo {}  decoder {}", d.vo, d.decoder);
        let _ = write!(self.info[5], "Delayed {}  mistimed {}", d.delayed, d.mistimed);
        let style = TextStyle::new(18.0, Weight::Regular);
        let lh = 26.0;
        r.fill_rrect(Rect::new(x, y, 640.0, lh * self.info.len() as f32 + 24.0), 8.0, PANEL);
        for (n, l) in self.info.iter().enumerate() {
            r.text(l.as_str(), &style, Point::new(x + 12.0, y + 12.0 + n as f32 * lh), TEXT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::home::playinfo::{Segment, Trickplay};
    use jellyfin_api::playback::PlayMethod;

    fn track(index: i32, mpv: Option<i64>, ext: bool) -> Track {
        Track {
            index,
            mpv_id: mpv,
            external: ext.then(|| format!("/sub/{index}.srt")),
            label: format!("t{index}"),
            language: String::new(),
        }
    }

    pub fn info() -> PlayInfo {
        PlayInfo {
            item_id: "item".into(),
            title: "Show".into(),
            subtitle: "S1:E1 · Pilot".into(),
            is_episode: true,
            media_source_id: "item".into(),
            play_session_id: "ps".into(),
            play_method: PlayMethod::DirectPlay,
            stream_path: "/Videos/item/stream?static=true".into(),
            duration: 600.0,
            start: 12.0,
            audio: vec![track(1, Some(1), false), track(2, Some(2), false)],
            subtitles: vec![track(3, Some(1), false), track(4, None, true)],
            default_audio: Some(2),
            default_subtitle: Some(4),
            chapters: vec![(0.0, "A".into()), (300.0, "B".into())],
            segments: vec![
                Segment { kind: SegmentKind::Intro, start: 30.0, end: 60.0 },
                Segment { kind: SegmentKind::Credits, start: 560.0, end: 600.0 },
            ],
            trickplay: Some(Trickplay {
                width: 320,
                height: 180,
                cols: 10,
                rows: 10,
                count: 60,
                interval: 10.0,
            }),
            next: None,
            fps: None,
        }
    }

    fn fetched(info: PlayInfo) -> PlayFetched {
        PlayFetched {
            info,
            base_url: "http://srv".into(),
            authorization: "MediaBrowser Token=\"t\"".into(),
        }
    }

    #[test]
    fn request_uses_default_tracks() {
        let (ui, req) = PlayerUi::new(fetched(info()), Instant::now());
        assert_eq!(req.url, "http://srv/Videos/item/stream?static=true");
        assert_eq!(req.start_secs, 12.0);
        assert_eq!(req.audio_track.as_deref(), Some("2"));
        // The default subtitle is external: none at start, added on load.
        assert_eq!(req.sub_track.as_deref(), Some("no"));
        assert_eq!(ui.pending_external, Some(4));
        assert_eq!(req.title, "Show · S1:E1 · Pilot");
    }

    #[test]
    fn scrub_accelerates_and_clamps() {
        let t = Instant::now();
        let (mut ui, _) = PlayerUi::new(fetched(info()), t);
        for k in 0..12 {
            ui.scrub(1.0, 100.0, t + Duration::from_millis(100 * k));
        }
        // 3 x 10 s, 7 x 30 s, 2 x 60 s.
        assert_eq!(ui.scrub.as_ref().unwrap().target, 100.0 + 30.0 + 210.0 + 120.0);
        for k in 0..30 {
            ui.scrub(1.0, 0.0, t + Duration::from_millis(1300 + 100 * k));
        }
        assert_eq!(ui.scrub.as_ref().unwrap().target, 599.0, "never past the end");
        // A pause resets the speed.
        ui.scrub(-1.0, 0.0, t + Duration::from_secs(10));
        assert_eq!(ui.scrub.as_ref().unwrap().target, 589.0);
    }

    #[test]
    fn skip_buttons_and_next_card() {
        let t = Instant::now();
        let (ui, _) = PlayerUi::new(fetched(info()), t);
        assert_eq!(ui.skippable(45.0), Some((SegmentKind::Intro, 60.0)));
        assert_eq!(ui.skippable(59.5), None, "not in the last second");
        assert_eq!(ui.skippable(570.0), Some((SegmentKind::Credits, 600.0)));
        assert_eq!(ui.next_at, None, "no next episode");
        let mut with_next = info();
        with_next.next = Some(crate::home::model::Card {
            id: "e2".into(),
            kind: Default::default(),
            collection: None,
            title: "Show".into(),
            subtitle: "S1:E2 · Two".into(),
            meta: String::new(),
            overview: String::new(),
            image: None,
            backdrop: None,
            progress: None,
            played: false,
        });
        let (ui, _) = PlayerUi::new(fetched(with_next), t);
        assert_eq!(ui.next_at, Some(560.0), "the countdown starts at the credits");
        assert_eq!(ui.skippable(570.0), None, "the next-episode card replaces Skip credits");
    }

    #[test]
    fn menu_rows_and_selection() {
        let (ui, _) = PlayerUi::new(fetched(info()), Instant::now());
        let rows = ui.rows();
        assert_eq!(rows, [Row::Audio(0), Row::Audio(1), Row::SubOff, Row::Sub(0), Row::Sub(1)]);
        assert!(ui.row_selected(Row::Audio(1)));
        assert!(ui.row_selected(Row::Sub(1)), "default external subtitle");
        assert_eq!(ui.embedded_subs(), 1);
    }

    #[test]
    fn trickplay_wants_sheets_while_scrubbing() {
        let t = Instant::now();
        let (mut ui, _) = PlayerUi::new(fetched(info()), t);
        assert!(ui.wants().is_empty(), "nothing until a scrub");
        ui.scrub(1.0, 100.0, t);
        let w = ui.wants();
        assert_eq!(w.len(), 1, "60 thumbnails fit one sheet");
        assert_eq!((w[0].w, w[0].h), (3200, 1080), "6 rows on the only sheet");
        assert!(
            matches!(&w[0].source, ImageSource::Server(p) if p.contains("/Trickplay/320/0.jpg"))
        );
    }
}
