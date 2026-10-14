//! A mock Jellyfin server (spec 13) for deterministic development and CI:
//! canned JSON for a synthetic library (any size, for the M6 scroll
//! benchmark), generated artwork at the requested size, Quick Connect that
//! approves itself after a few polls, token checks (401 without one) and
//! LAN discovery.
//!
//! It implements only the endpoints jellyroll calls, with the field names
//! and shapes of the Jellyfin API version in docs/VERSIONS.md. Minimal
//! HTTP/1.1: one request per connection.
//!
//! Playback (M7): with `media` set, every movie and episode streams that
//! file (byte ranges, so mpv can seek) and lasts `media_secs`; items have
//! chapters, trickplay tiles, intro/credits segments, two audio tracks and
//! an external subtitle. Playback reports are recorded: they update resume
//! positions and `GET /Mock/Reports` lists them (tests assert on them).
//!
//! WebSocket (`/socket`, spec 8): like the server, it sends `ForceKeepAlive`
//! on connect and answers `KeepAlive`; favorite/watched changes are pushed
//! as `UserDataChanged`. `POST /Mock/Send` pushes its body (a message such
//! as `{"MessageType":"Playstate","Data":{"Command":"Pause"}}`) to every
//! open socket: remote control from the command line, and for tests.
//! `POST /Sessions/Capabilities/Full` is recorded (`GET /Mock/Capabilities`).
//! `GET /Mock/Requests` lists every request seen ("METHOD /path").

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::broadcast;

pub const TOKEN: &str = "mock-token";
/// A description long enough to be cut on the detail page (its More).
const LONG_OVERVIEW: &str = "A lighthouse keeper on a remote island finds a radio that \
    receives tomorrow's weather, and slowly realizes the storms it forecasts are not \
    natural. As the village below grows dependent on the warnings, she must decide \
    whether to keep listening, to share what she hears, or to break the radio before \
    it breaks her. Along the way an old friend returns, a ship goes missing, and the \
    island's oldest story turns out to be a set of instructions. Filmed over three \
    winters on the north coast, with a score recorded inside the lighthouse itself.";

pub const SERVER_ID: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f0";
pub const USER_ID: &str = "5e7a1c0de5e7a1c0de5e7a1c0de5e7a1";
/// A second user ("kid", no password; password sign-in only), for
/// switching users. Sees the same library.
pub const KID_ID: &str = "c0ffee00c0ffee00c0ffee00c0ffee00";
pub const KID_TOKEN: &str = "mock-token-kid";
pub const QUICK_CONNECT_CODE: &str = "424242";

#[derive(Clone, Debug)]
pub struct MockConfig {
    pub server_name: String,
    pub version: String,
    /// Movies in the Movies library (series and albums scale with it).
    pub movies: usize,
    /// Quick Connect reports "authenticated" after this many polls.
    pub approve_after: u32,
    /// Added to every response, to see loading states.
    pub latency: Duration,
    pub quick_connect_enabled: bool,
    /// A video file every movie and episode streams (M7).
    pub media: Option<std::path::PathBuf>,
    /// Its duration in seconds (items report this runtime).
    pub media_secs: u32,
    /// False: PlaybackInfo says files can't be sent as they are, and only
    /// a transcode request gets a stream (D71 testing).
    pub direct_play: bool,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            server_name: "Jellyroll Mock".into(),
            version: "10.11.0".into(),
            movies: 40,
            approve_after: 2,
            latency: Duration::ZERO,
            quick_connect_enabled: true,
            media: None,
            media_secs: 60,
            direct_play: true,
        }
    }
}

struct Server {
    cfg: MockConfig,
    lib: Library,
    polls: AtomicU32,
    /// Requests seen, by path (tests assert on them).
    log: Mutex<Vec<String>>,
    /// Playback reports received, in order.
    reports: Mutex<Vec<Value>>,
    /// Resume positions from the last progress/stopped report, by item.
    positions: Mutex<HashMap<String, i64>>,
    /// The `media` file, read on first use.
    media: Mutex<Option<Arc<Vec<u8>>>>,
    /// Messages for every open WebSocket.
    sockets: broadcast::Sender<String>,
    /// The last capabilities a client posted.
    capabilities: Mutex<Value>,
}

impl Server {
    fn new(cfg: MockConfig, lib: Library) -> Self {
        Self {
            lib,
            cfg,
            polls: AtomicU32::new(0),
            log: Mutex::default(),
            reports: Mutex::default(),
            positions: Mutex::default(),
            media: Mutex::default(),
            sockets: broadcast::channel(64).0,
            capabilities: Mutex::new(Value::Null),
        }
    }

    /// Sends a message to every open socket.
    fn push(&self, msg: Value) {
        let _ = self.sockets.send(msg.to_string());
    }
}

// ---- synthetic library -------------------------------------------------------

#[derive(Clone)]
struct Item {
    json: Value,
}

struct Library {
    views: Vec<Value>,
    movies: Vec<Item>,
    series: Vec<Item>,
    episodes: Vec<Item>,
    albums: Vec<Item>,
    people: Vec<Item>,
}

const MOVIE_TITLES: [&str; 12] = [
    "The Long Voyage Home",
    "千と千尋の神隠し",
    "Midnight in the Archive",
    "Северный ветер",
    "Arrival",
    "Glass Onion",
    "The Quiet Earth",
    "Paprika",
    "Dune: Part Two",
    "Moon",
    "Heat",
    "Everything Everywhere All at Once",
];
const SHOW_TITLES: [&str; 6] = ["Severance", "Andor", "Shōgun", "The Bear", "Dark", "Slow Horses"];
const GENRES: [&str; 6] =
    ["Drama", "Science Fiction", "Comedy", "Thriller", "Animation", "Documentary"];
const PEOPLE: [(&str, &str); 4] = [
    ("Amy Adams", "Louise Banks"),
    ("Jeremy Renner", "Ian Donnelly"),
    ("Forest Whitaker", "Colonel Weber"),
    ("Denis Villeneuve", "Director"),
];

fn id(kind: u8, n: usize) -> String {
    format!("{kind:02x}{n:030x}")
}

/// Deterministic per-id color, also used for the BlurHash so placeholder
/// and artwork match.
fn color(id: &str) -> [u8; 3] {
    let mut h: u32 = 2166136261;
    for b in id.bytes() {
        h = (h ^ b as u32).wrapping_mul(16777619);
    }
    let hue = (h % 360) as f32;
    hsv(hue, 0.55, 0.75)
}

fn hsv(h: f32, s: f32, v: f32) -> [u8; 3] {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [((r + m) * 255.0) as u8, ((g + m) * 255.0) as u8, ((b + m) * 255.0) as u8]
}

