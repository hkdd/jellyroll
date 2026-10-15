//! Live updates and remote control (spec 8): keeps the server's WebSocket
//! open while signed in. Changes made elsewhere (watched, favorites, new
//! or removed items) reach the UI as `Event::Changed`, gathered for a
//! moment so a burst (a season marked watched) is one refresh; remote
//! control from other Jellyfin apps arrives as `Event::Remote` at once.
//!
//! The socket reconnects with jittered exponential backoff (spec 8) and
//! after a reconnect the UI refreshes, since changes may have been missed
//! while it was down. The rest of the app never waits on it: if the
//! socket can't connect, browsing and playback work as before.

use std::time::Duration;

use jellyfin_api::dto::{GeneralCommandType as G, PlayCommand, PlaystateCommand as P};
use jellyfin_api::socket::{self, ServerMessage};
use jellyfin_api::{Client, endpoints as ep};
use tokio::sync::watch;
use tokio::time::Instant;

use super::session::Out;
use super::{Changed, Event, Remote, fetch};
use crate::input::InputAction;

/// Changes are gathered this long before the UI hears of them.
const GATHER: Duration = Duration::from_secs(1);
/// How often the capabilities are posted again while connected (D79).
/// Jellyfin 12.2 only counts a session as active on a few requests
/// (capabilities, opening the socket, playback reports), not browsing or
/// keep-alives, and the dashboard and remotes list sessions active in the
/// last 16 minutes (`activeWithinSeconds=960`). Without this an idle
/// jellyroll drops off both while still connected.
const ACTIVITY_EVERY: Duration =
    if cfg!(test) { Duration::from_millis(300) } else { Duration::from_secs(5 * 60) };
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// A message shown with no timeout of its own stays this long.
const MESSAGE_DEFAULT: Duration = Duration::from_secs(5);

/// The general commands this app carries out (posted as capabilities, so
/// other apps offer only these).
pub const SUPPORTED: &[G] = &[
    G::MoveUp,
    G::MoveDown,
    G::MoveLeft,
    G::MoveRight,
    G::Select,
    G::Back,
    G::GoHome,
    G::GoToSearch,
    G::ToggleContextMenu,
    G::ToggleOsdMenu,
    G::ToggleStats,
    G::SendString,
    G::DisplayMessage,
    G::SetAudioStreamIndex,
    G::SetSubtitleStreamIndex,
    G::VolumeUp,
    G::VolumeDown,
    G::ToggleMute,
    G::Mute,
    G::Unmute,
    G::SetVolume,
];

/// A random-ish factor in [0.5, 1.5) for backoff jitter (std only: the
/// hasher's keys are random per process and per call).
fn jitter() -> f64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    h.write_u64(t.map_or(0, |d| d.as_nanos() as u64));
    0.5 + (h.finish() % 1_000_000) as f64 / 1_000_000.0
}

/// Delay before reconnect attempt `n` (0-based).
fn backoff(n: u32, jitter: f64) -> Duration {
    let base = BACKOFF_MIN.saturating_mul(1 << n.min(6)).min(BACKOFF_MAX);
    base.mul_f64(jitter)
}

