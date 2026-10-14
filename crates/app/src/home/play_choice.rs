//! Asked before playing something this device can't play directly (D71):
//! the server can't send the file as it is, or this GPU can't decode its
//! video. The choice isn't remembered.

use jellyroll_render::{Color, Point, Rect, Renderer, TextStyle, Weight};

use super::playinfo::Check;
use super::strings as s;
use super::theme::{TEXT, TEXT_DIM};
use super::widgets;
use crate::input::InputAction;
use crate::net::PlayFetched;

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Choice {
    Software,
    Transcode,
    Cancel,
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Stay,
    Picked(Choice),
}

pub struct PlayChoice {
    /// The direct-play answer: played as is for Software; its item,
    /// position and tracks for a transcode request.
    pub fetched: Box<PlayFetched>,
    options: Vec<Choice>,
    focus: usize,
    message: Vec<String>,
}

impl PlayChoice {
    pub fn new(check: &Check, fetched: Box<PlayFetched>) -> Self {
        let (options, message) = match check {
            Check::NoHardwareDecode { codec, uhd } => {
                let mut m = vec![s::fill(s::NO_HW_DECODE_VIDEO, &[codec])];
                if *uhd {
                    m.push(s::SOFTWARE_4K_NOTE.into());
                }
                (vec![Choice::Software, Choice::Transcode, Choice::Cancel], m)
            }
            _ => (vec![Choice::Transcode, Choice::Cancel], vec![s::NO_DIRECT_PLAY.into()]),
        };
        // Start on Server transcode: it plays smoothly on any of these.
        let focus = options.iter().position(|c| *c == Choice::Transcode).unwrap_or(0);
        Self { fetched, options, focus, message }
    }

    fn label(c: Choice) -> &'static str {
        match c {
            Choice::Software => s::PLAY_SOFTWARE,
            Choice::Transcode => s::PLAY_TRANSCODE,
            Choice::Cancel => s::CANCEL,
        }
    }

    pub fn on_action(&mut self, a: InputAction) -> Outcome {
        match a {
            InputAction::Up if self.focus > 0 => self.focus -= 1,
            InputAction::Down if self.focus + 1 < self.options.len() => self.focus += 1,
            InputAction::Select => return Outcome::Picked(self.options[self.focus]),
            InputAction::Back => return Outcome::Picked(Choice::Cancel),
            _ => {}
        }
        Outcome::Stay
    }

    pub fn draw<R: Renderer>(&self, r: &mut R) {
        let (lw, lh) = r.logical_size();
        r.fill_rect(Rect::new(0.0, 0.0, lw, lh), Color::rgba(0, 0, 0, 170));
        let w = 760.0;
        let body = TextStyle::new(24.0, Weight::Regular);
        let lines: Vec<String> =
            self.message.iter().flat_map(|m| super::status::wrap(r, m, &body, w - 80.0)).collect();
        let n = self.options.len() as f32;
        let h = 116.0 + lines.len() as f32 * 34.0 + 24.0 + n * 66.0 + 20.0;
        let panel = Rect::new((lw - w) / 2.0, (lh - h) / 2.0, w, h);
        r.fill_rrect(panel, 18.0, Color::rgba(24, 27, 36, 250));
        let title = TextStyle::new(32.0, Weight::Bold).max_width(w - 80.0);
        r.text(s::CANT_PLAY_DIRECTLY, &title, Point::new(panel.x + 40.0, panel.y + 36.0), TEXT);
        let mut y = panel.y + 100.0;
        for l in &lines {
            r.text(l, &body, Point::new(panel.x + 40.0, y), TEXT_DIM);
            y += 34.0;
        }
        y += 24.0;
        for (i, c) in self.options.iter().enumerate() {
            let rect = Rect::new(panel.x + 40.0, y, w - 80.0, 52.0);
            widgets::draw_button(r, rect, Self::label(*c), i == self.focus, false);
            y += 66.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fetched() -> Box<PlayFetched> {
        Box::new(PlayFetched {
            info: crate::home::player::tests::info(),
            base_url: "http://srv".into(),
            authorization: String::new(),
        })
    }

    #[test]
    fn hardware_choice_offers_software_and_starts_on_transcode() {
        let check = Check::NoHardwareDecode { codec: "HEVC 10-bit".into(), uhd: true };
        let mut c = PlayChoice::new(&check, fetched());
        assert_eq!(c.options, [Choice::Software, Choice::Transcode, Choice::Cancel]);
        assert_eq!(c.message.len(), 2, "with the 4K note");
        assert_eq!(c.on_action(InputAction::Select), Outcome::Picked(Choice::Transcode));
        c.on_action(InputAction::Up);
        assert_eq!(c.on_action(InputAction::Select), Outcome::Picked(Choice::Software));
        assert_eq!(c.on_action(InputAction::Back), Outcome::Picked(Choice::Cancel));
    }

    #[test]
    fn no_direct_play_offers_transcode_or_cancel() {
        let mut c = PlayChoice::new(&Check::NoDirectPlay, fetched());
        assert_eq!(c.options, [Choice::Transcode, Choice::Cancel]);
        c.on_action(InputAction::Down);
        assert_eq!(c.on_action(InputAction::Select), Outcome::Picked(Choice::Cancel));
    }
}