const BASE83: &[u8] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz#$%*+,-.:;=?@[]^_{|}~";

/// A one-component BlurHash of a solid color.
fn blurhash(c: [u8; 3]) -> String {
    let mut v = ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32;
    let mut s = [b'0'; 6];
    for i in (2..6).rev() {
        s[i] = BASE83[(v % 83) as usize];
        v /= 83;
    }
    String::from_utf8(s.to_vec()).unwrap_or_default()
}

fn images(id: &str, kinds: &[&str]) -> (Value, Value) {
    let tag = format!("t{}", &id[id.len() - 6..]);
    let hash = blurhash(color(id));
    let mut tags = serde_json::Map::new();
    let mut hashes = serde_json::Map::new();
    for k in kinds {
        tags.insert((*k).into(), json!(tag));
        hashes.insert((*k).into(), json!({ tag.clone(): hash }));
    }
    hashes.insert("Backdrop".into(), json!({ tag.clone(): hash }));
    (Value::Object(tags), Value::Object(hashes))
}

impl Library {
    /// `runtime`: every playable item's length in ticks (the `media` file).
    fn new(movies: usize, runtime: Option<i64>) -> Self {
        let views = vec![
            view("Movies", "movies", 1),
            view("Shows", "tvshows", 2),
            view("Music", "music", 3),
        ];
        let movies: Vec<Item> = (0..movies)
            .map(|n| {
                let i = id(0x10, n);
                let (tags, hashes) = images(&i, &["Primary", "Thumb"]);
                let rating = ["PG", "PG-13", "R"][n % 3];
                let title = MOVIE_TITLES[n % MOVIE_TITLES.len()];
                let name = if n < MOVIE_TITLES.len() { title.to_string() } else { format!("{title} {}", n / MOVIE_TITLES.len() + 1) };
                Item {
                    json: json!({
                        "Id": i, "Name": name, "Type": "Movie", "MediaType": "Video", "IsFolder": false,
                        "ProductionYear": 1990 + (n % 35) as i32, "OfficialRating": rating,
                        "CommunityRating": 6.0 + (n % 30) as f64 / 10.0,
                        "RunTimeTicks": runtime.unwrap_or((85 + (n % 60) as i64) * 600_000_000),
                        "Overview": if n % 2 == 0 {
                            format!("Synthetic movie number {n} from the mock server. {}", LONG_OVERVIEW)
                        } else {
                            format!("Synthetic movie number {n} from the mock server.")
                        },
                        "SortName": name.to_lowercase(),
                        "Genres": [GENRES[n % GENRES.len()], GENRES[(n + 3) % GENRES.len()]],
                        "DateCreated": format!("2024-01-01T00:00:{:02}Z", n % 60),
                        "PrimaryImageAspectRatio": 0.6667,
                        "ImageTags": tags, "ImageBlurHashes": hashes, "BackdropImageTags": [format!("b{n}")],
                        "UserData": { "Played": n % 4 == 1, "PlaybackPositionTicks": 0, "PlayCount": 0, "IsFavorite": n % 7 == 0 },
                    }),
                }
            })
            .collect();
        let series_count = (movies.len() / 4).max(SHOW_TITLES.len());
        let series: Vec<Item> = (0..series_count)
            .map(|n| {
                let i = id(0x20, n);
                let (tags, hashes) = images(&i, &["Primary", "Thumb"]);
                Item {
                    json: json!({
                        "Id": i, "Name": SHOW_TITLES[n % SHOW_TITLES.len()], "Type": "Series", "IsFolder": true,
                        "ProductionYear": 2015 + (n % 10) as i32, "OfficialRating": "TV-MA",
                        "Overview": "A synthetic series.", "PrimaryImageAspectRatio": 0.6667,
                        "ImageTags": tags, "ImageBlurHashes": hashes, "BackdropImageTags": [format!("sb{n}")],
                        "UserData": { "Played": false, "PlaybackPositionTicks": 0, "PlayCount": 0, "IsFavorite": n % 3 == 0, "UnplayedItemCount": 8 },
                    }),
                }
            })
            .collect();
        let episodes: Vec<Item> = series
            .iter()
            .enumerate()
            .flat_map(|(s, se)| {
                let sid = se.json["Id"].as_str().unwrap_or_default().to_string();
                let sname = se.json["Name"].clone();
                let sbd = se.json["BackdropImageTags"].clone();
                (0..8).map(move |e| {
                    let i = id(0x30, s * 100 + e);
                    let (tags, hashes) = images(&i, &["Primary"]);
                    // Two seasons of four.
                    let season = e / 4 + 1;
                    Item {
                        json: json!({
                            "Id": i, "Name": format!("Chapter {}", e + 1), "Type": "Episode", "MediaType": "Video",
                            "SeriesName": sname, "SeriesId": sid, "SeriesPrimaryImageTag": format!("t{}", &sid[sid.len() - 6..]),
                            "SeasonId": format!("{sid}s{season}"),
                            "ParentIndexNumber": season, "IndexNumber": e % 4 + 1, "RunTimeTicks": runtime.unwrap_or(45i64 * 600_000_000),
                            "ProductionYear": 2022, "Overview": "A synthetic episode.",
                            "PrimaryImageAspectRatio": 1.7778, "ImageTags": tags, "ImageBlurHashes": hashes,
                            "ParentBackdropItemId": sid, "ParentBackdropImageTags": sbd,
                            "UserData": { "Played": false, "PlaybackPositionTicks": 0, "PlayCount": 0, "IsFavorite": false },
                        }),
                    }
                })
            })
            .collect();
        let albums: Vec<Item> = (0..(movies.len() / 2).max(8))
            .map(|n| {
                let i = id(0x40, n);
                let (tags, hashes) = images(&i, &["Primary"]);
                Item {
                    json: json!({
                        "Id": i, "Name": format!("Album {}", n + 1), "Type": "MusicAlbum", "IsFolder": true,
                        "ProductionYear": 1970 + (n % 50) as i32, "PrimaryImageAspectRatio": 1.0,
                        "ImageTags": tags, "ImageBlurHashes": hashes,
                    }),
                }
            })
            .collect();
        let people = PEOPLE
            .iter()
            .enumerate()
            .map(|(i, (name, _))| Item {
                json: json!({ "Id": id(0x60, i), "Name": name, "Type": "Person" }),
            })
            .collect();
        Self { views, movies, series, episodes, albums, people }
    }

