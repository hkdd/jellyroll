//! Page data on demand (M6): library grid pages, item details, seasons and
//! episodes, similar items, genres, favorite/played toggles, and what
//! playback needs. Requests come from the UI tagged with an id; results go
//! back as `Event::Fetched` with the same id, so a page ignores answers
//! meant for a page it has since left.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use jellyfin_api::endpoints::{self as ep, ItemsQuery};
use jellyfin_api::{Client, Error};
use tokio::sync::{Notify, watch};
use tokio::task::JoinSet;

use super::session::Out;
use super::{Event, Fetch, Fetched};
use crate::home::model::{self, Shape};

const CONCURRENCY: usize = 4;

/// The signed-in client and user id; None while signed out.
pub type Session = Option<(Client, String)>;

#[derive(Default)]
pub struct FetchQueue {
    jobs: Mutex<VecDeque<(u64, Fetch)>>,
    notify: Notify,
}

impl FetchQueue {
    pub fn push(&self, id: u64, f: Fetch) {
        self.jobs.lock().unwrap_or_else(|e| e.into_inner()).push_back((id, f));
        self.notify.notify_one();
    }

    fn pop(&self) -> Option<(u64, Fetch)> {
        self.jobs.lock().unwrap_or_else(|e| e.into_inner()).pop_front()
    }
}

pub async fn worker(queue: Arc<FetchQueue>, session: watch::Receiver<Session>, out: Out) {
    let mut running = JoinSet::new();
    loop {
        while running.len() < CONCURRENCY {
            let Some((id, f)) = queue.pop() else { break };
            let s = session.borrow().clone();
            let out = out.clone();
            running.spawn(async move {
                let result = match s {
                    Some((client, user)) => run(&client, &user, f).await,
                    None => Fetched::Failed("not signed in".into()),
                };
                out.send(Event::Fetched { id, result }).await;
            });
        }
        tokio::select! {
            Some(_) = running.join_next(), if !running.is_empty() => {}
            _ = queue.notify.notified() => {}
        }
    }
}

fn cards(items: &[jellyfin_api::dto::BaseItemDto], shape: Shape) -> Vec<model::Card> {
    items.iter().filter_map(|i| model::card(i, shape)).collect()
}

async fn run(c: &Client, user: &str, f: Fetch) -> Fetched {
    match fetch(c, user, f).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("fetch failed: {e}");
            match e {
                // The page shows this; the app goes offline and keeps
                // checking until the server is back (then pages reload).
                Error::Network(_) => Fetched::Failed(super::UNREACHABLE.into()),
                e => Fetched::Failed(e.to_string()),
            }
        }
    }
}

/// The item (chapters, trickplay, series), its PlaybackInfo and media
/// segments at once, then the next episode for an episode.
async fn play(
    c: &Client,
    user: &str,
    id: &str,
    start_ticks: i64,
    tracks: super::TrackChoice,
    transcode: bool,
) -> Result<Fetched, Error> {
    let item_req = ep::item(user, id);
    let t = jellyfin_api::playback::TranscodeRequest {
        start_ticks,
        audio_stream_index: tracks.audio,
        subtitle_stream_index: tracks.subtitle,
    };
    let info_req = jellyfin_api::playback::playback_info(user, id, transcode.then_some(&t));
    let seg_req = jellyfin_api::playback::media_segments(id);
    let (item, info, segs) = tokio::join!(c.send(&item_req), c.send(&info_req), c.send(&seg_req));
    let (item, info) = (item?, info?);
    // Older servers or none detected: no skip buttons.
    let segs = segs.ok().and_then(|s| s.items).unwrap_or_default();
    let next = match (&item.series_id, item.r#type) {
        (Some(series), Some(jellyfin_api::dto::BaseItemKind::Episode)) => c
            .send(&ep::episodes_from(user, series, id))
            .await
            .ok()
            .and_then(|r| r.items)
            .and_then(|v| v.into_iter().nth(1)),
        _ => None,
    };
    Ok(match crate::home::playinfo::build(&item, &info, &segs, next.as_ref(), start_ticks) {
        Ok(mut info) => Fetched::Play(Box::new(super::PlayFetched {
            info: {
                info.choose(tracks);
                info
            },
            base_url: c.base_url().to_string(),
            authorization: c.token_header(),
        })),
        Err(e) => Fetched::Failed(e),
    })
}

