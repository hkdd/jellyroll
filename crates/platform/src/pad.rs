//! Gamepad helpers: analog stick and trigger to digital presses.
//!
//! The left stick acts as a second D-pad and the triggers as buttons, so
//! every input source produces the same press/release stream and the app's
//! in-app repeat (spec 5.4) treats them identically. Hysteresis (press at
//! 60%, release below 35%) keeps a stick resting near the threshold from
//! chattering.

use crate::PadButton;

const PRESS: f32 = 0.60;
const RELEASE: f32 = 0.35;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Analog {
    LeftX,
    LeftY,
    LeftTrigger,
    RightTrigger,
}

/// Per-gamepad state of the virtual buttons driven by analog inputs.
#[derive(Default, Debug, Clone)]
pub struct AnalogState {
    left: bool,
    right: bool,
    up: bool,
    down: bool,
    lt: bool,
    rt: bool,
}

/// Up to two transitions per update (e.g. stick flips from left to right).
pub type Transitions = [Option<(PadButton, bool)>; 2];

fn hysteresis(pressed: &mut bool, v: f32) -> Option<bool> {
    if !*pressed && v >= PRESS {
        *pressed = true;
        Some(true)
    } else if *pressed && v < RELEASE {
        *pressed = false;
        Some(false)
    } else {
        None
    }
}

impl AnalogState {
    /// `value` is normalized: sticks -1.0..=1.0 (Y positive = down, as SDL
    /// reports), triggers 0.0..=1.0.
    pub fn update(&mut self, axis: Analog, value: f32) -> Transitions {
        let mut out: Transitions = [None, None];
        match axis {
            Analog::LeftX => {
                if let Some(p) = hysteresis(&mut self.left, -value) {
                    out[0] = Some((PadButton::StickLeft, p));
                }
                if let Some(p) = hysteresis(&mut self.right, value) {
                    out[1] = Some((PadButton::StickRight, p));
                }
            }
            Analog::LeftY => {
                if let Some(p) = hysteresis(&mut self.up, -value) {
                    out[0] = Some((PadButton::StickUp, p));
                }
                if let Some(p) = hysteresis(&mut self.down, value) {
                    out[1] = Some((PadButton::StickDown, p));
                }
            }
            Analog::LeftTrigger => {
                if let Some(p) = hysteresis(&mut self.lt, value) {
                    out[0] = Some((PadButton::LeftTrigger, p));
                }
            }
            Analog::RightTrigger => {
                if let Some(p) = hysteresis(&mut self.rt, value) {
                    out[0] = Some((PadButton::RightTrigger, p));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn presses(t: Transitions) -> Vec<(PadButton, bool)> {
        t.into_iter().flatten().collect()
    }

    #[test]
    fn stick_presses_and_releases_with_hysteresis() {
        let mut s = AnalogState::default();
        assert!(presses(s.update(Analog::LeftX, 0.5)).is_empty(), "below press threshold");
        assert_eq!(presses(s.update(Analog::LeftX, 0.7)), vec![(PadButton::StickRight, true)]);
        assert!(presses(s.update(Analog::LeftX, 0.4)).is_empty(), "still held above release");
        assert_eq!(presses(s.update(Analog::LeftX, 0.2)), vec![(PadButton::StickRight, false)]);
    }

    #[test]
    fn stick_flip_releases_then_presses() {
        let mut s = AnalogState::default();
        s.update(Analog::LeftX, 0.9);
        assert_eq!(
            presses(s.update(Analog::LeftX, -0.9)),
            vec![(PadButton::StickLeft, true), (PadButton::StickRight, false)]
        );
    }

    #[test]
    fn y_axis_up_is_negative() {
        let mut s = AnalogState::default();
        assert_eq!(presses(s.update(Analog::LeftY, -1.0)), vec![(PadButton::StickUp, true)]);
    }

    #[test]
    fn triggers_are_buttons() {
        let mut s = AnalogState::default();
        assert_eq!(
            presses(s.update(Analog::RightTrigger, 0.8)),
            vec![(PadButton::RightTrigger, true)]
        );
        assert_eq!(
            presses(s.update(Analog::RightTrigger, 0.1)),
            vec![(PadButton::RightTrigger, false)]
        );
    }
}