    fn find(&self, id: &str) -> Option<&Item> {
        if let Some(i) = self.people.iter().find(|i| i.json["Id"] == id) {
            return Some(i);
        }
        self.movies
            .iter()
            .chain(&self.series)
            .chain(&self.episodes)
            .chain(&self.albums)
            .find(|i| i.json["Id"] == id)
    }
}

fn view(name: &str, collection: &str, n: usize) -> Value {
    let i = id(0x01, n);
    let (tags, hashes) = images(&i, &["Primary"]);
    json!({
        "Id": i, "Name": name, "Type": "CollectionFolder", "CollectionType": collection, "IsFolder": true,
        "PrimaryImageAspectRatio": 1.7778, "ImageTags": tags, "ImageBlurHashes": hashes,
    })
}

fn with_progress(item: &Item, pct: f64) -> Value {
    let mut v = item.json.clone();
    v["UserData"]["PlayedPercentage"] = json!(pct);
    v["UserData"]["PlaybackPositionTicks"] =
        json!(((v["RunTimeTicks"].as_i64().unwrap_or(0)) as f64 * pct / 100.0) as i64);
    v
}

fn sort_name(j: &Value) -> String {
    j["SortName"].as_str().or(j["Name"].as_str()).unwrap_or("").to_lowercase()
}

/// An item as `/Items/{id}` returns it: with cast and media streams.
fn detail(item: &Value) -> Value {
    let mut v = item.clone();
    let n = item["Id"].as_str().map_or(0, |s| s.bytes().map(|b| b as usize).sum::<usize>());
    let people: Vec<Value> = PEOPLE
        .iter()
        .enumerate()
        .map(|(i, (name, role))| json!({ "Id": id(0x60, i), "Name": name, "Role": role, "Type": "Actor", "PrimaryImageTag": format!("p{i}") }))
        .collect();
    v["People"] = json!(people);
    v["Taglines"] = json!(["Synthetic, but convincing."]);
    if matches!(item["Type"].as_str(), Some("Movie" | "Episode")) {
        chapters_and_trickplay(&mut v);
        // As the server does for a signed-in user: the media source with
        // the user's default tracks (the item page's track picker). Its
        // tracks are the ones that play.
        v["MediaSources"] = playback_info(item)["MediaSources"].clone();
    }
    v["MediaStreams"] = if n.is_multiple_of(2) {
        json!([
            { "Type": "Video", "Codec": "hevc", "BitDepth": 10, "Width": 3840, "Height": 2160, "VideoRangeType": "HDR10", "Index": 0, "AverageFrameRate": 23.976, "RealFrameRate": 23.976 },
            { "Type": "Audio", "Codec": "truehd", "Channels": 8, "IsDefault": true, "AudioSpatialFormat": "DolbyAtmos", "Index": 1 },
            { "Type": "Subtitle", "Codec": "subrip", "Index": 2 }
        ])
    } else {
        json!([
            { "Type": "Video", "Codec": "h264", "BitDepth": 8, "Width": 1920, "Height": 1080, "VideoRangeType": "SDR", "Index": 0, "AverageFrameRate": 25.0, "RealFrameRate": 50.0 },
            { "Type": "Audio", "Codec": "eac3", "Channels": 6, "IsDefault": true, "Index": 1 }
        ])
    };
    v
}

/// An item's playback: one media source (the item id) with two audio
/// tracks, an embedded subtitle and an external one (M7).
fn playback_info(item: &Value) -> Value {
    let id = item["Id"].as_str().unwrap_or_default();
    json!({
        "PlaySessionId": format!("ps-{id}"),
        "MediaSources": [{
            "Id": id, "Protocol": "Http", "Container": "mkv",
            "SupportsDirectPlay": true, "SupportsDirectStream": true, "SupportsTranscoding": false,
            "RunTimeTicks": item["RunTimeTicks"],
            "DefaultAudioStreamIndex": 1, "DefaultSubtitleStreamIndex": -1,
            "MediaStreams": [
                { "Type": "Video", "Codec": "h264", "Width": 1280, "Height": 720, "Index": 0, "AverageFrameRate": 23.976, "RealFrameRate": 23.976 },
                { "Type": "Audio", "Codec": "aac", "Channels": 2, "Language": "eng",
                  "DisplayTitle": "English - AAC - Stereo - Default", "IsDefault": true, "Index": 1 },
                { "Type": "Audio", "Codec": "aac", "Channels": 2, "Language": "eng",
                  "DisplayTitle": "Commentary - AAC - Stereo", "Title": "Commentary", "Index": 2 },
                { "Type": "Subtitle", "Codec": "subrip", "Language": "eng", "DisplayTitle": "English - SUBRIP",
                  "IsTextSubtitleStream": true, "DeliveryMethod": "Embed", "Index": 3 },
                { "Type": "Subtitle", "Codec": "srt", "Language": "fre", "DisplayTitle": "Français - SRT - External",
                  "IsExternal": true, "IsTextSubtitleStream": true, "DeliveryMethod": "External",
                  "DeliveryUrl": format!("/Videos/{id}/{id}/Subtitles/4/0/Stream.srt"), "Index": 4 },
            ],
        }],
    })
}

/// Intro at 10-20% of the runtime, credits in the last 10% (episodes) or
/// 5% (movies).
fn segments(item: &Value) -> Vec<Value> {
    let id = item["Id"].as_str().unwrap_or_default();
    let rt = item["RunTimeTicks"].as_i64().unwrap_or(0);
    let mut v = vec![json!({ "Id": format!("{id}i"), "ItemId": id, "Type": "Intro",
                             "StartTicks": rt / 10, "EndTicks": rt / 5 })];
    let tail = if item["Type"] == "Episode" { rt / 10 } else { rt / 20 };
    v.push(json!({ "Id": format!("{id}o"), "ItemId": id, "Type": "Outro",
                   "StartTicks": rt - tail, "EndTicks": rt }));
    v
}

/// Trickplay at 320 px: one 10x10 sheet of 320x180 tiles per 100
/// thumbnails, one thumbnail every 10 s.
const TRICK_W: u32 = 320;
const TRICK_H: u32 = 180;
const TRICK_INTERVAL_MS: i64 = 10_000;

fn chapters_and_trickplay(item: &mut Value) {
    let id = item["Id"].as_str().unwrap_or_default().to_string();
    let rt = item["RunTimeTicks"].as_i64().unwrap_or(0);
    item["Chapters"] = json!((0..5)
        .map(|i| json!({ "StartPositionTicks": rt / 5 * i, "Name": format!("Chapter {}", i + 1) }))
        .collect::<Vec<_>>());
    let count = (rt / 10_000 / TRICK_INTERVAL_MS).max(1);
    item["Trickplay"] = json!({ id: { TRICK_W.to_string(): {
        "Width": TRICK_W, "Height": TRICK_H, "TileWidth": 10, "TileHeight": 10,
        "ThumbnailCount": count, "Interval": TRICK_INTERVAL_MS, "Bandwidth": 50_000,
    }}});
}

