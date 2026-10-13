//! The server screen (spec 8: manual URL entry plus discovery): type the
//! server's address, with a physical keyboard or the on-screen one (remote,
//! gamepad), or search the LAN and pick a server.

use jellyroll_render::{Color, Point, Rect, Renderer, TextStyle, Weight};

use super::theme::{ACCENT, BG, NOTICE, TEXT, TEXT_DIM};
use crate::focus::{Dir, FocusGraph, NodeId};
use crate::input::InputAction;
use crate::net::{Cmd, FoundServer};

/// On-screen keyboard: enough for host names, IPv4 addresses and URLs.
const KEY_ROWS: [&str; 4] = ["1234567890", "qwertyuiop", "asdfghjkl.", "zxcvbnm-:/"];
/// Bottom row: label, text it inserts (None = action), width in keys.
const SPECIAL: [(&str, Special, f32); 4] = [
    ("Delete", Special::Delete, 2.0),
    ("Clear", Special::Clear, 2.0),
    (":8096", Special::Insert(":8096"), 3.0),
    ("https://", Special::Insert("https://"), 3.0),
];
const MAX_LEN: usize = 200;

#[derive(Copy, Clone, PartialEq, Debug)]
enum Special {
    Delete,
    Clear,
    Insert(&'static str),
}

#[derive(Copy, Clone, PartialEq, Debug)]
enum Element {
    Char(char),
    Special(Special),
    Connect,
    Search,
    Found(usize),
}

const KEY_W: f32 = 84.0;
const KEY_H: f32 = 64.0;
const GAP: f32 = 10.0;
const KB_W: f32 = 10.0 * KEY_W + 9.0 * GAP;
const LIST_W: f32 = 560.0;
const COL_GAP: f32 = 80.0;
const TOP: f32 = 110.0;

const TITLE: TextStyle = TextStyle::new(44.0, Weight::Bold);
const FIELD: TextStyle = TextStyle::new(34.0, Weight::Regular).max_width(KB_W - 40.0);
const SMALL: TextStyle = TextStyle::new(20.0, Weight::Regular);
const KEY: TextStyle = TextStyle::new(28.0, Weight::Regular);
const BUTTON: TextStyle = TextStyle::new(24.0, Weight::Bold);
const ITEM: TextStyle = TextStyle::new(24.0, Weight::Bold).max_width(LIST_W - 40.0);
const ITEM_SUB: TextStyle = TextStyle::new(18.0, Weight::Regular).max_width(LIST_W - 40.0);

pub struct ServerEntry {
    text: String,
    error: Option<String>,
    found: Vec<FoundServer>,
    searching: bool,
    elements: Vec<(Element, Rect)>,
    graph: FocusGraph,
    focus: usize,
    /// Logical width the layout was built for.
    lw: f32,
}

impl ServerEntry {
    pub fn new(address: &str) -> Self {
        let mut e = Self {
            text: address.to_string(),
            error: None,
            found: Vec::new(),
            searching: false,
            elements: Vec::new(),
            graph: FocusGraph::new(),
            focus: 0,
            lw: 0.0,
        };
        e.layout(1920.0);
        e
    }

    /// Search results or an error from the network thread. What the user
    /// typed is kept.
    pub fn update(&mut self, error: Option<String>, found: Vec<FoundServer>, searching: bool) {
        let had = self.found.len();
        self.error = error;
        self.found = found;
        self.searching = searching;
        if self.found.len() != had {
            let lw = self.lw;
            self.lw = 0.0;
            self.layout(lw);
            // Results arrived: put focus on the first one.
            if had == 0
                && let Some(i) = self.elements.iter().position(|e| matches!(e.0, Element::Found(_)))
            {
                self.set_focus(i);
            }
        }
    }

    fn x0(lw: f32) -> f32 {
        ((lw - (KB_W + COL_GAP + LIST_W)) / 2.0).max(20.0)
    }

    fn field_rect(lw: f32) -> Rect {
        Rect::new(Self::x0(lw), TOP + 90.0, KB_W, 76.0)
    }

    fn layout(&mut self, lw: f32) {
        if lw == self.lw {
            return;
        }
        self.lw = lw;
        let focused = self.elements.get(self.focus).map(|e| e.0);
        self.elements.clear();
        let x0 = Self::x0(lw);
        let mut y = TOP + 270.0;
        for row in KEY_ROWS {
            for (i, c) in row.chars().enumerate() {
                let r = Rect::new(x0 + i as f32 * (KEY_W + GAP), y, KEY_W, KEY_H);
                self.elements.push((Element::Char(c), r));
            }
            y += KEY_H + GAP;
        }
        let mut x = x0;
        for (_, s, units) in SPECIAL {
            let w = units * KEY_W + (units - 1.0) * GAP;
            self.elements.push((Element::Special(s), Rect::new(x, y, w, KEY_H)));
            x += w + GAP;
        }
        y += KEY_H + 28.0;
        let half = (KB_W - GAP) / 2.0;
        self.elements.push((Element::Connect, Rect::new(x0, y, half, 72.0)));
        self.elements.push((Element::Search, Rect::new(x0 + half + GAP, y, half, 72.0)));
        let lx = x0 + KB_W + COL_GAP;
        for i in 0..self.found.len().min(6) {
            let r = Rect::new(lx, TOP + 270.0 + i as f32 * 92.0, LIST_W, 80.0);
            self.elements.push((Element::Found(i), r));
        }
        self.graph.set_nodes(self.elements.iter().enumerate().map(|(i, e)| (i as NodeId, e.1)));
        let f = focused.and_then(|f| self.elements.iter().position(|e| e.0 == f)).unwrap_or(0);
        self.set_focus(f);
    }

