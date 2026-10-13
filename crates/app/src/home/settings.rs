//! Settings (spec 10.1): the account (sign out, change server), display
//! (UI resolution above 1080p (D44), refresh-rate matching), audio output
//! (device, passthrough, channels), and what this device is (versions,
//! display, which codecs the GPU decodes). Input and cache settings are in
//! config.toml.
//!
//! Each section is a row of buttons: Left/Right within a row, Up/Down
//! between rows, Select changes the value (cycling through the choices).

use std::time::Instant;

use jellyroll_render::{Point, Rect, Renderer, TextStyle, Weight};

use super::pages::{Change, Ctx, Open};
use super::strings as s;
use super::theme::{BG, TEXT, TEXT_DIM};
use super::widgets::{self, Viewport};
use crate::audio::AudioDevice;
use crate::config::UiResolution;
use crate::input::InputAction;
use crate::net::Cmd;

/// mpv `audio-channels` choices, in the order Select cycles them.
pub const CHANNELS: [&str; 4] = ["auto", "stereo", "5.1", "7.1"];

/// The current setting values, as HomeApp resolves them (config, then
/// choices saved from this page).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Values {
    /// The UI resolution in effect; None when there is no choice.
    pub resolution: Option<UiResolution>,
    pub match_refresh: bool,
    /// The output resolution matching is remembered for ("3840x2160").
    pub output: String,
    pub audio_device: String,
    /// Passthrough on the current audio device.
    pub passthrough: bool,
    pub channels: String,
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum Item {
    SignOut,
    ChangeServer,
    UiResolution,
    MatchRefresh,
    AudioDevice,
    Passthrough,
    Channels,
}

pub struct SettingsView {
    vp: Viewport,
    account: String,
    about: Vec<(String, String)>,
    values: Values,
    devices: Vec<AudioDevice>,
    /// (row, column) in `rows()`.
    focus: (usize, usize),
}

impl SettingsView {
    pub fn new(
        vp: Viewport,
        user: &str,
        server: &str,
        about: Vec<(String, String)>,
        values: Values,
        devices: Vec<AudioDevice>,
    ) -> Self {
        Self {
            vp,
            account: s::fill(s::SIGNED_IN_AS, &[user, server]),
            about,
            values,
            devices,
            focus: (0, 0),
        }
    }

    fn rows(&self) -> [Vec<Item>; 3] {
        let mut display = Vec::new();
        if self.values.resolution.is_some() {
            display.push(Item::UiResolution);
        }
        display.push(Item::MatchRefresh);
        [
            vec![Item::SignOut, Item::ChangeServer],
            display,
            vec![Item::AudioDevice, Item::Passthrough, Item::Channels],
        ]
    }

    /// New values after a change was applied (or saved elsewhere).
    pub fn set_values(&mut self, values: Values) {
        self.values = values;
        let rows = self.rows();
        self.focus.1 = self.focus.1.min(rows[self.focus.0].len() - 1);
    }

    /// The audio device list arrived (it is probed in the background).
    pub fn set_devices(&mut self, devices: Vec<AudioDevice>) {
        self.devices = devices;
    }

    pub fn has_devices(&self) -> bool {
        !self.devices.is_empty()
    }

    pub fn set_viewport(&mut self, vp: Viewport) -> bool {
        let changed = vp != self.vp;
        self.vp = vp;
        changed
    }

    fn device_label(&self) -> String {
        let name = &self.values.audio_device;
        let label = self
            .devices
            .iter()
            .find(|d| &d.name == name)
            .map(|d| d.label().to_string())
            .unwrap_or_else(|| if name == "auto" { s::AUDIO_AUTO.into() } else { name.clone() });
        s::fill(s::AUDIO_OUTPUT, &[&label])
    }