const SUBTITLES: &str = "1\r\n00:00:02,000 --> 00:00:30,000\r\nSous-titres externes (mock)\r\n\r\n";

fn query_result(items: Vec<Value>) -> Value {
    let n = items.len();
    json!({ "Items": items, "TotalRecordCount": n, "StartIndex": 0 })
}

// ---- HTTP ------------------------------------------------------------------------

struct Request {
    method: String,
    path: String,
    query: HashMap<String, String>,
    authorized: bool,
    /// `Range: bytes=N-` start offset.
    range_start: Option<u64>,
    /// `Sec-WebSocket-Key` of an upgrade request.
    ws_key: Option<String>,
}

struct Response {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
    /// `Content-Range` for partial content (206).
    content_range: Option<String>,
}

impl Response {
    fn json(v: Value) -> Self {
        Self {
            status: 200,
            content_type: "application/json; charset=utf-8",
            body: v.to_string().into_bytes(),
            content_range: None,
        }
    }

    fn status(status: u16) -> Self {
        Self { status, content_type: "text/plain", body: Vec::new(), content_range: None }
    }

    fn bytes(content_type: &'static str, body: Vec<u8>) -> Self {
        Self { status: 200, content_type, body, content_range: None }
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16)
                {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                        continue;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_request(head: &str) -> Option<Request> {
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_string();
    let target = first.next()?;
    let (path, qs) = target.split_once('?').unwrap_or((target, ""));
    let query = qs
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (k.to_ascii_lowercase(), percent_decode(v))
        })
        .collect();
    let mut authorized = false;
    let mut range_start = None;
    let mut ws_key = None;
    for l in lines {
        let Some((k, v)) = l.split_once(':') else { continue };
        if k.eq_ignore_ascii_case("authorization")
            && [TOKEN, KID_TOKEN].iter().any(|t| v.contains(&format!("Token=\"{t}\"")))
        {
            authorized = true;
        }
        if k.eq_ignore_ascii_case("sec-websocket-key") {
            ws_key = Some(v.trim().to_string());
        }
        if k.eq_ignore_ascii_case("range") {
            range_start = v
                .trim()
                .strip_prefix("bytes=")
                .and_then(|r| r.split('-').next())
                .and_then(|n| n.parse().ok());
        }
    }
    Some(Request { method, path: path.to_string(), query, authorized, range_start, ws_key })
}

