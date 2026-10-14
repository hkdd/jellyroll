//! The Jellyfin UI (spec 10.1): server and sign-in screens, then browsing:
//! Home at the bottom of a page stack, with Library, Detail, Search and
//! Settings pages pushed on top (Back pops; each page keeps its own focus
//! and scroll). Owns the GPU textures for artwork, tells the network thread
//! which images and data it needs, and hands playback requests to the
//! frame loop.

mod accounts;
mod blade;
mod detail;
mod library;
pub mod model;
mod osk;
mod pages;
pub mod player;
pub mod playinfo;
mod power_menu;
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

use jellyroll_render::{Color, Point, Rect, Renderer, TextStyle, Weight};
use jellyroll_video::va::DecodeCaps;

use crate::config::UiResolution;
use crate::input::InputAction;
use crate::net::{Changed, Cmd, Event, Fetch, Fetched, NetHandle, Remote, Status};
use detail::DetailView;
use library::LibraryView;
use pages::{Change, Ctx, Open};
use search::SearchView;
use server_entry::ServerEntry;
use settings::SettingsView;
use signin::SignInForm;
use status::StatusView;
use strings as s;
use textures::Textures;
use view::HomeView;
use widgets::{Viewport, Wants};

/// Per-frame image upload time (spec 9): the frame's other work at 4K is
/// ~2 ms on the reference HTPC, and the CPU budget is 4 ms (spec 2).
const UPLOAD_BUDGET: Duration = Duration::from_micros(1500);
/// How long a toast stays up.
const TOAST_FOR: Duration = Duration::from_secs(6);
/// Remote commands waiting for the frame loop; more are dropped (a
/// flood from another app must not pile up).
const REMOTE_QUEUE: usize = 32;
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
    /// Who's watching?
    Accounts(accounts::AccountPicker),
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
    /// Initial mpv `aid`/`sid` (from the server's default tracks).
    pub audio_track: Option<String>,
    pub sub_track: Option<String>,
    /// Video frame rate, for refresh-rate matching.
    pub fps: Option<f64>,
    /// Filled from settings when the request is taken (`take_play`).
    pub match_refresh: bool,
    pub audio: AudioOut,
    /// The server says the video is interlaced.
    pub interlaced: bool,
    /// Filled from settings when the request is taken.
    pub video: VideoOut,
}

/// Video processing for a playback (config, then Settings).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VideoOut {
    pub interpolation: bool,
    pub deinterlace: bool,
    pub tone_mapping: String,
}

/// Audio output settings for a playback (config, then Settings).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioOut {
    /// mpv `audio-device`; None: mpv's default.
    pub device: Option<String>,
    /// mpv `audio-spdif` (bitstream formats); None: decode everything.
    pub spdif: Option<String>,
    /// mpv `audio-channels`; None: mpv's default.
    pub channels: Option<String>,
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
    /// mpv's audio outputs (probed once in the background).
    audio_devices: Arc<OnceLock<Vec<crate::audio::AudioDevice>>>,
    /// Device facts for Settings (versions, GPU, display).
    about: Vec<(String, String)>,
    /// Play fetch in flight (request id).
    play_req: Option<u64>,
    /// The player while something plays (M7).
    player: Option<player::PlayerUi>,
    /// Trickplay sheets last requested (texture keys).
    player_wants: Vec<u64>,
    /// A short message over the page (e.g. why something won't play).
    toast: Option<(String, Instant)>,
    /// Remote control from other Jellyfin apps, for the frame loop.
    remote: std::collections::VecDeque<Remote>,
    /// Artwork bytes on disk (Settings shows it).
    cache_used: Option<u64>,
    /// A video is starting (from Play until its first frame): the page
    /// shows dimmed with this title and "Starting…" instead of a black
    /// screen. Empty title: not known yet.
    starting: Option<(String, Instant)>,
    /// Open over everything else (spec 5.5).
    power: Option<power_menu::PowerMenu>,
    /// Exit chosen in the power menu, for the frame loop.
    exit_requested: bool,
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

/// The UI resolution in effect, the other choices made in Settings, the
/// config defaults they override, and where the choices are saved.
pub struct UiSetting {
    pub resolution: UiResolution,
    pub prefs: crate::ui_prefs::UiPrefs,
    pub display: crate::config::Display,
    pub audio: crate::config::Audio,
    pub playback: crate::config::Playback,
    /// config.toml's artwork disk cache size (MB).
    pub image_cache_mb: u64,
    pub save_to: Option<std::path::PathBuf>,
    /// The power menu offers exiting to the console (dev, --allow-exit).
    pub allow_exit: bool,
}

