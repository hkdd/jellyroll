//! Input normalization (spec 5.4). M1 covers the keyboard; gamepad and CEC
//! feed the same `InputAction` stream in M4.

use std::time::{Duration, Instant};

use jellyroll_platform::{Key, Modifiers};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)] // TextInput/Backspace arrive with the on-screen keyboard (M4).
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
    key: Key,
    action: InputAction,
    since: Instant,
    next: Instant,
}

impl KeyRepeat {
    pub fn new(cfg: RepeatConfig) -> Self {
        Self { cfg, held: None }
    }

    pub fn press(&mut self, key: Key, action: InputAction, now: Instant) {
        self.held = action.repeats().then_some(Held {
            key,
            action,
            since: now,
            next: now + self.cfg.initial_delay,
        });
    }

    pub fn release(&mut self, key: Key) {
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
    fn repeat_timing() {
        let t0 = Instant::now();
        let mut r = KeyRepeat::default();
        r.press(Key::Down, InputAction::Down, t0);
        assert_eq!(r.tick(t0 + ms(100)), None);
        assert_eq!(r.next_deadline(), Some(t0 + ms(350)));
        assert_eq!(r.tick(t0 + ms(350)), Some(InputAction::Down));
        assert_eq!(r.tick(t0 + ms(400)), None);
        assert_eq!(r.tick(t0 + ms(430)), Some(InputAction::Down));
        // Accelerated after 1.5 s.
        assert_eq!(r.tick(t0 + ms(1600)), Some(InputAction::Down));
        assert_eq!(r.next_deadline(), Some(t0 + ms(1640)));
        r.release(Key::Down);
        assert_eq!(r.next_deadline(), None);
    }

    #[test]
    fn non_repeating_and_other_key_release() {
        let t0 = Instant::now();
        let mut r = KeyRepeat::default();
        r.press(Key::Enter, InputAction::Select, t0);
        assert_eq!(r.next_deadline(), None);
        r.press(Key::Up, InputAction::Up, t0);
        r.release(Key::Down);
        assert!(r.next_deadline().is_some());
    }
}