    fn set_focus(&mut self, i: usize) {
        self.focus = i;
        self.graph.focus(i as NodeId);
    }

    fn insert(&mut self, s: &str) {
        for c in s.chars() {
            if self.text.chars().count() < MAX_LEN {
                self.text.push(c);
            }
        }
        self.error = None;
    }

    /// Handles an action; returns whether it was used and a command for
    /// the network thread.
    pub fn on_action(&mut self, a: InputAction) -> (bool, Option<Cmd>) {
        if let Some(dir) = Dir::from_action(a) {
            return match self.graph.navigate(dir) {
                Some(id) => {
                    self.focus = id as usize;
                    (true, None)
                }
                None => (false, None),
            };
        }
        match a {
            InputAction::TextInput(c) => {
                let mut b = [0u8; 4];
                self.insert(c.encode_utf8(&mut b));
                (true, None)
            }
            InputAction::Backspace => (self.text.pop().is_some(), None),
            InputAction::Submit => (true, self.connect()),
            InputAction::Select => match self.elements.get(self.focus).map(|e| e.0) {
                Some(Element::Char(c)) => {
                    let mut b = [0u8; 4];
                    self.insert(c.encode_utf8(&mut b));
                    (true, None)
                }
                Some(Element::Special(Special::Delete)) => (self.text.pop().is_some(), None),
                Some(Element::Special(Special::Clear)) => {
                    self.text.clear();
                    (true, None)
                }
                Some(Element::Special(Special::Insert(s))) => {
                    self.insert(s);
                    (true, None)
                }
                Some(Element::Connect) => (true, self.connect()),
                Some(Element::Search) if !self.searching => {
                    self.searching = true;
                    self.error = None;
                    (true, Some(Cmd::Search))
                }
                Some(Element::Found(i)) => match self.found.get(i) {
                    Some(s) => {
                        self.text = s.address.clone();
                        (true, self.connect())
                    }
                    None => (false, None),
                },
                _ => (false, None),
            },
            _ => (false, None),
        }
    }

    fn connect(&mut self) -> Option<Cmd> {
        let t = self.text.trim();
        if t.is_empty() {
            self.error = Some("Enter the server's address first.".into());
            return None;
        }
        Some(Cmd::Connect(t.to_string()))
    }