impl Server {
    fn handle(&self, req: &Request, body: &[u8]) -> Response {
        if let Ok(mut l) = self.log.lock() {
            l.push(format!("{} {}", req.method, req.path));
        }
        let lib = &self.lib;
        let limit = |d: usize| req.query.get("limit").and_then(|v| v.parse().ok()).unwrap_or(d);
        let open = matches!(
            req.path.as_str(),
            "/System/Info/Public"
                | "/QuickConnect/Enabled"
                | "/QuickConnect/Initiate"
                | "/QuickConnect/Connect"
                | "/Users/AuthenticateWithQuickConnect"
                | "/Users/AuthenticateByName"
        ) || req.path.starts_with("/Items/") && req.path.contains("/Images/")
            // Test hooks, easy to call with curl.
            || req.path.starts_with("/Mock/");
        if !open && !req.authorized {
            return Response::status(401);
        }
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/System/Info/Public") => Response::json(json!({
                "LocalAddress": "http://127.0.0.1:8096", "ServerName": self.cfg.server_name,
                "Version": self.cfg.version, "ProductName": "Jellyfin Server", "OperatingSystem": "",
                "Id": SERVER_ID, "StartupWizardCompleted": true,
            })),
            ("GET", "/QuickConnect/Enabled") => {
                Response::json(json!(self.cfg.quick_connect_enabled))
            }
            ("POST", "/QuickConnect/Initiate") => {
                self.polls.store(0, Ordering::Relaxed);
                Response::json(self.qc(false))
            }
            ("GET", "/QuickConnect/Connect") => {
                if req.query.get("secret").map(String::as_str) != Some("mock-secret") {
                    return Response::status(404);
                }
                let n = self.polls.fetch_add(1, Ordering::Relaxed) + 1;
                Response::json(self.qc(n >= self.cfg.approve_after))
            }
            ("POST", "/Users/AuthenticateWithQuickConnect") => {
                let ok = serde_json::from_slice::<Value>(body)
                    .ok()
                    .is_some_and(|v| v["Secret"] == "mock-secret")
                    && self.polls.load(Ordering::Relaxed) >= self.cfg.approve_after;
                if ok { Response::json(self.auth("demo")) } else { Response::status(401) }
            }
            ("POST", "/Users/AuthenticateByName") => {
                let name = serde_json::from_slice::<Value>(body)
                    .ok()
                    .and_then(|v| v["Username"].as_str().map(String::from))
                    .unwrap_or_default();
                if name == "demo" || name == "kid" {
                    Response::json(self.auth(&name))
                } else {
                    Response::status(401)
                }
            }
            ("GET", "/UserViews") => Response::json(query_result(lib.views.clone())),
            ("GET", "/UserItems/Resume") => {
                let mut v: Vec<Value> = lib
                    .movies
                    .iter()
                    .take(3)
                    .enumerate()
                    .map(|(i, m)| with_progress(m, 20.0 + i as f64 * 25.0))
                    .collect();
                v.extend(lib.episodes.iter().step_by(9).take(3).map(|e| with_progress(e, 50.0)));
                v.truncate(limit(12));
                Response::json(query_result(v))
            }
            ("GET", "/Shows/NextUp") => Response::json(query_result(
                lib.episodes
                    .iter()
                    .skip(1)
                    .step_by(8)
                    .take(limit(16))
                    .map(|e| e.json.clone())
                    .collect(),
            )),
            ("GET", "/Items/Latest") => {
                let parent = req.query.get("parentid").map(String::as_str).unwrap_or("");
                let src: &[Item] = match parent {
                    p if p == lib.views[0]["Id"] => &lib.movies,
                    p if p == lib.views[1]["Id"] => &lib.series,
                    p if p == lib.views[2]["Id"] => &lib.albums,
                    _ => &[],
                };
                Response::json(Value::Array(
                    src.iter().rev().take(limit(16)).map(|i| i.json.clone()).collect(),
                ))
            }
            ("GET", "/Items") => self.items(req),
            ("GET", p) if p.starts_with("/Items/") && p.contains("/Images/") => self.image(req),
            ("GET", p) if p.starts_with("/Items/") && p.ends_with("/Similar") => Response::json(
                query_result(lib.movies.iter().take(12).map(|i| i.json.clone()).collect()),
            ),
            ("GET", p) if p.starts_with("/Items/") => {
                let id = &p["/Items/".len()..];
                match lib.find(id) {
                    Some(i) => {
                        let mut d = detail(&i.json);
                        if let Some(t) = self.positions.lock().ok().and_then(|p| p.get(id).copied())
                        {
                            d["UserData"]["PlaybackPositionTicks"] = json!(t);
                        }
                        Response::json(d)
                    }
                    None => Response::status(404),
                }
            }
            ("GET", p) if p.starts_with("/Shows/") && p.ends_with("/Seasons") => {
                let series = p.trim_start_matches("/Shows/").trim_end_matches("/Seasons");
                let seasons: Vec<Value> = (1..=2)
                    .map(|n| json!({ "Id": format!("{series}s{n}"), "Name": format!("Season {n}"), "Type": "Season",
                                     "SeriesId": series, "IndexNumber": n, "ImageTags": {} }))
                    .collect();
                Response::json(query_result(seasons))
            }
            ("GET", p) if p.starts_with("/Shows/") && p.ends_with("/Episodes") => {
                let series = p.trim_start_matches("/Shows/").trim_end_matches("/Episodes");
                let all = lib.episodes.iter().filter(|e| e.json["SeriesId"] == series);
                let eps: Vec<Value> = match req.query.get("startitemid") {
                    // From an episode on, in series order (what plays next).
                    Some(start) => all
                        .skip_while(|e| e.json["Id"] != *start)
                        .take(limit(100))
                        .map(|e| e.json.clone())
                        .collect(),
                    None => {
                        let season = req.query.get("seasonid").map(|s| Value::from(s.as_str()));
                        all.filter(|e| {
                            season.is_none() || e.json.get("SeasonId") == season.as_ref()
                        })
                        .map(|e| e.json.clone())
                        .collect()
                    }
                };
                Response::json(query_result(eps))
            }
            ("GET", "/Years") => {
                let lib = &self.lib;
                let src: &[Item] = match req.query.get("parentid") {
                    Some(p) if *p == lib.views[0]["Id"] => &lib.movies,
                    Some(p) if *p == lib.views[1]["Id"] => &lib.series,
                    Some(p) if *p == lib.views[2]["Id"] => &lib.albums,
                    _ => &[],
                };
                let mut ys: Vec<i64> =
                    src.iter().filter_map(|i| i.json["ProductionYear"].as_i64()).collect();
                ys.sort_unstable_by(|a, b| b.cmp(a));
                ys.dedup();
                let v = ys.iter().map(|y| json!({ "Name": y.to_string(), "Id": y.to_string() }));
                Response::json(query_result(v.collect()))
            }
            ("GET", "/Genres") => {
                let g: Vec<Value> = GENRES.iter().map(|g| json!({ "Name": g, "Id": g })).collect();
                Response::json(query_result(g))
            }
            ("GET", "/Users/Public") => {
                Response::json(json!([{ "Name": "demo", "Id": USER_ID, "HasPassword": false },
                                      { "Name": "kid", "Id": KID_ID, "HasPassword": false }]))
            }
            ("POST" | "DELETE", p)
                if p.starts_with("/UserFavoriteItems/") || p.starts_with("/UserPlayedItems/") =>
            {
                let on = req.method == "POST";
                let fav = p.starts_with("/UserFavoriteItems/");
                let item = p.rsplit('/').next().unwrap_or_default();
                let data = json!({ "IsFavorite": fav && on, "Played": !fav && on, "PlaybackPositionTicks": 0, "PlayCount": 0, "Key": "", "ItemId": item });
                // Like the server: every session of the user hears of it.
                self.push(json!({ "MessageType": "UserDataChanged",
                                  "Data": { "UserId": USER_ID, "UserDataList": [data] } }));
                Response::json(data)
            }
            ("POST", "/Sessions/Capabilities/Full") => {
                let Ok(v) = serde_json::from_slice::<Value>(body) else {
                    return Response::status(400);
                };
                if let Ok(mut c) = self.capabilities.lock() {
                    *c = v;
                }
                Response::status(204)
            }
            ("GET", "/Mock/Requests") => Response::json(Value::Array(
                self.log.lock().map(|l| l.iter().map(|x| json!(x)).collect()).unwrap_or_default(),
            )),
            ("GET", "/Mock/Capabilities") => {
                Response::json(self.capabilities.lock().map(|c| c.clone()).unwrap_or_default())
            }
            ("POST", "/Mock/Send") => match serde_json::from_slice::<Value>(body) {
                Ok(v) => {
                    let n = self.sockets.receiver_count();
                    self.push(v);
                    Response::json(json!({ "Sockets": n }))
                }
                Err(_) => Response::status(400),
            },
            ("POST", "/Sessions/Logout") => Response::status(204),
            ("POST", p) if p.starts_with("/Sessions/Playing") => self.report(p, body),
            ("DELETE", "/Videos/ActiveEncodings") => Response::status(204),
            ("GET", "/Mock/Reports") => Response::json(Value::Array(
                self.reports.lock().map(|r| r.clone()).unwrap_or_default(),
            )),
            ("POST", p) if p.starts_with("/Items/") && p.ends_with("/PlaybackInfo") => {
                let id = p.trim_start_matches("/Items/").trim_end_matches("/PlaybackInfo");
                let req = serde_json::from_slice::<Value>(body).unwrap_or_default();
                match lib.find(id) {
                    Some(i) => {
                        let mut info = playback_info(&i.json);
                        let src = &mut info["MediaSources"][0];
                        if req["EnableTranscoding"] == true && req["EnableDirectPlay"] == false {
                            // A "transcode": the same file, as the server
                            // would give its URL (token included).
                            src["SupportsDirectPlay"] = json!(false);
                            src["SupportsDirectStream"] = json!(false);
                            src["SupportsTranscoding"] = json!(true);
                            src["TranscodingUrl"] = json!(format!(
                                "/Videos/{id}/stream?static=true&mediaSourceId={id}&ApiKey={TOKEN}"
                            ));
                        } else if !self.cfg.direct_play {
                            src["SupportsDirectPlay"] = json!(false);
                            src["SupportsDirectStream"] = json!(false);
                            src["SupportsTranscoding"] = json!(true);
                        }
                        Response::json(info)
                    }
                    None => Response::status(404),
                }
            }
            ("GET", p) if p.starts_with("/MediaSegments/") => {
                let id = &p["/MediaSegments/".len()..];
                match lib.find(id) {
                    Some(i) => Response::json(query_result(segments(&i.json))),
                    None => Response::status(404),
                }
            }
            ("GET", p) if p.starts_with("/Videos/") && p.contains("/Trickplay/") => {
                self.trickplay_tile(p)
            }
            ("GET", p) if p.starts_with("/Videos/") && p.contains("/Subtitles/") => {
                Response::bytes("text/plain; charset=utf-8", SUBTITLES.as_bytes().to_vec())
            }
            ("GET", p) if p.starts_with("/Videos/") && p.ends_with("/stream") => self.stream(req),
            _ => Response::status(404),
        }
    }

    /// `/Items`: library listings, search, favorites, alphabet index.
    fn items(&self, req: &Request) -> Response {
        let lib = &self.lib;
        let q = |k: &str| req.query.get(k).map(String::as_str);
        let src: Vec<&Item> = match q("parentid") {
            Some(p) if p == lib.views[0]["Id"] => lib.movies.iter().collect(),
            Some(p) if p == lib.views[1]["Id"] => lib.series.iter().collect(),
            Some(p) if p == lib.views[2]["Id"] => lib.albums.iter().collect(),
            Some(_) => Vec::new(),
            None => lib.movies.iter().chain(&lib.series).chain(&lib.episodes).collect(),
        };
        let types: Vec<&str> =
            q("includeitemtypes").map(|t| t.split(',').collect()).unwrap_or_default();
        let filters = q("filters").unwrap_or("");
        let fav = filters.contains("IsFavorite") || q("isfavorite") == Some("true");
        let unplayed = filters.contains("IsUnplayed");
        let term = q("searchterm").map(str::to_lowercase);
        let genres: Vec<&str> = q("genres").map(|g| g.split('|').collect()).unwrap_or_default();
        let years: Vec<i64> = q("years")
            .map(|y| y.split(',').filter_map(|y| y.trim().parse().ok()).collect())
            .unwrap_or_default();
        let before = q("namelessthan").map(str::to_lowercase);
        let mut v: Vec<&Value> = src
            .iter()
            .map(|i| &i.json)
            .filter(|j| types.is_empty() || types.iter().any(|t| j["Type"] == *t))
            .filter(|j| {
                years.is_empty() || j["ProductionYear"].as_i64().is_some_and(|y| years.contains(&y))
            })
            .filter(|j| !fav || j["UserData"]["IsFavorite"] == true)
            .filter(|j| !unplayed || j["UserData"]["Played"] != true)
            .filter(|j| {
                term.as_ref()
                    .is_none_or(|t| j["Name"].as_str().unwrap_or("").to_lowercase().contains(t))
            })
            .filter(|j| {
                genres.is_empty()
                    || j["Genres"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|x| genres.iter().any(|g| x == *g)))
            })
            .filter(|j| before.as_ref().is_none_or(|b| sort_name(j).as_str() < b.as_str()))
            .collect();
        let key = q("sortby").unwrap_or("SortName").split(',').next().unwrap_or("SortName");
        match key {
            "DateCreated" => {
                v.sort_by(|a, b| a["DateCreated"].as_str().cmp(&b["DateCreated"].as_str()))
            }
            "ProductionYear" => v.sort_by_key(|j| j["ProductionYear"].as_i64()),
            "CommunityRating" => v.sort_by(|a, b| {
                a["CommunityRating"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .total_cmp(&b["CommunityRating"].as_f64().unwrap_or(0.0))
            }),
            _ => v.sort_by_key(|j| sort_name(j)),
        }
        if q("sortorder") == Some("Descending") {
            v.reverse();
        }
        let total = v.len();
        let start: usize = q("startindex").and_then(|s| s.parse().ok()).unwrap_or(0);
        let limit: usize = q("limit").and_then(|s| s.parse().ok()).unwrap_or(100);
        let page: Vec<Value> = v.into_iter().skip(start).take(limit).cloned().collect();
        Response::json(json!({ "Items": page, "TotalRecordCount": total, "StartIndex": start }))
    }

    /// Records a playback report; progress and stop set the resume point.
    fn report(&self, path: &str, body: &[u8]) -> Response {
        let Ok(v) = serde_json::from_slice::<Value>(body) else { return Response::status(400) };
        let kind = match path {
            "/Sessions/Playing" => "Start",
            "/Sessions/Playing/Progress" => "Progress",
            "/Sessions/Playing/Stopped" => "Stopped",
            _ => return Response::status(404),
        };
        if let (Some(item), Some(t)) = (v["ItemId"].as_str(), v["PositionTicks"].as_i64())
            && kind != "Start"
            && let Ok(mut p) = self.positions.lock()
        {
            // Like the server: past 90% the item is watched and the resume
            // point is cleared.
            let rt = self.lib.find(item).and_then(|i| i.json["RunTimeTicks"].as_i64()).unwrap_or(0);
            let watched = rt > 0 && t * 10 >= rt * 9;
            p.insert(item.to_string(), if watched { 0 } else { t });
        }
        if let Ok(mut r) = self.reports.lock() {
            r.push(
                json!({ "Kind": kind, "ItemId": v["ItemId"], "PositionTicks": v["PositionTicks"],
                           "IsPaused": v["IsPaused"], "PlaySessionId": v["PlaySessionId"],
                           "AudioStreamIndex": v["AudioStreamIndex"],
                           "SubtitleStreamIndex": v["SubtitleStreamIndex"] }),
            );
        }
        Response::status(204)
    }

    /// The `media` file, from `Range` on (mpv seeks with ranges).
    fn stream(&self, req: &Request) -> Response {
        let Some(path) = &self.cfg.media else { return Response::status(404) };
        let data = {
            let Ok(mut m) = self.media.lock() else { return Response::status(500) };
            if m.is_none() {
                match std::fs::read(path) {
                    Ok(d) => *m = Some(Arc::new(d)),
                    Err(_) => return Response::status(404),
                }
            }
            m.clone().unwrap_or_default()
        };
        let total = data.len() as u64;
        let start = req.range_start.unwrap_or(0).min(total);
        let ct = match path.extension().and_then(|e| e.to_str()) {
            Some("mkv") => "video/x-matroska",
            Some("webm") => "video/webm",
            _ => "video/mp4",
        };
        let mut r = Response::bytes(ct, data[start as usize..].to_vec());
        if req.range_start.is_some() {
            r.status = 206;
            r.content_range = Some(format!("bytes {start}-{}/{total}", total.saturating_sub(1)));
        }
        r
    }

    /// A trickplay sheet: tiles shaded by their position in the video,
    /// with a bar marking it, so seeking shows distinct previews.
    fn trickplay_tile(&self, path: &str) -> Response {
        let parts: Vec<&str> = path.split('/').collect();
        let (Some(item), Some(index)) = (parts.get(2), parts.get(5)) else {
            return Response::status(404);
        };
        let sheet: u32 = index.trim_end_matches(".jpg").parse().unwrap_or(0);
        let base = color(item);
        let (w, h) = (TRICK_W * 10, TRICK_H * 10);
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            let tile = sheet * 100 + (y / TRICK_H) * 10 + x / TRICK_W;
            let (tx, ty) = (x % TRICK_W, y % TRICK_H);
            let marker = (tile * 7) % TRICK_W;
            let k = 0.35 + 0.65 * ((tile % 20) as f32 / 20.0);
            if ty > TRICK_H - 24 && tx < marker {
                return image::Rgb([240, 240, 240]);
            }
            image::Rgb([
                (base[0] as f32 * k) as u8,
                (base[1] as f32 * k) as u8,
                (base[2] as f32 * k) as u8,
            ])
        });
        let mut png = Vec::new();
        if img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).is_err() {
            return Response::status(500);
        }
        Response::bytes("image/png", png)
    }

    fn qc(&self, authenticated: bool) -> Value {
        json!({
            "Authenticated": authenticated, "Secret": "mock-secret", "Code": QUICK_CONNECT_CODE,
            "DeviceId": "", "DeviceName": "", "AppName": "", "AppVersion": "", "DateAdded": "2026-01-01T00:00:00Z",
        })
    }

    fn auth(&self, name: &str) -> Value {
        let (id, token) = if name == "kid" { (KID_ID, KID_TOKEN) } else { (USER_ID, TOKEN) };
        json!({
            "User": { "Name": name, "Id": id, "ServerId": SERVER_ID, "HasPassword": false },
            "AccessToken": token, "ServerId": SERVER_ID,
        })
    }

    /// Generated artwork: the item's color with a diagonal stripe and a
    /// lighter disc, fitted inside maxWidth×maxHeight at the image type's
    /// natural aspect, like the real server.
    fn image(&self, req: &Request) -> Response {
        let parts: Vec<&str> = req.path.split('/').collect();
        let (Some(item), Some(kind)) = (parts.get(2), parts.get(4)) else {
            return Response::status(404);
        };
        let is_view = self.lib.views.iter().any(|v| v["Id"] == *item);
        let Some(found) = self
            .lib
            .find(item)
            .map(|i| i.json.clone())
            .or_else(|| is_view.then(|| json!({"Type": "CollectionFolder"})))
        else {
            return Response::status(404);
        };
        let aspect = match (*kind, found["Type"].as_str().unwrap_or("")) {
            ("Backdrop" | "Thumb", _) | (_, "Episode" | "CollectionFolder") => 16.0 / 9.0,
            (_, "MusicAlbum") => 1.0,
            _ => 2.0 / 3.0,
        };
        let q = |k: &str| req.query.get(k).and_then(|v| v.parse::<u32>().ok());
        let (mw, mh) = (
            q("maxwidth").unwrap_or(400).clamp(1, 4096),
            q("maxheight").unwrap_or(600).clamp(1, 4096),
        );
        let (w, h) = if mw as f32 / mh as f32 > aspect {
            (((mh as f32 * aspect).round() as u32).max(1), mh)
        } else {
            (mw, ((mw as f32 / aspect).round() as u32).max(1))
        };
        let base = color(item);
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let shade = 0.55 + 0.45 * (1.0 - fy);
            let stripe = ((fx - fy * 0.6 - 0.2).abs() < 0.04) as u32 as f32;
            let (dx, dy) = (fx - 0.62, (fy - 0.32) * h as f32 / w as f32);
            let disc = ((dx * dx + dy * dy).sqrt() < 0.16) as u32 as f32;
            let px = |c: u8| {
                let v = c as f32 * shade;
                let v = v + (255.0 - v) * (0.35 * stripe + 0.6 * disc);
                v.min(255.0) as u8
            };
            image::Rgb([px(base[0]), px(base[1]), px(base[2])])
        });
        let mut png = Vec::new();
        if img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).is_err() {
            return Response::status(500);
        }
        Response::bytes("image/png", png)
    }
}