impl UiSetting {
    fn save(&self) {
        if let Some(p) = &self.save_to {
            crate::ui_prefs::save(p, &self.prefs);
        }
    }

    /// Refresh-rate matching at an output resolution ("3840x2160").
    pub fn match_refresh(&self, res: &str) -> bool {
        self.prefs.match_refresh.get(res).copied().unwrap_or(self.display.match_refresh)
    }

    pub fn audio_device(&self) -> String {
        self.prefs.audio_device.clone().unwrap_or_else(|| self.audio.device.clone())
    }

    pub fn passthrough(&self, device: &str) -> bool {
        self.prefs.passthrough.get(device).copied().unwrap_or(self.audio.passthrough)
    }

    pub fn audio_channels(&self) -> String {
        self.prefs.audio_channels.clone().unwrap_or_else(|| self.audio.channels.clone())
    }

    pub fn interpolation(&self) -> bool {
        self.prefs.interpolation.unwrap_or(self.playback.interpolation)
    }

    pub fn deinterlace(&self) -> crate::config::Deinterlace {
        self.prefs.deinterlace.unwrap_or(self.playback.deinterlace)
    }

    pub fn tone_mapping(&self) -> String {
        self.prefs.tone_mapping.clone().unwrap_or_else(|| self.playback.tone_mapping.clone())
    }

    pub fn image_cache_mb(&self) -> u64 {
        self.prefs.image_cache_mb.unwrap_or(self.image_cache_mb)
    }

    pub fn video_out(&self, interlaced: bool) -> VideoOut {
        use crate::config::Deinterlace as D;
        VideoOut {
            interpolation: self.interpolation(),
            deinterlace: match self.deinterlace() {
                D::Auto => interlaced,
                D::Always => true,
                D::Off => false,
            },
            tone_mapping: self.tone_mapping(),
        }
    }

