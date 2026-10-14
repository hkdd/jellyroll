//! Settings (spec 10.1): the account (switch user, sign out), display
//! (UI resolution above 1080p (D44), refresh-rate matching), audio output
//! (device, passthrough, channels), playback (motion smoothing,
//! deinterlacing, HDR tone mapping), storage (artwork cache size, clear),
//! and what this device is (versions, display, which codecs the GPU
//! decodes). Input settings are in config.toml.
//!
//! The page scrolls to keep the focused row in view.
//!
//! Each section is a row of buttons: Left/Right within a row, Up/Down
//! between rows. On/off settings are toggles (Select flips them); the
//! others open a list from the right (`Blade`, D53).

use std::time::Instant;

use jellyroll_render::{Point, Rect, Renderer, TextStyle, Weight};

use super::blade::{self, Blade};
use super::pages::{Change, Ctx, Open};
use super::strings as s;
use super::theme::{BG, TEXT, TEXT_DIM};
use super::widgets::{self, Viewport};
use crate::anim::Tween;
use crate::audio::AudioDevice;
use crate::config::{Deinterlace, TONE_MAPPINGS, UiResolution};
use crate::input::InputAction;
use crate::net::Cmd;

/// mpv `audio-channels` choices, in the order Select cycles them.
pub const CHANNELS: [&str; 4] = ["auto", "stereo", "5.1", "7.1"];

const DEINTERLACE: [Deinterlace; 3] = [Deinterlace::Auto, Deinterlace::Always, Deinterlace::Off];
/// Artwork disk cache sizes offered (MB).
const IMAGE_CACHE_MB: [u64; 6] = [512, 1024, 2048, 4096, 8192, 16384];
const SCROLL: std::time::Duration = std::time::Duration::from_millis(160);

/// "512 MB", "2 GB", "1.5 GB".
pub fn size_label(bytes: u64) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb < 1024.0 {
        format!("{} MB", mb.round() as u64)
    } else {
        let gb = mb / 1024.0;
        if (gb - gb.round()).abs() < 0.05 {
            format!("{} GB", gb.round() as u64)
        } else {
            format!("{gb:.1} GB")
        }
    }
}

fn tone_name(t: &str) -> &str {
    match t {
        "auto" => s::TONE_AUTO,
        "bt.2390" => "BT.2390",
        "spline" => "Spline",
        "hable" => "Hable",
        "mobius" => "Mobius",
        "reinhard" => "Reinhard",
        "clip" => "Clip",
        other => other,
    }
}

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
    pub interpolation: bool,
    pub deinterlace: Deinterlace,
    pub tone_mapping: String,
    pub image_cache_mb: u64,
    /// Bytes of artwork on disk, once the network thread says.
    pub cache_used: Option<u64>,
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum Item {
    SwitchUser,
    SignOut,
    UiResolution,
    MatchRefresh,
    AudioDevice,
    Passthrough,
    Channels,
    Interpolation,
    Deinterlace,
    ToneMapping,
    ImageCache,
    ClearCache,
}

