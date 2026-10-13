//! Input normalization (spec 5.4): keyboard, gamepad and HDMI-CEC remote
//! all map to one `InputAction` stream, and one in-app repeat timer drives
//! held buttons from every source, so behaviour is identical whichever
//! device is used.

use std::time::{Duration, Instant};

use jellyroll_platform::{CecKey, Key, Modifiers, PadButton, PlatformEvent};

/// A physical button from any source; identifies what is being held for
/// repeat and release.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Button {
    Key(Key),
    Pad(PadButton),
    Cec(CecKey),
}

/// A press or release from any source.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Input {
    Press { button: Button, action: Option<InputAction> },
    Release(Button),
}

/// Normalizes a platform event into a press/release. OS key auto-repeat is
/// dropped (repeat is in-app); non-input events return `None`.
pub fn from_event(ev: &PlatformEvent) -> Option<Input> {
    Some(match *ev {
        PlatformEvent::KeyDown { repeat: true, .. } => return None,
        PlatformEvent::KeyDown { key, mods, .. } => {
            Input::Press { button: Button::Key(key), action: map_key(key, mods) }
        }
        PlatformEvent::KeyUp { key } => Input::Release(Button::Key(key)),
        PlatformEvent::PadDown { button } => {
            Input::Press { button: Button::Pad(button), action: map_pad(button) }
        }
        PlatformEvent::PadUp { button } => Input::Release(Button::Pad(button)),
        PlatformEvent::CecDown { key } => {
            Input::Press { button: Button::Cec(key), action: map_cec(key) }
        }
        PlatformEvent::CecUp { key } => Input::Release(Button::Cec(key)),
        _ => return None,
    })
}

/// Gamepad map (positional; South = A on Xbox pads): D-pad and left stick
/// navigate, A selects, B goes back, X play/pause, Y info, shoulders seek,
/// triggers previous/next, Start menu, View/Select search, Guide home.
pub fn map_pad(b: PadButton) -> Option<InputAction> {
    use InputAction as A;
    use PadButton as P;
    Some(match b {
        P::DPadUp | P::StickUp => A::Up,
        P::DPadDown | P::StickDown => A::Down,
        P::DPadLeft | P::StickLeft => A::Left,
        P::DPadRight | P::StickRight => A::Right,
        P::South => A::Select,
        P::East => A::Back,
        P::West => A::PlayPause,
        P::North => A::Info,
        P::LeftShoulder => A::SeekBack,
        P::RightShoulder => A::SeekFwd,
        P::LeftTrigger => A::Prev,
        P::RightTrigger => A::Next,
        P::Start => A::Menu,
        P::Back => A::Search,
        P::Guide => A::Home,
    })
}

/// HDMI-CEC TV remote map.
pub fn map_cec(k: CecKey) -> Option<InputAction> {
    use CecKey as C;
    use InputAction as A;
    Some(match k {
        C::Up => A::Up,
        C::Down => A::Down,
        C::Left => A::Left,
        C::Right => A::Right,
        C::Select | C::Enter => A::Select,
        C::Back => A::Back,
        C::RootMenu => A::Home,
        C::SetupMenu | C::ContentsMenu | C::Guide => A::Menu,
        C::Info => A::Info,
        C::Play | C::Pause | C::PlayPause => A::PlayPause,
        C::Stop => A::Stop,
        C::Rewind => A::SeekBack,
        C::FastForward => A::SeekFwd,
        C::SkipForward | C::ChannelUp => A::Next,
        C::SkipBackward | C::ChannelDown => A::Prev,
        C::VolumeUp => A::VolUp,
        C::VolumeDown => A::VolDown,
        C::Mute => A::Mute,
        C::Blue => A::Search,
        C::Number(_) | C::Red | C::Green | C::Yellow | C::Other(_) => return None,
    })
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)] // TextInput/Backspace arrive with the on-screen keyboard (M6).
pub enum InputAction {
    Up,
    Down,
    Left,
    Right,
    Select,
    Back,
    Home,
    Menu,
    PlayPause,
    Stop,
    Next,
    Prev,
    SeekFwd,
    SeekBack,
    Info,
    VolUp,
    VolDown,
    Mute,
    TextInput(char),
    Backspace,
    Search,
}

