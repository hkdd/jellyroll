//! The home screen fed by the Jellyfin server (spec 10.1, M5): status
//! screens until there is data, then the rows. Owns the GPU textures for
//! artwork and tells the network thread which images it needs.

pub mod model;
mod server_entry;
mod status;
mod textures;
pub mod theme;
mod view;

use std::time::{Duration, Instant};

use jellyroll_render::{Point, Renderer, TextStyle, Weight};

use crate::input::InputAction;
use crate::net::{Cmd, Event, NetHandle, Status};
use server_entry::ServerEntry;
use status::StatusView;
use textures::Textures;
use view::HomeView;

/// Per-frame texture upload budget (spec 9).
const UPLOAD_BUDGET: Duration = Duration::from_millis(2);
/// Network events handled per loop iteration (bounds the work per frame).
const EVENTS_PER_FRAME: usize = 64;

/// What is on screen. The home view itself is kept while other screens show
/// (it is cheap and keeps scroll/focus if the screen returns).
enum Screen {
    Blank,
    Status(StatusView),
    Entry(ServerEntry),
    Home,
}

pub struct HomeApp {
    net: NetHandle,
    screen: Screen,
    view: Option<HomeView>,
    textures: Textures,
    /// Logical width, safe area, pixels per logical pixel.
    viewport: (f32, f32, f32),
    wants_dirty: bool,
    last_wants: Vec<u64>,
}

impl HomeApp {
    pub fn new(net: NetHandle, texture_cap_bytes: u64, fb: (u32, u32), safe_area: f32) -> Self {
        let mut a = Self {
            net,
            screen: Screen::Blank,
            view: None,
            textures: Textures::new(texture_cap_bytes),
            viewport: (1920.0, safe_area, 1.0),
            wants_dirty: false,
            last_wants: Vec::new(),
        };
        a.set_output(fb, safe_area, Instant::now());
        a
    }

    /// Output size or safe area changed. Logical height is always 1080.
    pub fn set_output(&mut self, fb: (u32, u32), safe_area: f32, now: Instant) {
        let scale = fb.1.max(1) as f32 / 1080.0;
        self.viewport = (fb.0 as f32 / scale, safe_area, scale);
        if let Some(v) = &mut self.view {
            self.wants_dirty |= v.set_viewport(self.viewport.0, safe_area, scale, now);
        }
    }

    /// Handles network events. Returns true if the screen changed.
    pub fn poll(&mut self, now: Instant) -> bool {
        let mut dirty = false;
        for _ in 0..EVENTS_PER_FRAME {
            let Some(ev) = self.net.try_recv() else { break };
            match ev {
                Event::Status(Status::ServerEntry { address, error, found, searching }) => {
                    if !matches!(self.screen, Screen::Entry(_)) {
                        self.screen = Screen::Entry(ServerEntry::new(&address));
                    }
                    if let Screen::Entry(e) = &mut self.screen {
                        e.update(error, found, searching);
                    }
                    dirty = true;
                }
                Event::Status(s) => {
                    self.screen = Screen::Status(StatusView::new(s));
                    dirty = true;
                }
                Event::Home { data, fresh } => {
                    let (lw, safe, scale) = self.viewport;
                    match &mut self.view {
                        Some(v) => v.set_data(data, now),
                        None => self.view = Some(HomeView::new(data, lw, safe, scale, now)),
                    }
                    if fresh && let Some(v) = &mut self.view {
                        v.offline = false;
                        self.textures.forget_failures();
                    }
                    self.screen = Screen::Home;
                    self.wants_dirty = true;
                    dirty = true;
                }
                Event::Offline => {
                    if let Some(v) = &mut self.view {
                        dirty |= !v.offline;
                        v.offline = true;
                    }
                }
                Event::Image { key, w, h, rgba } => {
                    self.textures.queue(key, w, h, rgba);
                    dirty = true;
                }
                Event::ImageFailed { key } => self.textures.fail(key),
            }
        }
        dirty
    }

    /// A text field is active: the physical keyboard should type.
    pub fn wants_text(&self) -> bool {
        matches!(self.screen, Screen::Entry(_))
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
            Screen::Status(st) => match a {
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
            Screen::Home => match &mut self.view {
                Some(v) => {
                    let changed = v.on_action(a, now);
                    self.wants_dirty |= changed;
                    changed
                }
                None => false,
            },
            Screen::Blank => false,
        }
    }

    /// Per-frame bookkeeping: backdrop fades, image requests.
    pub fn update(&mut self, now: Instant) -> bool {
        let Some(v) = &mut self.view else { return false };
        let dirty = v.update(now, &self.textures);
        if self.wants_dirty && matches!(self.screen, Screen::Home) {
            self.wants_dirty = false;
            let jobs = v.wants(&self.textures);
            let keys: Vec<u64> = jobs.iter().map(|j| j.key).collect();
            if keys != self.last_wants && self.net.send(Cmd::WantImages(jobs)) {
                self.last_wants = keys;
            }
        }
        dirty
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        self.textures.uploads_pending()
            || (matches!(self.screen, Screen::Home)
                && self.view.as_ref().is_some_and(|v| v.is_animating(now)))
    }

    /// True once there is nothing left to upload (for benchmarks/startup).
    pub fn settled(&self) -> bool {
        !self.textures.uploads_pending()
    }

    pub fn draw<R: Renderer>(&mut self, r: &mut R, now: Instant, notice: &str) {
        self.textures.begin_frame();
        if self.textures.uploads_pending() && self.textures.pump(r, UPLOAD_BUDGET) {
            // Newly loaded (or evicted) images change what to request.
            self.wants_dirty = true;
        }
        let safe = self.viewport.1;
        match (&mut self.screen, &self.view) {
            (Screen::Status(st), _) => st.draw(r, safe, notice),
            (Screen::Entry(e), _) => e.draw(r, safe, notice),
            (Screen::Home, Some(v)) => v.draw(r, now, &mut self.textures, notice),
            _ => {
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