pub struct SettingsView {
    vp: Viewport,
    account: String,
    about: Vec<(String, String)>,
    values: Values,
    devices: Vec<AudioDevice>,
    /// (row, column) in `rows()`.
    focus: (usize, usize),
    /// A list open for this item.
    blade: Option<(Item, Blade)>,
    /// Where each row starts (logical y before scrolling), from the last
    /// draw, and the page height.
    row_tops: Vec<f32>,
    content_h: f32,
    scroll: Tween,
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
            blade: None,
            row_tops: Vec::new(),
            content_h: 0.0,
            scroll: Tween::settled(0.0, Instant::now()),
        }
    }

    fn rows(&self) -> [Vec<Item>; 5] {
        let mut display = Vec::new();
        if self.values.resolution.is_some() {
            display.push(Item::UiResolution);
        }
        display.push(Item::MatchRefresh);
        [
            vec![Item::SwitchUser, Item::SignOut],
            display,
            vec![Item::AudioDevice, Item::Passthrough, Item::Channels],
            vec![Item::Interpolation, Item::Deinterlace, Item::ToneMapping],
            vec![Item::ImageCache, Item::ClearCache],
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

    fn device_name(&self, d: &AudioDevice) -> String {
        if d.name == "auto" { s::AUDIO_AUTO.into() } else { d.label().to_string() }
    }

    fn device_label(&self) -> String {
        let name = &self.values.audio_device;
        let label = self
            .devices
            .iter()
            .find(|d| &d.name == name)
            .map(|d| self.device_name(d))
            .unwrap_or_else(|| if name == "auto" { s::AUDIO_AUTO.into() } else { name.clone() });
        s::fill(s::AUDIO_OUTPUT, &[&label])
    }

    fn channels_name(c: &str) -> &str {
        match c {
            "auto" | "" => s::CHANNELS_AUTO,
            "stereo" => s::CHANNELS_STEREO,
            other => other,
        }
    }

    /// On/off items are toggles; the rest open a list.
    fn toggle_state(&self, item: Item) -> Option<bool> {
        match item {
            Item::MatchRefresh => Some(self.values.match_refresh),
            Item::Passthrough => Some(self.values.passthrough),
            Item::Interpolation => Some(self.values.interpolation),
            _ => None,
        }
    }

    fn label(&self, item: Item) -> String {
        let v = &self.values;
        match item {
            Item::SwitchUser => s::SWITCH_USER.into(),
            Item::SignOut => s::SIGN_OUT.into(),
            Item::UiResolution => widgets::list_label(match v.resolution {
                Some(UiResolution::Native) => s::UI_RES_NATIVE,
                _ => s::UI_RES_1080,
            }),
            Item::MatchRefresh => s::MATCH_REFRESH.into(),
            Item::AudioDevice => widgets::list_label(&self.device_label()),
            Item::Passthrough => s::PASSTHROUGH.into(),
            Item::Channels => widgets::list_label(&s::fill(
                s::CHANNELS_LABEL,
                &[Self::channels_name(&v.channels)],
            )),
            Item::Interpolation => s::SMOOTHING.into(),
            Item::Deinterlace => widgets::list_label(&s::fill(
                s::DEINTERLACE_LABEL,
                &[match v.deinterlace {
                    Deinterlace::Auto => s::DEINTERLACE_AUTO,
                    Deinterlace::Always => s::DEINTERLACE_ALWAYS,
                    Deinterlace::Off => s::DEINTERLACE_OFF,
                }],
            )),
            Item::ToneMapping => {
                widgets::list_label(&s::fill(s::TONE_LABEL, &[tone_name(&v.tone_mapping)]))
            }
            Item::ImageCache => widgets::list_label(&s::fill(
                s::IMAGE_CACHE_LABEL,
                &[&size_label(v.image_cache_mb << 20)],
            )),
            Item::ClearCache => s::CLEAR_CACHE.into(),
        }
    }

    /// The list Select opens on `item`: (title, options, current).
    fn list(&self, item: Item) -> Option<(&'static str, Vec<String>, Option<usize>)> {
        let v = &self.values;
        Some(match item {
            Item::UiResolution => (
                s::UI_RES_TITLE,
                vec![s::UI_RES_OPT_NATIVE.into(), s::UI_RES_OPT_1080.into()],
                Some(usize::from(v.resolution != Some(UiResolution::Native))),
            ),
            Item::AudioDevice => {
                if self.devices.is_empty() {
                    return None;
                }
                (
                    s::AUDIO_OUTPUT_TITLE,
                    self.devices.iter().map(|d| self.device_name(d)).collect(),
                    self.devices.iter().position(|d| d.name == v.audio_device),
                )
            }
            Item::Channels => (
                s::CHANNELS_TITLE,
                CHANNELS.iter().map(|c| Self::channels_name(c).to_string()).collect(),
                CHANNELS.iter().position(|c| *c == v.channels).or(Some(0)),
            ),
            Item::Deinterlace => (
                s::DEINTERLACE_TITLE,
                vec![
                    s::DEINTERLACE_AUTO_LONG.into(),
                    s::DEINTERLACE_ALWAYS.into(),
                    s::DEINTERLACE_OFF.into(),
                ],
                DEINTERLACE.iter().position(|d| *d == v.deinterlace),
            ),
            Item::ToneMapping => (
                s::TONE_TITLE,
                TONE_MAPPINGS.iter().map(|t| tone_name(t).to_string()).collect(),
                TONE_MAPPINGS.iter().position(|t| *t == v.tone_mapping).or(Some(0)),
            ),
            Item::ImageCache => (
                s::IMAGE_CACHE_TITLE,
                IMAGE_CACHE_MB.iter().map(|mb| size_label(mb << 20)).collect(),
                IMAGE_CACHE_MB.iter().position(|mb| *mb == v.image_cache_mb),
            ),
            _ => return None,
        })
    }

    /// The change picking option `i` of `item`'s list makes.
    fn picked(&self, item: Item, i: usize) -> Option<Change> {
        Some(match item {
            Item::UiResolution => Change::UiResolution(if i == 0 {
                UiResolution::Native
            } else {
                UiResolution::Hd1080
            }),
            Item::AudioDevice => Change::AudioDevice(self.devices.get(i)?.name.clone()),
            Item::Channels => Change::Channels((*CHANNELS.get(i)?).into()),
            Item::Deinterlace => Change::Deinterlace(*DEINTERLACE.get(i)?),
            Item::ToneMapping => Change::ToneMapping((*TONE_MAPPINGS.get(i)?).into()),
            Item::ImageCache => Change::ImageCacheMb(*IMAGE_CACHE_MB.get(i)?),
            _ => return None,
        })
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        self.blade.as_ref().is_some_and(|(_, b)| b.is_animating(now))
            || self.scroll.is_animating(now)
    }

    pub fn on_action(&mut self, a: InputAction, now: Instant, ctx: &mut Ctx) -> bool {
        if let Some((item, b)) = &mut self.blade {
            let item = *item;
            match b.on_action(a) {
                blade::Outcome::Stay => {}
                blade::Outcome::Closed => self.blade = None,
                blade::Outcome::Picked(i) => {
                    if let Some(ch) = self.picked(item, i) {
                        ctx.open = Some(Open::Setting(ch));
                    }
                    self.blade = None;
                }
            }
            return true;
        }
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
                Item::SwitchUser => ctx.send(Cmd::SwitchUser),
                Item::SignOut => ctx.send(Cmd::SignOut),
                Item::MatchRefresh => {
                    ctx.open = Some(Open::Setting(Change::MatchRefresh(!self.values.match_refresh)))
                }
                Item::Passthrough => {
                    ctx.open = Some(Open::Setting(Change::Passthrough(!self.values.passthrough)))
                }
                Item::Interpolation => {
                    ctx.open =
                        Some(Open::Setting(Change::Interpolation(!self.values.interpolation)))
                }
                Item::ClearCache => ctx.open = Some(Open::Setting(Change::ClearCache)),
                item => match self.list(item) {
                    Some((title, options, current)) => {
                        self.blade = Some((item, Blade::new(title, options, current, now)));
                    }
                    None => return false,
                },
            },
            _ => return false,
        }
        self.scroll_to_focus(now);
        true
    }

    /// Keeps the focused row in view: the top rows show the title, the
    /// last one scrolls to the end of the page (About).
    fn scroll_to_focus(&mut self, now: Instant) {
        let Some(top) = self.row_tops.get(self.focus.0).copied() else { return };
        let (_, lh) = (self.vp.lw, 1080.0);
        let max = (self.content_h - lh + self.vp.sy()).max(0.0);
        let last = self.focus.0 + 1 == self.rows().len();
        let target = if last { max } else { (top - lh * 0.35).clamp(0.0, max) };
        if (target - self.scroll.target()).abs() > 0.5 {
            self.scroll.retarget(target, now, SCROLL);
        }
    }

    pub fn draw<R: Renderer>(&mut self, r: &mut R, now: Instant) {
        let (sx, sy0) = (self.vp.sx(), self.vp.sy());
        r.clear(BG);
        let offset = self.scroll.value(now);
        let sy = sy0 - offset;
        r.text(s::SETTINGS_TITLE, &TextStyle::new(40.0, Weight::Bold), Point::new(sx, sy), TEXT);
        let head = TextStyle::new(28.0, Weight::Bold);
        let body = TextStyle::new(22.0, Weight::Regular).max_width(1500.0);
        let rows = self.rows();
        let heads = [s::ACCOUNT, s::DISPLAY, s::AUDIO, s::PLAYBACK, s::STORAGE];
        let mut y = sy + 80.0;
        self.row_tops.clear();
        for (ri, items) in rows.iter().enumerate() {
            self.row_tops.push(y + offset);
            r.text(heads[ri], &head, Point::new(sx, y), TEXT);
            y += 44.0;
            if ri == 0 {
                r.text(&self.account, &body, Point::new(sx, y), TEXT_DIM);
                y += 40.0;
            }
            let mut x = sx;
            for (ci, item) in items.iter().enumerate() {
                let label = self.label(*item);
                let focused = self.focus == (ri, ci) && self.blade.is_none();
                match self.toggle_state(*item) {
                    Some(on) => {
                        let w = widgets::toggle_width(r, &label).max(200.0);
                        widgets::draw_toggle(r, Rect::new(x, y, w, 52.0), &label, on, focused);
                        x += w + 14.0;
                    }
                    None => {
                        let w = widgets::button_width(r, &label).max(200.0);
                        widgets::draw_button(r, Rect::new(x, y, w, 52.0), &label, focused, false);
                        x += w + 14.0;
                    }
                }
            }
            y += 64.0;
            let hint = match ri {
                1 => Some(s::fill(s::DISPLAY_HINT, &[&self.values.output])),
                2 => Some(s::AUDIO_HINT.to_string()),
                3 => Some(s::PLAYBACK_HINT.to_string()),
                4 => Some(match self.values.cache_used {
                    Some(used) => s::fill(
                        s::STORAGE_HINT,
                        &[&size_label(used), &size_label(self.values.image_cache_mb << 20)],
                    ),
                    None => s::STORAGE_HINT_UNKNOWN.to_string(),
                }),
                _ => None,
            };
            if let Some(h) = hint {
                let plain = TextStyle::new(22.0, Weight::Regular);
                for line in super::status::wrap(r, &h, &plain, 1500.0) {
                    r.text(&line, &body, Point::new(sx, y), TEXT_DIM);
                    y += 30.0;
                }
                y += 6.0;
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
        self.content_h = y + offset;
        if let Some((_, b)) = &self.blade {
            b.draw(r, now, self.vp.safe);
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
            interpolation: false,
            deinterlace: Deinterlace::Auto,
            tone_mapping: "auto".into(),
            image_cache_mb: 2048,
            cache_used: Some(300 << 20),
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
        v.on_action(InputAction::Select, Instant::now(), &mut ctx);
        match ctx.open {
            Some(Open::Setting(c)) => Some(c),
            _ => None,
        }
    }

    fn press(v: &mut SettingsView, a: InputAction) -> bool {
        let mut next = 0;
        let mut ctx = Ctx::new(&mut next);
        v.on_action(a, Instant::now(), &mut ctx)
    }

    #[test]
    fn display_row() {
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
        // A list: Select opens it on the current value; Up picks Native.
        assert_eq!(select(&mut v), None);
        assert!(v.blade.is_some());
        press(&mut v, InputAction::Up);
        assert_eq!(select(&mut v), Some(Change::UiResolution(UiResolution::Native)));
        assert!(v.blade.is_none());
        // A toggle: Select flips it directly.
        assert!(press(&mut v, InputAction::Right));
        assert_eq!(select(&mut v), Some(Change::MatchRefresh(false)));
        assert!(v.blade.is_none());
        assert!(!press(&mut v, InputAction::Right), "end of the row");
        assert!(press(&mut v, InputAction::Up));
        assert_eq!(v.focus, (0, 1), "back to the account buttons");
    }

    #[test]
    fn no_resolution_choice_on_1080p_outputs() {
        let mut v = SettingsView::new(VP, "alice", "home", Vec::new(), values(None), devices());
        press(&mut v, InputAction::Down);
        assert_eq!(v.toggle_state(v.rows()[1][0]), Some(true));
        assert_eq!(select(&mut v), Some(Change::MatchRefresh(false)));
    }

    #[test]
    fn audio_row() {
        let mut v = SettingsView::new(VP, "alice", "home", Vec::new(), values(None), devices());
        press(&mut v, InputAction::Down);
        press(&mut v, InputAction::Down);
        assert_eq!(v.focus, (2, 0));
        // Output list: opens on "auto" (first); the HDMI device is 2 down.
        assert_eq!(select(&mut v), None);
        press(&mut v, InputAction::Down);
        press(&mut v, InputAction::Down);
        assert_eq!(select(&mut v), Some(Change::AudioDevice("alsa/hdmi:CARD=PCH,DEV=0".into())));
        // Back closes a list without a change.
        select(&mut v);
        assert!(press(&mut v, InputAction::Back));
        assert!(v.blade.is_none());
        press(&mut v, InputAction::Right);
        assert_eq!(select(&mut v), Some(Change::Passthrough(true)));
        press(&mut v, InputAction::Right);
        select(&mut v);
        press(&mut v, InputAction::Down);
        assert_eq!(select(&mut v), Some(Change::Channels("stereo".into())));
    }

    #[test]
    fn device_list_waits_for_the_probe() {
        let mut v = SettingsView::new(VP, "alice", "home", Vec::new(), values(None), Vec::new());
        press(&mut v, InputAction::Down);
        press(&mut v, InputAction::Down);
        assert_eq!(select(&mut v), None);
        assert!(v.blade.is_none());
        assert_eq!(v.device_label(), s::fill(s::AUDIO_OUTPUT, &[s::AUDIO_AUTO]));
    }

    #[test]
    fn playback_and_storage_rows() {
        let mut v = SettingsView::new(VP, "alice", "home", Vec::new(), values(None), devices());
        for _ in 0..3 {
            press(&mut v, InputAction::Down);
        }
        assert_eq!(v.focus, (3, 0));
        assert_eq!(select(&mut v), Some(Change::Interpolation(true)), "a toggle");
        press(&mut v, InputAction::Right);
        assert_eq!(v.label(Item::Deinterlace), "Deinterlacing: Automatic  ›");
        select(&mut v);
        press(&mut v, InputAction::Down);
        assert_eq!(select(&mut v), Some(Change::Deinterlace(Deinterlace::Always)));
        press(&mut v, InputAction::Right);
        select(&mut v);
        press(&mut v, InputAction::Down);
        assert_eq!(select(&mut v), Some(Change::ToneMapping("bt.2390".into())));
        press(&mut v, InputAction::Down);
        assert_eq!(v.focus, (4, 1), "the column is kept where it can be");
        press(&mut v, InputAction::Left);
        assert_eq!(v.label(Item::ImageCache), "Artwork cache: 2 GB  ›");
        select(&mut v);
        press(&mut v, InputAction::Down);
        assert_eq!(select(&mut v), Some(Change::ImageCacheMb(4096)));
        press(&mut v, InputAction::Right);
        assert_eq!(select(&mut v), Some(Change::ClearCache));
    }

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(size_label(300 << 20), "300 MB");
        assert_eq!(size_label(2048 << 20), "2 GB");
        assert_eq!(size_label(1536 << 20), "1.5 GB");
    }
}
