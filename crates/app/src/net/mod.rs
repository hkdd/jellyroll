//! The network thread (spec 4): a single-threaded tokio runtime that owns
//! the Jellyfin session, the caches and image fetching. It talks to the
//! render thread only through bounded channels: commands in (`try_send`,
//! never blocks the renderer) and events out (`try_recv` each frame), and
//! wakes the render loop through the platform waker when it sends one.

mod fetch;
pub mod images;
mod reports;
mod session;
pub mod socket;
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
    /// screen (also "Add server" on the account picker).
    ChangeServer,
    /// Show the account picker (Settings, "Switch user"); nobody is
    /// signed out.
    SwitchUser,
    /// Continue as this saved account (the account picker).
    PickAccount { server_id: String, user_id: String },
    /// Sign in another user on the current server, keeping the others.
    AddUser,
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
    /// Stop the server's transcode for this play session (D71).
    StopTranscode { play_session_id: String },
    /// Report the artwork cache's use (`Event::Storage`).
    StorageInfo,
    /// A new artwork disk cache size in bytes (Settings).
    SetImageCache(u64),
    /// Delete cached artwork and saved pages (Settings), then reload.
    ClearCache,
    /// Answers once every command sent before it has been handled (a
    /// report queued is a report saved to disk): for a clean exit.
    Flush(std::sync::mpsc::Sender<()>),
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
    /// Who's watching? The signed-in accounts on this device (spec 8:
    /// user picker on startup), and the one in use when switching.
    Accounts {
        accounts: Vec<Account>,
        current: Option<usize>,
    },
}

/// A saved, signed-in user on a server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub server_id: String,
    pub server_name: String,
    pub user_id: String,
    pub user_name: String,
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
    /// The years present in a library (of these item types).
    Years {
        parent: String,
        include_types: Option<String>,
    },
    SetFavorite {
        id: String,
        on: bool,
    },
    SetPlayed {
        id: String,
        on: bool,
    },
    /// Everything to play an item (PlaybackInfo, chapters, segments,
    /// trickplay, the next episode), from `start_ticks`, with the tracks
    /// chosen on the item page.
    Play {
        id: String,
        start_ticks: i64,
        tracks: TrackChoice,
        /// Have the server transcode (the user chose it, D71).
        transcode: bool,
    },
}

/// Tracks chosen before Play, as server stream indexes; None keeps the
/// server's default for the user. A subtitle of -1 means off.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrackChoice {
    pub audio: Option<i32>,
    pub subtitle: Option<i32>,
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

/// What changed on the server (spec 8: live updates), gathered over a
/// moment.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Changed {
    /// Items whose data changed (ids as `socket::norm_id` writes them).
    pub items: std::collections::BTreeSet<String>,
    /// Items were added or removed, or the socket was down a while:
    /// anything may have changed.
    pub library: bool,
}

/// Remote control from another Jellyfin app (spec 8).
#[derive(Clone, Debug, PartialEq)]
pub enum Remote {
    /// As if pressed on the remote.
    Input(crate::input::InputAction),
    /// Typed into the active text field.
    Text(String),
    SetPaused(bool),
    /// Seek to this many seconds.
    Seek(f64),
    Stop,
    Play {
        id: String,
        start_ticks: i64,
    },
    /// Jellyfin stream index; -1 turns subtitles off.
    AudioStream(i32),
    SubtitleStream(i32),
    /// Show a message for a while.
    Message {
        text: String,
        timeout: std::time::Duration,
    },
}

/// Events for the UI.
#[derive(Debug)]
pub enum Event {
    Status(Status),
    /// Changes made elsewhere: refresh what shows them.
    Changed(Changed),
    /// Bytes of artwork in the disk cache.
    Storage {
        used: u64,
    },
    Remote(Remote),
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

