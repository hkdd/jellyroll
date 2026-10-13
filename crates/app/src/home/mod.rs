//! The Jellyfin UI (spec 10.1): server and sign-in screens, then browsing:
//! Home at the bottom of a page stack, with Library, Detail, Search and
//! Settings pages pushed on top (Back pops; each page keeps its own focus
//! and scroll). Owns the GPU textures for artwork, tells the network thread
//! which images and data it needs, and hands playback requests to the
//! frame loop.

mod detail;
mod library;
pub mod model;
mod osk;
mod pages;
mod search;
mod server_entry;
mod settings;
mod signin;
mod status;
mod strings;
mod textures;
pub mod theme;
mod view;
mod widgets;

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use jellyroll_render::{Point, Renderer, TextStyle, Weight};
use jellyroll_video::va::DecodeCaps;

use crate::config::UiResolution;
use crate::input::InputAction;
use crate::net::{Cmd, Event, Fetch, Fetched, NetHandle, Status};
use detail::DetailView;
use library::LibraryView;
use pages::{Ctx, Open};
use search::SearchView;
use server_entry::ServerEntry;
use settings::SettingsView;
use signin::SignInForm;
use status::StatusView;
use textures::Textures;
use view::HomeView;
use widgets::{Viewport, Wants};

/// Per-frame texture upload budget (spec 9).
/// Per-frame image upload time: the frame's other work at 4K is ~2 ms on
/// the reference HTPC, and the CPU budget is 4 ms (spec 2).
const UPLOAD_BUDGET: Duration = Duration::from_micros(1500);
/// Network events handled per loop iteration (bounds the work per frame).
const EVENTS_PER_FRAME: usize = 64;
/// How often image requests are rebuilt while images keep arriving.
const WANTS_AFTER_UPLOAD: Duration = Duration::from_millis(100);

/// What is on screen.
enum Screen {
    Blank,
    Status(StatusView),
    Entry(ServerEntry),
    SignIn(SignInForm),
    /// Home and the page stack.
    Browse,
}

enum Page {
    Library(Box<LibraryView>),
    Detail(Box<DetailView>),
    Search(Box<SearchView>),
    Settings(Box<SettingsView>),
}

/// What the frame loop needs to start playback.
pub struct PlayRequest {
    pub url: String,
    /// `Authorization` header value for the stream request.
    pub authorization: String,
    pub start_secs: f64,
    pub title: String,
}

pub struct HomeApp {
    net: NetHandle,
    screen: Screen,
    home: Option<HomeView>,
    stack: Vec<Page>,
    /// Shown from cache while the server is unreachable.
    offline: bool,
    textures: Textures,
    vp: Viewport,
    wants_dirty: bool,
    last_wants: Vec<u64>,
    next_id: u64,
    /// What the GPU decodes (probed once in the background).
    caps: Arc<OnceLock<Option<DecodeCaps>>>,
    /// Device facts for Settings (versions, GPU, display).
    about: Vec<(String, String)>,
    /// Stream fetch in flight: (request id, start, title).
    play_req: Option<(u64, i64, String)>,
    play: Option<PlayRequest>,
    /// The Quick Connect screen the password form came from.
    qc: Option<Status>,
    /// Counts loading work started (requests sent, image wants rebuilt);
    /// frames doing it are not steady-state frames (D40).
    load_work: u64,
    /// Images arrived since the image requests were last rebuilt; rebuilt
    /// at most every `WANTS_AFTER_UPLOAD` (input rebuilds them at once).
    uploaded: bool,
    wants_at: Instant,
    /// Time the last frame spent uploading images (for benchmarks).
    pub last_upload_ms: f32,
    ui: UiSetting,
    fb: (u32, u32),
    /// A new UI render scale for the frame loop to apply.
    new_render_scale: Option<f32>,
    /// Drop all textures before the next draw (their size is stale).
    purge_textures: bool,
}

/// The UI resolution in effect and where a choice made in Settings is saved.
pub struct UiSetting {
    pub resolution: UiResolution,
    pub save_to: Option<std::path::PathBuf>,
}

