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
            Fetched::Failed(e.to_string())
        }
    }
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
        Fetch::Genres(parent) => {
            let r = c.send(&ep::genres(user, &parent)).await?;
            Fetched::Names(r.items.unwrap_or_default().into_iter().filter_map(|g| g.name).collect())
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
        Fetch::Stream { id, media_source } => Fetched::Stream {
            url: format!("{}{}", c.base_url(), ep::stream_path(&id, &media_source)),
            authorization: c.token_header(),
        },
    })
}
