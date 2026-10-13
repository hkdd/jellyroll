//! sd_notify integration (spec 5.1): READY=1 after the first frame, and
//! WATCHDOG=1 driven from the render loop itself so a hung loop trips the
//! watchdog. All calls are no-ops when not started by systemd.

use std::time::{Duration, Instant};

use sd_notify::NotifyState;

pub struct Systemd {
    /// Ping interval (half of WatchdogSec), if the watchdog is enabled.
    ping_every: Option<Duration>,
    last_ping: Instant,
    ready: bool,
}

impl Systemd {
    pub fn new(now: Instant) -> Self {
        let ping_every = sd_notify::watchdog_enabled().map(|d| d / 2);
        if let Some(p) = ping_every {
            tracing::info!(ping_ms = p.as_millis() as u64, "systemd watchdog enabled");
        }
        Self { ping_every, last_ping: now, ready: false }
    }

    pub fn ready(&mut self, first_frame: Duration) {
        if self.ready {
            return;
        }
        self.ready = true;
        let status = format!("first frame after {} ms", first_frame.as_millis());
        if let Err(e) = sd_notify::notify(&[NotifyState::Ready, NotifyState::Status(&status)]) {
            tracing::warn!("sd_notify READY failed: {e}");
        }
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.ping_every.map(|p| self.last_ping + p)
    }

    pub fn tick(&mut self, now: Instant) {
        if self.next_deadline().is_some_and(|t| now >= t) {
            self.last_ping = now;
            if let Err(e) = sd_notify::notify(&[NotifyState::Watchdog]) {
                tracing::warn!("sd_notify WATCHDOG failed: {e}");
            }
        }
    }

    pub fn stopping(&self) {
        let _ = sd_notify::notify(&[NotifyState::Stopping]);
    }
}
