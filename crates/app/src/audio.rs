//! Audio outputs (spec 7.1): the devices mpv can play to, for Settings and
//! `--print-diagnostics`.

use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct AudioDevice {
    /// mpv `audio-device` value, e.g. "alsa/hdmi:CARD=PCH,DEV=0".
    pub name: String,
    /// What mpv calls it, e.g. "HDA Intel PCH, HDMI 0".
    #[serde(default)]
    pub description: String,
}

impl AudioDevice {
    pub fn label(&self) -> &str {
        if self.description.is_empty() { &self.name } else { &self.description }
    }
}

/// mpv's list; "auto" (mpv's choice) first.
pub fn devices() -> Result<Vec<AudioDevice>, String> {
    parse(&jellyroll_video::audio_devices_json()?)
}

fn parse(json: &str) -> Result<Vec<AudioDevice>, String> {
    let mut v: Vec<AudioDevice> =
        serde_json::from_str(json).map_err(|e| format!("audio device list: {e}"))?;
    if let Some(i) = v.iter().position(|d| d.name == "auto") {
        let auto = v.remove(i);
        v.insert(0, auto);
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mpv_list_with_auto_first() {
        let json = r#"[{"name":"pipewire","description":"Default (pipewire)"},
            {"name":"auto","description":"Autoselect device"},
            {"name":"alsa/hdmi:CARD=PCH,DEV=0","description":"HDA Intel PCH, HDMI 0"},
            {"name":"alsa/null"}]"#;
        let v = parse(json).unwrap();
        assert_eq!(v[0].name, "auto");
        assert_eq!(v[2].label(), "HDA Intel PCH, HDMI 0");
        assert_eq!(v[3].label(), "alsa/null");
        assert!(parse("nope").is_err());
    }
}
