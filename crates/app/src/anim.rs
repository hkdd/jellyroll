//! Animation primitives. Time-based (not frame-count), short fixed durations.
//! There is one animation clock: the render loop samples `Instant::now()` once
//! per frame and every animation in that frame reads that same timestamp.

use std::time::{Duration, Instant};

pub fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

#[derive(Copy, Clone, Debug)]
pub struct Tween {
    from: f32,
    to: f32,
    start: Instant,
    duration: Duration,
}

impl Tween {
    pub fn settled(v: f32, now: Instant) -> Self {
        Self { from: v, to: v, start: now, duration: Duration::ZERO }
    }

    /// Retargets from the current (possibly mid-flight) value, so rapid input
    /// never jumps.
    pub fn retarget(&mut self, to: f32, now: Instant, duration: Duration) {
        self.from = self.value(now);
        self.to = to;
        self.start = now;
        self.duration = duration;
    }

    pub fn value(&self, now: Instant) -> f32 {
        if self.duration.is_zero() {
            return self.to;
        }
        let t =
            now.saturating_duration_since(self.start).as_secs_f32() / self.duration.as_secs_f32();
        self.from + (self.to - self.from) * ease_out_cubic(t)
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.start) < self.duration
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_endpoints() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        assert!(ease_out_cubic(0.5) > 0.5);
    }

    #[test]
    fn tween_progress_and_retarget() {
        let t0 = Instant::now();
        let d = Duration::from_millis(100);
        let mut tw = Tween::settled(0.0, t0);
        assert!(!tw.is_animating(t0));
        tw.retarget(10.0, t0, d);
        assert!(tw.is_animating(t0 + Duration::from_millis(50)));
        let mid = tw.value(t0 + Duration::from_millis(50));
        assert!(mid > 5.0 && mid < 10.0);
        assert_eq!(tw.value(t0 + d), 10.0);
        assert!(!tw.is_animating(t0 + d));
        // Retarget mid-flight continues from the current value.
        tw.retarget(0.0, t0 + Duration::from_millis(50), d);
        assert_eq!(tw.value(t0 + Duration::from_millis(50)), mid);
    }
}
