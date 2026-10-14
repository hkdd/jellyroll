//! Who's watching? (spec 8: several users and servers, a user picker at
//! startup). One tile per signed-in account, then "Add user" (another
//! person on this server) and "Add server". Picking an account switches
//! to it without signing anyone out. Opened at startup when more than one
//! account is signed in, from Settings ("Switch user"), and after signing
//! out while others remain.

use jellyroll_render::{Color, Point, Rect, Renderer, TextStyle, Weight};

use super::strings as s;
use super::theme::{BG, TEXT, TEXT_DIM};
use super::widgets;
use crate::input::InputAction;
use crate::net::store::fnv1a;
use crate::net::{Account, Cmd};

const TILE: f32 = 180.0;
const GAP: f32 = 64.0;
const TILES_Y: f32 = 380.0;
const BUTTONS_Y: f32 = 800.0;
const BUTTON_H: f32 = 56.0;

const TITLE: TextStyle = TextStyle::new(48.0, Weight::Bold);
const INITIAL: TextStyle = TextStyle::new(84.0, Weight::Bold);
const NAME: TextStyle = TextStyle::new(28.0, Weight::Bold).max_width(TILE + GAP - 8.0);
const SERVER: TextStyle = TextStyle::new(20.0, Weight::Regular).max_width(TILE + GAP - 8.0);

/// Avatar colors, picked per user id.
const PALETTE: [Color; 6] = [
    Color::rgb(0, 140, 190),
    Color::rgb(170, 70, 150),
    Color::rgb(40, 150, 100),
    Color::rgb(200, 120, 40),
    Color::rgb(110, 90, 200),
    Color::rgb(190, 70, 70),
];

pub struct AccountPicker {
    accounts: Vec<Account>,
    /// The account in use (switching); Back returns to it.
    current: Option<usize>,
    /// 0..n: accounts; n: Add user; n + 1: Add server.
    focus: usize,
    /// Accounts are on more than one server: show which.
    servers_differ: bool,
}

impl AccountPicker {
    pub fn new(accounts: Vec<Account>, current: Option<usize>) -> Self {
        let current = current.filter(|c| *c < accounts.len());
        let servers_differ = accounts.iter().any(|a| a.server_id != accounts[0].server_id);
        Self { focus: current.unwrap_or(0), accounts, current, servers_differ }
    }

    fn n(&self) -> usize {
        self.accounts.len()
    }

    fn pick(&self, i: usize) -> Cmd {
        let a = &self.accounts[i];
        Cmd::PickAccount { server_id: a.server_id.clone(), user_id: a.user_id.clone() }
    }

    /// (used, command for the network thread)
    pub fn on_action(&mut self, a: InputAction) -> (bool, Option<Cmd>) {
        let n = self.n();
        let on_tiles = self.focus < n;
        match a {
            InputAction::Left if on_tiles && self.focus > 0 => self.focus -= 1,
            InputAction::Right if on_tiles && self.focus + 1 < n => self.focus += 1,
            InputAction::Left if self.focus == n + 1 => self.focus = n,
            InputAction::Right if self.focus == n => self.focus = n + 1,
            InputAction::Down if on_tiles => self.focus = n,
            InputAction::Up if !on_tiles && n > 0 => self.focus = self.current.unwrap_or(0),
            InputAction::Select => {
                let cmd = match self.focus {
                    i if i < n => self.pick(i),
                    i if i == n => Cmd::AddUser,
                    _ => Cmd::ChangeServer,
                };
                return (true, Some(cmd));
            }
            // Changed your mind: back to whoever was watching.
            InputAction::Back => {
                return (self.current.is_some(), self.current.map(|c| self.pick(c)));
            }
            _ => return (false, None),
        }
        (true, None)
    }

    fn color(a: &Account) -> Color {
        PALETTE[(fnv1a(&[a.user_id.as_bytes()]) % PALETTE.len() as u64) as usize]
    }

