//! User configuration (spec 11): `~/.config/jellyroll/config.toml`.
//!
//! Read-only: the app never writes this file (settings changed in the UI go
//! to the state store), so user comments and formatting are never lost.
//!
//! Loading never fails. A missing file means defaults; a missing key means
//! that key's default; an unknown key, an out-of-range value or a file that
//! does not parse produces a warning (logged, and shown on screen) and falls
//! back to defaults. Precedence: defaults < config file < command line.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::input::RepeatConfig;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub log: Log,
    pub server: Server,
    pub display: Display,
    pub audio: Audio,
    pub playback: Playback,
    pub input: Input,
    pub cache: Cache,
    pub debug: Debug,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Server {
    /// Server address, e.g. "http://192.168.1.10:8096". Empty: the last
    /// server signed in to, else the first one found on the LAN.
    pub url: String,
    /// PEM file with an extra CA or the server's self-signed certificate
    /// (spec 12). Empty: system roots only.
    pub ca_file: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Cache {
    /// Disk cache for downloaded artwork (spec 9).
    pub images_mb: u64,
    /// GPU memory for artwork textures (spec 2: hard cap, LRU).
    pub textures_mb: u64,
}

impl Default for Cache {
    fn default() -> Self {
        Self { images_mb: 2048, textures_mb: 512 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Log {
    /// `RUST_LOG` syntax: `info`, or `target=level,...`. RUST_LOG wins.
    pub level: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Display {
    /// Fraction of each screen edge kept clear for TV overscan.
    pub safe_area: f32,
    /// Double instead of triple buffering on KMSDRM.
    pub double_buffer: bool,
    /// Resolution the UI is drawn at (spec 6.4). Video always plays at the
    /// output's full resolution. A choice made in Settings overrides this.
    pub ui_resolution: UiResolution,
    /// Switch the output to the video's frame rate while playing (spec
    /// 5.2). Settings can turn it off per output resolution.
    pub match_refresh: bool,
    /// Wait after a refresh switch before playback starts, for TVs that
    /// take a while to show the new mode (the start would be missed).
    pub refresh_switch_delay_ms: u64,
    /// Minutes without input before the screensaver starts (while
    /// browsing or paused); 0 = off. Settings offers 5, 10, 15 or off.
    pub screensaver_minutes: u64,
}

/// Audio output (spec 7.1). Settings choices win over these.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Audio {
    /// mpv audio device ("pipewire", "alsa/hdmi:CARD=PCH,DEV=0", ...);
    /// "auto" = mpv's default.
    pub device: String,
    /// Send compressed audio to the receiver (bitstream) instead of
    /// decoding it, on every device unless Settings says otherwise.
    pub passthrough: bool,
    /// Formats sent as bitstream when passthrough is on (mpv
    /// `audio-spdif`); "dts-hd" covers DTS too.
    pub passthrough_codecs: String,
    /// Decoded output channels: "auto", "stereo", "5.1", "7.1"...
    /// (mpv `audio-channels`; "auto" uses what the device reports).
    pub channels: String,
}

impl Default for Audio {
    fn default() -> Self {
        Self {
            device: "auto".into(),
            passthrough: false,
            passthrough_codecs: "ac3,eac3,dts-hd,truehd".into(),
            channels: "auto".into(),
        }
    }
}

/// When to deinterlace (D67).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Deinterlace {
    /// Videos the server reports as interlaced.
    #[default]
    Auto,
    Always,
    Off,
}

/// mpv `tone-mapping` choices offered (present in mpv 0.37 and current).
pub const TONE_MAPPINGS: [&str; 7] =
    ["auto", "bt.2390", "spline", "hable", "mobius", "reinhard", "clip"];

/// HDR10 passthrough choices (M9).
pub const HDR_MODES: [&str; 2] = ["auto", "off"];

/// Video rendering choices (D85): "auto" is fast for 4K and HDR video.
pub const RENDER_QUALITIES: [&str; 3] = ["auto", "high", "fast"];

/// Video processing (spec 7.1); Settings can override each.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Playback {
    /// Motion smoothing (mpv `interpolation`): off by default.
    pub interpolation: bool,
    /// "auto" (interlaced videos only), "always" or "off".
    pub deinterlace: Deinterlace,
    /// HDR to SDR tone mapping: one of `TONE_MAPPINGS`.
    pub tone_mapping: String,
    /// Scaling and HDR processing: one of `RENDER_QUALITIES`.
    pub render_quality: String,
    /// HDR10 passthrough: one of `HDR_MODES`.
    pub hdr: String,
}

impl Default for Playback {
    fn default() -> Self {
        Self {
            interpolation: false,
            deinterlace: Deinterlace::Auto,
            tone_mapping: "auto".into(),
            render_quality: "auto".into(),
            hdr: "auto".into(),
        }
    }
}

/// UI drawing resolution (D44).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiResolution {
    /// 1080p, upscaled, on outputs taller than 1080 pixels.
    Auto,
    /// The output's own resolution: sharpest, the most work per frame.
    /// The default: chosen after testing on the reference HTPC (D44).
    #[default]
    Native,
    /// Drawn at 1080p and upscaled by the GPU.
    #[serde(rename = "1080p")]
    Hd1080,
}

impl UiResolution {
    /// The fraction of an output `fb_height` pixels tall the UI is drawn at.
    pub fn render_scale(self, fb_height: u32) -> f32 {
        match self {
            UiResolution::Native => 1.0,
            UiResolution::Auto | UiResolution::Hd1080 if fb_height > 1080 => {
                (1080.0 / fb_height as f32).max(0.5)
            }
            UiResolution::Auto | UiResolution::Hd1080 => 1.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Input {
    /// Use an HDMI-CEC adapter (TV remote) when one is present.
    pub cec: bool,
    /// Turn the TV on and switch it to this input at startup.
    pub cec_wake_tv: bool,
    /// Specific CEC device, e.g. "/dev/cec0"; empty = first available.
    pub cec_device: String,
    pub repeat_delay_ms: u64,
    pub repeat_interval_ms: u64,
    pub repeat_accelerate_after_ms: u64,
    pub repeat_fast_interval_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Debug {
    /// Show the perf HUD at startup.
    pub hud: bool,
    /// Let Esc/Back on the top level quit.
    pub allow_exit: bool,
}

impl Default for Log {
    fn default() -> Self {
        Self { level: "info".into() }
    }
}

impl Default for Display {
    fn default() -> Self {
        Self {
            safe_area: crate::scene::DEFAULT_SAFE_AREA,
            double_buffer: false,
            ui_resolution: UiResolution::Native,
            match_refresh: true,
            refresh_switch_delay_ms: 0,
            screensaver_minutes: 10,
        }
    }
}

impl Default for Input {
    fn default() -> Self {
        let r = RepeatConfig::default();
        Self {
            cec: true,
            cec_wake_tv: true,
            cec_device: String::new(),
            repeat_delay_ms: r.initial_delay.as_millis() as u64,
            repeat_interval_ms: r.interval.as_millis() as u64,
            repeat_accelerate_after_ms: r.accelerate_after.as_millis() as u64,
            repeat_fast_interval_ms: r.fast_interval.as_millis() as u64,
        }
    }
}

impl Input {
    pub fn repeat(&self) -> RepeatConfig {
        RepeatConfig {
            initial_delay: Duration::from_millis(self.repeat_delay_ms),
            interval: Duration::from_millis(self.repeat_interval_ms),
            accelerate_after: Duration::from_millis(self.repeat_accelerate_after_ms),
            fast_interval: Duration::from_millis(self.repeat_fast_interval_ms),
        }
    }
}

/// Result of loading: always a usable config, plus anything worth telling
/// the user about.
pub struct Loaded {
    pub config: Config,
    pub path: PathBuf,
    pub warnings: Vec<String>,
}

/// `$XDG_CONFIG_HOME/jellyroll/config.toml`, else `~/.config/...`.
pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("jellyroll").join("config.toml"))
}

pub fn load(path: &Path) -> Loaded {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Loaded { config: Config::default(), path: path.into(), warnings: Vec::new() };
        }
        Err(e) => {
            return Loaded {
                config: Config::default(),
                path: path.into(),
                warnings: vec![format!("{}: cannot read ({e}); using defaults", path.display())],
            };
        }
    };
    let (config, warnings) = parse(&text);
    let warnings = warnings.into_iter().map(|w| format!("{}: {w}", path.display())).collect();
    Loaded { config, path: path.into(), warnings }
}

/// Parses config text. Separated from file I/O for tests.
pub fn parse(text: &str) -> (Config, Vec<String>) {
    let mut warnings = Vec::new();
    let table: toml::Table = match toml::from_str(text) {
        Ok(t) => t,
        Err(e) => {
            warnings.push(format!("invalid TOML, using defaults: {}", e.message()));
            if let Some(span) = e.span() {
                let (line, col) = line_col(text, span.start);
                let last = warnings.len() - 1;
                warnings[last] = format!("line {line}, column {col}: {}", warnings[last]);
            }
            return (Config::default(), warnings);
        }
    };

    // Unknown keys (usually typos) are reported, not fatal. The set of known
    // keys is derived from the defaults, so it can never drift from the
    // struct definitions.
    if let Ok(toml::Value::Table(known)) = toml::Value::try_from(Config::default()) {
        unknown_keys(&table, &known, "", &mut warnings);
    }

    let mut config = match Config::deserialize(toml::Value::Table(table)) {
        Ok(c) => c,
        Err(e) => {
            warnings.push(format!("{}; using defaults", e.message()));
            return (Config::default(), warnings);
        }
    };
    validate(&mut config, &mut warnings);
    (config, warnings)
}

fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..offset.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let col = before.len() - before.rfind('\n').map_or(0, |i| i + 1) + 1;
    (line, col)
}