    pub fn audio_out(&self) -> AudioOut {
        let device = self.audio_device();
        let some = |v: String| (!v.is_empty() && v != "auto").then_some(v);
        AudioOut {
            spdif: self.passthrough(&device).then(|| self.audio.passthrough_codecs.clone()),
            channels: some(self.audio_channels()),
            device: some(device),
        }
    }
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
        // mpv's audio outputs, for Settings (probing takes a moment).
        let audio_devices = Arc::new(OnceLock::new());
        let d = audio_devices.clone();
        let _ = std::thread::Builder::new().name("audio-probe".into()).spawn(move || {
            let list = crate::audio::devices().unwrap_or_else(|e| {
                tracing::warn!("{e}");
                Vec::new()
            });
            tracing::info!(n = list.len(), "audio outputs found");
            let _ = d.set(list);
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
            audio_devices,
            about,
            play_req: None,
            player: None,
            player_wants: Vec::new(),
            toast: None,
            remote: Default::default(),
            cache_used: None,
            starting: None,
            power: None,
            exit_requested: false,
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
        // A cache size chosen in Settings wins over config.toml's.
        if let Some(mb) = a.ui.prefs.image_cache_mb {
            a.net.send(Cmd::SetImageCache(mb << 20));
        }
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
            Open::Play { id, start_ticks, tracks, title, .. } => {
                self.starting = Some((title, now));
                let req =
                    self.with_ctx(now, |_, ctx| ctx.fetch(Fetch::Play { id, start_ticks, tracks }));
                self.play_req = Some(req);
                return;
            }
            Open::Library { id, title, collection } => self.with_ctx(now, |_, ctx| {
                Page::Library(Box::new(LibraryView::new(id, title, collection, vp, now, ctx)))
            }),
            Open::Detail { id } => self
                .with_ctx(now, |_, ctx| Page::Detail(Box::new(DetailView::new(id, vp, now, ctx)))),
            Open::Search => Page::Search(Box::new(SearchView::new(vp, now))),
            Open::Settings => {
                self.net.send(Cmd::StorageInfo);
                let (user, server) = self
                    .home
                    .as_ref()
                    .map(|h| (h.data.user_name.clone(), h.data.server_name.clone()))
                    .unwrap_or_default();
                let devices = self.audio_devices.get().cloned().unwrap_or_default();
                let values = self.settings_values();
                Page::Settings(Box::new(SettingsView::new(
                    vp,
                    &user,
                    &server,
                    self.about(),
                    values,
                    devices,
                )))
            }
            Open::Setting(c) => {
                self.apply_setting(c);
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

    /// What Settings shows.
    fn settings_values(&self) -> settings::Values {
        let device = self.ui.audio_device();
        settings::Values {
            resolution: self.resolution_choice(),
            match_refresh: self.ui.match_refresh(&self.output_key()),
            output: self.output_key(),
            passthrough: self.ui.passthrough(&device),
            audio_device: device,
            channels: self.ui.audio_channels(),
            interpolation: self.ui.interpolation(),
            deinterlace: self.ui.deinterlace(),
            tone_mapping: self.ui.tone_mapping(),
            image_cache_mb: self.ui.image_cache_mb(),
            cache_used: self.cache_used,
        }
    }

    fn apply_setting(&mut self, c: Change) {
        tracing::info!(?c, "setting changed");
        match c {
            Change::UiResolution(r) => self.set_resolution(r),
            Change::MatchRefresh(on) => {
                let key = self.output_key();
                self.ui.prefs.match_refresh.insert(key, on);
            }
            Change::AudioDevice(d) => self.ui.prefs.audio_device = Some(d),
            Change::Passthrough(on) => {
                let d = self.ui.audio_device();
                self.ui.prefs.passthrough.insert(d, on);
            }
            Change::Channels(ch) => self.ui.prefs.audio_channels = Some(ch),
            Change::Interpolation(on) => self.ui.prefs.interpolation = Some(on),
            Change::Deinterlace(d) => self.ui.prefs.deinterlace = Some(d),
            Change::ToneMapping(t) => self.ui.prefs.tone_mapping = Some(t),
            Change::ImageCacheMb(mb) => {
                self.ui.prefs.image_cache_mb = Some(mb);
                self.net.send(Cmd::SetImageCache(mb << 20));
            }
            Change::ClearCache => {
                self.net.send(Cmd::ClearCache);
                // Artwork in GPU memory goes too, so everything shown is
                // loaded again.
                self.purge_textures = true;
            }
        }
        self.ui.save();
        let values = self.settings_values();
        if let Some(Page::Settings(v)) = self.stack.last_mut() {
            v.set_values(values);
        }
    }

    fn set_resolution(&mut self, r: UiResolution) {
        let before = self.ui.resolution.render_scale(self.fb.1);
        self.ui.resolution = r;
        self.ui.prefs.ui_resolution = Some(r);
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
                Event::Status(Status::Accounts { accounts, current }) => {
                    self.stack.clear();
                    self.player_wants.clear();
                    self.screen = Screen::Accounts(accounts::AccountPicker::new(accounts, current));
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
                    let back_online = fresh && self.offline;
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
                    if back_online {
                        tracing::info!("server reachable again");
                        self.with_ctx(now, |me, ctx| match me.stack.last_mut() {
                            Some(Page::Library(v)) => v.retry(ctx),
                            Some(Page::Detail(v)) => v.retry(ctx),
                            _ => false,
                        });
                    }
                    self.wants_dirty = true;
                }
                Event::Changed(c) => self.on_changed(&c, now),
                Event::Storage { used } => {
                    self.cache_used = Some(used);
                    let values = self.settings_values();
                    if let Some(Page::Settings(v)) = self.stack.last_mut() {
                        v.set_values(values);
                    }
                }
                Event::Remote(r) => {
                    if self.remote.len() < REMOTE_QUEUE {
                        self.remote.push_back(r);
                    }
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
                    // A request found the server gone: show it at once and
                    // have the session check (it reconnects and refreshes).
                    if matches!(&result, Fetched::Failed(e) if e == crate::net::UNREACHABLE)
                        && !self.offline
                    {
                        tracing::warn!("server unreachable; going offline");
                        self.offline = true;
                        if let Some(h) = &mut self.home {
                            h.offline = true;
                        }
                        self.net.send(Cmd::Retry);
                    }
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

    /// Changes made elsewhere (live updates, spec 8): the home screen and
    /// every open page that shows a changed item load it again.
    fn on_changed(&mut self, c: &Changed, now: Instant) {
        if !matches!(self.screen, Screen::Browse) {
            return;
        }
        self.send_all(vec![Cmd::Refresh]);
        self.with_ctx(now, |me, ctx| {
            for p in me.stack.iter_mut() {
                match p {
                    Page::Library(v) => v.on_changed(c, ctx),
                    Page::Detail(v) => v.on_changed(c, ctx),
                    Page::Search(_) | Page::Settings(_) => false,
                };
            }
        });
    }

    /// A video is starting (Play pressed, until its first frame).
    pub fn is_starting(&self) -> bool {
        self.starting.is_some()
    }

    fn set_starting(&mut self, title: &str, now: Instant) {
        let since = self.starting.as_ref().map_or(now, |s| s.1);
        self.starting = Some((title.to_string(), since));
    }

    /// The next remote command for the frame loop.
    pub fn take_remote(&mut self) -> Option<Remote> {
        self.remote.pop_front()
    }

    /// Carries out a remote command (spec 8): during playback through the
    /// player, otherwise like the remote's keys on the current screen.
    /// `Stop` tells the frame loop to end playback.
    pub fn remote(
        &mut self,
        rc: Remote,
        pb: Option<&mut crate::playback::Playback>,
        now: Instant,
    ) -> player::Outcome {
        let cont = player::Outcome::Continue { redraw: true };
        if let Remote::Message { text, timeout } = rc {
            self.toast = Some((text, now + timeout));
            return cont;
        }
        let pb = pb.filter(|_| self.player.is_some());
        match (rc, pb) {
            (Remote::Input(a), Some(pb)) => self.player_action(a, pb, now),
            (Remote::Play { id, start_ticks }, pb) => {
                // Another item now: stop this one (reported) first.
                let out = match pb {
                    Some(pb) => self.with_ctx(now, |me, ctx| match me.player.as_mut() {
                        Some(pl) => pl.stop(pb, ctx),
                        None => player::Outcome::Stop,
                    }),
                    None => cont,
                };
                if matches!(self.screen, Screen::Browse) {
                    let (media_source, title) = (String::new(), String::new());
                    let tracks = Default::default();
                    self.open(Open::Play { id, media_source, start_ticks, title, tracks }, now);
                }
                out
            }
            (rc, Some(pb)) => {
                let out = self.with_ctx(now, |me, ctx| match me.player.as_mut() {
                    Some(pl) => pl.remote(&rc, pb, now, ctx),
                    None => player::Outcome::Stop,
                });
                self.update_player_wants();
                out
            }
            (Remote::Input(a), None) => {
                self.on_action(a, now);
                cont
            }
            (Remote::Text(t), None) => {
                for c in t.chars() {
                    self.on_action(InputAction::TextInput(c), now);
                }
                cont
            }
            // Playback commands with nothing playing.
            (_, None) => cont,
        }
    }

    fn fetched(&mut self, id: u64, result: Fetched, now: Instant) -> bool {
        if self.play_req == Some(id) {
            self.play_req = None;
            match result {
                Fetched::Play(p) => {
                    let (ui, req) = player::PlayerUi::new(*p, now);
                    self.set_starting(&req.title, now);
                    self.player = Some(ui);
                    self.play = Some(req);
                }
                Fetched::Failed(e) => {
                    self.starting = None;
                    tracing::warn!("can't play: {e}");
                    self.toast = Some((e, now + TOAST_FOR));
                }
                _ => {}
            }
            return true;
        }
        if let Some(pl) = &mut self.player
            && pl.on_fetched(id, &result)
        {
            return true;
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
        let mut req = self.play.take()?;
        req.match_refresh = self.ui.match_refresh(&self.output_key());
        req.audio = self.ui.audio_out();
        req.video = self.ui.video_out(req.interlaced);
        Some(req)
    }

    /// The output resolution, as refresh-matching choices are keyed.
    fn output_key(&self) -> String {
        format!("{}x{}", self.fb.0, self.fb.1)
    }

    /// Something is playing through the player UI.
    pub fn has_player(&self) -> bool {
        self.player.is_some()
    }

    /// Input during playback.
    pub fn player_action(
        &mut self,
        a: InputAction,
        pb: &mut crate::playback::Playback,
        now: Instant,
    ) -> player::Outcome {
        let out = self.with_ctx(now, |me, ctx| match me.player.as_mut() {
            Some(pl) => pl.on_action(a, pb, now, ctx),
            None => player::Outcome::Stop,
        });
        self.update_player_wants();
        out
    }

    /// Per-frame playback work (reports, countdowns, the end). On `Next`,
    /// `take_play` has the next episode's request.
    pub fn player_tick(
        &mut self,
        pb: &mut crate::playback::Playback,
        now: Instant,
    ) -> player::Tick {
        let t = self.with_ctx(now, |me, ctx| match me.player.as_mut() {
            Some(pl) => pl.tick(pb, now, ctx),
            None => player::Tick::Finished,
        });
        if pb.started {
            // The first frame is on screen.
            self.starting = None;
        }
        if let player::Tick::Next(n) = &t {
            let (ui, req) = player::PlayerUi::new((**n).clone(), now);
            tracing::info!(title = %req.title, "playing the next episode");
            self.set_starting(&req.title, now);
            self.player = Some(ui);
            self.play = Some(req);
        }
        self.update_player_wants();
        t
    }

    fn update_player_wants(&mut self) {
        let Some(pl) = &self.player else { return };
        let jobs = pl.wants();
        let keys: Vec<u64> =
            jobs.iter().filter(|j| !self.textures.known(j.key)).map(|j| j.key).collect();
        if keys != self.player_wants {
            let jobs = jobs.into_iter().filter(|j| keys.contains(&j.key)).collect();
            if self.net.send(Cmd::WantImages(jobs)) {
                self.player_wants = keys;
            }
        }
    }

    pub fn player_overlay_visible(
        &self,
        now: Instant,
        st: &jellyroll_video::PlaybackState,
    ) -> bool {
        self.player.as_ref().is_some_and(|p| p.overlay_visible(now, st))
            || self.textures.uploads_pending()
            || self.toast.as_ref().is_some_and(|t| now < t.1)
    }

    pub fn player_deadline(&self, now: Instant) -> Option<Instant> {
        let toast = self.toast.as_ref().map(|t| t.1).filter(|t| *t > now);
        match (self.player.as_ref().and_then(|p| p.next_deadline(now)), toast) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Draws the player's overlay (and uploads any trickplay sheets).
    pub fn draw_player<R: Renderer>(
        &mut self,
        r: &mut R,
        st: &jellyroll_video::PlaybackState,
        now: Instant,
        safe_area: f32,
    ) {
        if self.textures.uploads_pending() {
            let _upload = tracing::trace_span!("upload").entered();
            self.textures.pump(r, UPLOAD_BUDGET);
        }
        self.textures.begin_frame();
        if let Some(pl) = &mut self.player {
            pl.draw(r, &mut self.textures, st, now, safe_area);
        }
        self.draw_toast(r, now, safe_area, true);
    }

    /// The app is quitting (systemd stop, Exit in the power menu): report
    /// where playback stopped, and give the network thread a moment to
    /// save it, so the server (now or at the next start) has it.
    pub fn shutdown(&mut self, pb: Option<&mut crate::playback::Playback>, now: Instant) {
        if let (Some(pb), true) = (pb, self.player.is_some()) {
            self.with_ctx(now, |me, ctx| {
                if let Some(pl) = me.player.as_mut() {
                    pl.stop(pb, ctx);
                }
            });
        }
        if !self.net.flush(std::time::Duration::from_secs(1)) {
            tracing::warn!("network thread didn't confirm before exit");
        }
    }

    /// Playback ended: refresh what it changed (resume points, watched).
    pub fn playback_finished(&mut self, now: Instant) {
        self.player = None;
        self.starting = None;
        self.player_wants.clear();
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
        if let Some(m) = &mut self.power {
            match m.on_action(a) {
                power_menu::Outcome::Stay => {}
                power_menu::Outcome::Close => self.power = None,
                power_menu::Outcome::Exit => self.exit_requested = true,
            }
            return true;
        }
        let used = self.screen_action(a, now);
        // Back with nothing left to go back to: the power menu.
        if !used && a == InputAction::Back {
            self.power = Some(power_menu::PowerMenu::new(self.ui.allow_exit));
            return true;
        }
        used
    }

    /// Exit to the console was chosen (dev only).
    pub fn take_exit(&mut self) -> bool {
        std::mem::take(&mut self.exit_requested)
    }

    fn screen_action(&mut self, a: InputAction, now: Instant) -> bool {
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
            Screen::Accounts(p) => {
                let (used, cmd) = p.on_action(a);
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
                    // Already home: back to the top (the nav bar).
                    InputAction::Home => {
                        if let Some(h) = &mut self.home {
                            h.go_to_top(now);
                        }
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
                    Some(Page::Settings(v)) => v.on_action(a, now, ctx),
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
        let mut devices_arrived = self.power.as_mut().is_some_and(|m| m.update());
        if let (Some(Page::Settings(v)), Some(list)) =
            (self.stack.last_mut(), self.audio_devices.get())
            && !v.has_devices()
            && !list.is_empty()
        {
            v.set_devices(list.clone());
            devices_arrived = true;
        }
        let mut dirty = devices_arrived
            || match self.stack.last_mut() {
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
        if self.textures.uploads_pending() || self.starting.is_some() {
            return true;
        }
        if !matches!(self.screen, Screen::Browse) {
            return false;
        }
        match self.stack.last() {
            Some(Page::Library(v)) => v.is_animating(now),
            Some(Page::Detail(v)) => v.is_animating(now),
            Some(Page::Search(v)) => v.is_animating(now),
            Some(Page::Settings(v)) => v.is_animating(now),
            None => self.home.as_ref().is_some_and(|h| h.is_animating(now)),
        }
    }

    /// When the loop must wake even without input (debounced search).
    pub fn next_deadline(&self) -> Option<Instant> {
        let page = match self.stack.last() {
            _ if self.uploaded => Some(self.wants_at),
            Some(Page::Search(v)) if matches!(self.screen, Screen::Browse) => v.next_deadline(),
            _ => None,
        };
        // The toast's end needs a redraw too, as does a power request's
        // answer.
        let now = Instant::now();
        let toast = self.toast.as_ref().map(|t| t.1);
        let toast = match (toast, self.power.as_ref().and_then(|m| m.next_deadline(now))) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        match (page, toast) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
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
            let _upload = tracing::trace_span!("upload").entered();
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
            Screen::Accounts(p) => p.draw(r),
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
        if let Some((title, since)) = &self.starting {
            draw_starting(r, title, now - *since);
        }
        self.draw_toast(r, now, safe, false);
        if let Some(m) = &self.power {
            m.draw(r);
        }
    }

    /// A short message near the bottom; over video (remote messages) at
    /// the top, clear of the player's controls.
    fn draw_toast<R: Renderer>(&mut self, r: &mut R, now: Instant, safe: f32, top: bool) {
        if let Some((msg, until)) = &self.toast {
            if now < *until {
                let (lw, lh) = r.logical_size();
                let style = TextStyle::new(24.0, Weight::Bold).max_width(lw * 0.6);
                let m = r.measure_text(msg, &style);
                let rect = Rect::new(
                    (lw - m.width) / 2.0 - 24.0,
                    if top { lh * safe + 40.0 } else { lh - lh * safe - m.height - 60.0 },
                    m.width + 48.0,
                    m.height + 28.0,
                );
                r.fill_rrect(rect, 12.0, Color::rgba(14, 16, 22, 240));
                r.text(msg, &style, Point::new(rect.x + 24.0, rect.y + 14.0), theme::NOTICE);
            } else {
                self.toast = None;
            }
        }
    }
}

/// Over the page while a video starts: dimmed, with the title, a spinner
/// and "Starting…" (the TV may be switching refresh rate, mpv opening the
/// stream).
fn draw_starting<R: Renderer>(r: &mut R, title: &str, elapsed: Duration) {
    let (lw, lh) = r.logical_size();
    // Fade in over 150 ms, so a quick start doesn't flash.
    let a = (elapsed.as_secs_f32() / 0.15).min(1.0);
    r.fill_rect(Rect::new(0.0, 0.0, lw, lh), Color::rgba(0, 0, 0, (200.0 * a) as u8));
    let (cx, cy) = (lw / 2.0, lh / 2.0);
    // Eight dots around a circle, the lit one going round once a second.
    let turn = elapsed.as_secs_f32() % 1.0;
    for i in 0..8 {
        let ang = i as f32 / 8.0 * std::f32::consts::TAU;
        let (x, y) = (cx + 34.0 * ang.sin(), cy - 60.0 - 34.0 * ang.cos());
        let behind = (turn * 8.0 - i as f32).rem_euclid(8.0);
        let alpha = (1.0 - behind / 8.0) * a;
        r.fill_rrect(Rect::new(x - 6.0, y - 6.0, 12.0, 12.0), 6.0, theme::ACCENT.with_alpha(alpha));
    }
    let style = TextStyle::new(44.0, Weight::Bold).max_width(lw * 0.8);
    let mut y = cy + 10.0;
    if !title.is_empty() {
        let m = r.measure_text(title, &style);
        r.text(title, &style, Point::new((lw - m.width) / 2.0, y), theme::TEXT.with_alpha(a));
        y += m.height + 16.0;
    }
    let sub = TextStyle::new(26.0, Weight::Regular);
    let m = r.measure_text(s::STARTING, &sub);
    r.text(s::STARTING, &sub, Point::new((lw - m.width) / 2.0, y), theme::TEXT_DIM.with_alpha(a));
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