/// What a server message asks of the UI.
pub fn remote(msg: &ServerMessage) -> Option<Remote> {
    use InputAction as A;
    Some(match msg {
        ServerMessage::Playstate { command, seek_ticks } => match command {
            P::Stop => Remote::Stop,
            P::Pause => Remote::SetPaused(true),
            P::Unpause => Remote::SetPaused(false),
            P::PlayPause => Remote::Input(A::PlayPause),
            P::Seek => Remote::Seek((*seek_ticks)? as f64 / 10_000_000.0),
            P::Rewind => Remote::Input(A::SeekBack),
            P::FastForward => Remote::Input(A::SeekFwd),
            P::NextTrack => Remote::Input(A::Next),
            P::PreviousTrack => Remote::Input(A::Prev),
            P::Unrecognized => return None,
        },
        ServerMessage::GeneralCommand { name, arguments } => {
            let arg = |k: &str| arguments.get(k).map(String::as_str);
            let index = || arg("Index").and_then(|v| v.trim().parse::<i32>().ok());
            match name {
                G::MoveUp => Remote::Input(A::Up),
                G::MoveDown => Remote::Input(A::Down),
                G::MoveLeft => Remote::Input(A::Left),
                G::MoveRight => Remote::Input(A::Right),
                G::Select => Remote::Input(A::Select),
                G::Back => Remote::Input(A::Back),
                G::GoHome => Remote::Input(A::Home),
                G::GoToSearch => Remote::Input(A::Search),
                G::ToggleContextMenu | G::ToggleOsdMenu => Remote::Input(A::Menu),
                G::ToggleStats => Remote::Input(A::Info),
                G::SendString => Remote::Text(arg("String")?.to_string()),
                G::DisplayMessage => {
                    let text = arg("Text").unwrap_or_default().trim();
                    let header = arg("Header").unwrap_or_default().trim();
                    let text = match (header.is_empty(), text.is_empty()) {
                        (_, true) if header.is_empty() => return None,
                        (true, _) => text.to_string(),
                        (false, true) => header.to_string(),
                        (false, false) => format!("{header}: {text}"),
                    };
                    let ms = arg("TimeoutMs").and_then(|v| v.trim().parse::<u64>().ok());
                    let timeout = ms
                        .filter(|ms| *ms > 0)
                        .map_or(MESSAGE_DEFAULT, Duration::from_millis)
                        .min(Duration::from_secs(60));
                    Remote::Message { text, timeout }
                }
                G::VolumeUp => Remote::Input(A::VolUp),
                G::VolumeDown => Remote::Input(A::VolDown),
                G::ToggleMute => Remote::Input(A::Mute),
                G::Mute => Remote::SetMute(true),
                G::Unmute => Remote::SetMute(false),
                G::SetVolume => {
                    let v = arg("Volume")?.trim().parse::<f64>().ok()?;
                    Remote::SetVolume(v.round().clamp(0.0, 100.0) as u8)
                }
                G::SetAudioStreamIndex => Remote::AudioStream(index()?),
                G::SetSubtitleStreamIndex => Remote::SubtitleStream(index()?),
                _ => return None,
            }
        }
        ServerMessage::Play(req) => {
            // No play queue: "play now" plays the chosen item; adding to a
            // queue (next/last) is not offered.
            if !matches!(req.play_command, None | Some(PlayCommand::PlayNow)) {
                tracing::info!(command = ?req.play_command, "remote: play queues not supported");
                return None;
            }
            let ids = req.item_ids.as_deref().unwrap_or_default();
            let i = req.start_index.unwrap_or(0).max(0) as usize;
            let id = ids.get(i).or(ids.first())?;
            Remote::Play { id: id.clone(), start_ticks: req.start_position_ticks.unwrap_or(0) }
        }
        _ => return None,
    })
}

/// Changes gathered before the UI hears of them.
#[derive(Default)]
struct Gathered {
    changed: Changed,
    since: Option<Instant>,
}

impl Gathered {
    fn add(&mut self, items: impl IntoIterator<Item = String>, library: bool, now: Instant) {
        self.changed.items.extend(items.into_iter().map(|i| norm_id(&i)));
        self.changed.library |= library;
        self.since.get_or_insert(now);
    }

    fn due(&self) -> Option<Instant> {
        self.since.map(|t| t + GATHER)
    }

    fn take(&mut self) -> Changed {
        self.since = None;
        std::mem::take(&mut self.changed)
    }
}

enum End {
    /// Signed out, or another user/server: start over.
    SessionChanged,
    /// The connection failed or dropped.
    Lost(String),
}

pub async fn worker(mut session: watch::Receiver<fetch::Session>, out: Out) {
    let mut failures: u32 = 0;
    let mut connected_before = false;
    loop {
        let current = session.borrow_and_update().clone();
        let Some((client, user)) = current else {
            connected_before = false;
            failures = 0;
            if session.changed().await.is_err() {
                return;
            }
            continue;
        };
        match run(&client, &user, &out, &mut session, connected_before).await {
            (End::SessionChanged, _) => {
                failures = 0;
                connected_before = false;
            }
            (End::Lost(why), opened) => {
                if opened {
                    connected_before = true;
                    failures = 0;
                }
                let wait = backoff(failures, jitter());
                failures = failures.saturating_add(1);
                tracing::info!("live updates: {why}; reconnecting in {:.1} s", wait.as_secs_f64());
                // A new session doesn't wait for the old one's backoff.
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    r = session.changed() => {
                        if r.is_err() {
                            return;
                        }
                        failures = 0;
                        connected_before = false;
                    }
                }
            }
        }
    }
}

