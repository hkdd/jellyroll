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

/// What a card is, so Select knows what to open.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Movie,
    Series,
    Season,
    Episode,
    /// A library (My Media tile): opens its grid.
    Library,
    /// A cast/crew member.
    Person,
    /// A playlist: its page lists the entries.
    Playlist,
    /// A collection (box set) or plain folder: its page lists the items.
    Folder,
    #[default]
    Other,
}

impl Kind {
    pub fn from_item(item: &BaseItemDto) -> Kind {
        match item.r#type {
            Some(BaseItemKind::Movie) => Kind::Movie,
            Some(BaseItemKind::Series) => Kind::Series,
            Some(BaseItemKind::Season) => Kind::Season,
            Some(BaseItemKind::Episode) => Kind::Episode,
            Some(BaseItemKind::CollectionFolder | BaseItemKind::UserView) => Kind::Library,
            Some(BaseItemKind::Playlist) => Kind::Playlist,
            Some(BaseItemKind::Person) => Kind::Person,
            Some(BaseItemKind::BoxSet | BaseItemKind::Folder) => Kind::Folder,
            _ => Kind::Other,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Card {
    pub id: String,
    #[serde(default)]
    pub kind: Kind,
    /// Library type, for library tiles (decides the grid's item types).
    #[serde(default)]
    pub collection: Option<CollectionType>,
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
    /// Watched (a series: every episode).
    #[serde(default)]
    pub played: bool,
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
    /// Shown when the focused card has none: the newest movie's (D76).
    #[serde(default)]
    pub backdrop: Option<Art>,
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
pub fn backdrop(item: &BaseItemDto) -> Option<Art> {
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
        kind: Kind::from_item(item),
        collection: item.collection_type,
        title,
        subtitle,
        meta: meta.join(" · "),
        overview: item.overview.clone().unwrap_or_default(),
        image: card_image(item, shape),
        backdrop: backdrop(item),
        progress,
        played: item.user_data.as_ref().and_then(|u| u.played).unwrap_or(false),
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

// ---- detail pages -------------------------------------------------------------

/// A codec/format badge on a detail page. `decode` is the video codec the
/// GPU must decode, so the UI can flag content this machine can't play in
/// hardware (D19).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Badge {
    pub text: String,
    pub decode: Option<jellyroll_video::va::Codec>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Detail {
    pub card: Card,
    pub tagline: String,
    pub genres: String,
    pub badges: Vec<Badge>,
    pub played: bool,
    pub favorite: bool,
    pub resume_ticks: i64,
    pub people: Vec<Card>,
    pub series_id: Option<String>,
    pub season_id: Option<String>,
    /// For playback: the item's default media source.
    pub media_source_id: String,
    /// Its tracks, for choosing before Play: (server stream index, label).
    pub audio: Vec<(i32, String)>,
    pub subtitles: Vec<(i32, String)>,
    /// What the server would pick for this user (language preferences).
    pub default_audio: Option<i32>,
    pub default_subtitle: Option<i32>,
}

/// Tracks of one type in the item's default media source.
fn track_options(
    streams: &[jellyfin_api::dto::MediaStream],
    kind: jellyfin_api::dto::MediaStreamType,
) -> Vec<(i32, String)> {
    let mut v: Vec<(i32, String)> = streams
        .iter()
        .filter(|s| s.r#type == Some(kind))
        .filter_map(|s| Some((s.index?, super::playinfo::label(s))))
        .collect();
    v.sort_by_key(|t| t.0);
    v
}

pub(crate) fn video_codec(
    codec: &str,
    bit_depth: i32,
    profile: &str,
) -> Option<jellyroll_video::va::Codec> {
    use jellyroll_video::va::Codec as C;
    let ten = bit_depth >= 10 || profile.contains("10");
    Some(match codec.to_ascii_lowercase().as_str() {
        "h264" | "avc" => C::H264,
        "hevc" | "h265" if ten => C::Hevc10,
        "hevc" | "h265" => C::Hevc8,
        "vp9" if ten => C::Vp9_10,
        "vp9" => C::Vp9_8,
        "av1" => C::Av1,
        "mpeg2video" | "mpeg2" => C::Mpeg2,
        "vc1" => C::Vc1,
        "vp8" => C::Vp8,
        _ => return None,
    })
}

/// Badges for the default video and audio streams and subtitles.
pub fn badges(item: &BaseItemDto) -> Vec<Badge> {
    use jellyfin_api::dto::{AudioSpatialFormat, MediaStreamType, VideoRangeType as V};
    let streams = item.media_streams.as_deref().unwrap_or_default();
    let mut out = Vec::new();
    let pick = |t: MediaStreamType| {
        let mut it = streams.iter().filter(move |s| s.r#type == Some(t));
        let all: Vec<_> = it.by_ref().collect();
        all.iter().find(|s| s.is_default == Some(true)).or(all.first()).copied()
    };
    if let Some(v) = pick(MediaStreamType::Video) {
        let (w, h) = (v.width.unwrap_or(0), v.height.unwrap_or(0));
        let res = if w >= 3800 || h >= 2100 {
            "4K"
        } else if w >= 1900 || h >= 1050 {
            "1080p"
        } else if w >= 1270 || h >= 700 {
            "720p"
        } else if w > 0 {
            "SD"
        } else {
            ""
        };
        if !res.is_empty() {
            out.push(Badge { text: res.into(), decode: None });
        }
        let codec = v.codec.as_deref().unwrap_or("");
        let profile = v.profile.as_deref().unwrap_or("");
        let decode = video_codec(codec, v.bit_depth.unwrap_or(8), profile);
        if let Some(c) = decode {
            out.push(Badge { text: c.name().into(), decode });
        }
        let range = match v.video_range_type {
            Some(V::HDR10) => "HDR10",
            Some(V::HDR10Plus) => "HDR10+",
            Some(V::HLG) => "HLG",
            Some(
                V::DOVI
                | V::DOVIWithHDR10
                | V::DOVIWithHLG
                | V::DOVIWithSDR
                | V::DOVIWithEL
                | V::DOVIWithHDR10Plus
                | V::DOVIWithELHDR10Plus,
            ) => "Dolby Vision",
            _ => "",
        };
        if !range.is_empty() {
            out.push(Badge { text: range.into(), decode: None });
        }
    }
    if let Some(a) = pick(MediaStreamType::Audio) {
        let codec = match a.codec.as_deref().unwrap_or("").to_ascii_lowercase().as_str() {
            "truehd" => "TrueHD",
            "eac3" => "Dolby Digital+",
            "ac3" => "Dolby Digital",
            "dts" if a.profile.as_deref().is_some_and(|p| p.contains("MA")) => "DTS-HD MA",
            "dts" => "DTS",
            "aac" => "AAC",
            "flac" => "FLAC",
            "opus" => "Opus",
            "mp3" => "MP3",
            "" => "",
            _ => "Audio",
        };
        let ch = match a.channels.unwrap_or(0) {
            8 => "7.1",
            6 => "5.1",
            2 => "Stereo",
            1 => "Mono",
            _ => "",
        };
        let text =
            [codec, ch].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" ");
        if !text.is_empty() {
            out.push(Badge { text, decode: None });
        }
        match a.audio_spatial_format {
            Some(AudioSpatialFormat::DolbyAtmos) => {
                out.push(Badge { text: "Atmos".into(), decode: None })
            }
            Some(AudioSpatialFormat::DTSX) => {
                out.push(Badge { text: "DTS:X".into(), decode: None })
            }
            _ => {}
        }
    }
    if streams.iter().any(|s| s.r#type == Some(MediaStreamType::Subtitle)) {
        out.push(Badge { text: "Subtitles".into(), decode: None });
    }
    out
}

fn person(p: &jellyfin_api::dto::BaseItemPerson) -> Option<Card> {
    let id = p.id.clone()?;
    Some(Card {
        image: p.primary_image_tag.clone().map(|tag| Art {
            item_id: id.clone(),
            kind: ImageType::Primary,
            tag,
            blurhash: None,
        }),
        id,
        kind: Kind::Person,
        collection: None,
        title: p.name.clone().unwrap_or_default(),
        subtitle: p.role.clone().unwrap_or_default(),
        meta: String::new(),
        overview: String::new(),
        backdrop: None,
        progress: None,
        played: false,
    })
}

pub fn detail(item: &BaseItemDto) -> Option<Detail> {
    use jellyfin_api::dto::MediaStreamType;
    let card = card(item, Shape::Poster)?;
    let ud = item.user_data.as_ref();
    // The default media source carries the server's per-user default
    // tracks; older answers without it fall back to the item's streams.
    let source = item.media_sources.as_deref().and_then(|s| s.first());
    let streams = source
        .and_then(|s| s.media_streams.as_deref())
        .or(item.media_streams.as_deref())
        .unwrap_or_default();
    let audio = track_options(streams, MediaStreamType::Audio);
    let subtitles = track_options(streams, MediaStreamType::Subtitle);
    let known =
        |list: &[(i32, String)], i: Option<i32>| i.filter(|i| list.iter().any(|t| t.0 == *i));
    let default_audio = known(&audio, source.and_then(|s| s.default_audio_stream_index))
        .or(audio.first().map(|t| t.0));
    let default_subtitle = known(&subtitles, source.and_then(|s| s.default_subtitle_stream_index));
    let mut genres = item.genres.clone().unwrap_or_default();
    genres.truncate(4);
    Some(Detail {
        tagline: item.taglines.as_ref().and_then(|t| t.first().cloned()).unwrap_or_default(),
        genres: genres.join(" · "),
        badges: badges(item),
        played: ud.and_then(|u| u.played).unwrap_or(false),
        favorite: ud.and_then(|u| u.is_favorite).unwrap_or(false),
        resume_ticks: ud.and_then(|u| u.playback_position_ticks).unwrap_or(0),
        people: item.people.iter().flatten().filter_map(person).take(24).collect(),
        series_id: item.series_id.clone(),
        season_id: item.season_id.clone(),
        media_source_id: source.and_then(|s| s.id.clone()).unwrap_or_else(|| card.id.clone()),
        audio,
        subtitles,
        default_audio,
        default_subtitle,
        card,
    })
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
    fn detail_badges_flag_the_decoder() {
        let m = parse(
            r#"{"Id":"m","Name":"X","Type":"Movie","Genres":["Drama","Sci-Fi"],
            "UserData":{"PlaybackPositionTicks":600000000,"IsFavorite":true,"Played":false},
            "People":[{"Id":"p1","Name":"Amy Adams","Role":"Louise","PrimaryImageTag":"pt"},{"Name":"no id"}],
            "MediaStreams":[
              {"Type":"Video","Codec":"hevc","BitDepth":10,"Width":3840,"Height":2160,"VideoRangeType":"HDR10"},
              {"Type":"Audio","Codec":"truehd","Channels":8,"IsDefault":true,"AudioSpatialFormat":"DolbyAtmos"},
              {"Type":"Audio","Codec":"aac","Channels":2},
              {"Type":"Subtitle","Codec":"srt"}]}"#,
        );
        let d = detail(&m).unwrap();
        let texts: Vec<&str> = d.badges.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(texts, ["4K", "HEVC 10-bit", "HDR10", "TrueHD 7.1", "Atmos", "Subtitles"]);
        assert_eq!(d.badges[1].decode, Some(jellyroll_video::va::Codec::Hevc10));
        assert_eq!(d.genres, "Drama · Sci-Fi");
        assert!(d.favorite && !d.played);
        assert_eq!(d.resume_ticks, 600_000_000);
        assert_eq!(d.people.len(), 1, "people without an id are skipped");
        assert_eq!(d.people[0].subtitle, "Louise");
        assert_eq!(d.card.kind, Kind::Movie);
    }

    #[test]
    fn cache_round_trip() {
        let d = HomeData {
            server_name: "s".into(),
            user_name: "u".into(),
            backdrop: None,
            rows: vec![row("r", "R", Shape::Poster, &[parse(r#"{"Id":"x","Name":"X"}"#)]).unwrap()],
        };
        let j = serde_json::to_vec(&d).unwrap();
        assert_eq!(serde_json::from_slice::<HomeData>(&j).unwrap(), d);
    }
}