    /// Waits (up to `timeout`) until the network thread has handled every
    /// command sent so far. False if it didn't answer in time.
    pub fn flush(&self, timeout: std::time::Duration) -> bool {
        let (tx, rx) = std::sync::mpsc::channel();
        self.send(Cmd::Flush(tx)) && rx.recv_timeout(timeout).is_ok()
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

    /// Calls the mock's test hooks (it closes after one response).
    fn mock_call(url: &str, method: &str, path: &str, body: &str) -> String {
        use std::io::{Read, Write};
        let addr = url.trim_start_matches("http://");
        let mut c = std::net::TcpStream::connect(addr).unwrap();
        write!(
            c,
            "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut out = String::new();
        c.read_to_string(&mut out).unwrap();
        out
    }

    /// Live updates and remote control (spec 8): once signed in the socket
    /// opens and capabilities are posted; remote commands arrive at once,
    /// data changes gathered.
    #[test]
    fn live_updates_against_mock_server() {
        let url =
            jellyroll_mock_server::spawn(MockConfig { approve_after: 1, ..Default::default() });
        let cfg = NetConfig {
            server_url: url.clone(),
            ca_file: None,
            image_cache_bytes: 1 << 20,
            data_dir: None,
            cache_dir: None,
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut h = NetHandle::start(cfg, Arc::new(|| {})).unwrap();
        while !matches!(next(&mut h, deadline), Event::Home { .. }) {}
        // Keep pushing a pause until a socket was open to hear it.
        let pause = r#"{"MessageType":"Playstate","Data":{"Command":"Pause"}}"#;
        while !mock_call(&url, "POST", "/Mock/Send", pause).contains(r#""Sockets":1"#) {
            assert!(Instant::now() < deadline, "socket never opened");
            std::thread::sleep(Duration::from_millis(20));
        }
        let caps = mock_call(&url, "GET", "/Mock/Capabilities", "");
        assert!(caps.contains(r#""SupportsMediaControl":true"#), "{caps}");
        assert!(caps.contains(r#""PlayableMediaTypes":["Video"]"#), "{caps}");
        loop {
            match next(&mut h, deadline) {
                Event::Remote(r) => {
                    assert_eq!(r, Remote::SetPaused(true));
                    break;
                }
                Event::Changed(_) | Event::Home { .. } | Event::Image { .. } => {}
                e => panic!("unexpected {e:?}"),
            }
        }
        let changed = format!(
            r#"{{"MessageType":"UserDataChanged","Data":{{"UserId":"{}",
               "UserDataList":[{{"ItemId":"ABCD-0123","Played":true}}]}}}}"#,
            jellyroll_mock_server::USER_ID
        );
        mock_call(&url, "POST", "/Mock/Send", &changed);
        let lib = r#"{"MessageType":"LibraryChanged","Data":{"ItemsAdded":["n1"]}}"#;
        mock_call(&url, "POST", "/Mock/Send", lib);
        let c = loop {
            if let Event::Changed(c) = next(&mut h, deadline) {
                break c;
            }
        };
        // One burst, one event.
        assert!(c.items.contains("abcd0123") && c.items.contains("n1"), "{c:?}");
        assert!(c.library);
    }

    /// Several users (spec 8): add a second user without signing the
    /// first out, a picker at the next start, switching, and signing out
    /// one of them.
    #[test]
    fn accounts_against_mock_server() {
        let url =
            jellyroll_mock_server::spawn(MockConfig { approve_after: 1, ..Default::default() });
        let dir = std::env::temp_dir().join(format!("jellyroll-acct-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cfg = NetConfig {
            server_url: url,
            ca_file: None,
            image_cache_bytes: 1 << 20,
            data_dir: Some(dir.join("data")),
            cache_dir: Some(dir.join("cache")),
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        let home = |h: &mut NetHandle| loop {
            match next(h, deadline) {
                Event::Home { data, fresh: true } => return data.user_name,
                Event::Status(Status::Problem { title, detail }) => panic!("{title}: {detail}"),
                _ => {}
            }
        };
        let accounts = |h: &mut NetHandle| loop {
            if let Event::Status(Status::Accounts { accounts, current }) = next(h, deadline) {
                let names: Vec<String> = accounts.iter().map(|a| a.user_name.clone()).collect();
                return (names, current);
            }
        };

        let mut h = NetHandle::start(cfg.clone(), Arc::new(|| {})).unwrap();
        assert_eq!(home(&mut h), "demo", "Quick Connect signs in demo");
        // Settings → Switch user → Add user, with a password this time.
        h.send(Cmd::SwitchUser);
        assert_eq!(accounts(&mut h), (vec!["demo".to_string()], Some(0)));
        h.send(Cmd::AddUser);
        loop {
            if let Event::Status(Status::QuickConnect { .. }) = next(&mut h, deadline) {
                break;
            }
        }
        h.send(Cmd::PasswordSignIn { username: "kid".into(), password: String::new() });
        assert_eq!(home(&mut h), "kid");
        drop(h);

        // Next start: who's watching?
        let mut h = NetHandle::start(cfg.clone(), Arc::new(|| {})).unwrap();
        let (names, current) = accounts(&mut h);
        assert_eq!(names, ["demo", "kid"]);
        assert_eq!(current, Some(1), "kid was watching last");
        h.send(Cmd::PickAccount {
            server_id: jellyroll_mock_server::SERVER_ID.into(),
            user_id: jellyroll_mock_server::USER_ID.into(),
        });
        assert_eq!(home(&mut h), "demo");
        // Signing demo out leaves kid: the picker again.
        h.send(Cmd::SignOut);
        assert_eq!(accounts(&mut h).0, ["kid"]);
        h.send(Cmd::PickAccount {
            server_id: jellyroll_mock_server::SERVER_ID.into(),
            user_id: jellyroll_mock_server::KID_ID.into(),
        });
        assert_eq!(home(&mut h), "kid");
        drop(h);
        let _ = std::fs::remove_dir_all(dir);
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
        assert!(h.send(Cmd::Fetch {
            id: 77,
            req: Fetch::Play {
                id: movie.clone(),
                start_ticks: 0,
                tracks: Default::default(),
                transcode: false,
            }
        }));
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
            user_id: String::new(),
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
