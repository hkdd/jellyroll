//! `--bench-scroll SECS` (spec 13, M6 exit criterion): scripted D-pad
//! traversal of a 10,000-item library, measured against the spec 2
//! budgets. It runs against an in-process mock server with a synthetic
//! library, in a throwaway data directory, already signed in, so it needs
//! no network or account and leaves the user's state alone.
//!
//! Script: wait for the home screen, open the Movies library from the nav
//! bar (Up, Right, Select, like a user), wait for the grid, then hold Down
//! (one press every 50 ms, faster than key repeat) for the whole window.
//! Pages, placeholders and artwork load while it scrolls, as they would.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use jellyroll_mock_server::{MockConfig, SERVER_ID, TOKEN, USER_ID};

use crate::input::InputAction;
use crate::net::NetConfig;
use crate::net::store::State;

pub const ITEMS: usize = 10_000;
/// From launch to the grid scrolling; past this the run fails.
pub const SETUP_TIMEOUT: Duration = Duration::from_secs(30);
const STEP: Duration = Duration::from_millis(400);
const GRID_SETTLE: Duration = Duration::from_millis(1500);
const SCROLL_STEP: Duration = Duration::from_millis(50);

/// Starts the mock and prepares a signed-in state for it.
pub fn setup() -> Result<(NetConfig, PathBuf), String> {
    setup_with("bench-scroll", MockConfig { movies: ITEMS, ..Default::default() })
}

/// The mock with `mock`, signed in, in a throwaway `jellyroll-<name>-<pid>`
/// directory (also used by `--soak`).
pub fn setup_with(name: &str, mock: MockConfig) -> Result<(NetConfig, PathBuf), String> {
    let dir = std::env::temp_dir().join(format!("jellyroll-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let url = jellyroll_mock_server::spawn(mock);
    let mut state = State::load(None);
    state.remember_server(SERVER_ID, "Jellyroll Mock", &url);
    state.remember_user(SERVER_ID, USER_ID, "demo", TOKEN);
    let data = dir.join("data");
    state.save(&data.join("state.json")).map_err(|e| format!("bench state: {e}"))?;
    Ok((
        NetConfig {
            server_url: url,
            ca_file: None,
            image_cache_bytes: 512 << 20,
            data_dir: Some(data),
            cache_dir: Some(dir.join("cache")),
        },
        dir,
    ))
}

#[derive(Debug, PartialEq)]
enum Phase {
    WaitHome,
    Nav(usize),
    WaitGrid,
    Scroll,
}

const NAV: [InputAction; 3] = [InputAction::Up, InputAction::Right, InputAction::Select];

pub struct Script {
    phase: Phase,
    next: Instant,
    pub presses: u64,
}

impl Script {
    pub fn new(now: Instant) -> Self {
        Self { phase: Phase::WaitHome, next: now, presses: 0 }
    }

    /// Measurement runs while scrolling.
    pub fn measuring(&self) -> bool {
        self.phase == Phase::Scroll
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        (self.phase != Phase::WaitHome).then_some(self.next)
    }

    /// The next scripted action if one is due. `home`: the home screen is
    /// showing; `page`: a page (the grid) is open.
    pub fn tick(&mut self, now: Instant, home: bool, page: bool) -> Option<InputAction> {
        if now < self.next {
            return None;
        }
        match self.phase {
            Phase::WaitHome if home => {
                self.phase = Phase::Nav(0);
                self.next = now + STEP;
                None
            }
            Phase::WaitHome => None,
            Phase::Nav(i) => {
                self.phase = if i + 1 < NAV.len() { Phase::Nav(i + 1) } else { Phase::WaitGrid };
                self.next = now + STEP;
                Some(NAV[i])
            }
            Phase::WaitGrid if page => {
                self.phase = Phase::Scroll;
                self.next = now + GRID_SETTLE;
                None
            }
            Phase::WaitGrid => None,
            Phase::Scroll => {
                self.next = now + SCROLL_STEP;
                self.presses += 1;
                Some(InputAction::Down)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_navigates_then_scrolls() {
        let t = Instant::now();
        let mut s = Script::new(t);
        assert_eq!(s.tick(t, false, false), None, "waits for home");
        assert_eq!(s.tick(t, true, false), None);
        let mut t = t + STEP;
        let mut got = Vec::new();
        for _ in 0..3 {
            got.extend(s.tick(t, true, false));
            t += STEP;
        }
        assert_eq!(got, NAV);
        assert_eq!(s.tick(t, true, true), None, "grid opened");
        assert!(s.measuring());
        t += GRID_SETTLE;
        assert_eq!(s.tick(t, true, true), Some(InputAction::Down));
        assert_eq!(s.tick(t + Duration::from_millis(10), true, true), None);
        assert_eq!(s.tick(t + SCROLL_STEP, true, true), Some(InputAction::Down));
    }
}
