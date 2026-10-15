//! A test decode for debug reports (D83): play a stream for a few seconds
//! in a hidden mpv (no video or audio output, hardware decoding copied back
//! to memory, verbose log) and report what mpv found and did.
//!
//! It blocks while it runs (it waits on mpv's events), so call it from a
//! background thread, never the render thread.

use std::ffi::{CStr, c_char};
use std::time::{Duration, Instant};

use crate::ffi::*;
use crate::{cstr, err};

pub struct ProbeOptions {
    pub url: String,
    /// `Authorization: …` and the like (mpv `http-header-fields`).
    pub http_header: Option<String>,
    /// mpv `hwdec`, e.g. "vaapi-copy" ("-copy": no video output to hand
    /// frames to).
    pub hwdec: String,
    /// DRM render node for VA-API, e.g. "/dev/dri/renderD128".
    pub render_node: Option<String>,
    /// How long to play once the first frame is decoded.
    pub play_for: Duration,
    /// Give up after this long in total (slow server, stuck demuxer).
    pub timeout: Duration,
}

pub struct Probe {
    /// How it ended, e.g. "played 8.0 s" or "ended: loading failed".
    pub outcome: String,
    /// (property, value as mpv prints it; JSON for lists and maps).
    pub properties: Vec<(String, String)>,
    /// mpv's log at verbose level: "[level] [module] text".
    pub log: Vec<String>,
}

/// Properties that say what the stream is and how it was decoded.
const PROPERTIES: &[&str] = &[
    "mpv-version",
    "ffmpeg-version",
    "file-format",
    "current-demuxer",
    "duration",
    "time-pos",
    "track-list",
    "video-codec",
    "video-format",
    "video-params",
    "video-dec-params",
    "hwdec",
    "hwdec-current",
    "container-fps",
    "estimated-vf-fps",
    "video-bitrate",
    "audio-codec",
    "audio-codec-name",
    "audio-params",
    "audio-bitrate",
    "decoder-frame-drop-count",
    "frame-drop-count",
    "demuxer-cache-state",
    "cache-speed",
    "metadata",
    "chapter-list",
];

const MAX_LOG_LINES: usize = 5000;

pub fn probe_stream(o: &ProbeOptions) -> Result<Probe, String> {
    // SAFETY: no preconditions; null means failure.
    let mpv = unsafe { mpv_create() };
    if mpv.is_null() {
        return Err("mpv_create failed".into());
    }
    let result = run(mpv, o);
    // SAFETY: handle valid; destroyed once, no render context.
    unsafe { mpv_terminate_destroy(mpv) };
    result
}

