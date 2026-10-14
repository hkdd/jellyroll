//! Refresh-rate matching (spec 5.2): the output mode for a video's frame
//! rate, at the current resolution.
//!
//! A mode fits when its refresh is a whole multiple of the frame rate: 23.976
//! fits 23.976 Hz exactly, 24 Hz nearly (a repeated frame every ~42 s, far
//! less judder than 3:2 at 60 Hz), and 47.952 / 119.88 Hz too. Among fitting
//! modes, the preferred multiple wins: film (23-24.5 fps) at its own rate,
//! anything else at the smallest multiple of at least 47 Hz (25 -> 50,
//! 29.97 -> 59.94) so the UI over the video stays fluid.

use jellyroll_platform::ModeInfo;

/// Within this relative error a mode is an exact match.
const EXACT: f64 = 0.0005;
/// Within this relative error it is close (23.976 fps at 24 Hz is 0.1%).
const CLOSE: f64 = 0.0015;

pub fn hz(m: &ModeInfo) -> f64 {
    if m.refresh_num > 0 && m.refresh_den > 0 {
        f64::from(m.refresh_num) / f64::from(m.refresh_den)
    } else {
        f64::from(m.refresh_hz)
    }
}

fn preferred_multiple(fps: f64) -> u32 {
    if (23.0..=24.5).contains(&fps) {
        return 1;
    }
    (1..=8).find(|k| f64::from(*k) * fps >= 47.0).unwrap_or(1)
}

/// The best mode for `fps` among `modes` at `base`'s resolution, or None
/// when nothing fits (play at the current mode). `base` is the mode the UI
/// runs at; ties go to the refresh closest to it.
pub fn pick<'a>(modes: &'a [ModeInfo], base: &ModeInfo, fps: f64) -> Option<&'a ModeInfo> {
    if !(10.0..=121.0).contains(&fps) {
        return None;
    }
    let pref = preferred_multiple(fps);
    let base_hz = hz(base);
    modes
        .iter()
        .filter(|m| m.width == base.width && m.height == base.height)
        .filter_map(|m| {
            let r = hz(m);
            let k = (r / fps).round().max(1.0);
            let err = (r / k - fps).abs() / fps;
            let tier = if err < EXACT {
                0
            } else if err < CLOSE {
                1
            } else {
                return None;
            };
            let k = k as u32;
            let penalty = match k.cmp(&pref) {
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
                std::cmp::Ordering::Less => 2,
            };
            Some(((tier, penalty, (r - base_hz).abs()), m))
        })
        .min_by(|a, b| (a.0.0, a.0.1).cmp(&(b.0.0, b.0.1)).then(a.0.2.total_cmp(&b.0.2)))
        .map(|(_, m)| m)
}

/// Same output mode (resolution and refresh).
pub fn same(a: &ModeInfo, b: &ModeInfo) -> bool {
    a.width == b.width && a.height == b.height && (hz(a) - hz(b)).abs() < 0.001
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(w: i32, h: i32, num: i32, den: i32) -> ModeInfo {
        ModeInfo {
            width: w,
            height: h,
            refresh_hz: num as f32 / den as f32,
            refresh_num: num,
            refresh_den: den,
        }
    }

    /// A typical 4K TV mode list (plus 1080p modes that must be ignored).
    fn tv() -> Vec<ModeInfo> {
        vec![
            mode(3840, 2160, 60, 1),
            mode(3840, 2160, 60000, 1001),
            mode(3840, 2160, 50, 1),
            mode(3840, 2160, 30, 1),
            mode(3840, 2160, 30000, 1001),
            mode(3840, 2160, 25, 1),
            mode(3840, 2160, 24, 1),
            mode(3840, 2160, 24000, 1001),
            mode(1920, 1080, 120, 1),
            mode(1920, 1080, 24, 1),
        ]
    }

    fn picked(modes: &[ModeInfo], fps: f64) -> Option<f64> {
        let base = mode(3840, 2160, 60, 1);
        pick(modes, &base, fps).map(|m| (hz(m) * 1000.0).round() / 1000.0)
    }

    #[test]
    fn spec_examples() {
        let m = tv();
        assert_eq!(picked(&m, 23.976), Some(23.976));
        assert_eq!(picked(&m, 24.0), Some(24.0));
        assert_eq!(picked(&m, 25.0), Some(50.0), "25 -> 50, not 25 Hz");
        assert_eq!(picked(&m, 29.97), Some(59.94));
        assert_eq!(picked(&m, 30.0), Some(60.0));
        assert_eq!(picked(&m, 50.0), Some(50.0));
        assert_eq!(picked(&m, 59.94), Some(59.94));
        assert_eq!(picked(&m, 60.0), Some(60.0));
    }

    #[test]
    fn close_fallbacks_and_misses() {
        // No 23.976 Hz mode: 24 Hz is close enough.
        let m: Vec<ModeInfo> = tv().into_iter().filter(|m| m.refresh_den == 1).collect();
        assert_eq!(picked(&m, 23.976), Some(24.0));
        // Only 60 Hz: 24 fps does not fit (3:2), stay put.
        let m = vec![mode(3840, 2160, 60, 1)];
        assert_eq!(picked(&m, 23.976), None);
        assert_eq!(picked(&m, 30.0), Some(60.0));
        // A 120 Hz mode carries 24 fps when nothing better exists.
        let m = vec![mode(3840, 2160, 60, 1), mode(3840, 2160, 120, 1)];
        assert_eq!(picked(&m, 24.0), Some(120.0));
        // Unknown or silly rates.
        assert_eq!(picked(&tv(), 0.0), None);
        assert_eq!(picked(&tv(), 1000.0), None);
    }

    #[test]
    fn rounded_rates_from_the_server() {
        // Servers report 23.976023 or 23.98; both are the NTSC film rate.
        assert_eq!(picked(&tv(), 23.976_023), Some(23.976));
        assert_eq!(picked(&tv(), 23.98), Some(23.976));
        assert_eq!(picked(&tv(), 29.97003), Some(59.94));
    }
}
