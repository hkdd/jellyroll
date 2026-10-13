//! On-screen keyboard (spec 10.3: `OnScreenKeyboard`) for the remote and
//! gamepad: a grid of keys placed by the page, which merges them into its
//! focus graph. Physical keyboards type directly instead (text input).

use jellyroll_render::{Color, Point, Rect, Renderer, TextStyle, Weight};

use super::strings as s;
use super::theme::{BG, TEXT};

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Key {
    Char(char),
    Space,
    Delete,
    Clear,
}

pub const ROWS_LETTERS: [&str; 4] = ["abcdefghi", "jklmnopqr", "stuvwxyz'", "1234567890"];

pub struct Osk {
    pub keys: Vec<(Key, Rect)>,
    shifted: bool,
}

impl Osk {
    /// Lays out `rows` of single characters plus a bottom row of Space,
    /// Delete and Clear, starting at (x, y).
    pub fn new(x: f32, y: f32, key_w: f32, key_h: f32, gap: f32, rows: &[&str]) -> Self {
        let mut keys = Vec::new();
        let mut yy = y;
        let mut cols = 0;
        for row in rows {
            for (i, c) in row.chars().enumerate() {
                keys.push((
                    Key::Char(c),
                    Rect::new(x + i as f32 * (key_w + gap), yy, key_w, key_h),
                ));
                cols = cols.max(i + 1);
            }
            yy += key_h + gap;
        }
        let unit = key_w + gap;
        let wide = |n: f32| n * key_w + (n - 1.0) * gap;
        let space_units = (cols as f32 - 4.0).max(2.0);
        keys.push((Key::Space, Rect::new(x, yy, wide(space_units), key_h)));
        keys.push((Key::Delete, Rect::new(x + space_units * unit, yy, wide(2.0), key_h)));
        keys.push((Key::Clear, Rect::new(x + (space_units + 2.0) * unit, yy, wide(2.0), key_h)));
        Self { keys, shifted: false }
    }

    /// Applies a key to `text` (at most `max` characters). Returns true if
    /// the text changed.
    pub fn apply(&self, key: Key, text: &mut String, max: usize) -> bool {
        match key {
            Key::Char(c) if text.chars().count() < max => {
                text.push(if self.shifted { c.to_ascii_uppercase() } else { c });
                true
            }
            Key::Space if text.chars().count() < max => {
                text.push(' ');
                true
            }
            Key::Delete => text.pop().is_some(),
            Key::Clear => {
                let had = !text.is_empty();
                text.clear();
                had
            }
            _ => false,
        }
    }

    pub fn draw<R: Renderer>(&self, r: &mut R, focused: Option<usize>) {
        let key_style = TextStyle::new(26.0, Weight::Regular);
        let word_style = TextStyle::new(20.0, Weight::Bold);
        for (i, (k, rect)) in self.keys.iter().enumerate() {
            let f = focused == Some(i);
            r.fill_rrect(*rect, 8.0, if f { Color::WHITE } else { Color::rgba(255, 255, 255, 22) });
            let mut b = [0u8; 4];
            let (label, style) = match k {
                Key::Char(c) => (&*c.encode_utf8(&mut b), &key_style),
                Key::Space => (s::KEY_SPACE, &word_style),
                Key::Delete => (s::KEY_DELETE, &word_style),
                Key::Clear => (s::KEY_CLEAR, &word_style),
            };
            let m = r.measure_text(label, style);
            let p = Point::new(
                (rect.x + (rect.w - m.width) / 2.0).round(),
                (rect.y + (rect.h - m.height) / 2.0).round(),
            );
            r.text(label, style, p, if f { BG } else { TEXT });
        }
    }
}

/// A text field with a caret; `mask` shows bullets (passwords).
pub fn draw_field<R: Renderer>(
    r: &mut R,
    rect: Rect,
    text: &str,
    placeholder: &str,
    focused: bool,
    mask: bool,
) {
    use super::theme::{ACCENT, TEXT_DIM};
    r.fill_rrect(rect, 10.0, Color::rgba(255, 255, 255, 24));
    r.stroke_rrect(rect, 10.0, 2.0, if focused { ACCENT } else { Color::rgba(255, 255, 255, 50) });
    let style = TextStyle::new(28.0, Weight::Regular).max_width(rect.w - 40.0);
    let ty = rect.y + (rect.h - 37.0) / 2.0;
    let shown;
    let t = if mask {
        shown = "•".repeat(text.chars().count());
        shown.as_str()
    } else {
        text
    };
    let width = if t.is_empty() {
        r.text(placeholder, &style, Point::new(rect.x + 18.0, ty), TEXT_DIM);
        0.0
    } else {
        r.text(t, &style, Point::new(rect.x + 18.0, ty), TEXT).width
    };
    if focused {
        r.fill_rect(Rect::new(rect.x + 20.0 + width, ty + 4.0, 3.0, 30.0), TEXT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_type_and_edit() {
        let o = Osk::new(0.0, 0.0, 60.0, 50.0, 8.0, &ROWS_LETTERS);
        assert_eq!(o.keys.len(), 9 * 4 + 1 + 3);
        let mut t = String::new();
        assert!(o.apply(Key::Char('a'), &mut t, 10));
        assert!(o.apply(Key::Space, &mut t, 10));
        assert!(o.apply(Key::Char('1'), &mut t, 10));
        assert_eq!(t, "a 1");
        assert!(o.apply(Key::Delete, &mut t, 10));
        assert_eq!(t, "a ");
        assert!(o.apply(Key::Clear, &mut t, 10));
        assert!(!o.apply(Key::Clear, &mut t, 10));
        let mut long = "x".repeat(10);
        assert!(!o.apply(Key::Char('y'), &mut long, 10), "length cap");
    }
}
