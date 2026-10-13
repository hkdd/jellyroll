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
    /// Render the UI at this fraction of the output resolution and upscale
    /// (0.5 - 1.0), for weak GPUs (spec 6.4).
    pub ui_render_scale: f32,
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
            ui_render_scale: 1.0,
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
    if c.display.ui_render_scale.is_nan() {
        c.display.ui_render_scale = 1.0;
    }
    clamp(&mut c.display.ui_render_scale, 0.5, 1.0, "display.ui_render_scale", out);
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
}
