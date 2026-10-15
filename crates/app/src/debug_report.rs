//! Debug report for one item (D83): everything needed to work out why it
//! doesn't play well, in one text file. Started from the item page; written
//! on a background thread to `<data dir>/debug/` (the service has a private
//! /tmp, so a file there would be invisible outside it).
//!
//! Sections: this machine (OS, GPU decode capabilities, display, audio,
//! packages, settings); the server's view of the item (raw item JSON with
//! its media streams, PlaybackInfo for direct play and for a transcode);
//! jellyroll's decision; a test decode (a hidden mpv plays the stream for a
//! few seconds with hardware decoding and a verbose log); recent log lines.
//! Access tokens are redacted.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::audio::AudioDevice;
use crate::net::DebugServer;

/// What the item page and the app know, handed to the writer thread.
pub struct Input {
    pub title: String,
    pub item_id: String,
    pub server: DebugServer,
    /// Settings → About lines (version, server, user...).
    pub about: Vec<(String, String)>,
    /// The effective settings, as (name, value).
    pub settings: Vec<(String, String)>,
    /// mpv's audio outputs, marked with the screen on each HDMI output.
    pub audio_devices: Vec<AudioDevice>,
    /// Where "automatic" would play now (None: mpv's default).
    pub auto_audio: Option<String>,
}

/// The file written, or why not. Set once, when done.
pub type Outcome = Arc<OnceLock<Result<PathBuf, String>>>;

/// Where the report for `title` will go (shown before it is written).
pub fn path_for(dir: &Path, title: &str, now: SystemTime) -> PathBuf {
    let name: String = title
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let name: String = name.chars().take(48).collect();
    let stamp =
        utc(now).replace([':', '-'], "").replace('T', "-").trim_end_matches('Z').to_string();
    dir.join(format!(
        "jellyroll-debug-{}-{stamp}.txt",
        if name.is_empty() { "item" } else { &name }
    ))
}

/// Writes the report on a background thread to `path`.
pub fn start(input: Input, path: PathBuf) -> Outcome {
    let out: Outcome = Arc::new(OnceLock::new());
    let o = out.clone();
    let spawned = std::thread::Builder::new().name("debug-report".into()).spawn(move || {
        let r = write(&input, &path).map(|_| path);
        match &r {
            Ok(p) => tracing::info!(path = %p.display(), "debug report written"),
            Err(e) => tracing::warn!("debug report: {e}"),
        }
        let _ = o.set(r);
    });
    if let Err(e) = spawned {
        let _ = out.set(Err(e.to_string()));
    }
    out
}