async fn connection(server: Arc<Server>, mut sock: TcpStream) {
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let Ok(n) = sock.read(&mut chunk).await else { return };
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > 64 * 1024 {
            return;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let len = head
        .split("\r\n")
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0)
        .min(1 << 20);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let Ok(n) = sock.read(&mut chunk).await else { return };
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    let resp = match parse_request(&head) {
        Some(req) if req.path == "/socket" => match (&req.ws_key, req.authorized) {
            (Some(key), true) => return websocket(server, sock, key.clone()).await,
            (None, _) => Response::status(400),
            (_, false) => Response::status(401),
        },
        Some(req) => {
            if !server.cfg.latency.is_zero() {
                tokio::time::sleep(server.cfg.latency).await;
            }
            server.handle(&req, &body)
        }
        None => Response::status(400),
    };
    let reason = match resp.status {
        200 => "OK",
        206 => "Partial Content",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Error",
    };
    let range = resp
        .content_range
        .as_ref()
        .map(|r| format!("Content-Range: {r}\r\nAccept-Ranges: bytes\r\n"))
        .unwrap_or_default();
    let header = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\n{range}Connection: close\r\n\r\n",
        resp.status,
        resp.content_type,
        resp.body.len()
    );
    let _ = sock.write_all(header.as_bytes()).await;
    let _ = sock.write_all(&resp.body).await;
    let _ = sock.shutdown().await;
}

