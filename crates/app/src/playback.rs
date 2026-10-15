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
    /// When mpv was started (the first-frame log says how long it took).
    opened_at: Instant,
    /// Since when playback has been waiting for the network.
    stalled_at: Option<Instant>,
    stalls: u32,
}

/// Bytes per second to Mbit/s.
pub fn mbit(bytes_per_sec: i64) -> f64 {
    bytes_per_sec as f64 * 8.0 / 1_000_000.0
}

fn title_from(url: &str) -> &str {
    let tail = url.rsplit(['/', '\\']).next().unwrap_or(url);
    if tail.is_empty() { url } else { tail }
}

impl Playback {
    pub fn start<P: Platform>(p: &P, url: &str, now: Instant) -> Result<Self, String> {
        Self::start_with(p, url, title_from(url), PlayerOptions::default(), now)
    }

    /// Plays `url` with `opts` (auth header, start position) under `title`.
    pub fn start_with<P: Platform>(
        p: &P,
        url: &str,
        title: &str,
        mut opts: PlayerOptions,
        now: Instant,
    ) -> Result<Self, String> {
        let (major, minor) = jellyroll_video::client_api_version();
        tracing::info!(api = format!("{major}.{minor}"), title, "starting playback");
        match jellyroll_video::va::probe() {
            Ok(caps) => {
                opts.render_node = Some(caps.device.clone().into());
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
        let mut player = Player::new(p.gl_loader(), p.waker(), &opts)?;
        player.load(url)?;
        Ok(Self {
            player,
            osd: Osd::new(title, now),
            events: Vec::with_capacity(16),
            ended: None,
            started: false,
            drops: FrameDrops::default(),
            drops_next: now,
            reported_decoder: false,
            opened_at: now,
            stalled_at: None,
            stalls: 0,
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
                PlayerEvent::FileLoaded | PlayerEvent::VideoReconfig => redraw = true,
                PlayerEvent::PlaybackRestart => {
                    // The first frame has been decoded, so mpv has settled on a
                    // decoder (hwdec-current arrives before this event; at
                    // FileLoaded it is not known yet).
                    if !self.started {
                        let ms = self.opened_at.elapsed().as_millis() as u64;
                        tracing::info!(ms, "first video frame (since mpv started)");
                    }
                    self.started = true;
                    self.report_decoder();
                    redraw = true;
                }
                PlayerEvent::EndFile { reason, error } => {
                    let d = self.player.frame_drops();
                    tracing::info!(
                        dropped_vo = d.vo,
                        dropped_decoder = d.decoder,
                        delayed = d.delayed,
                        stalls = self.stalls,
                        "playback totals"
                    );
                    match &error {
                        Some(e) => tracing::error!(?reason, "playback ended: {e}"),
                        None => tracing::info!(?reason, "playback ended"),
                    }
                    self.ended = Some((reason, error));
                }
                PlayerEvent::Shutdown => self.ended = Some((EndReason::Quit, None)),
            }
        }
        self.note_stall(now);
        redraw |= self.player.frame_ready();
        if self.osd.diagnostics && now >= self.drops_next {
            self.drops_next = now + DROPS_REFRESH;
            self.drops = self.player.frame_drops();
            redraw = true;
        }
        redraw
    }

    /// Logs when playback stops to wait for the network and when it resumes,
    /// so a slow connection shows in the log (and debug reports).
    fn note_stall(&mut self, now: Instant) {
        let s = &self.player.state;
        let waiting = self.started && s.buffering && !s.seeking;
        match (waiting, self.stalled_at) {
            (true, None) => {
                self.stalled_at = Some(now);
                self.stalls += 1;
                tracing::warn!(
                    at = format!("{:.1}", s.time_pos),
                    network_mbit = format!("{:.1}", mbit(s.cache_speed)),
                    "buffering: the stream is arriving slower than it plays"
                );
            }
            (false, Some(t)) => {
                self.stalled_at = None;
                tracing::info!(
                    waited = format!("{:.1}", now.duration_since(t).as_secs_f64()),
                    "buffering done"
                );
            }
            _ => {}
        }
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
