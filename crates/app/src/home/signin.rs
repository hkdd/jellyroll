//! Password sign-in (spec 8: the fallback to Quick Connect): username and
//! password fields, the server's public users to pick from, and the
//! on-screen keyboard. A physical keyboard types into the focused field;
//! Enter moves from username to password, then signs in. The password is
//! sent once and never stored (D34).

use jellyroll_render::{Point, Rect, Renderer, TextStyle, Weight};

use super::osk::{self, Osk};
use super::strings as s;
use super::theme::{BG, NOTICE, TEXT, TEXT_DIM};
use super::widgets;
use crate::focus::{Dir, FocusGraph, NodeId};
use crate::input::InputAction;
use crate::net::{Cmd, PublicUser};

const MAX_LEN: usize = 128;
const FIELD_W: f32 = 600.0;
/// Node ids: keys are 0.., then these.
const USER_FIELD: NodeId = 1000;
const PASS_FIELD: NodeId = 1001;
const SIGN_IN: NodeId = 1002;
const USER_BASE: NodeId = 2000;

pub struct SignInForm {
    server: String,
    username: String,
    password: String,
    users: Vec<PublicUser>,
    error: Option<String>,
    busy: bool,
    osk: Osk,
    graph: FocusGraph,
    focus: NodeId,
    /// The field the keyboard types into.
    editing: NodeId,
    rects: Vec<(NodeId, Rect)>,
}

impl SignInForm {
    pub fn new(server: &str, sx: f32, sy: f32) -> Self {
        let mut f = Self {
            server: server.into(),
            username: String::new(),
            password: String::new(),
            users: Vec::new(),
            error: None,
            busy: false,
            osk: Osk::new(sx, sy + 330.0, 64.0, 56.0, 8.0, &osk::ROWS_LETTERS),
            graph: FocusGraph::new(),
            focus: USER_FIELD,
            editing: USER_FIELD,
            rects: Vec::new(),
        };
        f.layout(sx, sy);
        f
    }

    fn layout(&mut self, sx: f32, sy: f32) {
        self.rects.clear();
        self.rects.push((USER_FIELD, Rect::new(sx, sy + 110.0, FIELD_W, 66.0)));
        self.rects.push((PASS_FIELD, Rect::new(sx, sy + 196.0, FIELD_W, 66.0)));
        self.rects.push((SIGN_IN, Rect::new(sx + FIELD_W + 30.0, sy + 196.0, 220.0, 66.0)));
        let lx = sx + FIELD_W + 300.0;
        for i in 0..self.users.len().min(8) {
            self.rects.push((
                USER_BASE + i as NodeId,
                Rect::new(lx, sy + 150.0 + i as f32 * 70.0, 420.0, 58.0),
            ));
        }
        let nodes = self
            .rects
            .iter()
            .copied()
            .chain(self.osk.keys.iter().enumerate().map(|(i, k)| (i as NodeId, k.1)));
        self.graph.set_nodes(nodes.collect::<Vec<_>>());
        self.graph.focus(self.focus);
    }

    pub fn set_users(&mut self, users: Vec<PublicUser>, sx: f32, sy: f32) {
        self.users = users;
        self.layout(sx, sy);
    }

    pub fn failed(&mut self, msg: String) {
        self.busy = false;
        self.error = Some(msg);
        self.password.clear();
        self.focus = PASS_FIELD;
        self.editing = PASS_FIELD;
        self.graph.focus(self.focus);
    }

    fn field(&mut self) -> &mut String {
        if self.editing == PASS_FIELD { &mut self.password } else { &mut self.username }
    }

    fn submit(&mut self) -> Option<Cmd> {
        if self.username.trim().is_empty() {
            self.error = Some(format!("{}?", s::USERNAME));
            self.focus = USER_FIELD;
            self.editing = USER_FIELD;
            self.graph.focus(self.focus);
            return None;
        }
        self.busy = true;
        self.error = None;
        Some(Cmd::PasswordSignIn {
            username: self.username.trim().into(),
            password: self.password.clone(),
        })
    }

