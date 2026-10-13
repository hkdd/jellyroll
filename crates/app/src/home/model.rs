//! What the home screen shows, independent of the API's DTO shapes: rows
//! of cards, each with the artwork to request and the text to draw. Built
//! on the network thread from server responses, cached as JSON for the
//! instant start (spec 9), and diffed by item id when fresh data arrives.

use jellyfin_api::dto::{BaseItemDto, BaseItemKind, CollectionType};
use jellyfin_api::endpoints::ImageType;
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Shape {
    /// 2:3 movie/series poster.
    Poster,
    /// 16:9 thumbnail (episodes, Continue Watching, library tiles).
    Landscape,
    /// 1:1 album art.
    Square,
}

impl Shape {
    /// Card size in logical pixels (1080p layout).
    pub fn size(self) -> (f32, f32) {
        match self {
            Shape::Poster => (160.0, 240.0),
            Shape::Landscape => (300.0, 169.0),
            Shape::Square => (200.0, 200.0),
        }
    }
}

/// Artwork reference: enough to build a sized, tagged image URL.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Art {
    pub item_id: String,
    pub kind: ImageType,
    pub tag: String,
    pub blurhash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Card {
    pub id: String,
    pub title: String,
    /// Second line under the card: episode, year...
    pub subtitle: String,
    /// Hero line: year · rating · runtime.
    pub meta: String,
    pub overview: String,
    pub image: Option<Art>,
    pub backdrop: Option<Art>,
    /// Playback progress 0..1 for partially watched items.
    pub progress: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Row {
    /// Stable identity across refreshes ("resume", "latest:<library>").
    pub key: String,
    pub title: String,
    pub shape: Shape,
    pub cards: Vec<Card>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HomeData {
    pub server_name: String,
    pub user_name: String,
    pub rows: Vec<Row>,
}

fn first_tag(tags: &Option<Vec<String>>) -> Option<String> {
    tags.as_ref()?.first().filter(|t| !t.is_empty()).cloned()
}

fn image_tag(item: &BaseItemDto, kind: ImageType) -> Option<String> {
    item.image_tags.as_ref()?.get(kind.as_str())?.clone().filter(|t| !t.is_empty())
}

fn blurhash(item: &BaseItemDto, kind: ImageType, tag: &str) -> Option<String> {
    let h = item.image_blur_hashes.as_ref()?;
    let map = match kind {
        ImageType::Primary => h.primary.as_ref(),
        ImageType::Backdrop => h.backdrop.as_ref(),
        ImageType::Thumb => h.thumb.as_ref(),
        ImageType::Logo => h.logo.as_ref(),
    }?;
    map.get(tag)?.clone()
}

fn own_art(item: &BaseItemDto, kind: ImageType) -> Option<Art> {
    let id = item.id.clone()?;
    let tag = if kind == ImageType::Backdrop {
        first_tag(&item.backdrop_image_tags)?
    } else {
        image_tag(item, kind)?
    };
    Some(Art { blurhash: blurhash(item, kind, &tag), item_id: id, kind, tag })
}

/// Backdrop for the hero: the item's own, else its parent's (episode →
/// series). Parent blurhashes aren't in the item, so none for those.
fn backdrop(item: &BaseItemDto) -> Option<Art> {
    own_art(item, ImageType::Backdrop).or_else(|| {
        Some(Art {
            item_id: item.parent_backdrop_item_id.clone()?,
            kind: ImageType::Backdrop,
            tag: first_tag(&item.parent_backdrop_image_tags)?,
            blurhash: None,
        })
    })
}

fn card_image(item: &BaseItemDto, shape: Shape) -> Option<Art> {
    let is_episode = item.r#type == Some(BaseItemKind::Episode);
    match shape {
        Shape::Landscape if is_episode => own_art(item, ImageType::Primary)
            .or_else(|| {
                Some(Art {
                    item_id: item.parent_thumb_item_id.clone()?,
                    kind: ImageType::Thumb,
                    tag: item.parent_thumb_image_tag.clone()?,
                    blurhash: None,
                })
            })
            .or_else(|| backdrop(item)),
        Shape::Landscape => own_art(item, ImageType::Thumb)
            .or_else(|| own_art(item, ImageType::Backdrop))
            .or_else(|| own_art(item, ImageType::Primary)),
        Shape::Poster if is_episode => item
            .series_primary_image_tag
            .clone()
            .zip(item.series_id.clone())
            .map(|(tag, id)| Art { item_id: id, kind: ImageType::Primary, tag, blurhash: None }),
        Shape::Poster | Shape::Square => own_art(item, ImageType::Primary),
    }
}

/// "1h 52m" / "45m".
fn runtime(ticks: i64) -> Option<String> {
    let mins = ticks / 600_000_000;
    match mins {
        m if m <= 0 => None,
        m if m < 60 => Some(format!("{m}m")),
        m => Some(format!("{}h {:02}m", m / 60, m % 60)),
    }
}

pub fn card(item: &BaseItemDto, shape: Shape) -> Option<Card> {
    let id = item.id.clone()?;
    let name = item.name.clone().unwrap_or_default();
    let is_episode = item.r#type == Some(BaseItemKind::Episode);
    let episode = match (item.parent_index_number, item.index_number) {
        (Some(s), Some(e)) => format!("S{s}:E{e}"),
        (None, Some(e)) => format!("E{e}"),
        _ => String::new(),
    };
    let (title, subtitle) = match (is_episode, item.series_name.as_ref()) {
        (true, Some(series)) if !episode.is_empty() => {
            (series.clone(), format!("{episode} · {name}"))
        }
        (true, Some(series)) => (series.clone(), name),
        _ => (name, item.production_year.map(|y| y.to_string()).unwrap_or_default()),
    };
    let mut meta: Vec<String> = Vec::new();
    if let Some(y) = item.production_year {
        meta.push(y.to_string());
    }
    if let Some(r) = item.official_rating.as_ref().filter(|r| !r.is_empty()) {
        meta.push(r.clone());
    }
    if let Some(rt) = item.run_time_ticks.and_then(runtime) {
        meta.push(rt);
    }
    if let Some(c) = item.community_rating.filter(|c| *c > 0.0) {
        meta.push(format!("{c:.1}/10"));
    }
    let progress = item
        .user_data
        .as_ref()
        .and_then(|u| u.played_percentage)
        .map(|p| (p / 100.0).clamp(0.0, 1.0) as f32)
        .filter(|p| *p > 0.0);
    Some(Card {
        id,
        title,
        subtitle,
        meta: meta.join(" · "),
        overview: item.overview.clone().unwrap_or_default(),
        image: card_image(item, shape),
        backdrop: backdrop(item),
        progress,
    })
}

pub fn row(
    key: impl Into<String>,
    title: impl Into<String>,
    shape: Shape,
    items: &[BaseItemDto],
) -> Option<Row> {
    let cards: Vec<Card> = items.iter().filter_map(|i| card(i, shape)).collect();
    (!cards.is_empty()).then(|| Row { key: key.into(), title: title.into(), shape, cards })
}

/// Shape of a library's Latest row.
pub fn library_shape(c: Option<CollectionType>) -> Shape {
    match c {
        Some(CollectionType::Music) => Shape::Square,
        Some(CollectionType::Homevideos | CollectionType::Photos | CollectionType::Musicvideos) => {
            Shape::Landscape
        }
        _ => Shape::Poster,
    }
}

/// Libraries that get a "Latest" row (as jellyfin-web's home screen).
pub fn has_latest_row(c: Option<CollectionType>) -> bool {
    !matches!(
        c,
        Some(
            CollectionType::Playlists
                | CollectionType::Boxsets
                | CollectionType::Livetv
                | CollectionType::Folders
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> BaseItemDto {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn episode_in_continue_watching() {
        let ep = parse(
            r#"{"Id":"e1","Name":"Pilot","Type":"Episode","SeriesName":"Severance","SeriesId":"s1",
            "ParentIndexNumber":1,"IndexNumber":1,"RunTimeTicks":34200000000,"ProductionYear":2022,
            "ImageTags":{"Primary":"pt"},"ImageBlurHashes":{"Primary":{"pt":"LEHV6nWB2yk8"}},
            "ParentBackdropItemId":"s1","ParentBackdropImageTags":["bt"],
            "UserData":{"PlayedPercentage":42.5,"Played":false}}"#,
        );
        let c = card(&ep, Shape::Landscape).unwrap();
        assert_eq!(c.title, "Severance");
        assert_eq!(c.subtitle, "S1:E1 · Pilot");
        assert_eq!(c.meta, "2022 · 57m");
        let img = c.image.unwrap();
        assert_eq!(
            (img.item_id.as_str(), img.kind, img.tag.as_str()),
            ("e1", ImageType::Primary, "pt")
        );
        assert_eq!(img.blurhash.as_deref(), Some("LEHV6nWB2yk8"));
        let bd = c.backdrop.unwrap();
        assert_eq!((bd.item_id.as_str(), bd.kind), ("s1", ImageType::Backdrop));
        assert!((c.progress.unwrap() - 0.425).abs() < 1e-6);
    }

    #[test]
    fn movie_poster_and_hero() {
        let m = parse(
            r#"{"Id":"m1","Name":"Arrival","Type":"Movie","ProductionYear":2016,"OfficialRating":"PG-13",
            "RunTimeTicks":69600000000,"CommunityRating":7.9,"Overview":"Linguist.",
            "ImageTags":{"Primary":"p","Thumb":"t"},"BackdropImageTags":["b0","b1"]}"#,
        );
        let c = card(&m, Shape::Poster).unwrap();
        assert_eq!(c.subtitle, "2016");
        assert_eq!(c.meta, "2016 · PG-13 · 1h 56m · 7.9/10");
        assert_eq!(c.image.unwrap().tag, "p");
        assert_eq!(c.backdrop.unwrap().tag, "b0");
        assert_eq!(c.progress, None);
        let l = card(&m, Shape::Landscape).unwrap();
        assert_eq!(l.image.unwrap().kind, ImageType::Thumb, "landscape prefers Thumb");
    }

    #[test]
    fn sparse_items_dont_break() {
        assert!(card(&parse(r#"{"Name":"no id"}"#), Shape::Poster).is_none());
        let c = card(&parse(r#"{"Id":"x"}"#), Shape::Poster).unwrap();
        assert!(c.image.is_none() && c.backdrop.is_none());
        assert!(row("k", "Empty", Shape::Poster, &[]).is_none());
    }

    #[test]
    fn library_rows() {
        assert_eq!(library_shape(Some(CollectionType::Music)), Shape::Square);
        assert_eq!(library_shape(Some(CollectionType::Movies)), Shape::Poster);
        assert!(!has_latest_row(Some(CollectionType::Playlists)));
        assert!(has_latest_row(Some(CollectionType::Tvshows)));
        assert!(has_latest_row(None));
    }

    #[test]
    fn cache_round_trip() {
        let d = HomeData {
            server_name: "s".into(),
            user_name: "u".into(),
            rows: vec![row("r", "R", Shape::Poster, &[parse(r#"{"Id":"x","Name":"X"}"#)]).unwrap()],
        };
        let j = serde_json::to_vec(&d).unwrap();
        assert_eq!(serde_json::from_slice::<HomeData>(&j).unwrap(), d);
    }
}