impl HomeApp {
    pub fn new(
        net: NetHandle,
        texture_cap_bytes: u64,
        fb: (u32, u32),
        safe_area: f32,
        about: Vec<(String, String)>,
        ui: UiSetting,
    ) -> Self {
        let caps = Arc::new(OnceLock::new());
        let c = caps.clone();
        let _ = std::thread::Builder::new().name("va-probe".into()).spawn(move || {
            let _ = c.set(jellyroll_video::va::probe().ok());
        });
        let mut a = Self {
            net,
            screen: Screen::Blank,
            home: None,
            stack: Vec::new(),
            offline: false,
            textures: Textures::new(texture_cap_bytes),
            vp: Viewport { lw: 1920.0, safe: safe_area, scale: 1.0 },
            wants_dirty: false,
            last_wants: Vec::new(),
            next_id: 0,
            caps,
            about,
            play_req: None,
            play: None,
            qc: None,
            load_work: 0,
            uploaded: false,
            wants_at: Instant::now(),
            last_upload_ms: 0.0,
            ui,
            fb,
            new_render_scale: None,
            purge_textures: false,
        };
        a.set_output(fb, safe_area, Instant::now());
        a
    }

    /// Output size or safe area changed. Logical height is always 1080.
    pub fn set_output(&mut self, fb: (u32, u32), safe_area: f32, now: Instant) {
        self.fb = fb;
        let device = fb.1.max(1) as f32 / 1080.0;
        // Artwork is sized for the pixels the UI is drawn at.
        let scale = device * self.ui.resolution.render_scale(fb.1);
        self.vp = Viewport { lw: fb.0 as f32 / device, safe: safe_area, scale };
        let vp = self.vp;
        if let Some(h) = &mut self.home {
            self.wants_dirty |= h.set_viewport(vp, now);
        }
        for p in &mut self.stack {
            self.wants_dirty |= match p {
                Page::Library(v) => v.set_viewport(vp, now),
                Page::Detail(v) => v.set_viewport(vp, now),
                Page::Search(v) => v.set_viewport(vp, now),
                Page::Settings(v) => v.set_viewport(vp),
            };
        }
    }

    fn send_all(&mut self, cmds: Vec<Cmd>) {
        for c in cmds {
            if !self.net.send(c) {
                tracing::warn!("network queue full; a request was dropped");
            }
        }
    }

    /// Runs `f` with a request context, then sends what it asked for and
    /// opens what it opened.
    fn with_ctx<T>(&mut self, now: Instant, f: impl FnOnce(&mut Self, &mut Ctx) -> T) -> T {
        let mut next = self.next_id;
        let mut ctx = Ctx::new(&mut next);
        let out = f(self, &mut ctx);
        let Ctx { cmds, open, .. } = ctx;
        self.next_id = next;
        self.load_work += (!cmds.is_empty()) as u64;
        self.send_all(cmds);
        if let Some(o) = open {
            self.open(o, now);
        }
        out
    }

    fn open(&mut self, o: Open, now: Instant) {
        let vp = self.vp;
        let page = match o {
            Open::Play { id, media_source, start_ticks, title } => {
                let req =
                    self.with_ctx(now, |_, ctx| ctx.fetch(Fetch::Stream { id, media_source }));
                self.play_req = Some((req, start_ticks, title));
                return;
            }
            Open::Library { id, title, collection } => self.with_ctx(now, |_, ctx| {
                Page::Library(Box::new(LibraryView::new(id, title, collection, vp, now, ctx)))
            }),
            Open::Detail { id } => self
                .with_ctx(now, |_, ctx| Page::Detail(Box::new(DetailView::new(id, vp, now, ctx)))),
            Open::Search => Page::Search(Box::new(SearchView::new(vp, now))),
            Open::Settings => {
                let (user, server) = self
                    .home
                    .as_ref()
                    .map(|h| (h.data.user_name.clone(), h.data.server_name.clone()))
                    .unwrap_or_default();
                let res = self.resolution_choice();
                Page::Settings(Box::new(SettingsView::new(vp, &user, &server, self.about(), res)))
            }
            Open::UiResolution(r) => {
                self.set_resolution(r);
                return;
            }
        };
        self.stack.push(page);
        self.wants_dirty = true;
    }

    /// What Settings offers: None on outputs of 1080 lines or fewer (the
    /// choice makes no difference there).
    fn resolution_choice(&self) -> Option<UiResolution> {
        (self.fb.1 > 1080).then(|| match self.ui.resolution.render_scale(self.fb.1) < 1.0 {
            true => UiResolution::Hd1080,
            false => UiResolution::Native,
        })
    }

    fn set_resolution(&mut self, r: UiResolution) {
        let before = self.ui.resolution.render_scale(self.fb.1);
        self.ui.resolution = r;
        if let Some(p) = &self.ui.save_to {
            crate::ui_prefs::save(p, &crate::ui_prefs::UiPrefs { ui_resolution: Some(r) });
        }
        let after = r.render_scale(self.fb.1);
        if after != before {
            tracing::info!(?r, render_scale = after, "UI resolution changed");
            self.new_render_scale = Some(after);
            self.purge_textures = true;
        }
    }