    pub fn draw<R: Renderer>(&self, r: &mut R) {
        let (lw, _) = r.logical_size();
        r.clear(BG);
        let m = r.measure_text(s::WHOS_WATCHING, &TITLE);
        r.text(s::WHOS_WATCHING, &TITLE, Point::new((lw - m.width) / 2.0, 220.0), TEXT);

        let n = self.n();
        let row_w = n as f32 * TILE + n.saturating_sub(1) as f32 * GAP;
        let mut x = (lw - row_w) / 2.0;
        for (i, a) in self.accounts.iter().enumerate() {
            let focused = self.focus == i;
            let rect = Rect::new(x, TILES_Y, TILE, TILE);
            if focused {
                let ring = Rect::new(rect.x - 8.0, rect.y - 8.0, TILE + 16.0, TILE + 16.0);
                r.fill_rrect(ring, (TILE + 16.0) / 2.0, Color::WHITE);
            }
            r.fill_rrect(rect, TILE / 2.0, Self::color(a));
            let initial: String =
                a.user_name.chars().next().unwrap_or('?').to_uppercase().collect();
            let im = r.measure_text(&initial, &INITIAL);
            let at = Point::new(x + (TILE - im.width) / 2.0, TILES_Y + (TILE - im.height) / 2.0);
            r.text(&initial, &INITIAL, at, Color::WHITE);
            let fg = if focused { TEXT } else { TEXT_DIM };
            let nm = r.measure_text(&a.user_name, &NAME);
            r.text(
                &a.user_name,
                &NAME,
                Point::new(x + (TILE - nm.width) / 2.0, TILES_Y + TILE + 24.0),
                fg,
            );
            if self.servers_differ {
                let sm = r.measure_text(&a.server_name, &SERVER);
                let at = Point::new(x + (TILE - sm.width) / 2.0, TILES_Y + TILE + 64.0);
                r.text(&a.server_name, &SERVER, at, TEXT_DIM);
            }
            x += TILE + GAP;
        }

        let labels = [s::ADD_USER, s::ADD_SERVER];
        let widths: Vec<f32> =
            labels.iter().map(|l| widgets::button_width(r, l).max(200.0)).collect();
        let total = widths.iter().sum::<f32>() + 20.0;
        let mut x = (lw - total) / 2.0;
        for (k, (label, w)) in labels.iter().zip(widths).enumerate() {
            let rect = Rect::new(x, BUTTONS_Y, w, BUTTON_H);
            widgets::draw_button(r, rect, label, self.focus == n + k, false);
            x += w + 20.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acct(server: &str, user: &str) -> Account {
        Account {
            server_id: server.into(),
            server_name: format!("{server} server"),
            user_id: user.into(),
            user_name: user.into(),
        }
    }

    #[test]
    fn picks_adds_and_backs_out() {
        let mut p = AccountPicker::new(vec![acct("s", "ann"), acct("s", "bob")], Some(1));
        assert_eq!(p.focus, 1, "starts on whoever is watching");
        assert!(!p.servers_differ);
        p.on_action(InputAction::Left);
        let (_, cmd) = p.on_action(InputAction::Select);
        assert!(
            matches!(cmd, Some(Cmd::PickAccount { ref user_id, .. }) if user_id == "ann"),
            "{cmd:?}"
        );
        p.on_action(InputAction::Down);
        assert!(matches!(p.on_action(InputAction::Select).1, Some(Cmd::AddUser)));
        p.on_action(InputAction::Right);
        assert!(matches!(p.on_action(InputAction::Select).1, Some(Cmd::ChangeServer)));
        p.on_action(InputAction::Up);
        assert_eq!(p.focus, 1);
        let (used, cmd) = p.on_action(InputAction::Back);
        assert!(used);
        assert!(matches!(cmd, Some(Cmd::PickAccount { ref user_id, .. }) if user_id == "bob"));
    }

    #[test]
    fn at_startup_back_does_nothing() {
        let mut p = AccountPicker::new(vec![acct("a", "ann"), acct("b", "bob")], None);
        assert!(p.servers_differ);
        assert!(!p.on_action(InputAction::Back).0);
    }
}