/// The server's side of `/socket`: as Jellyfin does, ForceKeepAlive on
/// connect, KeepAlive answered, and whatever `Server::push` sends.
async fn websocket(server: Arc<Server>, mut sock: TcpStream, key: String) {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
    use tokio_tungstenite::tungstenite::protocol::Role;

    let head = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Accept: {}\r\n\r\n",
        derive_accept_key(key.as_bytes())
    );
    if sock.write_all(head.as_bytes()).await.is_err() {
        return;
    }
    if let Ok(mut l) = server.log.lock() {
        l.push("WS open".into());
    }
    let mut rx = server.sockets.subscribe();
    let mut ws =
        tokio_tungstenite::WebSocketStream::from_raw_socket(sock, Role::Server, None).await;
    let force = json!({ "MessageType": "ForceKeepAlive", "Data": 60 }).to_string();
    if ws.send(Message::text(force)).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            m = rx.recv() => match m {
                Ok(text) => {
                    if ws.send(Message::text(text)).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            },
            m = ws.next() => match m {
                Some(Ok(Message::Text(t))) => {
                    let kind = serde_json::from_str::<Value>(t.as_str())
                        .ok()
                        .and_then(|v| v["MessageType"].as_str().map(String::from))
                        .unwrap_or_default();
                    if let Ok(mut l) = server.log.lock() {
                        l.push(format!("WS {kind}"));
                    }
                    if kind == "KeepAlive" {
                        let ka = json!({ "MessageType": "KeepAlive" }).to_string();
                        if ws.send(Message::text(ka)).await.is_err() {
                            break;
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }
    if let Ok(mut l) = server.log.lock() {
        l.push("WS closed".into());
    }
}

/// Serves HTTP on `listener` until the task is dropped.
pub async fn serve(listener: TcpListener, cfg: MockConfig) {
    let runtime = cfg.media.as_ref().map(|_| cfg.media_secs as i64 * 10_000_000);
    let lib = Library::new(cfg.movies, runtime);
    let server = Arc::new(Server::new(cfg, lib));
    loop {
        let Ok((sock, _)) = listener.accept().await else { continue };
        tokio::spawn(connection(server.clone(), sock));
    }
}

/// Answers LAN discovery (`Who is JellyfinServer?`) on `sock`.
pub async fn discovery(sock: UdpSocket, name: String, address: String) {
    let reply =
        json!({ "Address": address, "Id": SERVER_ID, "Name": name, "EndpointAddress": null })
            .to_string();
    let mut buf = [0u8; 256];
    loop {
        let Ok((n, from)) = sock.recv_from(&mut buf).await else { continue };
        if buf[..n].eq_ignore_ascii_case(b"who is jellyfinserver?") {
            let _ = sock.send_to(reply.as_bytes(), from).await;
        }
    }
}

/// Starts the mock on 127.0.0.1 (random port) in a background thread, for
/// tests. Returns its base URL.
pub fn spawn(cfg: MockConfig) -> String {
    let (tx, rx) = std::sync::mpsc::channel::<SocketAddr>();
    std::thread::spawn(move || {
        let rt =
            tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
        rt.block_on(async move {
            let l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let _ = tx.send(l.local_addr().expect("addr"));
            serve(l, cfg).await;
        });
    });
    let addr = rx.recv().expect("mock server start");
    format!("http://{addr}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_requests() {
        let r = parse_request(
            "GET /Items/Latest?userId=u&parentId=a%20b&limit=16 HTTP/1.1\r\nHost: x\r\n\
             Authorization: MediaBrowser Client=\"J\", Token=\"mock-token\"",
        )
        .unwrap();
        assert_eq!(r.path, "/Items/Latest");
        assert_eq!(r.query["parentid"], "a b");
        assert!(r.authorized);
    }

    #[test]
    fn library_scales() {
        let l = Library::new(10_000, None);
        assert_eq!(l.movies.len(), 10_000);
        assert!(l.find(&id(0x10, 9_999)).is_some());
        assert_eq!(blurhash([0, 0, 0]), "000000");
    }

    fn server(media: Option<std::path::PathBuf>) -> Server {
        let cfg = MockConfig { media, media_secs: 30, ..Default::default() };
        let lib = Library::new(8, cfg.media.as_ref().map(|_| 300_000_000));
        Server::new(cfg, lib)
    }

    fn req(method: &str, target: &str, range: Option<u64>) -> Request {
        let mut r = parse_request(&format!(
            "{method} {target} HTTP/1.1\r\nAuthorization: MediaBrowser Token=\"mock-token\""
        ))
        .unwrap();
        r.range_start = range;
        r
    }

    fn body(r: &Response) -> Value {
        serde_json::from_slice(&r.body).unwrap()
    }

    #[test]
    fn playback_routes() {
        let s = server(None);
        let movie = id(0x10, 0);
        let info =
            body(&s.handle(&req("POST", &format!("/Items/{movie}/PlaybackInfo"), None), b"{}"));
        assert_eq!(info["MediaSources"][0]["Id"], movie.as_str());
        assert_eq!(info["MediaSources"][0]["MediaStreams"][4]["IsExternal"], true);
        let d = body(&s.handle(&req("GET", &format!("/Items/{movie}"), None), b""));
        assert_eq!(d["Chapters"].as_array().unwrap().len(), 5);
        assert_eq!(d["Trickplay"][&movie]["320"]["TileWidth"], 10);
        let seg = body(&s.handle(&req("GET", &format!("/MediaSegments/{movie}"), None), b""));
        assert_eq!(seg["Items"][0]["Type"], "Intro");
        let ep = id(0x30, 3);
        let series = id(0x20, 0);
        let next = body(&s.handle(
            &req("GET", &format!("/Shows/{series}/Episodes?startItemId={ep}&limit=2"), None),
            b"",
        ));
        assert_eq!(next["Items"][0]["Id"], ep.as_str());
        assert_eq!(next["Items"][1]["Id"], id(0x30, 4).as_str());
        let tile =
            s.handle(&req("GET", &format!("/Videos/{movie}/Trickplay/320/0.jpg"), None), b"");
        assert_eq!((tile.status, tile.content_type), (200, "image/png"));
    }

    #[test]
    fn reports_set_resume_positions() {
        let s = server(None);
        let movie = id(0x10, 1);
        let r = s.handle(
            &req("POST", "/Sessions/Playing/Stopped", None),
            json!({ "ItemId": movie, "PositionTicks": 1234 }).to_string().as_bytes(),
        );
        assert_eq!(r.status, 204);
        let d = body(&s.handle(&req("GET", &format!("/Items/{movie}"), None), b""));
        assert_eq!(d["UserData"]["PlaybackPositionTicks"], 1234);
        let reps = body(&s.handle(&req("GET", "/Mock/Reports", None), b""));
        assert_eq!(reps[0]["Kind"], "Stopped");
    }

    #[test]
    fn stream_supports_ranges() {
        let f =
            std::env::temp_dir().join(format!("jellyroll-mock-media-{}.mp4", std::process::id()));
        std::fs::write(&f, b"0123456789").unwrap();
        let s = server(Some(f.clone()));
        let movie = id(0x10, 0);
        let full = s.handle(&req("GET", &format!("/Videos/{movie}/stream"), None), b"");
        assert_eq!((full.status, full.body.len()), (200, 10));
        let part = s.handle(&req("GET", &format!("/Videos/{movie}/stream"), Some(4)), b"");
        assert_eq!(part.status, 206);
        assert_eq!(part.body, b"456789");
        assert_eq!(part.content_range.as_deref(), Some("bytes 4-9/10"));
        let _ = std::fs::remove_file(f);
    }
}
