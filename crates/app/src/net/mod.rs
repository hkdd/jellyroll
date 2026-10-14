//! The network thread (spec 4): a single-threaded tokio runtime that owns
//! the Jellyfin session, the caches and image fetching. It talks to the
//! render thread only through bounded channels: commands in (`try_send`,
//! never blocks the renderer) and events out (`try_recv` each frame), and
//! wakes the render loop through the platform waker when it sends one.

mod fetch;
pub mod images;
mod reports;
mod session;
pub mod store;

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc;

use jellyfin_api::endpoints::ItemsQuery;

use crate::home::model::{Card, Detail, HomeData, Shape};
pub use images::Finish;

/// Commands from the UI.
#[derive(Debug)]
pub enum Cmd {
    /// Retry after a problem (Select on an error screen).
    Retry,
    /// Connect to this address (typed or picked on the server screen).
    Connect(String),
    /// Look for servers on the LAN.
    Search,
    /// Leave the current server's problem/sign-in screen for the server
    /// screen.
    ChangeServer,
    /// Fetch page data; the answer is `Event::Fetched` with this id.
    Fetch { id: u64, req: Fetch },
    /// Fetch the home screen again (after playback, favorites...).
    Refresh,
    /// Sign out of this server (revokes the token), then sign in again.
    SignOut,
    /// Sign in with a password instead of Quick Connect.
    PasswordSignIn { username: String, password: String },
    /// List the server's public users (for the password sign-in form).
    ListUsers,
    /// Replace the queue of wanted images, most important first. Images
    /// already in flight finish; queued ones not in the new list are
    /// dropped (spec 4: cancel work for rows scrolled away).
    WantImages(Vec<ImageJob>),
    /// A playback report (start/progress/stop), sent in order and retried
    /// until delivered (spec 7.2).
    Report(Box<jellyfin_api::playback::Report>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageJob {
    /// Texture key: identifies source + size + finish.
    pub key: u64,
    pub source: ImageSource,
    pub w: u32,
    pub h: u32,
    pub finish: Finish,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageSource {
    /// Server path with size and tag (`endpoints::image_path`).
    Server(String),
    BlurHash(String),
}

/// What the UI should show while there is no home screen.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Choose a server: type an address or search the LAN. `address`
    /// prefills the field when the screen opens; later updates (search
    /// results, errors) keep whatever the user typed.
    ServerEntry {
        address: String,
        error: Option<String>,
        found: Vec<FoundServer>,
        searching: bool,
    },
    Connecting {
        server: String,
    },
    /// Signing in with Quick Connect: show `code`.
    QuickConnect {
        server: String,
        /// Server base URL, to show where the code is entered.
        url: String,
        code: String,
    },
    /// Something needs the user: a title, an explanation, and whether
    /// Select retries.
    Problem {
        title: String,
        detail: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundServer {
    pub name: String,
    pub address: String,
}

/// Page data requests (M6).
#[derive(Clone, Debug, PartialEq)]
pub enum Fetch {
    /// One page of a library grid (or search results).
    Items {
        query: ItemsQuery,
        shape: Shape,
    },
    /// Index of the first item at or after `letter` in name order.
    LetterIndex {
        query: ItemsQuery,
        letter: char,
    },
    Detail(String),
    Seasons(String),
    Episodes {
        series: String,
        season: String,
    },
    Similar(String),
    /// A person's movies and shows.
    Filmography(String),
    /// What a playlist (true) or collection/folder (false) contains.
    Contents {
        id: String,
        playlist: bool,
    },
    Genres(String),
    SetFavorite {
        id: String,
        on: bool,
    },
    SetPlayed {
        id: String,
        on: bool,
    },
    /// Everything to play an item (PlaybackInfo, chapters, segments,
    /// trickplay, the next episode), from `start_ticks`.
    Play {
        id: String,
        start_ticks: i64,
    },
}

/// What a page shows when a request failed because the server can't be
/// reached (and how the UI recognizes that case).
pub const UNREACHABLE: &str = "Can't reach the server. Retrying automatically…";

#[derive(Debug)]
pub enum Fetched {
    Page { start: u32, total: u32, cards: Vec<Card> },
    Index(u32),
    Detail(Box<Detail>),
    Cards(Vec<Card>),
    Names(Vec<String>),
    UserData { favorite: bool, played: bool },
    Play(Box<PlayFetched>),
    Failed(String),
}

/// The player's data plus how to reach the server (stream URL, external
/// subtitles).
#[derive(Clone, Debug, PartialEq)]
pub struct PlayFetched {
    pub info: crate::home::playinfo::PlayInfo,
    pub base_url: String,
    /// `Authorization` header value for media requests.
    pub authorization: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicUser {
    pub name: String,
    pub has_password: bool,
}

/// Events for the UI.
#[derive(Debug)]
pub enum Event {
    Status(Status),
    /// Home data. `fresh` is false for the cached copy shown at startup.
    Home {
        data: HomeData,
        fresh: bool,
    },
    /// The server can't be reached; the home screen (if shown) is stale.
    Offline,
    Image {
        key: u64,
        w: u32,
        h: u32,
        /// Premultiplied.
        rgba: Vec<u8>,
        opaque: bool,
    },
    ImageFailed {
        key: u64,
    },
    Fetched {
        id: u64,
        result: Fetched,
    },
    /// The server's public users (answer to `ListUsers`).
    Users(Vec<PublicUser>),
    /// Password sign-in failed; the form shows why.
    SignInFailed(String),
}

#[derive(Clone, Debug, Default)]
pub struct NetConfig {
    /// Server address from the config file; empty = the last server used,
    /// else ask the user (server screen).
    pub server_url: String,
    /// Extra CA / self-signed certificate (PEM file).
    pub ca_file: Option<PathBuf>,
    pub image_cache_bytes: u64,
    /// Where state.json lives (`store::data_dir()`); None = nothing is
    /// remembered across runs.
    pub data_dir: Option<PathBuf>,
    /// Metadata and image caches (`store::cache_dir()`); None = no caching.
    pub cache_dir: Option<PathBuf>,
}

pub struct NetHandle {
    cmd: mpsc::Sender<Cmd>,
    events: mpsc::Receiver<Event>,
    #[cfg_attr(not(test), allow(dead_code))]
    thread: std::thread::JoinHandle<()>,
}

impl NetHandle {
    pub fn start(cfg: NetConfig, wake: Arc<dyn Fn() + Send + Sync>) -> std::io::Result<Self> {
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let (ev_tx, ev_rx) = mpsc::channel(256);
        let thread = std::thread::Builder::new().name("net".into()).spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .max_blocking_threads(4)
                .thread_name("net-worker")
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!("network runtime: {e}");
                    return;
                }
            };
            rt.block_on(session::run(cfg, session::Out { tx: ev_tx, wake }, cmd_rx));
            tracing::debug!("network thread exiting");
        })?;
        Ok(Self { cmd: cmd_tx, events: ev_rx, thread })
    }

    /// Stops the network thread and waits for it (it releases the caches'
    /// file locks on exit). Pending blocking work finishes first.
    #[cfg(test)]
    pub fn stop(self) {
        let Self { cmd, events, thread } = self;
        drop(cmd);
        drop(events);
        let _ = thread.join();
    }

    /// Non-blocking; false if the queue is full (the command is dropped;
    /// the UI re-sends image wants as it scrolls anyway).
    pub fn send(&self, c: Cmd) -> bool {
        self.cmd.try_send(c).is_ok()
    }

    pub fn try_recv(&mut self) -> Option<Event> {
        self.events.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use jellyfin_api::endpoints::{ImageType, image_path};
    use jellyroll_mock_server::{MockConfig, QUICK_CONNECT_CODE};

    use super::*;

    fn next(h: &mut NetHandle, deadline: Instant) -> Event {
        loop {
            if let Some(e) = h.try_recv() {
                return e;
            }
            assert!(Instant::now() < deadline, "timed out waiting for a network event");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The whole M5 flow against the mock server: connect, Quick Connect,
    /// home rows, images; then a restart shows the cached home first
    /// (stale-while-revalidate) and needs no sign-in.
    #[test]
    fn session_against_mock_server() {
        let url =
            jellyroll_mock_server::spawn(MockConfig { approve_after: 1, ..Default::default() });
        let dir = std::env::temp_dir().join(format!("jellyroll-net-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cfg = NetConfig {
            server_url: url,
            ca_file: None,
            image_cache_bytes: 16 << 20,
            data_dir: Some(dir.join("data")),
            cache_dir: Some(dir.join("cache")),
        };
        let deadline = Instant::now() + Duration::from_secs(30);

        let mut h = NetHandle::start(cfg.clone(), Arc::new(|| {})).unwrap();
        let mut saw_code = false;
        let data = loop {
            match next(&mut h, deadline) {
                Event::Status(Status::QuickConnect { code, .. }) => {
                    assert_eq!(code, QUICK_CONNECT_CODE);
                    saw_code = true;
                }
                Event::Status(Status::Problem { title, detail }) => panic!("{title}: {detail}"),
                Event::Home { data, fresh } => {
                    assert!(fresh, "no cache on first run");
                    break data;
                }
                _ => {}
            }
        };
        assert!(saw_code, "first run signs in with Quick Connect");
        let titles: Vec<&str> = data.rows.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "My Media",
                "Continue Watching",
                "Next Up",
                "Latest Movies",
                "Latest Shows",
                "Latest Music",
                "Favorites"
            ]
        );
        assert_eq!(data.server_name, "Jellyroll Mock");
        assert_eq!(data.user_name, "demo");

        // Images: a server poster and a BlurHash placeholder.
        let art = data.rows[3].cards[0].image.clone().unwrap();
        assert_eq!(art.kind, ImageType::Primary);
        let path = image_path(&art.item_id, art.kind, &art.tag, 100, 150);
        let hash = art.blurhash.clone().unwrap();
        assert!(h.send(Cmd::WantImages(vec![
            ImageJob {
                key: 1,
                source: ImageSource::Server(path),
                w: 100,
                h: 150,
                finish: Finish::Card { radius: 6.0 }
            },
            ImageJob {
                key: 2,
                source: ImageSource::BlurHash(hash),
                w: 21,
                h: 32,
                finish: Finish::Card { radius: 0.0 }
            },
        ])));
        let mut got = Vec::new();
        while got.len() < 2 {
            match next(&mut h, deadline) {
                Event::Image { key, w, h, rgba, .. } => {
                    assert_eq!(rgba.len(), (w * h * 4) as usize);
                    got.push((key, w, h));
                }
                Event::ImageFailed { key } => panic!("image {key} failed"),
                _ => {}
            }
        }
        got.sort();
        assert_eq!(got, [(1, 100, 150), (2, 21, 32)]);

        // Playback (M7): play data, then a stop report the server records
        // as the resume point.
        let movie = data.rows[3].cards[0].id.clone();
        assert!(
            h.send(Cmd::Fetch { id: 77, req: Fetch::Play { id: movie.clone(), start_ticks: 0 } })
        );
        let pf = loop {
            match next(&mut h, deadline) {
                Event::Fetched { id: 77, result: Fetched::Play(p) } => break p,
                Event::Fetched { id: 77, result } => panic!("{result:?}"),
                _ => {}
            }
        };
        assert_eq!(pf.info.audio.len(), 2);
        assert!(pf.info.subtitles.iter().any(|t| t.external.is_some()));
        assert!(pf.authorization.contains("Token="));
        assert!(h.send(Cmd::Report(Box::new(jellyfin_api::playback::Report {
            kind: jellyfin_api::playback::ReportKind::Stopped,
            item_id: movie.clone(),
            media_source_id: pf.info.media_source_id.clone(),
            play_session_id: pf.info.play_session_id.clone(),
            position_ticks: 1234,
            paused: false,
            audio_stream_index: Some(1),
            subtitle_stream_index: None,
            play_method: pf.info.play_method,
        }))));
        let mut n = 100;
        loop {
            n += 1;
            assert!(h.send(Cmd::Fetch { id: n, req: Fetch::Detail(movie.clone()) }));
            let resume = loop {
                match next(&mut h, deadline) {
                    Event::Fetched { id, result: Fetched::Detail(d) } if id == n => {
                        break d.resume_ticks;
                    }
                    _ => {}
                }
            };
            if resume == 1234 {
                break;
            }
            assert!(Instant::now() < deadline, "the stop report never arrived");
            std::thread::sleep(Duration::from_millis(50));
        }
        h.stop();
        let cached_images = std::fs::read_dir(dir.join("cache/images")).unwrap().count();
        assert_eq!(cached_images, 1, "server image cached on disk");

        // Restart: cached home immediately, then fresh, no sign-in.
        let mut h = NetHandle::start(cfg, Arc::new(|| {})).unwrap();
        let mut homes = Vec::new();
        while homes.len() < 2 {
            match next(&mut h, deadline) {
                Event::Status(Status::QuickConnect { .. }) => panic!("token should be remembered"),
                Event::Status(Status::Problem { title, detail }) => panic!("{title}: {detail}"),
                Event::Home { data: d, fresh } => homes.push((d, fresh)),
                _ => {}
            }
        }
        assert!(!homes[0].1 && homes[1].1, "cached first, then fresh");
        assert_eq!(homes[0].0, data, "cache holds what was shown");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// No server configured or remembered: the server screen comes first;
    /// a bad address returns to it with an error; a good one signs in;
    /// Back from Quick Connect returns to it.
    #[test]
    fn server_screen_flow() {
        let url =
            jellyroll_mock_server::spawn(MockConfig { approve_after: 1000, ..Default::default() });
        let dir = std::env::temp_dir().join(format!("jellyroll-entry-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cfg = NetConfig {
            server_url: String::new(),
            ca_file: None,
            image_cache_bytes: 1 << 20,
            data_dir: Some(dir.join("data")),
            cache_dir: Some(dir.join("cache")),
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut h = NetHandle::start(cfg, Arc::new(|| {})).unwrap();
        let entry = |h: &mut NetHandle| loop {
            match next(h, deadline) {
                Event::Status(Status::ServerEntry { error, .. }) => return error,
                Event::Status(Status::Problem { title, detail }) => panic!("{title}: {detail}"),
                _ => {}
            }
        };
        assert_eq!(entry(&mut h), None, "server screen first, no automatic search");

        // Nothing listens on port 1: back to the server screen, with why.
        assert!(h.send(Cmd::Connect("127.0.0.1:1".into())));
        let err = entry(&mut h).expect("an error for the bad address");
        assert!(err.contains("Couldn't connect to http://127.0.0.1:1"), "{err}");

        assert!(h.send(Cmd::Connect(url)));
        loop {
            match next(&mut h, deadline) {
                Event::Status(Status::QuickConnect { .. }) => break,
                Event::Status(Status::Problem { title, detail }) => panic!("{title}: {detail}"),
                _ => {}
            }
        }
        assert!(h.send(Cmd::ChangeServer));
        assert_eq!(entry(&mut h), None, "Back from sign-in returns to the server screen");
        h.stop();
        let _ = std::fs::remove_dir_all(dir);
    }
}