    /// A UI render scale changed in Settings, for the frame loop to give
    /// the renderer (then `set_output` to resize the artwork).
    pub fn take_render_scale(&mut self) -> Option<f32> {
        self.new_render_scale.take()
    }

    fn about(&self) -> Vec<(String, String)> {
        let mut a = self.about.clone();
        let decode = match self.caps.get() {
            Some(Some(c)) => {
                let list = |v: Vec<jellyroll_video::va::Codec>| {
                    v.iter().map(|c| c.name()).collect::<Vec<_>>().join(", ")
                };
                let mut s = list(c.supported());
                let no = list(c.unsupported());
                if !no.is_empty() {
                    s = format!("{s}   (not: {no})");
                }
                s
            }
            Some(None) => "none (no VA-API driver)".into(),
            None => "checking…".into(),
        };
        a.push(("Hardware decode".into(), decode));
        a
    }

    /// Handles network events. Returns true if the screen changed.
    pub fn poll(&mut self, now: Instant) -> bool {
        let mut dirty = false;
        for _ in 0..EVENTS_PER_FRAME {
            let Some(ev) = self.net.try_recv() else { break };
            dirty = true;
            match ev {
                Event::Status(Status::ServerEntry { address, error, found, searching }) => {
                    self.stack.clear();
                    if !matches!(self.screen, Screen::Entry(_)) {
                        self.screen = Screen::Entry(ServerEntry::new(&address));
                    }
                    if let Screen::Entry(e) = &mut self.screen {
                        e.update(error, found, searching);
                    }
                }
                Event::Status(s) => {
                    self.stack.clear();
                    if let Status::QuickConnect { .. } = &s {
                        self.qc = Some(s.clone());
                        // A new code while the password form is open: stay
                        // in the form.
                        if matches!(self.screen, Screen::SignIn(_)) {
                            continue;
                        }
                    }
                    self.screen = Screen::Status(StatusView::new(s));
                }
                Event::Home { data, fresh } => {
                    let vp = self.vp;
                    match &mut self.home {
                        Some(h) => h.set_data(data, now),
                        None => self.home = Some(HomeView::new(data, vp, now)),
                    }
                    if fresh {
                        self.offline = false;
                        self.textures.forget_failures();
                    }
                    if let Some(h) = &mut self.home {
                        h.offline = self.offline;
                    }
                    if !matches!(self.screen, Screen::Browse) {
                        self.screen = Screen::Browse;
                        self.stack.clear();
                    }
                    self.wants_dirty = true;
                }
                Event::Offline => {
                    self.offline = true;
                    if let Some(h) = &mut self.home {
                        h.offline = true;
                    }
                }
                Event::Image { key, w, h, rgba, opaque } => {
                    self.textures.queue(key, w, h, rgba, opaque)
                }
                Event::ImageFailed { key } => {
                    self.textures.fail(key);
                    dirty = false;
                }
                Event::Fetched { id, result } => {
                    dirty = self.fetched(id, result, now);
                    self.wants_dirty |= dirty;
                }
                Event::Users(users) => {
                    let (sx, sy) = (self.vp.sx(), self.vp.sy());
                    if let Screen::SignIn(f) = &mut self.screen {
                        f.set_users(users, sx, sy);
                    }
                }
                Event::SignInFailed(msg) => {
                    if let Screen::SignIn(f) = &mut self.screen {
                        f.failed(msg);
                    }
                }
            }
        }
        dirty
    }

    fn fetched(&mut self, id: u64, result: Fetched, now: Instant) -> bool {
        if let Some((req, start_ticks, title)) = self.play_req.take() {
            if req == id {
                if let Fetched::Stream { url, authorization } = result {
                    self.play = Some(PlayRequest {
                        url,
                        authorization,
                        start_secs: start_ticks as f64 / 1e7,
                        title,
                    });
                }
                return true;
            }
            self.play_req = Some((req, start_ticks, title));
        }
        let mut used = false;
        self.with_ctx(now, |me, ctx| {
            for p in me.stack.iter_mut() {
                used |= match p {
                    Page::Library(v) => v.on_fetched(id, &result, now, ctx),
                    Page::Detail(v) => v.on_fetched(id, &result, now, ctx),
                    Page::Search(v) => v.on_fetched(id, &result, now),
                    Page::Settings(_) => false,
                };
            }
        });
        used
    }

    /// A playback request for the frame loop, once its stream URL is known.
    pub fn take_play(&mut self) -> Option<PlayRequest> {
        self.play.take()
    }

