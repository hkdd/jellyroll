//! `--soak HOURS` (spec 15, M10): drive the UI for hours against the
//! built-in mock server and watch memory. Every cycle browses like a user
//! (home rows, a library grid scrolled deep, an item page, search) and,
//! with `--soak-media FILE`, plays a minute of video. Every minute it
//! samples the process RSS, the GPU texture memory and Skia's cache into
//! `soak.csv` (in the working directory) and the log. At the end it
//! compares RSS after warm-up with RSS at the end: growth beyond
//! `RSS_GROWTH_LIMIT` fails the run (exit 1).
//!
//! Every cycle starts with Home (back to the home screen from anywhere), so
//! a step that went somewhere unexpected can't derail the rest.

use std::io::Write;
use std::time::{Duration, Instant};

use crate::input::InputAction;

const SAMPLE_EVERY: Duration = Duration::from_secs(60);
/// RSS before this is warm-up (caches filling): not compared.
const WARMUP: Duration = Duration::from_secs(15 * 60);
/// Each end of the comparison averages samples over this long.
const WINDOW: Duration = Duration::from_secs(10 * 60);
/// Allowed RSS growth between the windows: the larger of these.
const RSS_GROWTH_LIMIT: f64 = 0.10;
const RSS_GROWTH_MIN_MB: f64 = 32.0;
/// How long a played video runs per cycle.
const PLAY_FOR: Duration = Duration::from_secs(60);

/// One step: an action, then how long to wait before the next.
type Step = (Option<InputAction>, Duration);

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// One cycle of the script.
fn cycle(play: bool) -> Vec<Step> {
    use InputAction as A;
    // Home twice: back to the home screen, then to its top (the nav bar's
    // Home tab), wherever the last step ended.
    let home = [(Some(A::Home), ms(1000)), (Some(A::Home), ms(1000))];
    let mut s: Vec<Step> = home.to_vec();
    // Along the home rows.
    for _ in 0..2 {
        s.push((Some(A::Down), ms(400)));
    }
    for _ in 0..12 {
        s.push((Some(A::Right), ms(150)));
    }
    s.extend(home);
    // The first library (Movies) from the nav bar, scrolled well down.
    s.extend([(Some(A::Right), ms(400)), (Some(A::Select), ms(3000))]);
    for _ in 0..60 {
        s.push((Some(A::Down), ms(120)));
    }
    s.push((None, ms(1500)));
    // An item page; Play is focused there.
    s.push((Some(A::Select), ms(3000)));
    if play {
        s.push((Some(A::Select), PLAY_FOR));
        // Stop (or, if it already ended, back to the grid).
        s.push((Some(A::Back), ms(3000)));
    }
    s.extend(home);
    // Search and type a little.
    s.push((Some(A::Search), ms(1000)));
    for c in ['t', 'h', 'e'] {
        s.push((Some(A::TextInput(c)), ms(300)));
    }
    s.push((None, ms(2500)));
    s
}

#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub at: Duration,
    pub rss_mb: f64,
    pub texture_mb: f64,
    pub gpu_cache_mb: f64,
}

pub struct Soak {
    start: Instant,
    end: Instant,
    steps: Vec<Step>,
    step: usize,
    next_step: Instant,
    next_sample: Instant,
    pub cycles: u64,
    pub samples: Vec<Sample>,
    csv: Option<std::fs::File>,
}

impl Soak {
    /// `csv`: where the samples go (None: only the log).
    pub fn new(
        duration: Duration,
        play: bool,
        csv: Option<&std::path::Path>,
        now: Instant,
    ) -> Self {
        let csv = csv
            .map(std::fs::File::create)
            .transpose()
            .and_then(|f| {
                let Some(mut f) = f else { return Ok(None) };
                writeln!(f, "minutes,rss_mb,texture_mb,gpu_cache_mb,cycles")?;
                Ok(Some(f))
            })
            .map_err(|e| tracing::warn!("soak.csv: {e}"))
            .ok()
            .flatten();
        Self {
            start: now,
            end: now + duration,
            steps: cycle(play),
            step: 0,
            // Give the home screen time to load first.
            next_step: now + Duration::from_secs(8),
            next_sample: now + SAMPLE_EVERY,
            cycles: 0,
            samples: Vec::new(),
            csv,
        }
    }

    pub fn finished(&self, now: Instant) -> bool {
        now >= self.end
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        Some(self.next_step.min(self.next_sample).min(self.end))
    }

    /// The scripted action due now, if any.
    pub fn tick(&mut self, now: Instant) -> Option<InputAction> {
        if now < self.next_step {
            return None;
        }
        let (a, wait) = self.steps[self.step];
        self.step += 1;
        if self.step == self.steps.len() {
            self.step = 0;
            self.cycles += 1;
        }
        self.next_step = now + wait;
        a
    }

    /// Whether a sample is due (the caller gathers the numbers).
    pub fn sample_due(&self, now: Instant) -> bool {
        now >= self.next_sample
    }