/// One connection. The bool: whether the socket opened (resets backoff).
async fn run(
    client: &Client,
    user: &str,
    out: &Out,
    session: &mut watch::Receiver<fetch::Session>,
    reconnect: bool,
) -> (End, bool) {
    // Remote control needs capabilities posted for this session first.
    if let Err(e) = client.send_no_content(&ep::post_capabilities(SUPPORTED)).await {
        return (End::Lost(format!("capabilities: {e}")), false);
    }
    let mut sock = match client.open_socket().await {
        Ok(s) => s,
        Err(e) => return (End::Lost(format!("connect: {e}")), false),
    };
    tracing::info!("live updates connected");
    let mut gathered = Gathered::default();
    if reconnect {
        // Whatever changed while the socket was down.
        gathered.add([], true, Instant::now());
    }
    let mut timeout = socket::DEFAULT_KEEPALIVE_TIMEOUT;
    let mut next_keepalive = Instant::now() + timeout / 2;
    let mut next_activity = Instant::now() + ACTIVITY_EVERY;
    let mut last_heard = Instant::now();
    let end = loop {
        let flush_at = gathered.due();
        // Nothing heard for a whole timeout (keep-alives are answered):
        // the connection is dead even if TCP hasn't noticed.
        let dead_at = last_heard + timeout + timeout / 2;
        tokio::select! {
            r = session.changed() => {
                if r.is_err() {
                    break End::SessionChanged;
                }
                let same = session
                    .borrow()
                    .as_ref()
                    .is_some_and(|(c, u)| u == user && c.base_url() == client.base_url());
                if !same {
                    break End::SessionChanged;
                }
            }
            _ = tokio::time::sleep_until(next_keepalive) => {
                if let Err(e) = sock.send_keep_alive().await {
                    break End::Lost(e.to_string());
                }
                next_keepalive = Instant::now() + timeout / 2;
            }
            _ = tokio::time::sleep_until(dead_at) => {
                break End::Lost("no answer from the server".into());
            }
            _ = tokio::time::sleep_until(next_activity) => {
                // A failure here doesn't end the socket; the next try may work.
                if let Err(e) = client.send_no_content(&ep::post_capabilities(SUPPORTED)).await {
                    tracing::warn!("keeping the session active: {e}");
                }
                next_activity = Instant::now() + ACTIVITY_EVERY;
            }
            _ = tokio::time::sleep_until(flush_at.unwrap_or(dead_at)), if flush_at.is_some() => {
                let c = gathered.take();
                tracing::debug!(items = c.items.len(), library = c.library, "server changes");
                out.send(Event::Changed(c)).await;
            }
            m = sock.next() => {
                last_heard = Instant::now();
                let msg = match m {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => break End::Lost(e.to_string()),
                    None => break End::Lost("closed by the server".into()),
                };
                match &msg {
                    ServerMessage::ForceKeepAlive(t) => {
                        timeout = (*t).max(Duration::from_secs(4));
                        next_keepalive = Instant::now();
                    }
                    ServerMessage::KeepAlive => {}
                    ServerMessage::LibraryChanged(info) => {
                        let items = [&info.items_added, &info.items_removed, &info.items_updated]
                            .into_iter()
                            .flatten()
                            .flatten()
                            .cloned();
                        gathered.add(items, true, Instant::now());
                    }
                    ServerMessage::UserDataChanged(info) => {
                        if info.user_id.as_deref().is_none_or(|u| same_id(u, user)) {
                            let items =
                                socket::changed_items(info).filter_map(|d| d.item_id.clone());
                            gathered.add(items, false, Instant::now());
                        }
                    }
                    ServerMessage::ServerGoingAway => break End::Lost("server restarting".into()),
                    ServerMessage::UserChanged | ServerMessage::Other(_) => {}
                    m => match remote(m) {
                        Some(r) => {
                            tracing::info!(command = ?r, "remote control");
                            out.send(Event::Remote(r)).await;
                        }
                        None => tracing::debug!(message = ?m, "remote: not supported"),
                    },
                }
            }
        }
    };
    if gathered.since.is_some() {
        out.send(Event::Changed(gathered.take())).await;
    }
    sock.close().await;
    (end, true)
}

/// An id as `Changed` holds it: Jellyfin writes ids with or without
/// dashes depending on the endpoint.
pub fn norm_id(s: &str) -> String {
    s.chars().filter(|c| *c != '-').map(|c| c.to_ascii_lowercase()).collect()
}