async fn fetch(c: &Client, user: &str, f: Fetch) -> Result<Fetched, Error> {
    Ok(match f {
        Fetch::Items { query, shape } => {
            let r = c.send(&ep::items(user, &query)).await?;
            let items = r.items.unwrap_or_default();
            Fetched::Page {
                start: query.start,
                total: r.total_record_count.unwrap_or(items.len() as i32).max(0) as u32,
                cards: cards(&items, shape),
            }
        }
        Fetch::LetterIndex { query, letter } => {
            let q = ItemsQuery {
                name_less_than: Some(letter.to_string()),
                start: 0,
                limit: 0,
                ..query
            };
            let r = c.send(&ep::items(user, &q)).await?;
            Fetched::Index(r.total_record_count.unwrap_or(0).max(0) as u32)
        }
        Fetch::Detail(id) => match model::detail(&c.send(&ep::item(user, &id)).await?) {
            Some(d) => Fetched::Detail(Box::new(d)),
            None => Fetched::Failed("the server returned an item without an id".into()),
        },
        Fetch::Seasons(series) => {
            let r = c.send(&ep::seasons(user, &series)).await?;
            Fetched::Cards(cards(&r.items.unwrap_or_default(), Shape::Poster))
        }
        Fetch::Episodes { series, season } => {
            let r = c.send(&ep::episodes(user, &series, &season)).await?;
            Fetched::Cards(cards(&r.items.unwrap_or_default(), Shape::Landscape))
        }
        Fetch::Similar(id) => {
            let r = c.send(&ep::similar(user, &id, 16)).await?;
            Fetched::Cards(cards(&r.items.unwrap_or_default(), Shape::Poster))
        }
        Fetch::Filmography(id) => {
            let r = c.send(&ep::filmography(user, &id)).await?;
            Fetched::Cards(cards(&r.items.unwrap_or_default(), Shape::Poster))
        }
        Fetch::Contents { id, playlist } => {
            let req =
                if playlist { ep::playlist_items(user, &id) } else { ep::children(user, &id) };
            let r = c.send(&req).await?;
            Fetched::Cards(cards(&r.items.unwrap_or_default(), Shape::Poster))
        }
        Fetch::Genres(parent) => {
            let r = c.send(&ep::genres(user, &parent)).await?;
            Fetched::Names(r.items.unwrap_or_default().into_iter().filter_map(|g| g.name).collect())
        }
        Fetch::Years { parent, include_types } => {
            let r = c.send(&ep::years(user, &parent, include_types.as_deref())).await?;
            Fetched::Names(r.items.unwrap_or_default().into_iter().filter_map(|y| y.name).collect())
        }
        Fetch::SetFavorite { id, on } => {
            let d = c.send(&ep::set_favorite(user, &id, on)).await?;
            Fetched::UserData {
                favorite: d.is_favorite.unwrap_or(on),
                played: d.played.unwrap_or(false),
            }
        }
        Fetch::SetPlayed { id, on } => {
            let d = c.send(&ep::set_played(user, &id, on)).await?;
            Fetched::UserData {
                favorite: d.is_favorite.unwrap_or(false),
                played: d.played.unwrap_or(on),
            }
        }
        Fetch::Play { id, start_ticks, tracks, transcode } => {
            play(c, user, &id, start_ticks, tracks, transcode).await?
        }
        Fetch::Debug(id) => Fetched::Debug(Box::new(debug(c, user, &id).await)),
    })
}

/// Everything the server says about playing an item (D83). Each part keeps
/// its own error, so a report is written whatever fails.
async fn debug(c: &Client, user: &str, id: &str) -> super::DebugServer {
    use jellyfin_api::playback::{TranscodeRequest, playback_info};
    fn pretty(r: Result<serde_json::Value, Error>) -> String {
        match r {
            Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_default(),
            Err(e) => format!("(error: {e})"),
        }
    }
    let body = |b: &Option<String>| {
        b.as_deref()
            .and_then(|b| serde_json::from_str::<serde_json::Value>(b).ok())
            .and_then(|v| serde_json::to_string_pretty(&v).ok())
            .unwrap_or_default()
    };
    let direct = playback_info(user, id, None);
    let t = TranscodeRequest { start_ticks: 0, ..Default::default() };
    let transcode = playback_info(user, id, Some(&t));
    let (direct_body, transcode_body) = (body(&direct.body), body(&transcode.body));
    let reqs = (ep::public_info().raw(), ep::item(user, id).raw(), direct.raw(), transcode.raw());
    let (server_info, item, d, t) =
        tokio::join!(c.send(&reqs.0), c.send(&reqs.1), c.send(&reqs.2), c.send(&reqs.3));
    let play = match play(c, user, id, 0, super::TrackChoice::default(), false).await {
        Ok(Fetched::Play(p)) => Ok(*p),
        Ok(Fetched::Failed(e)) => Err(e),
        Ok(_) => Err("unexpected answer".into()),
        Err(e) => Err(e.to_string()),
    };
    super::DebugServer {
        server_info: pretty(server_info),
        item: pretty(item),
        direct: (direct_body, pretty(d)),
        transcode: (transcode_body, pretty(t)),
        play,
    }
}