    /// Playback ended: refresh what it changed (resume points, watched).
    pub fn playback_finished(&mut self, now: Instant) {
        self.send_all(vec![Cmd::Refresh]);
        self.with_ctx(now, |me, ctx| {
            if let Some(Page::Detail(d)) = me.stack.last_mut() {
                d.reload(ctx);
            }
        });
        self.wants_dirty = true;
    }

    /// A text field is active: the physical keyboard should type.
    pub fn wants_text(&self) -> bool {
        match self.screen {
            Screen::Entry(_) | Screen::SignIn(_) => true,
            Screen::Browse => matches!(self.stack.last(), Some(Page::Search(_))),
            _ => false,
        }
    }

    /// Applies an action. Returns true if it was used (a redraw is due and
    /// Back must not quit the app).
    pub fn on_action(&mut self, a: InputAction, now: Instant) -> bool {
        match &mut self.screen {
            Screen::Entry(e) => {
                let (used, cmd) = e.on_action(a);
                if let Some(c) = cmd {
                    self.net.send(c);
                }
                used
            }
            Screen::SignIn(f) => {
                if a == InputAction::Back {
                    self.screen = match self.qc.clone() {
                        Some(s) => Screen::Status(StatusView::new(s)),
                        None => Screen::Blank,
                    };
                    return true;
                }
                let (used, cmd) = f.on_action(a);
                if let Some(c) = cmd {
                    self.net.send(c);
                }
                used
            }
            Screen::Status(st) => match a {
                InputAction::Left | InputAction::Right if st.buttons() > 0 => {
                    st.move_focus(a == InputAction::Right)
                }
                InputAction::Select if st.buttons() > 0 && st.button_focus() == 1 => {
                    let server = match &st.status {
                        Status::QuickConnect { server, .. } => server.clone(),
                        _ => String::new(),
                    };
                    self.screen =
                        Screen::SignIn(SignInForm::new(&server, self.vp.sx(), self.vp.sy()));
                    self.net.send(Cmd::ListUsers);
                    true
                }
                InputAction::Select if st.retryable() => {
                    self.net.send(Cmd::Retry);
                    true
                }
                InputAction::Back if st.can_change_server() => {
                    self.net.send(Cmd::ChangeServer);
                    true
                }
                _ => false,
            },
            Screen::Browse => {
                match a {
                    InputAction::Home if !self.stack.is_empty() => {
                        self.stack.clear();
                        self.wants_dirty = true;
                        return true;
                    }
                    InputAction::Search if !matches!(self.stack.last(), Some(Page::Search(_))) => {
                        self.open(Open::Search, now);
                        return true;
                    }
                    _ => {}
                }
                let used = self.with_ctx(now, |me, ctx| match me.stack.last_mut() {
                    Some(Page::Library(v)) => v.on_action(a, now, ctx),
                    Some(Page::Detail(v)) => v.on_action(a, now, ctx),
                    Some(Page::Search(v)) => v.on_action(a, now, ctx),
                    Some(Page::Settings(v)) => v.on_action(a, ctx),
                    None => me.home.as_mut().is_some_and(|h| h.on_action(a, now, ctx)),
                });
                if !used && a == InputAction::Back && !self.stack.is_empty() {
                    self.stack.pop();
                    self.wants_dirty = true;
                    return true;
                }
                self.wants_dirty |= used;
                used
            }
            Screen::Blank => false,
        }
    }

    /// Per-frame bookkeeping: backdrop fades, debounced search, image
    /// requests.
    pub fn update(&mut self, now: Instant) -> bool {
        if !matches!(self.screen, Screen::Browse) {
            return false;
        }
        let tex = &self.textures;
        let mut dirty = match self.stack.last_mut() {
            Some(Page::Detail(v)) => v.update(now, tex),
            Some(_) => false,
            None => self.home.as_mut().is_some_and(|h| h.update(now, tex)),
        };
        dirty |= self.with_ctx(now, |me, ctx| match me.stack.last_mut() {
            Some(Page::Search(v)) => v.update(now, ctx),
            _ => false,
        });
        if self.uploaded && now >= self.wants_at {
            self.uploaded = false;
            self.wants_dirty = true;
        }
        if self.wants_dirty {
            self.wants_dirty = false;
            self.uploaded = false;
            self.wants_at = now + WANTS_AFTER_UPLOAD;
            self.load_work += 1;
            let mut w = Wants::new(&self.textures, self.vp);
            match self.stack.last() {
                Some(Page::Library(v)) => v.wants(&mut w),
                Some(Page::Detail(v)) => v.wants(&mut w),
                Some(Page::Search(v)) => v.wants(&mut w),
                Some(Page::Settings(_)) => {}
                None => {
                    if let Some(h) = &self.home {
                        h.wants(&mut w);
                    }
                }
            }
            let jobs = w.jobs;
            let keys: Vec<u64> = jobs.iter().map(|j| j.key).collect();
            if keys != self.last_wants && self.net.send(Cmd::WantImages(jobs)) {
                self.last_wants = keys;
            }
        }
        dirty
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        if self.textures.uploads_pending() {
            return true;
        }
        if !matches!(self.screen, Screen::Browse) {
            return false;
        }
        match self.stack.last() {
            Some(Page::Library(v)) => v.is_animating(now),
            Some(Page::Detail(v)) => v.is_animating(now),
            Some(Page::Search(v)) => v.is_animating(now),
            Some(Page::Settings(_)) => false,
            None => self.home.as_ref().is_some_and(|h| h.is_animating(now)),
        }
    }

