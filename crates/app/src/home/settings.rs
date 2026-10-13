//! Settings (spec 10.1): the account (sign out, change server), the UI
//! resolution on displays above 1080p (D44), and what this device is
//! (versions, display, which codecs the GPU decodes). Input and cache
//! settings are in config.toml for now.

use std::time::Instant;

use jellyroll_render::{Point, Rect, Renderer, TextStyle, Weight};

use super::pages::{Ctx, Open};
use super::strings as s;
use super::theme::{BG, TEXT, TEXT_DIM};
use super::widgets::{self, Viewport};
use crate::config::UiResolution;
use crate::input::InputAction;
use crate::net::Cmd;

#[derive(Copy, Clone, Debug, PartialEq)]
enum Action {
    SignOut,
    ChangeServer,
}

const ACTIONS: [(Action, &str); 2] =
    [(Action::SignOut, s::SIGN_OUT), (Action::ChangeServer, s::CHANGE_SERVER)];
/// Focus index of the resolution button (after the account buttons).
const RES: usize = ACTIONS.len();

pub struct SettingsView {
    vp: Viewport,
    account: String,
    about: Vec<(String, String)>,
    /// The UI resolution in effect; None when there is no choice.
    resolution: Option<UiResolution>,
    focus: usize,
    rects: Vec<Rect>,
}

impl SettingsView {
    pub fn new(
        vp: Viewport,
        user: &str,
        server: &str,
        about: Vec<(String, String)>,
        resolution: Option<UiResolution>,
    ) -> Self {
        Self {
            vp,
            account: s::fill(s::SIGNED_IN_AS, &[user, server]),
            about,
            resolution,
            focus: 0,
            rects: Vec::new(),
        }
    }

    fn resolution_label(&self) -> &'static str {
        match self.resolution {
            Some(UiResolution::Native) => s::UI_RES_NATIVE,
            _ => s::UI_RES_1080,
        }
    }

    pub fn set_viewport(&mut self, vp: Viewport) -> bool {
        let changed = vp != self.vp;
        self.vp = vp;
        if changed {
            self.rects.clear();
        }
        changed
    }

    pub fn on_action(&mut self, a: InputAction, ctx: &mut Ctx) -> bool {
        match a {
            InputAction::Left if (1..RES).contains(&self.focus) => self.focus -= 1,
            InputAction::Right if self.focus + 1 < RES => self.focus += 1,
            InputAction::Down if self.focus < RES && self.resolution.is_some() => {
                self.focus = RES;
            }
            InputAction::Up if self.focus == RES => self.focus = 0,
            InputAction::Select if self.focus == RES => {
                let next = match self.resolution {
                    Some(UiResolution::Native) => UiResolution::Hd1080,
                    _ => UiResolution::Native,
                };
                self.resolution = Some(next);
                ctx.open = Some(Open::UiResolution(next));
            }
            InputAction::Select => ctx.send(match ACTIONS[self.focus].0 {
                Action::SignOut => Cmd::SignOut,
                Action::ChangeServer => Cmd::ChangeServer,
            }),
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
        let mut y = sy + 90.0;
        r.text(s::ACCOUNT, &head, Point::new(sx, y), TEXT);
        y += 46.0;
        r.text(&self.account, &body, Point::new(sx, y), TEXT_DIM);
        y += 46.0;
        if self.rects.is_empty() {
            let mut x = sx;
            for (_, label) in ACTIONS {
                let w = widgets::button_width(r, label).max(200.0);
                self.rects.push(Rect::new(x, y, w, 52.0));
                x += w + 14.0;
            }
        }
        for (i, (_, label)) in ACTIONS.iter().enumerate() {
            widgets::draw_button(r, self.rects[i], label, self.focus == i, false);
        }
        y += 100.0;
        if self.resolution.is_some() {
            r.text(s::DISPLAY, &head, Point::new(sx, y), TEXT);
            y += 46.0;
            let label = self.resolution_label();
            let w = widgets::button_width(r, label).max(200.0);
            widgets::draw_button(r, Rect::new(sx, y, w, 52.0), label, self.focus == RES, false);
            y += 66.0;
            r.text(s::UI_RES_HINT, &body, Point::new(sx, y), TEXT_DIM);
            y += 70.0;
        }
        r.text(s::ABOUT, &head, Point::new(sx, y), TEXT);
        y += 50.0;
        let key = TextStyle::new(22.0, Weight::Bold);
        for (k, v) in &self.about {
            r.text(k, &key, Point::new(sx, y), TEXT_DIM);
            r.text(v, &body, Point::new(sx + 320.0, y), TEXT);
            y += 36.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VP: Viewport = Viewport { lw: 1920.0, safe: 0.025, scale: 2.0 };

    #[test]
    fn resolution_toggles_between_native_and_1080p() {
        let mut v = SettingsView::new(VP, "alice", "home", Vec::new(), Some(UiResolution::Hd1080));
        let mut next = 0;
        let mut ctx = Ctx::new(&mut next);
        assert!(v.on_action(InputAction::Down, &mut ctx));
        assert_eq!(v.focus, RES);
        v.on_action(InputAction::Select, &mut ctx);
        assert!(matches!(ctx.open, Some(Open::UiResolution(UiResolution::Native))));
        assert_eq!(v.resolution_label(), s::UI_RES_NATIVE);
        let mut ctx = Ctx::new(&mut next);
        v.on_action(InputAction::Select, &mut ctx);
        assert!(matches!(ctx.open, Some(Open::UiResolution(UiResolution::Hd1080))));
        v.on_action(InputAction::Up, &mut ctx);
        assert_eq!(v.focus, 0, "back to the account buttons");
    }

    #[test]
    fn no_resolution_choice_on_1080p_outputs() {
        let mut v = SettingsView::new(VP, "alice", "home", Vec::new(), None);
        let mut next = 0;
        let mut ctx = Ctx::new(&mut next);
        assert!(!v.on_action(InputAction::Down, &mut ctx));
        assert_eq!(v.focus, 0);
    }
}