impl InputAction {
    /// Actions that auto-repeat while held.
    pub fn repeats(self) -> bool {
        use InputAction::*;
        matches!(self, Up | Down | Left | Right | SeekFwd | SeekBack | VolUp | VolDown | Backspace)
    }
}

/// Conventional (Kodi-like) keyboard map. Text entry (`TextInput`) is wired
/// to SDL text events with the on-screen keyboard in M4.
pub fn map_key(key: Key, _mods: Modifiers) -> Option<InputAction> {
    use InputAction as A;
    Some(match key {
        Key::Up => A::Up,
        Key::Down => A::Down,
        Key::Left => A::Left,
        Key::Right => A::Right,
        Key::Enter => A::Select,
        Key::Escape | Key::Backspace | Key::Back => A::Back,
        Key::Home => A::Home,
        Key::Menu | Key::Char('c') => A::Menu,
        Key::Space | Key::MediaPlayPause | Key::MediaPlay | Key::MediaPause | Key::Char('p') => {
            A::PlayPause
        }
        Key::MediaStop | Key::Char('x') => A::Stop,
        Key::MediaNext | Key::Char('.') => A::Next,
        Key::MediaPrev | Key::Char(',') => A::Prev,
        Key::MediaFastForward | Key::Char('f') => A::SeekFwd,
        Key::MediaRewind | Key::Char('r') => A::SeekBack,
        Key::Char('i') => A::Info,
        Key::VolumeUp | Key::Char('+') | Key::Char('=') => A::VolUp,
        Key::VolumeDown | Key::Char('-') => A::VolDown,
        Key::Mute | Key::Char('m') => A::Mute,
        Key::Search | Key::Char('/') => A::Search,
        _ => return None,
    })
}

#[derive(Copy, Clone, Debug)]
pub struct RepeatConfig {
    pub initial_delay: Duration,
    pub interval: Duration,
    /// After holding this long, repeats speed up to `fast_interval`.
    pub accelerate_after: Duration,
    pub fast_interval: Duration,
}

impl Default for RepeatConfig {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_millis(350),
            interval: Duration::from_millis(80),
            accelerate_after: Duration::from_millis(1500),
            fast_interval: Duration::from_millis(40),
        }
    }
}

/// In-app key repeat so every source behaves identically. OS repeat events
/// are ignored by the caller.
#[derive(Default)]
pub struct KeyRepeat {
    pub cfg: RepeatConfig,
    held: Option<Held>,
}

#[derive(Copy, Clone)]
struct Held {
    key: Button,
    action: InputAction,
    since: Instant,
    next: Instant,
}

impl KeyRepeat {
    pub fn new(cfg: RepeatConfig) -> Self {
        Self { cfg, held: None }
    }

    pub fn press(&mut self, key: Button, action: InputAction, now: Instant) {
        self.held = action.repeats().then_some(Held {
            key,
            action,
            since: now,
            next: now + self.cfg.initial_delay,
        });
    }

    pub fn release(&mut self, key: Button) {
        if self.held.is_some_and(|h| h.key == key) {
            self.held = None;
        }
    }

    /// Lost focus / VT switch: forget held keys so nothing repeats forever.
    pub fn reset(&mut self) {
        self.held = None;
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.held.map(|h| h.next)
    }