fn run(mpv: *mut mpv_handle, o: &ProbeOptions) -> Result<Probe, String> {
    let mut opts = vec![
        ("config", "no".to_string()),
        ("terminal", "no".to_string()),
        ("load-scripts", "no".to_string()),
        ("vo", "null".to_string()),
        ("ao", "null".to_string()),
        ("idle", "no".to_string()),
        ("keep-open", "no".to_string()),
        ("hwdec", o.hwdec.clone()),
    ];
    if let Some(h) = &o.http_header {
        opts.push(("http-header-fields", h.clone()));
    }
    if let Some(n) = &o.render_node {
        opts.push(("vaapi-device", n.clone()));
    }
    for (k, v) in &opts {
        let (k, v) = (cstr(k), cstr(v));
        // SAFETY: handle valid and not initialized; strings NUL-terminated.
        let rc = unsafe { mpv_set_option_string(mpv, k.as_ptr(), v.as_ptr()) };
        if rc < 0 {
            return Err(format!("mpv option {k:?}: {}", err(rc)));
        }
    }
    // SAFETY: handle valid; options set.
    let rc = unsafe { mpv_initialize(mpv) };
    if rc < 0 {
        return Err(format!("mpv init: {}", err(rc)));
    }
    let level = cstr("v");
    // SAFETY: handle valid; level NUL-terminated.
    unsafe { mpv_request_log_messages(mpv, level.as_ptr()) };
    let args = [cstr("loadfile"), cstr(&o.url)];
    let mut ptrs: Vec<*const c_char> = args.iter().map(|c| c.as_ptr()).collect();
    ptrs.push(std::ptr::null());
    // SAFETY: NULL-terminated array of NUL-terminated strings; mpv copies.
    let rc = unsafe { mpv_command_async(mpv, 0, ptrs.as_mut_ptr()) };
    if rc < 0 {
        return Err(format!("mpv loadfile: {}", err(rc)));
    }

    let start = Instant::now();
    let mut log = Vec::new();
    let mut playing_since: Option<Instant> = None;
    let outcome = loop {
        if let Some(t) = playing_since
            && t.elapsed() >= o.play_for
        {
            break format!("played {:.1} s after the first frame", t.elapsed().as_secs_f64());
        }
        if start.elapsed() >= o.timeout {
            break format!(
                "gave up after {:.0} s ({})",
                o.timeout.as_secs_f64(),
                if playing_since.is_some() { "playing" } else { "no frame decoded yet" }
            );
        }
        // SAFETY: handle valid; the event is owned by mpv and valid until
        // the next wait_event. Blocking is fine off the render thread.
        let ev = unsafe { &*mpv_wait_event(mpv, 0.25) };
        match ev.event_id {
            MPV_EVENT_LOG_MESSAGE if log.len() < MAX_LOG_LINES => {
                // SAFETY: data points to mpv_event_log_message for this id;
                // its strings are NUL-terminated and valid until next wait.
                let (level, prefix, text) = unsafe {
                    let m = &*(ev.data as *const mpv_event_log_message);
                    (lossy(m.level), lossy(m.prefix), lossy(m.text))
                };
                log.push(format!("[{level}] [{prefix}] {}", text.trim_end()));
            }
            MPV_EVENT_PLAYBACK_RESTART if playing_since.is_none() => {
                playing_since = Some(Instant::now());
            }
            MPV_EVENT_END_FILE => {
                // SAFETY: data points to mpv_event_end_file for this id.
                let e = unsafe { &*(ev.data as *const mpv_event_end_file) };
                break match e.error {
                    0 => format!("file ended (reason {})", e.reason),
                    code => format!("file ended with an error: {}", err(code)),
                };
            }
            MPV_EVENT_SHUTDOWN => break "mpv shut down".into(),
            _ => {}
        }
    };
    let properties =
        PROPERTIES.iter().map(|p| (p.to_string(), property(mpv, p))).collect::<Vec<_>>();
    Ok(Probe { outcome, properties, log })
}

fn lossy(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: caller passes a NUL-terminated string valid for this call.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// A property as mpv formats it (JSON for nodes); "(unavailable)" if unset.
fn property(mpv: *mut mpv_handle, name: &str) -> String {
    let n = cstr(name);
    // SAFETY: handle valid; name NUL-terminated; mpv allocates the result.
    let p = unsafe { mpv_get_property_string(mpv, n.as_ptr()) };
    if p.is_null() {
        return "(unavailable)".into();
    }
    let s = lossy(p);
    // SAFETY: allocated by mpv; freed once.
    unsafe { mpv_free(p.cast()) };
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_reports_an_error_not_a_hang() {
        let o = ProbeOptions {
            url: "/nonexistent/jellyroll-probe.mkv".into(),
            http_header: None,
            hwdec: "no".into(),
            render_node: None,
            play_for: Duration::from_secs(1),
            timeout: Duration::from_secs(10),
        };
        let p = probe_stream(&o).unwrap();
        assert!(p.outcome.contains("error") || p.outcome.contains("ended"), "{}", p.outcome);
        assert!(!p.log.is_empty(), "verbose log captured");
        assert!(p.properties.iter().any(|(k, v)| k == "mpv-version" && v.contains("mpv")));
    }
}