    /// Handles an action; returns whether it was used and a command.
    pub fn on_action(&mut self, a: InputAction) -> (bool, Option<Cmd>) {
        if self.busy {
            return (matches!(a, InputAction::Select | InputAction::Submit), None);
        }
        if let Some(dir) = Dir::from_action(a) {
            return match self.graph.navigate(dir) {
                Some(id) => {
                    self.focus = id;
                    if id == USER_FIELD || id == PASS_FIELD {
                        self.editing = id;
                    }
                    (true, None)
                }
                None => (false, None),
            };
        }
        match a {
            InputAction::TextInput(c) => {
                if self.field().chars().count() < MAX_LEN {
                    self.field().push(c);
                }
                (true, None)
            }
            InputAction::Backspace => (self.field().pop().is_some(), None),
            InputAction::Submit if self.editing == USER_FIELD => {
                self.editing = PASS_FIELD;
                self.focus = PASS_FIELD;
                self.graph.focus(self.focus);
                (true, None)
            }
            InputAction::Submit => (true, self.submit()),
            InputAction::Select => match self.focus {
                USER_FIELD | PASS_FIELD => {
                    self.editing = self.focus;
                    (true, None)
                }
                SIGN_IN => (true, self.submit()),
                id if id >= USER_BASE => {
                    if let Some(u) = self.users.get((id - USER_BASE) as usize) {
                        self.username = u.name.clone();
                        self.password.clear();
                        if u.has_password {
                            self.editing = PASS_FIELD;
                            self.focus = PASS_FIELD;
                            self.graph.focus(self.focus);
                            return (true, None);
                        }
                        return (true, self.submit());
                    }
                    (false, None)
                }
                id => {
                    let key = self.osk.keys[id as usize].0;
                    let mut t = std::mem::take(self.field());
                    let used = self.osk.apply(key, &mut t, MAX_LEN);
                    *self.field() = t;
                    (used, None)
                }
            },
            _ => (false, None),
        }
    }

    pub fn draw<R: Renderer>(&mut self, r: &mut R, sx: f32, sy: f32) {
        r.clear(BG);
        let title = format!("{} · {}", s::SIGN_IN, self.server);
        r.text(&title, &TextStyle::new(40.0, Weight::Bold), Point::new(sx, sy + 20.0), TEXT);
        let get =
            |id: NodeId| self.rects.iter().find(|r| r.0 == id).map(|r| r.1).unwrap_or_default();
        osk::draw_field(
            r,
            get(USER_FIELD),
            &self.username,
            s::USERNAME,
            self.editing == USER_FIELD,
            false,
        );
        osk::draw_field(
            r,
            get(PASS_FIELD),
            &self.password,
            s::PASSWORD,
            self.editing == PASS_FIELD,
            true,
        );
        if self.focus == USER_FIELD || self.focus == PASS_FIELD {
            let f = get(self.focus);
            r.stroke_rrect(f.inset(-4.0), 14.0, 3.0, jellyroll_render::Color::WHITE);
        }
        let label = if self.busy { s::SIGNING_IN } else { s::SIGN_IN };
        widgets::draw_button(r, get(SIGN_IN), label, self.focus == SIGN_IN, true);
        if let Some(e) = &self.error {
            r.text(e, &TextStyle::new(22.0, Weight::Bold), Point::new(sx, sy + 280.0), NOTICE);
        }
        let key_focus = (self.focus < 1000).then_some(self.focus as usize);
        self.osk.draw(r, key_focus);
        if !self.users.is_empty() {
            let lx = sx + FIELD_W + 300.0;
            r.text(
                s::USERS_ON_SERVER,
                &TextStyle::new(24.0, Weight::Bold),
                Point::new(lx, sy + 110.0),
                TEXT_DIM,
            );
            for (i, u) in self.users.iter().take(8).enumerate() {
                let id = USER_BASE + i as NodeId;
                widgets::draw_button(
                    r,
                    get(id),
                    &u.name,
                    self.focus == id,
                    self.username == u.name,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_username_and_password_then_sign_in() {
        let mut f = SignInForm::new("home", 48.0, 27.0);
        for c in "alice".chars() {
            f.on_action(InputAction::TextInput(c));
        }
        f.on_action(InputAction::Submit);
        for c in "pw".chars() {
            f.on_action(InputAction::TextInput(c));
        }
        let (_, cmd) = f.on_action(InputAction::Submit);
        assert!(
            matches!(cmd, Some(Cmd::PasswordSignIn { ref username, ref password }) if username == "alice" && password == "pw")
        );
        f.failed("Wrong username or password.".into());
        assert!(f.password.is_empty(), "password cleared after a failure");
        assert_eq!(f.username, "alice", "username kept");
    }

    #[test]
    fn picking_a_user_without_password_signs_in() {
        let mut f = SignInForm::new("home", 48.0, 27.0);
        f.set_users(vec![PublicUser { name: "kids".into(), has_password: false }], 48.0, 27.0);
        f.focus = USER_BASE;
        let (_, cmd) = f.on_action(InputAction::Select);
        assert!(
            matches!(cmd, Some(Cmd::PasswordSignIn { ref username, .. }) if username == "kids")
        );
    }
}
