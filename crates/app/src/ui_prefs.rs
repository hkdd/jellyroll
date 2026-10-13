//! Settings chosen in the app (the Settings page), kept apart from the
//! user's config.toml, which the app never rewrites. They win over it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::UiResolution;
use crate::net::store::atomic_write;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiPrefs {
    pub ui_resolution: Option<UiResolution>,
}

pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("ui.json")
}

/// Missing or unreadable: defaults (an unreadable file is logged).
pub fn load(path: &Path) -> UiPrefs {
    match std::fs::read(path) {
        Ok(b) => serde_json::from_slice(&b).unwrap_or_else(|e| {
            tracing::warn!("{}: unreadable ({e}); using defaults", path.display());
            UiPrefs::default()
        }),
        Err(_) => UiPrefs::default(),
    }
}

pub fn save(path: &Path, prefs: &UiPrefs) {
    let r = serde_json::to_vec_pretty(prefs)
        .map_err(std::io::Error::other)
        .and_then(|b| atomic_write(path, &b, 0o644));
    if let Err(e) = r {
        tracing::warn!("{}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_missing_file() {
        let dir = std::env::temp_dir().join(format!("jellyroll-ui-prefs-{}", std::process::id()));
        let p = path(&dir);
        assert_eq!(load(&p), UiPrefs::default());
        save(&p, &UiPrefs { ui_resolution: Some(UiResolution::Native) });
        assert_eq!(load(&p).ui_resolution, Some(UiResolution::Native));
        std::fs::write(&p, b"not json").unwrap();
        assert_eq!(load(&p), UiPrefs::default());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn render_scale_per_output() {
        assert_eq!(UiResolution::Auto.render_scale(2160), 0.5);
        assert_eq!(UiResolution::Hd1080.render_scale(1440), 0.75);
        assert_eq!(UiResolution::Auto.render_scale(1080), 1.0);
        assert_eq!(UiResolution::Native.render_scale(2160), 1.0);
        assert_eq!(UiResolution::Hd1080.render_scale(4320), 0.5, "never below half");
    }
}
