//! What the player needs to play an item (spec 7.2, 7.3), built from the
//! server's answers: the direct stream, tracks mapped to mpv's ids, the
//! user's default tracks, chapters, skippable segments, trickplay tiles and
//! the next episode. Pure, so it is unit-tested without a server.

use jellyfin_api::dto::{
    BaseItemDto, BaseItemKind, MediaSegmentDto, MediaSegmentType, MediaSourceInfo, MediaStream,
    MediaStreamType, PlaybackInfoResponse, SubtitleDeliveryMethod,
};
use jellyfin_api::endpoints::stream_path;
use jellyfin_api::playback::PlayMethod;

use super::model::{self, Card, Shape};

pub const TICKS_PER_SEC: f64 = 10_000_000.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    /// The server's stream index (reported back in progress reports).
    pub index: i32,
    /// mpv's 1-based id among tracks of its type, for embedded tracks.
    /// External subtitles get theirs when added.
    pub mpv_id: Option<i64>,
    /// External subtitle file (server path), added with `sub-add`.
    pub external: Option<String>,
    pub label: String,
    pub language: String,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SegmentKind {
    Intro,
    Recap,
    Preview,
    Commercial,
    Credits,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Segment {
    pub kind: SegmentKind,
    pub start: f64,
    pub end: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Trickplay {
    pub width: u32,
    pub height: u32,
    /// Tiles per sheet: columns x rows.
    pub cols: u32,
    pub rows: u32,
    pub count: u32,
    /// Seconds between thumbnails.
    pub interval: f64,
}

impl Trickplay {
    /// (sheet, column, row) of the thumbnail shown at `secs`.
    pub fn tile(&self, secs: f64) -> (u32, u32, u32) {
        let n = ((secs.max(0.0) / self.interval) as u32).min(self.count.saturating_sub(1));
        let per = (self.cols * self.rows).max(1);
        let i = n % per;
        (n / per, i % self.cols.max(1), i / self.cols.max(1))
    }

    /// Rows in `sheet` (the last one may be partly filled).
    pub fn sheet_rows(&self, sheet: u32) -> u32 {
        let per = self.cols * self.rows;
        let left = self.count.saturating_sub(sheet * per);
        left.div_ceil(self.cols.max(1)).clamp(1, self.rows.max(1))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlayInfo {
    pub item_id: String,
    /// Series or movie name; `subtitle` is "S1:E2 · Episode name" or year.
    pub title: String,
    pub subtitle: String,
    pub is_episode: bool,
    pub media_source_id: String,
    pub play_session_id: String,
    pub play_method: PlayMethod,
    /// Server path of the stream (the client adds the base URL).
    pub stream_path: String,
    pub duration: f64,
    pub start: f64,
    pub audio: Vec<Track>,
    pub subtitles: Vec<Track>,
    /// Server's choice from the user's language preferences.
    pub default_audio: Option<i32>,
    pub default_subtitle: Option<i32>,
    /// (start seconds, name)
    pub chapters: Vec<(f64, String)>,
    pub segments: Vec<Segment>,
    pub trickplay: Option<Trickplay>,
    pub next: Option<Card>,
    /// Video frame rate from the server's stream info, for refresh-rate
    /// matching (None when unknown).
    pub fps: Option<f64>,
    /// The server reports the video as interlaced (deinterlacing, D67).
    pub interlaced: bool,
}

impl PlayInfo {
    /// Applies tracks chosen on the item page over the server's defaults.
    /// A choice this file can't play (e.g. an external subtitle the
    /// server won't deliver) keeps the default.
    pub fn choose(&mut self, c: crate::net::TrackChoice) {
        if let Some(a) = c.audio.filter(|a| self.audio.iter().any(|t| t.index == *a)) {
            self.default_audio = Some(a);
        }
        match c.subtitle {
            Some(-1) => self.default_subtitle = None,
            Some(i) if self.subtitles.iter().any(|t| t.index == i) => {
                self.default_subtitle = Some(i)
            }
            _ => {}
        }
    }
}

pub(super) fn label(s: &MediaStream) -> String {
    if let Some(t) = s.display_title.as_ref().filter(|t| !t.is_empty()) {
        return t.clone();
    }
    let mut parts: Vec<String> = Vec::new();
    if let Some(l) = s.language.as_ref().filter(|l| !l.is_empty()) {
        parts.push(l.clone());
    }
    if let Some(c) = s.codec.as_ref() {
        parts.push(c.to_uppercase());
    }
    if parts.is_empty() { format!("Track {}", s.index.unwrap_or(0)) } else { parts.join(" - ") }
}

/// Tracks of one type, numbered the way mpv numbers them: embedded tracks
/// 1, 2, ... in stream order; external ones are added later.
fn tracks(streams: &[MediaStream], kind: MediaStreamType) -> Vec<Track> {
    let mut v: Vec<&MediaStream> = streams.iter().filter(|s| s.r#type == Some(kind)).collect();
    v.sort_by_key(|s| (s.is_external == Some(true), s.index.unwrap_or(i32::MAX)));
    let mut n = 0;
    v.into_iter()
        .filter_map(|s| {
            let index = s.index?;
            let external = s.is_external == Some(true);
            if external
                && !(s.delivery_method == Some(SubtitleDeliveryMethod::External)
                    && s.delivery_url.is_some())
            {
                // An external file the server won't deliver: unplayable.
                return None;
            }
            let mpv_id = (!external).then(|| {
                n += 1;
                n
            });
            Some(Track {
                index,
                mpv_id,
                external: if external { s.delivery_url.clone() } else { None },
                label: label(s),
                language: s.language.clone().unwrap_or_default(),
            })
        })
        .collect()
}

fn segment(s: &MediaSegmentDto) -> Option<Segment> {
    let kind = match s.r#type? {
        MediaSegmentType::Intro => SegmentKind::Intro,
        MediaSegmentType::Recap => SegmentKind::Recap,
        MediaSegmentType::Preview => SegmentKind::Preview,
        MediaSegmentType::Commercial => SegmentKind::Commercial,
        MediaSegmentType::Outro => SegmentKind::Credits,
        _ => return None,
    };
    let (start, end) = (s.start_ticks? as f64 / TICKS_PER_SEC, s.end_ticks? as f64 / TICKS_PER_SEC);
    (end > start).then_some(Segment { kind, start, end })
}

fn trickplay(item: &BaseItemDto, source: &str) -> Option<Trickplay> {
    let by_width = item.trickplay.as_ref()?.get(source)?.as_ref()?;
    // The smallest width at least 320 px, else the largest: previews are
    // drawn ~320 logical px wide.
    let mut infos: Vec<_> = by_width.values().collect();
    infos.sort_by_key(|i| i.width.unwrap_or(0));
    let i = infos.iter().find(|i| i.width.unwrap_or(0) >= 320).or(infos.last())?;
    let pos = |v: Option<i32>| v.filter(|v| *v > 0).map(|v| v as u32);
    Some(Trickplay {
        width: pos(i.width)?,
        height: pos(i.height)?,
        cols: pos(i.tile_width)?,
        rows: pos(i.tile_height)?,
        count: pos(i.thumbnail_count)?,
        interval: pos(i.interval)? as f64 / 1000.0,
    })
}

/// Builds the player's view of `item`. Errors are user-facing.
pub fn build(
    item: &BaseItemDto,
    info: &PlaybackInfoResponse,
    segments: &[MediaSegmentDto],
    next: Option<&BaseItemDto>,
    start_ticks: i64,
) -> Result<PlayInfo, String> {
    let item_id = item.id.clone().ok_or("the server returned an item without an id")?;
    let sources = info.media_sources.as_deref().unwrap_or_default();
    let source: &MediaSourceInfo = sources.first().ok_or("the server offered no media to play")?;
    let play_method = match (source.supports_direct_play, source.supports_direct_stream) {
        (Some(true), _) => PlayMethod::DirectPlay,
        (_, Some(true)) => PlayMethod::DirectStream,
        // D19: no transcoding.
        _ => {
            return Err("The server can't send this file as it is, and transcoding is off \
                        (direct play only)."
                .into());
        }
    };
    let source_id = source.id.clone().unwrap_or_else(|| item_id.clone());
    let streams = source.media_streams.as_deref().unwrap_or_default();
    let card = model::card(item, Shape::Landscape);
    let (title, subtitle) =
        card.map(|c| (c.title, c.subtitle)).unwrap_or_else(|| (String::new(), String::new()));
    let duration =
        source.run_time_ticks.or(item.run_time_ticks).unwrap_or(0) as f64 / TICKS_PER_SEC;
    let mut segments: Vec<Segment> = segments.iter().filter_map(segment).collect();
    segments.sort_by(|a, b| a.start.total_cmp(&b.start));
    Ok(PlayInfo {
        item_id: item_id.clone(),
        title,
        subtitle,
        is_episode: item.r#type == Some(BaseItemKind::Episode),
        stream_path: stream_path(&item_id, &source_id),
        play_method,
        media_source_id: source_id.clone(),
        play_session_id: info.play_session_id.clone().unwrap_or_default(),
        duration,
        start: start_ticks.max(0) as f64 / TICKS_PER_SEC,
        audio: tracks(streams, MediaStreamType::Audio),
        subtitles: tracks(streams, MediaStreamType::Subtitle),
        default_audio: source.default_audio_stream_index.filter(|i| *i >= 0),
        default_subtitle: source.default_subtitle_stream_index.filter(|i| *i >= 0),
        chapters: item
            .chapters
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter_map(|c| {
                Some((
                    c.start_position_ticks? as f64 / TICKS_PER_SEC,
                    c.name.clone().unwrap_or_default(),
                ))
            })
            .collect(),
        segments,
        trickplay: trickplay(item, &source_id),
        next: next.and_then(|n| model::card(n, Shape::Landscape)),
        fps: frame_rate(streams),
        interlaced: streams
            .iter()
            .any(|s| s.r#type == Some(MediaStreamType::Video) && s.is_interlaced == Some(true)),
    })
}

/// The video's frame rate: the average rate (what the frames actually
/// are), else the base rate, which for interlaced or variable-rate video
/// can be twice the frame rate.
fn frame_rate(streams: &[MediaStream]) -> Option<f64> {
    let v = streams.iter().find(|s| s.r#type == Some(MediaStreamType::Video))?;
    let sane = |r: Option<f32>| r.map(f64::from).filter(|r| (10.0..=121.0).contains(r));
    sane(v.average_frame_rate).or(sane(v.real_frame_rate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jellyfin_api::dto::TrickplayInfoDto;
    use std::collections::HashMap;

    fn stream(kind: MediaStreamType, index: i32) -> MediaStream {
        MediaStream { r#type: Some(kind), index: Some(index), ..Default::default() }
    }

    fn source(streams: Vec<MediaStream>) -> PlaybackInfoResponse {
        PlaybackInfoResponse {
            media_sources: Some(vec![MediaSourceInfo {
                id: Some("src".into()),
                supports_direct_play: Some(true),
                media_streams: Some(streams),
                default_audio_stream_index: Some(2),
                default_subtitle_stream_index: Some(-1),
                run_time_ticks: Some(600 * 10_000_000),
                ..Default::default()
            }]),
            play_session_id: Some("ps".into()),
            ..Default::default()
        }
    }

    fn item() -> BaseItemDto {
        BaseItemDto {
            id: Some("item".into()),
            name: Some("Movie".into()),
            r#type: Some(BaseItemKind::Movie),
            ..Default::default()
        }
    }

    #[test]
    fn tracks_map_to_mpv_ids() {
        let mut ext = stream(MediaStreamType::Subtitle, 9);
        ext.is_external = Some(true);
        ext.delivery_method = Some(SubtitleDeliveryMethod::External);
        ext.delivery_url = Some("/Videos/item/src/Subtitles/9/0/Stream.srt".into());
        let mut undeliverable = stream(MediaStreamType::Subtitle, 10);
        undeliverable.is_external = Some(true);
        let info = source(vec![
            stream(MediaStreamType::Video, 0),
            stream(MediaStreamType::Audio, 2),
            stream(MediaStreamType::Audio, 1),
            stream(MediaStreamType::Subtitle, 4),
            ext,
            undeliverable,
            stream(MediaStreamType::Subtitle, 3),
        ]);
        let p = build(&item(), &info, &[], None, 0).unwrap();
        let a: Vec<_> = p.audio.iter().map(|t| (t.index, t.mpv_id)).collect();
        assert_eq!(a, [(1, Some(1)), (2, Some(2))], "mpv numbers in stream order");
        let s: Vec<_> =
            p.subtitles.iter().map(|t| (t.index, t.mpv_id, t.external.is_some())).collect();
        assert_eq!(s, [(3, Some(1), false), (4, Some(2), false), (9, None, true)]);
        assert_eq!((p.default_audio, p.default_subtitle), (Some(2), None));
        assert_eq!(p.duration, 600.0);
        assert_eq!(p.stream_path, "/Videos/item/stream?static=true&mediaSourceId=src");
        assert_eq!(p.play_method, PlayMethod::DirectPlay);
    }

    #[test]
    fn no_direct_play_is_an_error() {
        let mut info = source(vec![]);
        info.media_sources.as_mut().unwrap()[0].supports_direct_play = Some(false);
        assert!(build(&item(), &info, &[], None, 0).unwrap_err().contains("transcoding is off"));
    }

    #[test]
    fn segments_chapters_and_trickplay() {
        let mut it = item();
        it.chapters = Some(vec![jellyfin_api::dto::ChapterInfo {
            start_position_ticks: Some(30 * 10_000_000),
            name: Some("Two".into()),
        }]);
        let tp = TrickplayInfoDto {
            width: Some(320),
            height: Some(180),
            tile_width: Some(10),
            tile_height: Some(10),
            thumbnail_count: Some(150),
            interval: Some(10_000),
            bandwidth: None,
        };
        it.trickplay =
            Some(HashMap::from([("src".to_string(), Some(HashMap::from([("320".into(), tp)])))]));
        let segs = [
            MediaSegmentDto {
                r#type: Some(MediaSegmentType::Outro),
                start_ticks: Some(500 * 10_000_000),
                end_ticks: Some(600 * 10_000_000),
                ..Default::default()
            },
            MediaSegmentDto {
                r#type: Some(MediaSegmentType::Intro),
                start_ticks: Some(10 * 10_000_000),
                end_ticks: Some(40 * 10_000_000),
                ..Default::default()
            },
        ];
        let p = build(&it, &source(vec![]), &segs, None, 12 * 10_000_000).unwrap();
        assert_eq!(p.chapters, [(30.0, "Two".to_string())]);
        assert_eq!(p.segments[0], Segment { kind: SegmentKind::Intro, start: 10.0, end: 40.0 });
        assert_eq!(p.segments[1].kind, SegmentKind::Credits);
        assert_eq!(p.start, 12.0);
        let t = p.trickplay.unwrap();
        assert_eq!(t.tile(0.0), (0, 0, 0));
        assert_eq!(t.tile(125.0), (0, 2, 1), "thumbnail 12: column 2, row 1");
        assert_eq!(t.tile(1105.0), (1, 0, 1), "thumbnail 110 is on sheet 1");
        assert_eq!(t.tile(1e9), (1, 9, 4), "clamped to the last thumbnail (149)");
        assert_eq!((t.sheet_rows(0), t.sheet_rows(1)), (10, 5));
    }

    #[test]
    fn choices_from_the_item_page_override_defaults() {
        use crate::net::TrackChoice;
        let mk = |index| Track {
            index,
            mpv_id: Some(index as i64),
            external: None,
            label: String::new(),
            language: String::new(),
        };
        let mut i = PlayInfo {
            item_id: String::new(),
            title: String::new(),
            subtitle: String::new(),
            is_episode: false,
            media_source_id: String::new(),
            play_session_id: String::new(),
            play_method: PlayMethod::DirectPlay,
            stream_path: String::new(),
            duration: 0.0,
            start: 0.0,
            audio: vec![mk(1), mk(2)],
            subtitles: vec![mk(3)],
            default_audio: Some(1),
            default_subtitle: Some(3),
            chapters: Vec::new(),
            segments: Vec::new(),
            trickplay: None,
            next: None,
            fps: None,
            interlaced: false,
        };
        i.choose(TrackChoice::default());
        assert_eq!((i.default_audio, i.default_subtitle), (Some(1), Some(3)));
        i.choose(TrackChoice { audio: Some(9), subtitle: Some(-1) });
        assert_eq!((i.default_audio, i.default_subtitle), (Some(1), None), "unknown audio kept");
        i.choose(TrackChoice { audio: Some(2), subtitle: Some(3) });
        assert_eq!((i.default_audio, i.default_subtitle), (Some(2), Some(3)));
    }
}
