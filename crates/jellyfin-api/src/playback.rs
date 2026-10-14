//! Playback requests (spec 7.2): the device profile sent with
//! `PlaybackInfo`, and the session reports (`/Sessions/Playing`, progress,
//! stopped). Hand-written: the profile is policy, not API surface.
//!
//! Policy (D19, D71): direct play first. mpv plays nearly any container,
//! codec and subtitle format, so the profile allows everything for direct
//! play and the first request disables transcoding. When that can't work
//! (the server can't send the file as it is, or this GPU can't decode it)
//! the app asks; a server transcode is a second request with direct play
//! off and an H.264 transcoding profile, where the server picks the rest
//! as it does for Jellyfin's own apps.

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

/// The device profile; `transcode`: for a server transcode (text
/// subtitles as files, the rest burned in; H.264 over HLS).
pub fn device_profile(transcode: bool) -> Value {
    let mut subs: Vec<Value> =
        EXTERNAL_SUBS.iter().map(|f| json!({ "Format": f, "Method": "External" })).collect();
    if transcode {
        subs.extend(
            EMBEDDED_SUBS
                .iter()
                .filter(|f| !EXTERNAL_SUBS.contains(f))
                .map(|f| json!({ "Format": f, "Method": "Encode" })),
        );
    } else {
        subs.extend(EMBEDDED_SUBS.iter().map(|f| json!({ "Format": f, "Method": "Embed" })));
    }
    let transcoding = if transcode {
        json!([{
            "Type": "Video", "Container": "ts", "Protocol": "hls", "Context": "Streaming",
            "VideoCodec": "h264", "AudioCodec": "aac,ac3,eac3,mp3",
            "MaxAudioChannels": "6", "BreakOnNonKeyFrames": true,
        }])
    } else {
        json!([])
    };
    json!({
        "Name": "jellyroll",
        "MaxStreamingBitrate": MAX_BITRATE,
        "MaxStaticBitrate": MAX_BITRATE,
        "DirectPlayProfiles": [
            { "Type": "Video", "Container": CONTAINERS },
            { "Type": "Audio" },
        ],
        "TranscodingProfiles": transcoding,
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

/// A server transcode's starting point and tracks.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TranscodeRequest {
    pub start_ticks: i64,
    pub audio_stream_index: Option<i32>,
    /// -1: no subtitles.
    pub subtitle_stream_index: Option<i32>,
}

/// `POST /Items/{id}/PlaybackInfo`: media sources with streams, default
/// tracks from the user's preferences, and a play session id. With
/// `transcode`, direct play is off and the source carries a
/// `TranscodingUrl`.
pub fn playback_info(
    user_id: &str,
    item_id: &str,
    transcode: Option<&TranscodeRequest>,
) -> Request<PlaybackInfoResponse> {
    let mut path = item_path("/Items/", item_id, "/PlaybackInfo?userId=");
    encode_component(&mut path, user_id);
    let mut body = json!({
        "UserId": user_id,
        "DeviceProfile": device_profile(transcode.is_some()),
        "MaxStreamingBitrate": MAX_BITRATE,
        "EnableDirectPlay": transcode.is_none(),
        "EnableDirectStream": transcode.is_none(),
        "EnableTranscoding": transcode.is_some(),
        "AllowVideoStreamCopy": true,
        "AllowAudioStreamCopy": true,
        "AutoOpenLiveStream": true,
    });
    if let Some(t) = transcode {
        body["StartTimeTicks"] = json!(t.start_ticks);
        if let Some(a) = t.audio_stream_index {
            body["AudioStreamIndex"] = json!(a);
        }
        if let Some(s) = t.subtitle_stream_index {
            body["SubtitleStreamIndex"] = json!(s);
        }
    }
    Request::new(Method::Post, path).json(body)
}

/// `DELETE /Videos/ActiveEncodings`: stops this device's server transcode
/// for a play session (Jellyfin's apps call it when playback stops).
pub fn stop_encoding(device_id: &str, play_session_id: &str) -> Request<()> {
    let mut path = String::from("/Videos/ActiveEncodings?deviceId=");
    encode_component(&mut path, device_id);
    path.push_str("&playSessionId=");
    encode_component(&mut path, play_session_id);
    Request::new(Method::Delete, path)
}

/// A `TranscodingUrl` without the access token the server puts in it
/// (`ApiKey=`/`api_key=`): the player sends the token in a header, and a
/// URL ends up in logs.
pub fn strip_token(url: &str) -> String {
    let Some((path, query)) = url.split_once('?') else { return url.to_string() };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|kv| {
            let k = kv.split('=').next().unwrap_or("");
            !k.eq_ignore_ascii_case("api_key") && !k.eq_ignore_ascii_case("apikey")
        })
        .collect();
    if kept.is_empty() { path.to_string() } else { format!("{path}?{}", kept.join("&")) }
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
    Transcode,
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
    /// Whose playback this is (not sent: the token says that). Queued
    /// reports go out only while this user is signed in; empty in reports
    /// saved by older versions.
    #[serde(default)]
    pub user_id: String,
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
        let r = playback_info("u 1", "i/2", None);
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
    fn transcode_request_and_cleanup() {
        let t = TranscodeRequest {
            start_ticks: 50,
            audio_stream_index: Some(2),
            subtitle_stream_index: Some(-1),
        };
        let r = playback_info("u", "i", Some(&t));
        let b: Value = serde_json::from_str(r.body.as_deref().unwrap()).unwrap();
        assert_eq!(
            (b["EnableDirectPlay"].clone(), b["EnableTranscoding"].clone()),
            (json!(false), json!(true))
        );
        assert_eq!(b["StartTimeTicks"], 50);
        assert_eq!(b["AudioStreamIndex"], 2);
        assert_eq!(b["SubtitleStreamIndex"], -1);
        let tp = &b["DeviceProfile"]["TranscodingProfiles"][0];
        assert_eq!(
            (tp["VideoCodec"].as_str(), tp["Protocol"].as_str()),
            (Some("h264"), Some("hls"))
        );
        let subs = b["DeviceProfile"]["SubtitleProfiles"].as_array().unwrap();
        assert!(subs.iter().any(|s| s["Format"] == "pgssub" && s["Method"] == "Encode"));
        let d = stop_encoding("dev 1", "ps");
        assert_eq!(
            (d.method, d.path.as_str()),
            (Method::Delete, "/Videos/ActiveEncodings?deviceId=dev%201&playSessionId=ps")
        );
        assert_eq!(
            strip_token("/videos/i/master.m3u8?DeviceId=d&ApiKey=secret&VideoCodec=h264"),
            "/videos/i/master.m3u8?DeviceId=d&VideoCodec=h264"
        );
        assert_eq!(strip_token("/a?api_key=x"), "/a");
        assert_eq!(strip_token("/a"), "/a");
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
            user_id: String::new(),
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
