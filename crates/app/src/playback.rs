//! Playback session for `--play` (M3): owns the mpv player and the OSD,
//! maps input to player commands and tracks how playback ended.

use std::time::{Duration, Instant};

use jellyroll_platform::Platform;
use jellyroll_video::{EndReason, FrameDrops, Player, PlayerEvent, PlayerOptions};

use crate::input::InputAction;
use crate::osd::Osd;

const SEEK_SHORT: f64 = 10.0;
const SEEK_LONG: f64 = 60.0;
/// How often mpv's drop counters are re-read for the diagnostics panel.
const DROPS_REFRESH: Duration = Duration::from_secs(1);

pub enum Outcome {
    Continue { redraw: bool },
    Exit,
}

pub struct Playback {
    pub player: Player,
    pub osd: Osd,
    events: Vec<PlayerEvent>,
    /// Set once playback ended (EOF, stop or error).
    pub ended: Option<(EndReason, Option<String>)>,
    /// First frame shown after load/seek (playback is running).
    pub started: bool,
    pub drops: FrameDrops,
    drops_next: Instant,
    reported_decoder: bool,
}

fn title_from(url: &str) -> &str {
    let tail = url.rsplit(['/', '\\']).next().unwrap_or(url);
    if tail.is_empty() { url } else { tail }
}

impl Playback {
    pub fn start<P: Platform>(p: &P, url: &str, now: Instant) -> Result<Self, String> {
        let mut player = Player::new(p.gl_loader(), p.waker(), &PlayerOptions::default())?;
        let (major, minor) = jellyroll_video::client_api_version();
        tracing::info!(api = format!("{major}.{minor}"), url, "starting playback");
        match jellyroll_video::va::probe() {
            Ok(caps) => {
                let list = |v: Vec<jellyroll_video::va::Codec>| {
                    v.iter().map(|c| c.name()).collect::<Vec<_>>().join(", ")
                };
                tracing::info!(
                    driver = %caps.driver,
                    decode = %list(caps.supported()),
                    no_decode = %list(caps.unsupported()),
                    "VA-API hardware decode"
                );
            }
            Err(e) => tracing::warn!("no VA-API hardware decode: {e}"),
        }
        player.load(url)?;
        Ok(Self {
            player,
            osd: Osd::new(title_from(url), now),
            events: Vec::with_capacity(16),
            ended: None,
            started: false,
            drops: FrameDrops::default(),
            drops_next: now,
            reported_decoder: false,
        })
    }

    /// Video is advancing (render every vsync).
    pub fn playing(&self) -> bool {
        self.started && !self.player.state.paused && self.ended.is_none()
    }

    /// Drains player events and new-frame signals. Returns true if a redraw
    /// is needed.
    pub fn poll(&mut self, now: Instant) -> bool {
        let mut redraw = false;
        self.events.clear();
        self.player.poll_events(&mut self.events);
        for i in 0..self.events.len() {
            match self.events[i].clone() {
                PlayerEvent::FileLoaded | PlayerEvent::VideoReconfig => {
                    self.report_decoder();
                    redraw = true;
                }
                PlayerEvent::PlaybackRestart => {
                    self.started = true;
                    redraw = true;
                }
                PlayerEvent::EndFile { reason, error } => {
                    match &error {
                        Some(e) => tracing::error!(?reason, "playback ended: {e}"),
                        None => tracing::info!(?reason, "playback ended"),
                    }
                    self.ended = Some((reason, error));
                }
                PlayerEvent::Shutdown => self.ended = Some((EndReason::Quit, None)),
            }
        }
        redraw |= self.player.frame_ready();
        if self.osd.diagnostics && now >= self.drops_next {
            self.drops_next = now + DROPS_REFRESH;
            self.drops = self.player.frame_drops();
            redraw = true;
        }
        redraw
    }

    /// Logs the active decoder once it is known. Software decoding is never
    /// silent (spec 7.1): it is logged as a warning and badged on screen.
    fn report_decoder(&mut self) {
        let s = &self.player.state;
        if s.video_codec.is_empty() || self.reported_decoder {
            return;
        }
        self.reported_decoder = true;
        if s.software_decode() {
            tracing::warn!(
                codec = %s.video_codec,
                width = s.width,
                height = s.height,
                "no hardware decoder for this video on this device; decoding in software"
            );
        } else {
            tracing::info!(
                codec = %s.video_codec,
                hwdec = %s.hwdec,
                width = s.width,
                height = s.height,
                fps = s.fps,
                "hardware decoding"
            );
        }
    }

    pub fn on_action(&mut self, a: InputAction, now: Instant) -> Outcome {
        use InputAction as A;
        match a {
            A::Back | A::Stop => {
                self.player.stop();
                return Outcome::Exit;
            }
            A::PlayPause | A::Select => self.player.toggle_pause(),
            A::Left | A::SeekBack => self.player.seek_relative(-SEEK_SHORT),
            A::Right | A::SeekFwd => self.player.seek_relative(SEEK_SHORT),
            A::Down => self.player.seek_relative(-SEEK_LONG),
            A::Up => self.player.seek_relative(SEEK_LONG),
            A::Info => {
                self.osd.diagnostics = !self.osd.diagnostics;
                self.drops_next = now;
            }
            _ => {}
        }
        // Any input shows the OSD (spec 7.3).
        self.osd.show(now);
        Outcome::Continue { redraw: true }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles() {
        assert_eq!(title_from("/media/Movies/Arrival (2016).mkv"), "Arrival (2016).mkv");
        assert_eq!(title_from("http://srv/Videos/abc/stream?static=true"), "stream?static=true");
        assert_eq!(title_from("clip.mp4"), "clip.mp4");
        assert_eq!(title_from("http://x/"), "http://x/");
    }
}
