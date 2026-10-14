//! Power menu (spec 5.5, 10.1): restart or shut down the HTPC through
//! logind, or exit to the console (dev only). Opened with Back where there
//! is nothing left to go back to.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use jellyroll_render::{Color, Point, Rect, Renderer, TextStyle, Weight};

use super::strings as s;
use super::theme::{TEXT, TEXT_DIM};
use super::widgets;
use crate::input::InputAction;
use crate::power::{self, Action, Allowed, Answer, Caps};

/// How often a running request is checked for its answer.
const POLL: Duration = Duration::from_millis(100);

#[derive(Copy, Clone, Debug, PartialEq)]
enum Item {
    Exit,
    Reboot,
    PowerOff,
    Cancel,
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Stay,
    Close,
    /// Quit the app (dev only).
    Exit,
}

/// Starts logind requests; replaced in tests.
type Requester = Box<dyn Fn(Action) -> Answer>;

pub struct PowerMenu {
    items: Vec<Item>,
    focus: usize,
    caps: Arc<OnceLock<Caps>>,
    pending: Option<(Action, Answer)>,
    message: Option<String>,
    request: Requester,
}

impl PowerMenu {
    /// `allow_exit`: offer exiting to the console (dev, `--allow-exit`).
    pub fn new(allow_exit: bool) -> Self {
        Self::with(allow_exit, power::query_in_background(), Box::new(power::request))
    }

    fn with(allow_exit: bool, caps: Arc<OnceLock<Caps>>, request: Requester) -> Self {
        let mut items = Vec::new();
        if allow_exit {
            items.push(Item::Exit);
        }
        items.extend([Item::Reboot, Item::PowerOff, Item::Cancel]);
        // Start on Cancel: a stray Select must not shut the HTPC down.
        let focus = items.len() - 1;
        Self { items, focus, caps, pending: None, message: None, request }
    }