    /// Returns at most one repeat per call. A late tick does not burst; the
    /// schedule restarts from `now`.
    pub fn tick(&mut self, now: Instant) -> Option<InputAction> {
        let h = self.held.as_mut()?;
        if now < h.next {
            return None;
        }
        let interval = if now.duration_since(h.since) >= self.cfg.accelerate_after {
            self.cfg.fast_interval
        } else {
            self.cfg.interval
        };
        h.next = now + interval;
        Some(h.action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn keymap_basics() {
        let m = Modifiers::default();
        assert_eq!(map_key(Key::Up, m), Some(InputAction::Up));
        assert_eq!(map_key(Key::Enter, m), Some(InputAction::Select));
        assert_eq!(map_key(Key::Escape, m), Some(InputAction::Back));
        assert_eq!(map_key(Key::F(3), m), None);
    }

    #[test]
    fn every_source_navigates_the_same() {
        let dirs = [InputAction::Up, InputAction::Down, InputAction::Left, InputAction::Right];
        let keys = [Key::Up, Key::Down, Key::Left, Key::Right];
        let pads =
            [PadButton::DPadUp, PadButton::DPadDown, PadButton::DPadLeft, PadButton::DPadRight];
        let sticks =
            [PadButton::StickUp, PadButton::StickDown, PadButton::StickLeft, PadButton::StickRight];
        let cec = [CecKey::Up, CecKey::Down, CecKey::Left, CecKey::Right];
        for i in 0..4 {
            assert_eq!(map_key(keys[i], Modifiers::default()), Some(dirs[i]));
            assert_eq!(map_pad(pads[i]), Some(dirs[i]));
            assert_eq!(map_pad(sticks[i]), Some(dirs[i]));
            assert_eq!(map_cec(cec[i]), Some(dirs[i]));
        }
        for (k, p, c) in [
            (Key::Enter, PadButton::South, CecKey::Select),
            (Key::Escape, PadButton::East, CecKey::Back),
        ] {
            assert_eq!(map_key(k, Modifiers::default()), map_pad(p));
            assert_eq!(map_pad(p), map_cec(c));
        }
    }

    #[test]
    fn events_normalize_and_os_repeat_is_dropped() {
        let down = PlatformEvent::PadDown { button: PadButton::South };
        assert_eq!(
            from_event(&down),
            Some(Input::Press {
                button: Button::Pad(PadButton::South),
                action: Some(InputAction::Select)
            })
        );
        let os_repeat =
            PlatformEvent::KeyDown { key: Key::Up, mods: Modifiers::default(), repeat: true };
        assert_eq!(from_event(&os_repeat), None);
        assert_eq!(
            from_event(&PlatformEvent::CecUp { key: CecKey::Up }),
            Some(Input::Release(Button::Cec(CecKey::Up)))
        );
        assert_eq!(from_event(&PlatformEvent::Exposed), None);
    }

    #[test]
    fn repeat_works_for_any_source() {
        let t0 = Instant::now();
        let mut r = KeyRepeat::default();
        r.press(Button::Cec(CecKey::Down), InputAction::Down, t0);
        assert_eq!(r.tick(t0 + ms(350)), Some(InputAction::Down));
        r.release(Button::Key(Key::Down));
        assert!(r.next_deadline().is_some(), "a different source's release does not stop it");
        r.release(Button::Cec(CecKey::Down));
        assert_eq!(r.next_deadline(), None);
    }

    #[test]
    fn repeat_timing() {
        let t0 = Instant::now();
        let mut r = KeyRepeat::default();
        r.press(Button::Key(Key::Down), InputAction::Down, t0);
        assert_eq!(r.tick(t0 + ms(100)), None);
        assert_eq!(r.next_deadline(), Some(t0 + ms(350)));
        assert_eq!(r.tick(t0 + ms(350)), Some(InputAction::Down));
        assert_eq!(r.tick(t0 + ms(400)), None);
        assert_eq!(r.tick(t0 + ms(430)), Some(InputAction::Down));
        // Accelerated after 1.5 s.
        assert_eq!(r.tick(t0 + ms(1600)), Some(InputAction::Down));
        assert_eq!(r.next_deadline(), Some(t0 + ms(1640)));
        r.release(Button::Key(Key::Down));
        assert_eq!(r.next_deadline(), None);
    }

    #[test]
    fn non_repeating_and_other_key_release() {
        let t0 = Instant::now();
        let mut r = KeyRepeat::default();
        r.press(Button::Key(Key::Enter), InputAction::Select, t0);
        assert_eq!(r.next_deadline(), None);
        r.press(Button::Key(Key::Up), InputAction::Up, t0);
        r.release(Button::Key(Key::Down));
        assert!(r.next_deadline().is_some());
    }
}
