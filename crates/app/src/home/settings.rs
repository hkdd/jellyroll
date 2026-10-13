//! Settings (spec 10.1): the account (sign out, change server) and what
//! this device is (versions, display, which codecs the GPU decodes). Display,
//! input and cache settings are in config.toml for now.

use std::time::Instant;

use jellyroll_render::{Point, Rect, Renderer, TextStyle, Weight};

use super::pages::Ctx;
use super::strings as s;
use super::theme::{BG, TEXT, TEXT_DIM};
use super::widgets::{self, Viewport};
use crate::input::InputAction;
use crate::net::Cmd;

#[derive(Copy, Clone, Debug, PartialEq)]
enum Action {
    SignOut,
    ChangeServer,
}

const ACTIONS: [(Action, &str); 2] =
    [(Action::SignOut, s::SIGN_OUT), (Action::ChangeServer, s::CHANGE_SERVER)];

pub struct SettingsView {
    vp: Viewport,
    account: String,
    about: Vec<(String, String)>,
    focus: usize,
    rects: Vec<Rect>,
}

impl SettingsView {
    pub fn new(vp: Viewport, user: &str, server: &str, about: Vec<(String, String)>) -> Self {
        Self {
            vp,
            account: s::fill(s::SIGNED_IN_AS, &[user, server]),
            about,
            focus: 0,
            rects: Vec::new(),
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
            InputAction::Left if self.focus > 0 => self.focus -= 1,
            InputAction::Right if self.focus + 1 < ACTIONS.len() => self.focus += 1,
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
