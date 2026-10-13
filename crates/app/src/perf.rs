//! Frame timing statistics. Fixed-size rings, no allocation after creation.

use std::time::Duration;

/// Fixed-capacity ring of f32 samples (milliseconds).
pub struct Ring<const N: usize> {
    buf: [f32; N],
    len: usize,
    head: usize,
}

impl<const N: usize> Default for Ring<N> {
    fn default() -> Self {
        Self { buf: [0.0; N], len: 0, head: 0 }
    }
}

impl<const N: usize> Ring<N> {
    pub fn push(&mut self, v: f32) {
        self.buf[self.head] = v;
        self.head = (self.head + 1) % N;
        self.len = (self.len + 1).min(N);
    }

    pub fn len(&self) -> usize {
        self.len
    }

    /// Samples oldest to newest.
    pub fn iter(&self) -> impl Iterator<Item = f32> + '_ {
        let start = (self.head + N - self.len) % N;
        (0..self.len).map(move |i| self.buf[(start + i) % N])
    }

    /// `(p50, p99, max)`, or zeros when empty. `scratch` avoids allocating.
    pub fn percentiles(&self, scratch: &mut [f32; N]) -> Percentiles {
        if self.len == 0 {
            return Percentiles::default();
        }
        let s = &mut scratch[..self.len];
        for (d, v) in s.iter_mut().zip(self.iter()) {
            *d = v;
        }
        s.sort_unstable_by(f32::total_cmp);
        let at = |q: f64| s[((q * (s.len() - 1) as f64).round() as usize).min(s.len() - 1)];
        Percentiles { p50: at(0.50), p99: at(0.99), max: s[s.len() - 1] }
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Percentiles {
    pub p50: f32,
    pub p99: f32,
    pub max: f32,
}

pub const HISTORY: usize = 600;

pub fn ms(d: Duration) -> f32 {
    d.as_secs_f32() * 1000.0
}

/// Per-frame measurements gathered by the loop.
#[derive(Default)]
pub struct FrameStats {
    /// CPU time from frame start until just before swap (update + draw).
    pub cpu_ms: Ring<HISTORY>,
    /// GPU time from GL timer queries (arrives a few frames late).
    pub gpu_ms: Ring<HISTORY>,
    /// Interval between consecutive presents while rendering continuously.
    pub present_ms: Ring<HISTORY>,
    pub frames: u64,
    pub missed_vsyncs: u64,
    /// Allocations during the most recent frame.
    pub allocs_last_frame: u64,
    /// Frames (after warm-up) that allocated at all.
    pub allocating_frames: u64,
}

impl FrameStats {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Decides whether a present interval missed vblank. Only meaningful when
/// the previous frame was also rendered (continuous animation).
pub fn is_missed_vsync(interval_ms: f32, refresh_hz: f32) -> bool {
    if refresh_hz <= 0.0 {
        return false;
    }
    let period = 1000.0 / refresh_hz;
    interval_ms > period * 1.5
}

/// Resident set size in bytes from /proc/self/statm (Linux).
pub fn rss_bytes() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = s.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * 4096)
}

/// Total user+system CPU time of this process from /proc/self/stat.
pub fn process_cpu_time() -> Option<Duration> {
    let s = std::fs::read_to_string("/proc/self/stat").ok()?;
    // Fields after the ")" of comm; utime and stime are fields 14 and 15.
    let rest = &s[s.rfind(')')? + 2..];
    let mut f = rest.split_whitespace();
    let utime: u64 = f.nth(11)?.parse().ok()?;
    let stime: u64 = f.next()?.parse().ok()?;
    // USER_HZ is 100 on every mainstream Linux configuration.
    Some(Duration::from_millis((utime + stime) * 10))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_wraps_and_orders() {
        let mut r: Ring<3> = Ring::default();
        for v in [1.0, 2.0, 3.0, 4.0] {
            r.push(v);
        }
        assert_eq!(r.len(), 3);
        assert_eq!(r.iter().collect::<Vec<_>>(), vec![2.0, 3.0, 4.0]);
    }

    #[test]
    fn percentiles() {
        let mut r: Ring<100> = Ring::default();
        for i in 1..=100 {
            r.push(i as f32);
        }
        let mut s = [0.0; 100];
        let p = r.percentiles(&mut s);
        assert_eq!(p.max, 100.0);
        assert!((p.p50 - 50.0).abs() <= 1.0);
        assert!((p.p99 - 99.0).abs() <= 1.0);
        let empty: Ring<4> = Ring::default();
        assert_eq!(empty.percentiles(&mut [0.0; 4]), Percentiles::default());
    }

    #[test]
    fn missed_vsync_threshold() {
        assert!(!is_missed_vsync(16.7, 60.0));
        assert!(!is_missed_vsync(20.0, 60.0));
        assert!(is_missed_vsync(33.3, 60.0));
        assert!(!is_missed_vsync(100.0, 0.0));
    }

    #[test]
    fn proc_readers() {
        assert!(rss_bytes().unwrap() > 0);
        assert!(process_cpu_time().is_some());
    }
}