fn write(input: &Input, path: &Path) -> Result<(), String> {
    let text = redact(&build(input));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

fn section(out: &mut String, title: &str) {
    let _ = write!(out, "\n\n===== {title} =====\n\n");
}

fn build(i: &Input) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "jellyroll debug report");
    let _ = writeln!(o, "item: {} ({})", i.title, i.item_id);
    let _ = writeln!(o, "written: {}", utc_now());

    section(&mut o, "This machine");
    for (k, v) in &i.about {
        let _ = writeln!(o, "{k}: {v}");
    }
    let _ = writeln!(o, "os: {}", os_release());
    let _ = writeln!(o, "kernel: {}", read_trim("/proc/sys/kernel/osrelease"));
    let _ = writeln!(o, "cpu: {}", cpu_model());
    let _ = writeln!(o, "memory: {}", mem_total());
    let caps = jellyroll_video::va::probe();
    match &caps {
        Ok(c) => {
            let names = |v: Vec<jellyroll_video::va::Codec>| {
                v.iter().map(|c| c.name()).collect::<Vec<_>>().join(", ")
            };
            let _ = writeln!(o, "va-api: {} on {}", c.driver, c.device);
            let _ = writeln!(o, "va-api decodes: {}", names(c.supported()));
            let _ = writeln!(o, "va-api can't decode: {}", names(c.unsupported()));
            let _ = writeln!(o, "va-api profiles: {:?}", c.va_profiles);
        }
        Err(e) => {
            let _ = writeln!(o, "va-api: {e}");
        }
    }
    let _ = writeln!(o, "LIBVA_DRIVER_NAME: {:?}", std::env::var("LIBVA_DRIVER_NAME").ok());
    let _ = writeln!(o, "\ndisplays (/sys/class/drm):\n{}", drm_connectors());
    let _ = writeln!(o, "packages:\n{}", packages());
    let _ = writeln!(o, "HDR (TV support, connector properties):");
    for l in jellyroll_platform::drm_hdr::report() {
        let _ = writeln!(o, "{l}");
    }

    section(&mut o, "Audio");
    let _ = writeln!(o, "sound cards (/proc/asound/cards):\n{}", read_trim("/proc/asound/cards"));
    let _ = writeln!(o, "\nHDMI outputs with a screen:");
    for s in jellyroll_platform::hdmi_audio::connected() {
        let _ = writeln!(
            o,
            "  card {} PCM {} ({}): {}",
            s.card,
            s.device,
            s.pcm_name,
            if s.monitor.is_empty() { "(no name)" } else { &s.monitor }
        );
    }
    let _ = writeln!(o, "\nmpv audio outputs:");
    for d in &i.audio_devices {
        let _ = writeln!(o, "  {}  =  {}", d.name, d.label());
    }
    let _ = writeln!(
        o,
        "\nautomatic would play to: {}",
        i.auto_audio.as_deref().unwrap_or("mpv default")
    );

    section(&mut o, "Settings");
    for (k, v) in &i.settings {
        let _ = writeln!(o, "{k}: {v}");
    }

    section(&mut o, "Server");
    o.push_str(&i.server.server_info);

    section(&mut o, "Item, as the server describes it (incl. media streams)");
    o.push_str(&i.server.item);

    section(&mut o, "PlaybackInfo: direct play request");
    o.push_str(&i.server.direct.0);
    section(&mut o, "PlaybackInfo: direct play answer");
    o.push_str(&i.server.direct.1);
    section(&mut o, "PlaybackInfo: transcode request");
    o.push_str(&i.server.transcode.0);
    section(&mut o, "PlaybackInfo: transcode answer");
    o.push_str(&i.server.transcode.1);

    section(&mut o, "jellyroll's decision");
    let probe_target = match &i.server.play {
        Ok(p) => {
            let check = p.info.check(caps.as_ref().ok());
            let _ = writeln!(o, "check: {check:?}");
            let _ = writeln!(o, "play method: {:?}", p.info.play_method);
            let _ = writeln!(o, "stream: {}{}", p.base_url, p.info.stream_path);
            let _ = writeln!(o, "video: {:?}", p.info.video);
            let _ = writeln!(o, "fps: {:?}  interlaced: {}", p.info.fps, p.info.interlaced);
            let _ = writeln!(o, "audio tracks: {:?}", p.info.audio);
            let _ = writeln!(o, "default audio: {:?}", p.info.default_audio);
            let _ = writeln!(o, "subtitles: {:?}", p.info.subtitles);
            let _ = writeln!(o, "default subtitle: {:?}", p.info.default_subtitle);
            let _ = writeln!(o, "\nfull PlayInfo:\n{:#?}", p.info);
            Some((format!("{}{}", p.base_url, p.info.stream_path), p.authorization.clone()))
        }
        Err(e) => {
            let _ = writeln!(o, "can't build a playback request: {e}");
            None
        }
    };

    section(&mut o, "Test decode (hidden mpv, hardware decoding, 8 s)");
    match probe_target {
        Some((url, auth)) => {
            let opts = jellyroll_video::probe::ProbeOptions {
                url,
                http_header: Some(format!("Authorization: {auth}")),
                hwdec: "vaapi-copy".into(),
                render_node: caps.as_ref().ok().map(|c| c.device.clone()),
                play_for: Duration::from_secs(8),
                timeout: Duration::from_secs(45),
            };
            match jellyroll_video::probe::probe_stream(&opts) {
                Ok(p) => {
                    let _ = writeln!(o, "outcome: {}\n", p.outcome);
                    for (k, v) in &p.properties {
                        let _ = writeln!(o, "{k}: {v}");
                    }
                    let _ = writeln!(o, "\nmpv log ({} lines):", p.log.len());
                    for l in &p.log {
                        let _ = writeln!(o, "{l}");
                    }
                }
                Err(e) => {
                    let _ = writeln!(o, "could not run: {e}");
                }
            }
        }
        None => {
            let _ = writeln!(o, "skipped: no stream to play (see above)");
        }
    }

    section(&mut o, "jellyroll log (recent)");
    for l in crate::logging::recent() {
        let _ = writeln!(o, "{l}");
    }
    o
}

// ---- system bits ----------------------------------------------------------

fn read_trim(p: &str) -> String {
    std::fs::read_to_string(p).map(|s| s.trim().to_string()).unwrap_or_else(|e| format!("({e})"))
}

fn os_release() -> String {
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                .map(|v| v.trim_matches('"').to_string())
        })
        .unwrap_or_else(|| "(unknown)".into())
}

fn cpu_model() -> String {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split_once(':'))
                .map(|(_, v)| v.trim().to_string())
        })
        .unwrap_or_else(|| "(unknown)".into())
}

