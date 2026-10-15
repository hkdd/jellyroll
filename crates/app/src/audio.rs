//! Audio outputs (spec 7.1): the devices mpv can play to, for Settings and
//! `--print-diagnostics`; and which HDMI output has the TV on it (D82).

use jellyroll_platform::hdmi_audio::HdmiSink;
use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct AudioDevice {
    /// mpv `audio-device` value, e.g. "alsa/hdmi:CARD=PCH,DEV=0".
    pub name: String,
    /// What mpv calls it, e.g. "HDA Intel PCH, HDMI 0".
    #[serde(default)]
    pub description: String,
    /// The screen on this HDMI output ("LG TV SSCR2"); for "auto", where
    /// it plays ("HDMI 0 · LG TV SSCR2"). Set by [`annotate`].
    #[serde(skip)]
    pub note: String,
}

impl AudioDevice {
    pub fn label(&self) -> String {
        let base = if self.description.is_empty() { &self.name } else { &self.description };
        if self.note.is_empty() { base.clone() } else { format!("{base} · {}", self.note) }
    }
}

/// mpv's ALSA device for an HDMI output with a screen: `alsa/hdmi:CARD=<id>,`
/// whose description names the PCM ("HDA Intel PCH, HDMI 0/HDMI Audio
/// Output"). With a screen connected ALSA names it after the screen instead
/// ("HDA Intel PCH, LG TV SSCR2/…", as on an N150), so that matches too; and
/// failing both, "HDMI n" is `DEV=n`.
fn device_for<'a>(devices: &'a [AudioDevice], sink: &HdmiSink) -> Option<&'a AudioDevice> {
    let prefix = format!("alsa/hdmi:CARD={},", sink.card);
    let named = |want: &str| {
        devices.iter().find(|d| {
            !want.is_empty()
                && d.name.starts_with(&prefix)
                && d.description
                    .split(['/', '\n'])
                    .next()
                    .and_then(|first| first.split_once(", "))
                    .is_some_and(|(_, pcm)| pcm.trim() == want)
        })
    };
    named(&sink.pcm_name).or_else(|| named(&sink.monitor)).or_else(|| {
        let n: u32 = sink.pcm_name.strip_prefix("HDMI ")?.trim().parse().ok()?;
        let name = format!("{prefix}DEV={n}");
        devices.iter().find(|d| d.name == name)
    })
}

/// What "auto" plays to: the first HDMI output with a screen on it. None:
/// no screen found on HDMI, leave it to mpv (PipeWire's default).
pub fn auto_device(devices: &[AudioDevice], sinks: &[HdmiSink]) -> Option<String> {
    sinks.iter().find_map(|s| device_for(devices, s)).map(|d| d.name.clone())
}

/// Marks the HDMI outputs that have a screen, and where "auto" plays.
pub fn annotate(mut devices: Vec<AudioDevice>, sinks: &[HdmiSink]) -> Vec<AudioDevice> {
    let mut auto_note = None;
    for s in sinks {
        let Some(name) = device_for(&devices, s).map(|d| d.name.clone()) else { continue };
        let screen = if s.monitor.is_empty() { "connected".to_string() } else { s.monitor.clone() };
        if auto_note.is_none() {
            auto_note = Some(format!("{} · {screen}", s.pcm_name));
        }
        // ALSA may already name the output after the screen.
        if let Some(d) = devices.iter_mut().find(|d| d.name == name)
            && !d.description.contains(&screen)
        {
            d.note = screen;
        }
    }
    if let (Some(n), Some(auto)) = (auto_note, devices.iter_mut().find(|d| d.name == "auto")) {
        auto.note = n;
    }
    devices
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

    fn dev(name: &str, description: &str) -> AudioDevice {
        AudioDevice { name: name.into(), description: description.into(), note: String::new() }
    }

    fn sink(device: u32, pcm: &str, monitor: &str) -> HdmiSink {
        HdmiSink { card: "PCH".into(), device, pcm_name: pcm.into(), monitor: monitor.into() }
    }

    /// As on an N150: the TV is on HDMI 0 (PCM 3), and PipeWire's default
    /// is the analog jack.
    fn n150() -> Vec<AudioDevice> {
        vec![
            dev("auto", "Autoselect device"),
            dev("pipewire", "Default (pipewire)"),
            dev("alsa/hw:CARD=PCH,DEV=0", "HDA Intel PCH, ALC897 Analog/Direct hardware device"),
            dev("alsa/hdmi:CARD=PCH,DEV=0", "HDA Intel PCH, HDMI 0/HDMI Audio Output"),
            dev("alsa/hdmi:CARD=PCH,DEV=1", "HDA Intel PCH, HDMI 1/HDMI Audio Output"),
            dev("alsa/hdmi:CARD=PCH,DEV=2", "HDA Intel PCH, HDMI 2/HDMI Audio Output"),
        ]
    }

    #[test]
    fn auto_plays_to_the_hdmi_output_with_the_tv() {
        let devices = n150();
        assert_eq!(
            auto_device(&devices, &[sink(3, "HDMI 0", "LG TV SSCR2")]).as_deref(),
            Some("alsa/hdmi:CARD=PCH,DEV=0")
        );
        assert_eq!(
            auto_device(&devices, &[sink(8, "HDMI 2", "")]).as_deref(),
            Some("alsa/hdmi:CARD=PCH,DEV=2"),
            "follows the TV to another port"
        );
        assert_eq!(auto_device(&devices, &[]), None, "no screen: mpv decides");
        let other_card = HdmiSink { card: "NVidia".into(), ..sink(3, "HDMI 0", "TV") };
        assert_eq!(auto_device(&devices, &[other_card]), None);
    }

    /// The N150's real list (debug report): ALSA names the connected output
    /// after the TV, not "HDMI 0".
    #[test]
    fn finds_the_output_alsa_names_after_the_tv() {
        let mut devices = n150();
        devices[3].description = "HDA Intel PCH, LG TV SSCR2/HDMI Audio Output".into();
        let tv = sink(3, "HDMI 0", "LG TV SSCR2");
        assert_eq!(auto_device(&devices, &[tv]).as_deref(), Some("alsa/hdmi:CARD=PCH,DEV=0"));
        // Neither name matches: HDMI n is DEV=n.
        devices[5].description = "HDA Intel PCH, Something/HDMI Audio Output".into();
        let other = sink(8, "HDMI 2", "Projector");
        assert_eq!(auto_device(&devices, &[other]).as_deref(), Some("alsa/hdmi:CARD=PCH,DEV=2"));
    }

    #[test]
    fn labels_show_the_screen() {
        let d = annotate(n150(), &[sink(3, "HDMI 0", "LG TV SSCR2")]);
        assert_eq!(d[0].note, "HDMI 0 · LG TV SSCR2");
        assert_eq!(d[3].label(), "HDA Intel PCH, HDMI 0/HDMI Audio Output · LG TV SSCR2");
        assert_eq!(d[4].label(), "HDA Intel PCH, HDMI 1/HDMI Audio Output");
        let unnamed = annotate(n150(), &[sink(7, "HDMI 1", "")]);
        assert_eq!(unnamed[4].note, "connected");
    }
}