    fn label(item: Item) -> &'static str {
        match item {
            Item::Exit => s::EXIT_APP,
            Item::Reboot => s::REBOOT,
            Item::PowerOff => s::POWER_OFF,
            Item::Cancel => s::CANCEL,
        }
    }

    fn allowed(&self, a: Action) -> Option<&Allowed> {
        let c = self.caps.get()?;
        Some(match a {
            Action::Reboot => &c.reboot,
            Action::PowerOff => &c.power_off,
        })
    }

    pub fn on_action(&mut self, a: InputAction) -> Outcome {
        if self.pending.is_some() {
            // The system is going down; nothing to do but wait.
            return Outcome::Stay;
        }
        match a {
            InputAction::Up if self.focus > 0 => self.focus -= 1,
            InputAction::Down if self.focus + 1 < self.items.len() => self.focus += 1,
            InputAction::Back => return Outcome::Close,
            InputAction::Select => {
                let action = match self.items[self.focus] {
                    Item::Cancel => return Outcome::Close,
                    Item::Exit => return Outcome::Exit,
                    Item::Reboot => Action::Reboot,
                    Item::PowerOff => Action::PowerOff,
                };
                match self.allowed(action) {
                    Some(Allowed::NeedsAuth | Allowed::No) => {
                        self.message = Some(s::POWER_NOT_ALLOWED.into());
                    }
                    // Yes, or not known yet / logind unreachable: try; the
                    // error (if any) says why.
                    _ => {
                        tracing::info!(?action, "power action requested");
                        self.message = Some(
                            match action {
                                Action::Reboot => s::RESTARTING,
                                Action::PowerOff => s::SHUTTING_DOWN,
                            }
                            .into(),
                        );
                        self.pending = Some((action, (self.request)(action)));
                    }
                }
            }
            _ => {}
        }
        Outcome::Stay
    }

    /// Picks up a request's answer. Returns true if the menu changed.
    pub fn update(&mut self) -> bool {
        let Some((_, cell)) = &self.pending else { return false };
        match cell.get() {
            // Accepted: the system goes down; keep the message up.
            Some(Ok(())) => false,
            Some(Err(e)) => {
                self.message = Some(s::fill(s::POWER_FAILED, &[e]));
                self.pending = None;
                true
            }
            None => false,
        }
    }

    pub fn next_deadline(&self, now: Instant) -> Option<Instant> {
        self.pending.as_ref().filter(|(_, c)| c.get().is_none()).map(|_| now + POLL)
    }

    pub fn draw<R: Renderer>(&self, r: &mut R) {
        let (lw, lh) = r.logical_size();
        r.fill_rrect(Rect::new(0.0, 0.0, lw, lh), 0.0, Color::rgba(0, 0, 0, 170));
        let w = 640.0;
        let n = self.items.len() as f32;
        let h = 120.0 + n * 66.0 + if self.message.is_some() { 90.0 } else { 0.0 };
        let panel = Rect::new((lw - w) / 2.0, (lh - h) / 2.0, w, h);
        r.fill_rrect(panel, 18.0, Color::rgba(24, 27, 36, 250));
        let title = TextStyle::new(32.0, Weight::Bold);
        r.text(s::POWER_TITLE, &title, Point::new(panel.x + 40.0, panel.y + 36.0), TEXT);
        let mut y = panel.y + 100.0;
        for (i, item) in self.items.iter().enumerate() {
            let rect = Rect::new(panel.x + 40.0, y, w - 80.0, 52.0);
            let focused = i == self.focus && self.pending.is_none();
            widgets::draw_button(r, rect, Self::label(*item), focused, false);
            y += 66.0;
        }
        if let Some(m) = &self.message {
            let style = TextStyle::new(22.0, Weight::Regular).max_width(w - 80.0);
            r.text(m, &style, Point::new(panel.x + 40.0, y + 6.0), TEXT_DIM);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn caps(reboot: Allowed, power_off: Allowed) -> Arc<OnceLock<Caps>> {
        let c = Arc::new(OnceLock::new());
        let _ = c.set(Caps { reboot, power_off });
        c
    }

    /// A requester that records calls and answers with `answer`.
    fn fake(answer: Result<(), String>) -> (Requester, Rc<RefCell<Vec<Action>>>) {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let c = calls.clone();
        let r: Requester = Box::new(move |a| {
            c.borrow_mut().push(a);
            let cell = Arc::new(OnceLock::new());
            let _ = cell.set(answer.clone());
            cell
        });
        (r, calls)
    }

    #[test]
    fn starts_on_cancel_and_closes() {
        let (req, calls) = fake(Ok(()));
        let mut m = PowerMenu::with(false, caps(Allowed::Yes, Allowed::Yes), req);
        assert_eq!(m.items, [Item::Reboot, Item::PowerOff, Item::Cancel]);
        assert_eq!(m.on_action(InputAction::Select), Outcome::Close);
        assert!(calls.borrow().is_empty());
    }

    #[test]
    fn power_off_goes_to_logind() {
        let (req, calls) = fake(Ok(()));
        let mut m = PowerMenu::with(true, caps(Allowed::Yes, Allowed::Yes), req);
        assert_eq!(m.items[0], Item::Exit);
        m.on_action(InputAction::Up);
        assert_eq!(m.on_action(InputAction::Select), Outcome::Stay);
        assert_eq!(*calls.borrow(), [Action::PowerOff]);
        assert_eq!(m.message.as_deref(), Some(s::SHUTTING_DOWN));
        assert!(!m.update(), "accepted: stays as is while the system goes down");
        assert_eq!(m.on_action(InputAction::Back), Outcome::Stay, "no way out once asked");
    }

    #[test]
    fn not_allowed_says_so_without_asking() {
        let (req, calls) = fake(Ok(()));
        let mut m = PowerMenu::with(false, caps(Allowed::NeedsAuth, Allowed::No), req);
        m.on_action(InputAction::Up);
        m.on_action(InputAction::Up);
        m.on_action(InputAction::Select);
        assert!(calls.borrow().is_empty());
        assert_eq!(m.message.as_deref(), Some(s::POWER_NOT_ALLOWED));
    }

    #[test]
    fn errors_are_shown_and_the_menu_stays_usable() {
        let (req, _) = fake(Err("Access denied".into()));
        let mut m = PowerMenu::with(false, Arc::new(OnceLock::new()), req);
        m.on_action(InputAction::Up);
        m.on_action(InputAction::Up);
        m.on_action(InputAction::Select);
        assert!(m.update());
        assert_eq!(m.message, Some(s::fill(s::POWER_FAILED, &["Access denied"])));
        assert_eq!(m.on_action(InputAction::Back), Outcome::Close);
    }

    #[test]
    fn exit_only_in_dev() {
        let (req, _) = fake(Ok(()));
        let mut m = PowerMenu::with(true, caps(Allowed::Yes, Allowed::Yes), req);
        m.focus = 0;
        assert_eq!(m.on_action(InputAction::Select), Outcome::Exit);
    }
}