fn mem_total() -> String {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            s.lines().next().map(|l| l.trim_start_matches("MemTotal:").trim().to_string())
        })
        .unwrap_or_else(|| "(unknown)".into())
}

/// Each DRM connector's status, and its modes when connected.
fn drm_connectors() -> String {
    let mut out = String::new();
    let Ok(dir) = std::fs::read_dir("/sys/class/drm") else { return "(none)".into() };
    let mut names: Vec<_> = dir
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("card") && n.contains('-'))
        .collect();
    names.sort();
    for n in names {
        let base = format!("/sys/class/drm/{n}");
        let status = read_trim(&format!("{base}/status"));
        let _ = write!(out, "  {n}: {status}");
        if status == "connected" {
            let modes = std::fs::read_to_string(format!("{base}/modes")).unwrap_or_default();
            let mut uniq: Vec<&str> = Vec::new();
            for m in modes.lines() {
                if !uniq.contains(&m) {
                    uniq.push(m);
                }
            }
            let _ = write!(out, " (modes: {})", uniq.join(", "));
        }
        out.push('\n');
    }
    out
}

/// Versions of the packages that decide what plays (Fedora/RPM; elsewhere
/// it just says rpm is missing).
fn packages() -> String {
    const PKGS: &[&str] = &[
        "intel-media-driver",
        "libva-intel-media-driver",
        "libva-intel-driver",
        "mesa-va-drivers",
        "mesa-va-drivers-freeworld",
        "mesa-dri-drivers",
        "libva",
        "ffmpeg",
        "ffmpeg-free",
        "libavcodec-free",
        "mpv-libs",
        "pipewire",
        "alsa-sof-firmware",
        "linux-firmware",
    ];
    match std::process::Command::new("rpm").arg("-q").args(PKGS).output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).lines().map(|l| format!("  {l}\n")).collect(),
        Err(e) => format!("  (rpm: {e})\n"),
    }
}

// ---- redaction and time ------------------------------------------------------

/// Removes access tokens: `api_key=…`/`ApiKey=…` in URLs and `Token="…"` in
/// authorization headers.
pub fn redact(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let lower = s.to_ascii_lowercase();
    let mut i = 0;
    while i < s.len() {
        let rest = &lower[i..];
        let (skip, end_at): (usize, &[char]) = if rest.starts_with("api_key=") {
            (8, &['&', '"', ' ', '\n', '\''])
        } else if rest.starts_with("apikey=") {
            (7, &['&', '"', ' ', '\n', '\''])
        } else if rest.starts_with("token=\"") {
            (7, &['"'])
        } else if rest.starts_with("\"accesstoken\": \"") {
            (16, &['"'])
        } else {
            let c = s[i..].chars().next().unwrap_or(' ');
            out.push(c);
            i += c.len_utf8();
            continue;
        };
        out.push_str(&s[i..i + skip]);
        i += skip;
        let len = s[i..].find(end_at).unwrap_or(s.len() - i);
        if len > 0 {
            out.push_str("REDACTED");
        }
        i += len;
    }
    out
}

pub fn utc_now() -> String {
    utc(SystemTime::now())
}

/// `2026-10-15T01:02:03Z` (no time-zone database needed).
fn utc(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil from days (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_redacted() {
        let s = r#"url http://srv/Videos/1/master.m3u8?ApiKey=abc123&VideoCodec=h264
header Authorization: MediaBrowser Client="Jellyroll", Token="secret", Version="1"
"AccessToken": "tok", api_key=zzz"#;
        let r = redact(s);
        assert!(!r.contains("abc123") && !r.contains("secret") && !r.contains("zzz"), "{r}");
        assert!(!r.contains("\"tok\""), "{r}");
        assert!(r.contains("ApiKey=REDACTED&VideoCodec=h264"), "{r}");
        assert!(r.contains(r#"Token="REDACTED", Version="1""#), "{r}");
        assert_eq!(redact("nothing here ü"), "nothing here ü");
    }

    #[test]
    fn utc_formats_a_known_time() {
        let t = UNIX_EPOCH + Duration::from_secs(1_791_336_123);
        assert_eq!(utc(t), "2026-10-07T01:22:03Z");
        assert_eq!(utc(UNIX_EPOCH), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn file_name_is_safe() {
        let t = UNIX_EPOCH + Duration::from_secs(1_791_336_123);
        let p = path_for(Path::new("/d"), "Dune: Part Two (2024) / Ü", t);
        assert_eq!(p, Path::new("/d/jellyroll-debug-dune-part-two-2024-20261007-012203.txt"));
        assert_eq!(
            path_for(Path::new("/d"), "???", t),
            Path::new("/d/jellyroll-debug-item-20261007-012203.txt")
        );
    }
}