    fn label(&self, item: Item) -> String {
        let v = &self.values;
        let on_off = |b: bool| if b { s::ON } else { s::OFF };
        match item {
            Item::SignOut => s::SIGN_OUT.into(),
            Item::ChangeServer => s::CHANGE_SERVER.into(),
            Item::UiResolution => match v.resolution {
                Some(UiResolution::Native) => s::UI_RES_NATIVE.into(),
                _ => s::UI_RES_1080.into(),
            },
            Item::MatchRefresh => s::fill(s::MATCH_REFRESH, &[on_off(v.match_refresh)]),
            Item::AudioDevice => self.device_label(),
            Item::Passthrough => s::fill(s::PASSTHROUGH, &[on_off(v.passthrough)]),
            Item::Channels => {
                let c = match v.channels.as_str() {
                    "auto" | "" => s::CHANNELS_AUTO,
                    "stereo" => s::CHANNELS_STEREO,
                    other => other,
                };
                s::fill(s::CHANNELS_LABEL, &[c])
            }
        }
    }

    /// The change Select makes on `item`.
    fn change(&self, item: Item) -> Option<Change> {
        let v = &self.values;
        Some(match item {
            Item::SignOut | Item::ChangeServer => return None,
            Item::UiResolution => Change::UiResolution(match v.resolution {
                Some(UiResolution::Native) => UiResolution::Hd1080,
                _ => UiResolution::Native,
            }),
            Item::MatchRefresh => Change::MatchRefresh(!v.match_refresh),
            Item::Passthrough => Change::Passthrough(!v.passthrough),
            Item::Channels => {
                let i = CHANNELS.iter().position(|c| *c == v.channels).unwrap_or(0);
                Change::Channels(CHANNELS[(i + 1) % CHANNELS.len()].into())
            }
            Item::AudioDevice => {
                if self.devices.is_empty() {
                    return None;
                }
                let i = self.devices.iter().position(|d| d.name == v.audio_device);
                let next = i.map_or(0, |i| (i + 1) % self.devices.len());
                Change::AudioDevice(self.devices[next].name.clone())
            }
        })
    }

    pub fn on_action(&mut self, a: InputAction, ctx: &mut Ctx) -> bool {
        let rows = self.rows();
        let (r, c) = self.focus;
        match a {
            InputAction::Left if c > 0 => self.focus.1 -= 1,
            InputAction::Right if c + 1 < rows[r].len() => self.focus.1 += 1,
            InputAction::Down if r + 1 < rows.len() => {
                self.focus = (r + 1, c.min(rows[r + 1].len() - 1));
            }
            InputAction::Up if r > 0 => self.focus = (r - 1, c.min(rows[r - 1].len() - 1)),
            InputAction::Select => match rows[r][c] {
                Item::SignOut => ctx.send(Cmd::SignOut),
                Item::ChangeServer => ctx.send(Cmd::ChangeServer),
                item => match self.change(item) {
                    Some(ch) => ctx.open = Some(Open::Setting(ch)),
                    None => return false,
                },
            },
            _ => return false,
        }
        true
    }