fn same_id(a: &str, b: &str) -> bool {
    norm_id(a) == norm_id(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_caps_and_jitters() {
        assert_eq!(backoff(0, 1.0), Duration::from_secs(1));
        assert_eq!(backoff(3, 1.0), Duration::from_secs(8));
        assert_eq!(backoff(20, 1.0), BACKOFF_MAX);
        assert_eq!(backoff(1, 0.5), Duration::from_secs(1));
        for _ in 0..50 {
            let j = jitter();
            assert!((0.5..1.5).contains(&j), "{j}");
        }
    }

    #[test]
    fn maps_remote_commands() {
        let ps = |command, seek_ticks| ServerMessage::Playstate { command, seek_ticks };
        assert_eq!(remote(&ps(P::Pause, None)), Some(Remote::SetPaused(true)));
        assert_eq!(remote(&ps(P::Seek, Some(600_000_000))), Some(Remote::Seek(60.0)));
        assert_eq!(remote(&ps(P::Seek, None)), None);
        assert_eq!(remote(&ps(P::Stop, None)), Some(Remote::Stop));
        let gc = |name, args: &[(&str, &str)]| ServerMessage::GeneralCommand {
            name,
            arguments: args.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        };
        assert_eq!(remote(&gc(G::MoveDown, &[])), Some(Remote::Input(InputAction::Down)));
        assert_eq!(
            remote(&gc(G::SetSubtitleStreamIndex, &[("Index", "-1")])),
            Some(Remote::SubtitleStream(-1))
        );
        assert_eq!(remote(&gc(G::SetAudioStreamIndex, &[("Index", "x")])), None);
        assert_eq!(
            remote(&gc(
                G::DisplayMessage,
                &[("Header", "Hi"), ("Text", "Dinner"), ("TimeoutMs", "3000")]
            )),
            Some(Remote::Message { text: "Hi: Dinner".into(), timeout: Duration::from_secs(3) })
        );
        assert_eq!(
            remote(&gc(G::DisplayMessage, &[("Text", "Dinner")])),
            Some(Remote::Message { text: "Dinner".into(), timeout: MESSAGE_DEFAULT })
        );
        assert_eq!(remote(&gc(G::DisplayMessage, &[])), None);
        assert_eq!(remote(&gc(G::TakeScreenshot, &[])), None);
        // Volume from remote-control apps.
        assert_eq!(remote(&gc(G::VolumeUp, &[])), Some(Remote::Input(InputAction::VolUp)));
        assert_eq!(remote(&gc(G::ToggleMute, &[])), Some(Remote::Input(InputAction::Mute)));
        assert_eq!(remote(&gc(G::Mute, &[])), Some(Remote::SetMute(true)));
        assert_eq!(remote(&gc(G::Unmute, &[])), Some(Remote::SetMute(false)));
        assert_eq!(remote(&gc(G::SetVolume, &[("Volume", "42")])), Some(Remote::SetVolume(42)));
        assert_eq!(remote(&gc(G::SetVolume, &[("Volume", "250")])), Some(Remote::SetVolume(100)));
        assert_eq!(remote(&gc(G::SetVolume, &[])), None);
        assert!(SUPPORTED.contains(&G::SetVolume));
    }

    #[test]
    fn plays_the_chosen_item_now() {
        use jellyfin_api::dto::PlayRequest;
        let play = |cmd, start: Option<i32>| {
            ServerMessage::Play(PlayRequest {
                item_ids: Some(vec!["a".into(), "b".into()]),
                start_position_ticks: Some(5),
                play_command: cmd,
                start_index: start,
                ..Default::default()
            })
        };
        assert_eq!(
            remote(&play(Some(PlayCommand::PlayNow), Some(1))),
            Some(Remote::Play { id: "b".into(), start_ticks: 5 })
        );
        assert_eq!(
            remote(&play(None, None)),
            Some(Remote::Play { id: "a".into(), start_ticks: 5 })
        );
        assert_eq!(remote(&play(Some(PlayCommand::PlayLast), None)), None);
    }

    #[test]
    fn ids_compare_without_dashes() {
        assert!(same_id(
            "5E7A1C0D-E5E7-A1C0-DE5E-7A1C0DE5E7A1",
            "5e7a1c0de5e7a1c0de5e7a1c0de5e7a1"
        ));
        assert!(!same_id("a", "b"));
    }
}