    pub fn draw<R: Renderer>(&mut self, r: &mut R, safe_area: f32, notice: &str) {
        let (lw, lh) = r.logical_size();
        self.layout(lw);
        r.clear(BG);
        let (sx, sy) = (lw * safe_area, lh * safe_area);
        r.text("Jellyroll", &TextStyle::new(32.0, Weight::Bold), Point::new(sx, sy), TEXT);

        let x0 = Self::x0(lw);
        r.text("Connect to your Jellyfin server", &TITLE, Point::new(x0, TOP), TEXT);

        // Address field with a caret.
        let f = Self::field_rect(lw);
        r.fill_rrect(f, 12.0, Color::rgba(255, 255, 255, 24));
        r.stroke_rrect(f, 12.0, 2.0, ACCENT);
        let ty = f.y + (f.h - 46.0) / 2.0;
        if self.text.is_empty() {
            r.text(
                "e.g. 192.168.1.10 or jellyfin.local",
                &FIELD,
                Point::new(f.x + 20.0, ty),
                TEXT_DIM,
            );
            r.fill_rect(Rect::new(f.x + 20.0, ty + 6.0, 3.0, 36.0), TEXT);
        } else {
            let m = r.text(&self.text, &FIELD, Point::new(f.x + 20.0, ty), TEXT);
            r.fill_rect(Rect::new(f.x + 22.0 + m.width, ty + 6.0, 3.0, 36.0), TEXT);
        }
        let hint = "http:// and port 8096 are added if you leave them out.";
        r.text(hint, &SMALL, Point::new(x0, f.bottom() + 14.0), TEXT_DIM);
        if let Some(e) = &self.error {
            let st = TextStyle::new(22.0, Weight::Bold).max_width(KB_W + COL_GAP + LIST_W);
            r.text(e, &st, Point::new(x0, f.bottom() + 46.0), NOTICE);
        }

        // Keys and buttons.
        for (i, (el, rect)) in self.elements.iter().enumerate() {
            let focused = i == self.focus;
            let (bg, fg) = match (focused, el) {
                (true, _) => (Color::WHITE, BG),
                (false, Element::Connect) => (ACCENT, Color::WHITE),
                (false, _) => (Color::rgba(255, 255, 255, 22), TEXT),
            };
            r.fill_rrect(*rect, 10.0, bg);
            match el {
                Element::Found(n) => {
                    if let Some(s) = self.found.get(*n) {
                        r.text(&s.name, &ITEM, Point::new(rect.x + 20.0, rect.y + 10.0), fg);
                        let sub = if focused { BG } else { TEXT_DIM };
                        r.text(
                            &s.address,
                            &ITEM_SUB,
                            Point::new(rect.x + 20.0, rect.y + 46.0),
                            sub,
                        );
                    }
                }
                _ => {
                    let mut b = [0u8; 4];
                    let (label, style) = match el {
                        Element::Char(c) => (&*c.encode_utf8(&mut b), &KEY),
                        Element::Special(s) => {
                            (SPECIAL.iter().find(|x| x.1 == *s).map_or("", |x| x.0), &KEY)
                        }
                        Element::Connect => ("Connect", &BUTTON),
                        Element::Search if self.searching => ("Searching…", &BUTTON),
                        Element::Search => ("Search my network", &BUTTON),
                        Element::Found(_) => ("", &KEY),
                    };
                    let m = r.measure_text(label, style);
                    let p = Point::new(
                        (rect.x + (rect.w - m.width) / 2.0).round(),
                        (rect.y + (rect.h - m.height) / 2.0).round(),
                    );
                    r.text(label, style, p, fg);
                }
            }
        }

        // Search results column.
        let lx = x0 + KB_W + COL_GAP;
        r.text(
            "On your network",
            &TextStyle::new(26.0, Weight::Bold),
            Point::new(lx, TOP + 220.0),
            TEXT,
        );
        if self.found.is_empty() {
            let msg = if self.searching {
                "Searching…"
            } else {
                "Choose Search my network to look for servers."
            };
            r.text(msg, &SMALL.max_width(LIST_W), Point::new(lx, TOP + 275.0), TEXT_DIM);
        }
        if !notice.is_empty() {
            r.text(
                notice,
                &TextStyle::new(20.0, Weight::Bold),
                Point::new(sx, lh - sy - 30.0),
                NOTICE,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(e: &mut ServerEntry, a: InputAction) -> Option<Cmd> {
        e.on_action(a).1
    }

    #[test]
    fn remote_types_an_address_with_the_keyboard() {
        let mut e = ServerEntry::new("");
        // Focus starts on '1'.
        press(&mut e, InputAction::Select);
        press(&mut e, InputAction::Right); // '2'
        press(&mut e, InputAction::Select);
        assert_eq!(e.text, "12");
        // Down three rows from '2' lands on 'x' (row zxcvbnm-:/).
        for _ in 0..3 {
            press(&mut e, InputAction::Down);
        }
        press(&mut e, InputAction::Select);
        assert_eq!(e.text, "12x");
        // Down to the special row, select Delete.
        press(&mut e, InputAction::Down);
        assert_eq!(e.elements[e.focus].0, Element::Special(Special::Delete));
        press(&mut e, InputAction::Select);
        assert_eq!(e.text, "12");
        // Down to Connect.
        press(&mut e, InputAction::Down);
        assert_eq!(e.elements[e.focus].0, Element::Connect);
        assert!(matches!(press(&mut e, InputAction::Select), Some(Cmd::Connect(a)) if a == "12"));
    }

    #[test]
    fn physical_keyboard_types_and_submits() {
        let mut e = ServerEntry::new("old");
        press(&mut e, InputAction::Backspace);
        for c in "1:8096".chars() {
            press(&mut e, InputAction::TextInput(c));
        }
        assert_eq!(e.text, "ol1:8096");
        assert!(
            matches!(press(&mut e, InputAction::Submit), Some(Cmd::Connect(a)) if a == "ol1:8096")
        );
        e.text.clear();
        assert!(press(&mut e, InputAction::Submit).is_none());
        assert!(e.error.is_some(), "empty address explains itself");
    }

    #[test]
    fn search_results_are_pickable() {
        let mut e = ServerEntry::new("");
        let search = e.elements.iter().position(|x| x.0 == Element::Search).unwrap();
        e.set_focus(search);
        assert!(matches!(press(&mut e, InputAction::Select), Some(Cmd::Search)));
        assert!(press(&mut e, InputAction::Select).is_none(), "no double search");
        e.update(
            None,
            vec![FoundServer { name: "Basement".into(), address: "http://10.0.0.5:8096".into() }],
            false,
        );
        assert_eq!(e.elements[e.focus].0, Element::Found(0), "focus jumps to the result");
        assert!(
            matches!(press(&mut e, InputAction::Select), Some(Cmd::Connect(a)) if a == "http://10.0.0.5:8096")
        );
    }
}