    pub fn draw<R: Renderer>(&mut self, r: &mut R, _now: Instant) {
        let (sx, sy) = (self.vp.sx(), self.vp.sy());
        r.clear(BG);
        r.text(s::SETTINGS_TITLE, &TextStyle::new(40.0, Weight::Bold), Point::new(sx, sy), TEXT);
        let head = TextStyle::new(28.0, Weight::Bold);
        let body = TextStyle::new(22.0, Weight::Regular).max_width(1500.0);
        let rows = self.rows();
        let heads = [s::ACCOUNT, s::DISPLAY, s::AUDIO];
        let mut y = sy + 80.0;
        for (ri, items) in rows.iter().enumerate() {
            r.text(heads[ri], &head, Point::new(sx, y), TEXT);
            y += 44.0;
            if ri == 0 {
                r.text(&self.account, &body, Point::new(sx, y), TEXT_DIM);
                y += 40.0;
            }
            let mut x = sx;
            for (ci, item) in items.iter().enumerate() {
                let label = self.label(*item);
                let w = widgets::button_width(r, &label).max(200.0);
                let focused = self.focus == (ri, ci);
                widgets::draw_button(r, Rect::new(x, y, w, 52.0), &label, focused, false);
                x += w + 14.0;
            }
            y += 64.0;
            let hint = match ri {
                1 => Some(s::fill(s::DISPLAY_HINT, &[&self.values.output])),
                2 => Some(s::AUDIO_HINT.to_string()),
                _ => None,
            };
            if let Some(h) = hint {
                r.text(&h, &body, Point::new(sx, y), TEXT_DIM);
                y += 36.0;
            }
            y += 28.0;
        }
        r.text(s::ABOUT, &head, Point::new(sx, y), TEXT);
        y += 46.0;
        let key = TextStyle::new(22.0, Weight::Bold);
        for (k, v) in &self.about {
            r.text(k, &key, Point::new(sx, y), TEXT_DIM);
            r.text(v, &body, Point::new(sx + 320.0, y), TEXT);
            y += 34.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VP: Viewport = Viewport { lw: 1920.0, safe: 0.025, scale: 2.0 };

    fn values(res: Option<UiResolution>) -> Values {
        Values {
            resolution: res,
            match_refresh: true,
            output: "3840x2160".into(),
            audio_device: "auto".into(),
            passthrough: false,
            channels: "auto".into(),
        }
    }

    fn devices() -> Vec<AudioDevice> {
        ["auto", "pipewire", "alsa/hdmi:CARD=PCH,DEV=0"]
            .iter()
            .map(|n| AudioDevice { name: n.to_string(), description: String::new() })
            .collect()
    }

    fn select(v: &mut SettingsView) -> Option<Change> {
        let mut next = 0;
        let mut ctx = Ctx::new(&mut next);
        v.on_action(InputAction::Select, &mut ctx);
        match ctx.open {
            Some(Open::Setting(c)) => Some(c),
            _ => None,
        }
    }

    fn press(v: &mut SettingsView, a: InputAction) -> bool {
        let mut next = 0;
        let mut ctx = Ctx::new(&mut next);
        v.on_action(a, &mut ctx)
    }

    #[test]
    fn display_row_toggles() {
        let mut v = SettingsView::new(
            VP,
            "alice",
            "home",
            Vec::new(),
            values(Some(UiResolution::Hd1080)),
            devices(),
        );
        assert!(press(&mut v, InputAction::Down));
        assert_eq!(v.focus, (1, 0));
        assert_eq!(select(&mut v), Some(Change::UiResolution(UiResolution::Native)));
        assert!(press(&mut v, InputAction::Right));
        assert_eq!(select(&mut v), Some(Change::MatchRefresh(false)));
        assert!(!press(&mut v, InputAction::Right), "end of the row");
        assert!(press(&mut v, InputAction::Up));
        assert_eq!(v.focus, (0, 1), "back to the account buttons");
    }

    #[test]
    fn no_resolution_choice_on_1080p_outputs() {
        let mut v = SettingsView::new(VP, "alice", "home", Vec::new(), values(None), devices());
        press(&mut v, InputAction::Down);
        assert_eq!(v.label(v.rows()[1][0]), s::fill(s::MATCH_REFRESH, &[s::ON]));
        assert_eq!(select(&mut v), Some(Change::MatchRefresh(false)));
    }

    #[test]
    fn audio_row_cycles() {
        let mut v = SettingsView::new(VP, "alice", "home", Vec::new(), values(None), devices());
        press(&mut v, InputAction::Down);
        press(&mut v, InputAction::Down);
        assert_eq!(v.focus, (2, 0));
        assert_eq!(select(&mut v), Some(Change::AudioDevice("pipewire".into())));
        let mut vals = values(None);
        vals.audio_device = "alsa/hdmi:CARD=PCH,DEV=0".into();
        v.set_values(vals.clone());
        assert_eq!(select(&mut v), Some(Change::AudioDevice("auto".into())), "wraps");
        press(&mut v, InputAction::Right);
        assert_eq!(select(&mut v), Some(Change::Passthrough(true)));
        press(&mut v, InputAction::Right);
        assert_eq!(select(&mut v), Some(Change::Channels("stereo".into())));
        vals.channels = "7.1".into();
        v.set_values(vals);
        assert_eq!(select(&mut v), Some(Change::Channels("auto".into())));
    }

    #[test]
    fn device_choice_waits_for_the_list() {
        let mut v = SettingsView::new(VP, "alice", "home", Vec::new(), values(None), Vec::new());
        press(&mut v, InputAction::Down);
        press(&mut v, InputAction::Down);
        assert_eq!(select(&mut v), None);
        assert_eq!(v.device_label(), s::fill(s::AUDIO_OUTPUT, &[s::AUDIO_AUTO]));
    }
}
