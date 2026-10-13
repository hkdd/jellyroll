//! Playback requests (spec 7.2): the device profile sent with
//! `PlaybackInfo`, and the session reports (`/Sessions/Playing`, progress,
//! stopped). Hand-written: the profile is policy, not API surface.
//!
//! Policy (D19): direct play only. mpv plays nearly any container, codec
//! and subtitle format, so the profile allows everything for direct play
//! and the request disables transcoding; what the GPU can't decode is
//! reported by the app, never silently transcoded or software-decoded.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::auth::encode_component;
use crate::dto::{MediaSegmentDtoQueryResult, PlaybackInfoResponse};
use crate::endpoints::{Method, Request};

/// Containers mpv plays (direct play profile; codecs unrestricted).
const CONTAINERS: &str = "mkv,webm,mp4,m4v,mov,ts,m2ts,mts,mpegts,avi,mpg,mpeg,vob,wmv,asf,flv,\
                          3gp,ogv,ogg";
/// Text subtitles the server can deliver as separate files (sidecars).
const EXTERNAL_SUBS: [&str; 6] = ["srt", "subrip", "ass", "ssa", "vtt", "webvtt"];
/// Subtitles mpv renders from inside the container.
const EMBEDDED_SUBS: [&str; 11] = [
    "srt", "subrip", "ass", "ssa", "vtt", "webvtt", "pgssub", "dvdsub", "dvbsub", "sub", "mov_text",
];
/// Upper bound for direct streaming (bits/s); the LAN is the limit.
const MAX_BITRATE: u64 = 400_000_000;

pub fn device_profile() -> Value {
    let mut subs: Vec<Value> =
        EXTERNAL_SUBS.iter().map(|f| json!({ "Format": f, "Method": "External" })).collect();
    subs.extend(EMBEDDED_SUBS.iter().map(|f| json!({ "Format": f, "Method": "Embed" })));
    json!({
        "Name": "jellyroll",
        "MaxStreamingBitrate": MAX_BITRATE,
        "MaxStaticBitrate": MAX_BITRATE,
        "DirectPlayProfiles": [
            { "Type": "Video", "Container": CONTAINERS },
            { "Type": "Audio" },
        ],
        "TranscodingProfiles": [],
        "ContainerProfiles": [],
        "CodecProfiles": [],
        "SubtitleProfiles": subs,
    })
}

fn item_path(prefix: &str, item_id: &str, suffix: &str) -> String {
    let mut p = String::from(prefix);
    encode_component(&mut p, item_id);
    p.push_str(suffix);
    p
}

/// `POST /Items/{id}/PlaybackInfo`: media sources with streams, default
/// tracks from the user's preferences, and a play session id.
pub fn playback_info(user_id: &str, item_id: &str) -> Request<PlaybackInfoResponse> {
    let mut path = item_path("/Items/", item_id, "/PlaybackInfo?userId=");
    encode_component(&mut path, user_id);
    Request::new(Method::Post, path).json(json!({
        "UserId": user_id,
        "DeviceProfile": device_profile(),
        "MaxStreamingBitrate": MAX_BITRATE,
        "EnableDirectPlay": true,
        "EnableDirectStream": true,
        "EnableTranscoding": false,
        "AllowVideoStreamCopy": true,
        "AllowAudioStreamCopy": true,
        "AutoOpenLiveStream": true,
    }))
}

/// `GET /MediaSegments/{id}`: intro, outro (credits), recap, preview and
/// commercial ranges (server 10.10+; empty when none were detected).
pub fn media_segments(item_id: &str) -> Request<MediaSegmentDtoQueryResult> {
    Request::new(Method::Get, item_path("/MediaSegments/", item_id, ""))
}

/// Path of trickplay tile sheet `index` at `width` (an image; fetched
/// through the image pipeline).
pub fn trickplay_tile_path(item_id: &str, width: u32, index: u32, media_source_id: &str) -> String {
    let mut p = item_path("/Videos/", item_id, &format!("/Trickplay/{width}/{index}.jpg"));
    p.push_str("?mediaSourceId=");
    encode_component(&mut p, media_source_id);
    p
}