    /// When the loop must wake even without input (debounced search).
    pub fn next_deadline(&self) -> Option<Instant> {
        match self.stack.last() {
            _ if self.uploaded => Some(self.wants_at),
            Some(Page::Search(v)) if matches!(self.screen, Screen::Browse) => v.next_deadline(),
            _ => None,
        }
    }

    /// True once there is nothing left to upload (for benchmarks/startup).
    /// For `--bench-scroll`: (the home screen is showing, a library grid
    /// is open with its first page loaded).
    pub fn bench_state(&self) -> (bool, bool) {
        let browse = matches!(self.screen, Screen::Browse) && self.home.is_some();
        let grid = matches!(self.stack.last(), Some(Page::Library(l)) if l.loaded());
        (browse && self.stack.is_empty(), browse && grid)
    }

    /// Loading work started so far (see `load_work`).
    pub fn load_work(&self) -> u64 {
        self.load_work
    }

    /// Measured time of one texture upload (small, large images), ns.
    pub fn upload_ns(&self) -> [f64; 2] {
        self.textures.upload_ns()
    }

    pub fn settled(&self) -> bool {
        !self.textures.uploads_pending()
    }

    pub fn draw<R: Renderer>(&mut self, r: &mut R, now: Instant, notice: &str) {
        if std::mem::take(&mut self.purge_textures) {
            self.textures.clear(r);
            self.last_wants.clear();
            self.wants_dirty = true;
        }
        self.textures.begin_frame();
        self.last_upload_ms = 0.0;
        if self.textures.uploads_pending() {
            let t = Instant::now();
            if self.textures.pump(r, UPLOAD_BUDGET) {
                // Newly loaded (or evicted) images change what to request.
                self.uploaded = true;
            }
            self.last_upload_ms = t.elapsed().as_secs_f32() * 1000.0;
        }
        let safe = self.vp.safe;
        let (sx, sy) = (self.vp.sx(), self.vp.sy());
        match &mut self.screen {
            Screen::Status(st) => st.draw(r, safe, notice),
            Screen::Entry(e) => e.draw(r, safe, notice),
            Screen::SignIn(f) => f.draw(r, sx, sy),
            Screen::Browse => {
                let caps = self.caps.get().and_then(|c| c.as_ref());
                match self.stack.last_mut() {
                    Some(Page::Library(v)) => v.draw(r, now, &mut self.textures),
                    Some(Page::Detail(v)) => v.draw(r, now, &mut self.textures, caps),
                    Some(Page::Search(v)) => v.draw(r, now, &mut self.textures),
                    Some(Page::Settings(v)) => v.draw(r, now),
                    None => {
                        if let Some(h) = &mut self.home {
                            h.draw(r, now, &mut self.textures, notice);
                        }
                    }
                }
            }
            Screen::Blank => {
                let (lw, lh) = r.logical_size();
                r.clear(theme::BG);
                r.text(
                    "Jellyroll",
                    &TextStyle::new(32.0, Weight::Bold),
                    Point::new(lw * safe, lh * safe),
                    theme::TEXT,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    /// Every UI string renders with the bundled fonts (no missing glyphs:
    /// ★ and → once rendered as blanks).
    #[test]
    fn strings_have_glyphs() {
        let mut shaper = jellyroll_render::text::Shaper::new();
        let style = jellyroll_render::TextStyle::new(20.0, jellyroll_render::Weight::Regular);
        for s in super::strings::ALL.iter().chain(["•", "…", "·"].iter()) {
            let t = shaper.shape(s, &style);
            assert!(t.glyphs.iter().all(|g| g.id != 0), "missing glyph in {s:?}");
        }
    }
}