    pub fn record(&mut self, now: Instant, texture_bytes: u64, gpu_cache_bytes: u64) {
        self.next_sample = now + SAMPLE_EVERY;
        let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
        let s = Sample {
            at: now - self.start,
            rss_mb: crate::perf::rss_bytes().map_or(0.0, mb),
            texture_mb: mb(texture_bytes),
            gpu_cache_mb: mb(gpu_cache_bytes),
        };
        tracing::info!(
            minutes = s.at.as_secs() / 60,
            rss_mb = format!("{:.1}", s.rss_mb),
            texture_mb = format!("{:.1}", s.texture_mb),
            gpu_cache_mb = format!("{:.1}", s.gpu_cache_mb),
            cycles = self.cycles,
            "soak sample"
        );
        if let Some(f) = &mut self.csv {
            let _ = writeln!(
                f,
                "{:.1},{:.1},{:.1},{:.1},{}",
                s.at.as_secs_f64() / 60.0,
                s.rss_mb,
                s.texture_mb,
                s.gpu_cache_mb,
                self.cycles
            );
        }
        self.samples.push(s);
    }

    /// Prints the summary; true if RSS stayed flat.
    pub fn report(&self) -> bool {
        let total = self.samples.last().map_or(Duration::ZERO, |s| s.at);
        println!(
            "soak: {:.1} h, {} cycles, {} samples",
            total.as_secs_f64() / 3600.0,
            self.cycles,
            self.samples.len()
        );
        if let (Some(first), Some(last)) = (self.samples.first(), self.samples.last()) {
            println!(
                "rss_mb: first {:.1} last {:.1} max {:.1}",
                first.rss_mb,
                last.rss_mb,
                self.samples.iter().map(|s| s.rss_mb).fold(0.0, f64::max)
            );
            println!(
                "texture_mb: last {:.1}  gpu_cache_mb: last {:.1}",
                last.texture_mb, last.gpu_cache_mb
            );
        }
        match verdict(&self.samples) {
            Verdict::TooShort => {
                println!(
                    "result: n/a (needs at least {} min to compare)",
                    (WARMUP + 2 * WINDOW).as_secs() / 60
                );
                true
            }
            Verdict::Flat { early, late } => {
                println!("rss after warm-up: {early:.1} MB, at the end: {late:.1} MB");
                println!("result: PASS");
                true
            }
            Verdict::Grew { early, late } => {
                println!("rss after warm-up: {early:.1} MB, at the end: {late:.1} MB");
                println!(
                    "FAIL: memory grew by {:.1} MB ({:.0}%)",
                    late - early,
                    (late / early - 1.0) * 100.0
                );
                println!("result: FAIL");
                false
            }
        }
    }
}

#[derive(Debug, PartialEq)]
enum Verdict {
    TooShort,
    Flat { early: f64, late: f64 },
    Grew { early: f64, late: f64 },
}

fn mean_rss(samples: &[Sample], from: Duration, to: Duration) -> Option<f64> {
    let v: Vec<f64> =
        samples.iter().filter(|s| s.at >= from && s.at < to).map(|s| s.rss_mb).collect();
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

/// Mean RSS in the window after warm-up against the last window.
fn verdict(samples: &[Sample]) -> Verdict {
    let Some(end) = samples.last().map(|s| s.at) else { return Verdict::TooShort };
    if end < WARMUP + 2 * WINDOW {
        return Verdict::TooShort;
    }
    let (Some(early), Some(late)) = (
        mean_rss(samples, WARMUP, WARMUP + WINDOW),
        mean_rss(samples, end.saturating_sub(WINDOW), end + Duration::from_secs(1)),
    ) else {
        return Verdict::TooShort;
    };
    let limit = (early * RSS_GROWTH_LIMIT).max(RSS_GROWTH_MIN_MB);
    if late - early > limit { Verdict::Grew { early, late } } else { Verdict::Flat { early, late } }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(minutes: u64, rss: impl Fn(u64) -> f64) -> Vec<Sample> {
        (1..=minutes)
            .map(|m| Sample {
                at: Duration::from_secs(m * 60),
                rss_mb: rss(m),
                texture_mb: 0.0,
                gpu_cache_mb: 0.0,
            })
            .collect()
    }

    #[test]
    fn flat_after_warmup_passes() {
        // Climbs while caches fill, then flat with noise.
        let s =
            samples(240, |m| if m < 15 { 200.0 + m as f64 * 10.0 } else { 350.0 + (m % 3) as f64 });
        assert!(matches!(verdict(&s), Verdict::Flat { .. }));
    }

    #[test]
    fn steady_growth_fails() {
        let s = samples(240, |m| 300.0 + m as f64 * 0.5);
        assert!(matches!(verdict(&s), Verdict::Grew { .. }));
    }

    #[test]
    fn short_runs_are_not_judged() {
        assert_eq!(verdict(&samples(20, |_| 300.0)), Verdict::TooShort);
    }

    #[test]
    fn the_script_loops_and_starts_from_home() {
        let t = Instant::now();
        let mut s = Soak::new(Duration::from_secs(3600), true, None, t);
        assert_eq!(s.tick(t), None, "waits for the home screen");
        let mut now = t + Duration::from_secs(8);
        assert_eq!(s.tick(now), Some(InputAction::Home));
        let n = s.steps.len();
        for _ in 1..n {
            now = s.next_step;
            s.tick(now);
        }
        assert_eq!(s.cycles, 1);
        assert_eq!(s.tick(s.next_step), Some(InputAction::Home), "next cycle");
    }
}