/// How the stream reaches the player (reported to the server).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayMethod {
    DirectPlay,
    DirectStream,
}

/// One playback report. Kept as plain data so unsent reports can be queued
/// and persisted (spec 7.2: exiting offline must not lose progress).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub kind: ReportKind,
    pub item_id: String,
    pub media_source_id: String,
    pub play_session_id: String,
    pub position_ticks: i64,
    pub paused: bool,
    pub audio_stream_index: Option<i32>,
    pub subtitle_stream_index: Option<i32>,
    pub play_method: PlayMethod,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReportKind {
    Start,
    Progress,
    Stopped,
}

impl Report {
    pub fn request(&self) -> Request<()> {
        let path = match self.kind {
            ReportKind::Start => "/Sessions/Playing",
            ReportKind::Progress => "/Sessions/Playing/Progress",
            ReportKind::Stopped => "/Sessions/Playing/Stopped",
        };
        let mut body = json!({
            "ItemId": self.item_id,
            "MediaSourceId": self.media_source_id,
            "PlaySessionId": self.play_session_id,
            "PositionTicks": self.position_ticks,
        });
        if self.kind != ReportKind::Stopped {
            let extra = json!({
                "CanSeek": true,
                "IsPaused": self.paused,
                "IsMuted": false,
                "PlayMethod": self.play_method,
                "AudioStreamIndex": self.audio_stream_index,
                // -1: subtitles off (Jellyfin's convention).
                "SubtitleStreamIndex": self.subtitle_stream_index.unwrap_or(-1),
            });
            if let (Value::Object(b), Value::Object(e)) = (&mut body, extra) {
                b.extend(e);
            }
        }
        Request::new(Method::Post, path.into()).json(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_info_disables_transcoding() {
        let r = playback_info("u 1", "i/2");
        assert_eq!(r.method, Method::Post);
        assert_eq!(r.path, "/Items/i%2F2/PlaybackInfo?userId=u%201");
        let b: Value = serde_json::from_str(r.body.as_deref().unwrap()).unwrap();
        assert_eq!(b["EnableTranscoding"], false);
        assert_eq!(b["DeviceProfile"]["DirectPlayProfiles"][0]["Type"], "Video");
        assert!(b["DeviceProfile"]["DirectPlayProfiles"][0].get("VideoCodec").is_none());
        let subs = b["DeviceProfile"]["SubtitleProfiles"].as_array().unwrap();
        assert!(subs.iter().any(|s| s["Format"] == "srt" && s["Method"] == "External"));
        assert!(subs.iter().any(|s| s["Format"] == "pgssub" && s["Method"] == "Embed"));
    }

    #[test]
    fn reports() {
        let mut r = Report {
            kind: ReportKind::Progress,
            item_id: "i".into(),
            media_source_id: "m".into(),
            play_session_id: "p".into(),
            position_ticks: 42,
            paused: true,
            audio_stream_index: Some(1),
            subtitle_stream_index: None,
            play_method: PlayMethod::DirectPlay,
        };
        let req = r.request();
        assert_eq!(req.path, "/Sessions/Playing/Progress");
        let b: Value = serde_json::from_str(req.body.as_deref().unwrap()).unwrap();
        assert_eq!(b["PositionTicks"], 42);
        assert_eq!(b["IsPaused"], true);
        assert_eq!(b["SubtitleStreamIndex"], -1);
        assert_eq!(b["PlayMethod"], "DirectPlay");
        r.kind = ReportKind::Stopped;
        let b: Value = serde_json::from_str(r.request().body.as_deref().unwrap()).unwrap();
        assert_eq!(b["PositionTicks"], 42);
        assert!(b.get("IsPaused").is_none());
        // Round-trips for the persisted retry queue.
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<Report>(&s).unwrap(), r);
    }

    #[test]
    fn paths() {
        assert_eq!(media_segments("a b").path, "/MediaSegments/a%20b");
        assert_eq!(
            trickplay_tile_path("i", 320, 2, "m"),
            "/Videos/i/Trickplay/320/2.jpg?mediaSourceId=m"
        );
    }
}