fn unknown_keys(user: &toml::Table, known: &toml::Table, prefix: &str, out: &mut Vec<String>) {
    for (k, v) in user {
        let name = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
        match (v, known.get(k)) {
            (_, None) => out.push(format!("unknown key '{name}' ignored")),
            (toml::Value::Table(u), Some(toml::Value::Table(kn))) => {
                unknown_keys(u, kn, &name, out)
            }
            _ => {}
        }
    }
}

fn clamp<T: PartialOrd + Copy + std::fmt::Display>(
    v: &mut T,
    lo: T,
    hi: T,
    name: &str,
    out: &mut Vec<String>,
) {
    if *v < lo || *v > hi {
        let c = if *v < lo { lo } else { hi };
        out.push(format!("{name} = {v} out of range [{lo}, {hi}], using {c}"));
        *v = c;
    }
}

fn validate(c: &mut Config, out: &mut Vec<String>) {
    if c.display.safe_area.is_nan() {
        c.display.safe_area = Display::default().safe_area;
    }
    clamp(&mut c.display.safe_area, 0.0, 0.1, "display.safe_area", out);
    clamp(
        &mut c.display.refresh_switch_delay_ms,
        0,
        10_000,
        "display.refresh_switch_delay_ms",
        out,
    );
    if !TONE_MAPPINGS.contains(&c.playback.tone_mapping.as_str()) {
        out.push(format!(
            "playback.tone_mapping = {:?} is not one of {}, using \"auto\"",
            c.playback.tone_mapping,
            TONE_MAPPINGS.join(", ")
        ));
        c.playback.tone_mapping = "auto".into();
    }
    if !RENDER_QUALITIES.contains(&c.playback.render_quality.as_str()) {
        out.push(format!(
            "playback.render_quality = {:?} is not one of {}, using \"auto\"",
            c.playback.render_quality,
            RENDER_QUALITIES.join(", ")
        ));
        c.playback.render_quality = "auto".into();
    }
    if !HDR_MODES.contains(&c.playback.hdr.as_str()) {
        out.push(format!(
            "playback.hdr = {:?} is not one of {}, using \"auto\"",
            c.playback.hdr,
            HDR_MODES.join(", ")
        ));
        c.playback.hdr = "auto".into();
    }
    clamp(&mut c.display.screensaver_minutes, 0, 240, "display.screensaver_minutes", out);
    clamp(&mut c.cache.images_mb, 64, 1 << 20, "cache.images_mb", out);
    clamp(&mut c.cache.textures_mb, 64, 4096, "cache.textures_mb", out);
    let i = &mut c.input;
    clamp(&mut i.repeat_delay_ms, 50, 2000, "input.repeat_delay_ms", out);
    clamp(&mut i.repeat_interval_ms, 10, 1000, "input.repeat_interval_ms", out);
    clamp(&mut i.repeat_accelerate_after_ms, 0, 10_000, "input.repeat_accelerate_after_ms", out);
    clamp(&mut i.repeat_fast_interval_ms, 10, 1000, "input.repeat_fast_interval_ms", out);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_default() {
        let (c, w) = parse("");
        assert_eq!(c, Config::default());
        assert!(w.is_empty());
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let (c, w) = parse("[debug]\nhud = true\n[input]\nrepeat_interval_ms = 60\n");
        assert!(w.is_empty(), "{w:?}");
        assert!(c.debug.hud);
        assert_eq!(c.input.repeat_interval_ms, 60);
        assert_eq!(c.input.repeat_delay_ms, Input::default().repeat_delay_ms);
        assert_eq!(c.display, Display::default());
    }

    #[test]
    fn unknown_keys_warn_but_load() {
        let (c, w) = parse("[debug]\nhudd = true\n[nope]\nx = 1\n");
        assert_eq!(c, Config::default());
        assert!(w.iter().any(|m| m.contains("debug.hudd")), "{w:?}");
        assert!(w.iter().any(|m| m.contains("'nope'")), "{w:?}");
    }

    #[test]
    fn syntax_error_reports_line_and_falls_back() {
        let (c, w) = parse("[debug]\nhud = \n");
        assert_eq!(c, Config::default());
        assert_eq!(w.len(), 1);
        assert!(w[0].starts_with("line 2"), "{w:?}");
    }

    #[test]
    fn wrong_type_falls_back() {
        let (c, w) = parse("[debug]\nhud = \"yes\"\n");
        assert_eq!(c, Config::default());
        assert_eq!(w.len(), 1, "{w:?}");
    }

    #[test]
    fn out_of_range_is_clamped() {
        let (c, w) = parse("[display]\nsafe_area = 0.5\n[input]\nrepeat_interval_ms = 1\n");
        assert_eq!(c.display.safe_area, 0.1);
        assert_eq!(c.input.repeat_interval_ms, 10);
        assert_eq!(w.len(), 2, "{w:?}");
    }

    #[test]
    fn example_file_is_valid_and_matches_defaults() {
        let (c, w) = parse(include_str!("../../../dist/config.example.toml"));
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c, Config::default());
    }

    #[test]
    fn missing_file_is_silent() {
        let l = load(Path::new("/nonexistent/jellyroll/config.toml"));
        assert!(l.warnings.is_empty());
        assert_eq!(l.config, Config::default());
    }

    #[test]
    fn playback_section() {
        let (c, w) = parse(
            "[playback]\ninterpolation = true\ndeinterlace = \"off\"\ntone_mapping = \"hable\"\n",
        );
        assert!(w.is_empty(), "{w:?}");
        assert!(c.playback.interpolation);
        assert_eq!(c.playback.deinterlace, Deinterlace::Off);
        assert_eq!(c.playback.tone_mapping, "hable");
        let (c, w) = parse("[playback]\ntone_mapping = \"magic\"\n");
        assert_eq!(c.playback.tone_mapping, "auto");
        assert_eq!(w.len(), 1);
    }
}
