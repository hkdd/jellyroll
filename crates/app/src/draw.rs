//! M1 stopgap drawing: solid rectangles flushed as scissored clears, plus a
//! 3x5 bitmap font for the perf HUD. No shaders, no textures.
//!
//! This is replaced by the `Renderer` trait + Skia backend in M2; it exists
//! so the render loop, pacing and perf HUD can be validated on hardware first.

use jellyroll_platform::Gl;

pub type Color = [f32; 4];

pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Reused every frame; capacity is retained so steady-state frames do not
/// allocate.
pub struct RectBatch {
    items: Vec<(Rect, Color)>,
}

impl RectBatch {
    pub fn with_capacity(n: usize) -> Self {
        Self { items: Vec::with_capacity(n) }
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: Color) {
        if w > 0 && h > 0 {
            self.items.push((Rect { x, y, w, h }, c));
        }
    }

    /// Hollow rectangle of thickness `t`.
    pub fn frame(&mut self, x: i32, y: i32, w: i32, h: i32, t: i32, c: Color) {
        self.rect(x, y, w, t, c);
        self.rect(x, y + h - t, w, t, c);
        self.rect(x, y + t, t, h - 2 * t, c);
        self.rect(x + w - t, y + t, t, h - 2 * t, c);
    }

    /// Draws `text` with the bitmap font, `px` pixels per font cell. Lit
    /// cells are merged into horizontal runs per row to keep the rect count
    /// low. Returns the width drawn.
    pub fn text(&mut self, x: i32, y: i32, px: i32, c: Color, text: &str) -> i32 {
        let n = text.len() as i32;
        for row in 0..5 {
            let mut run_start: Option<i32> = None;
            let mut col = 0;
            for ch in text.bytes() {
                let bits = glyph(ch)[row];
                for gx in 0..GLYPH_ADVANCE {
                    let on = gx < 3 && bits & (0b100 >> gx) != 0;
                    match (on, run_start) {
                        (true, None) => run_start = Some(col),
                        (false, Some(s)) => {
                            self.rect(x + s * px, y + row as i32 * px, (col - s) * px, px, c);
                            run_start = None;
                        }
                        _ => {}
                    }
                    col += 1;
                }
            }
            if let Some(s) = run_start {
                self.rect(x + s * px, y + row as i32 * px, (col - s) * px, px, c);
            }
        }
        n * GLYPH_ADVANCE * px
    }

    pub fn flush(&self, gl: &Gl, fb_height: i32) {
        gl.enable_scissor();
        for (r, c) in &self.items {
            gl.fill_rect_scissor(fb_height, r.x, r.y, r.w, r.h, *c);
        }
        gl.disable_scissor();
    }

    #[cfg(test)]
    pub fn items(&self) -> &[(Rect, Color)] {
        &self.items
    }
}

pub const GLYPH_ADVANCE: i32 = 4;
pub const LINE_HEIGHT: i32 = 7;

/// 3x5 glyph rows, bit 2 = leftmost column. Lowercase maps to uppercase.
fn glyph(ch: u8) -> [u8; 5] {
    match ch.to_ascii_uppercase() {
        b' ' => [0, 0, 0, 0, 0],
        b'0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        b'1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        b'2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        b'3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        b'4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        b'5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        b'6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        b'7' => [0b111, 0b001, 0b010, 0b010, 0b010],
        b'8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        b'9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        b'A' => [0b010, 0b101, 0b111, 0b101, 0b101],
        b'B' => [0b110, 0b101, 0b110, 0b101, 0b110],
        b'C' => [0b011, 0b100, 0b100, 0b100, 0b011],
        b'D' => [0b110, 0b101, 0b101, 0b101, 0b110],
        b'E' => [0b111, 0b100, 0b110, 0b100, 0b111],
        b'F' => [0b111, 0b100, 0b110, 0b100, 0b100],
        b'G' => [0b011, 0b100, 0b101, 0b101, 0b011],
        b'H' => [0b101, 0b101, 0b111, 0b101, 0b101],
        b'I' => [0b111, 0b010, 0b010, 0b010, 0b111],
        b'J' => [0b001, 0b001, 0b001, 0b101, 0b010],
        b'K' => [0b101, 0b101, 0b110, 0b101, 0b101],
        b'L' => [0b100, 0b100, 0b100, 0b100, 0b111],
        b'M' => [0b101, 0b111, 0b111, 0b101, 0b101],
        b'N' => [0b110, 0b101, 0b101, 0b101, 0b101],
        b'O' => [0b010, 0b101, 0b101, 0b101, 0b010],
        b'P' => [0b110, 0b101, 0b110, 0b100, 0b100],
        b'Q' => [0b010, 0b101, 0b101, 0b110, 0b011],
        b'R' => [0b110, 0b101, 0b110, 0b101, 0b101],
        b'S' => [0b011, 0b100, 0b010, 0b001, 0b110],
        b'T' => [0b111, 0b010, 0b010, 0b010, 0b010],
        b'U' => [0b101, 0b101, 0b101, 0b101, 0b111],
        b'V' => [0b101, 0b101, 0b101, 0b101, 0b010],
        b'W' => [0b101, 0b101, 0b111, 0b111, 0b101],
        b'X' => [0b101, 0b101, 0b010, 0b101, 0b101],
        b'Y' => [0b101, 0b101, 0b010, 0b010, 0b010],
        b'Z' => [0b111, 0b001, 0b010, 0b100, 0b111],
        b'.' => [0, 0, 0, 0, 0b010],
        b',' => [0, 0, 0, 0b010, 0b100],
        b':' => [0, 0b010, 0, 0b010, 0],
        b'-' => [0, 0, 0b111, 0, 0],
        b'+' => [0, 0b010, 0b111, 0b010, 0],
        b'=' => [0, 0b111, 0, 0b111, 0],
        b'_' => [0, 0, 0, 0, 0b111],
        b'/' => [0b001, 0b001, 0b010, 0b100, 0b100],
        b'%' => [0b101, 0b001, 0b010, 0b100, 0b101],
        b'(' => [0b010, 0b100, 0b100, 0b100, 0b010],
        b')' => [0b010, 0b001, 0b001, 0b001, 0b010],
        b'@' => [0b010, 0b101, 0b111, 0b100, 0b011],
        _ => [0b111, 0b001, 0b010, 0, 0b010], // '?'
    }
}

/// Fixed-capacity text line; `core::fmt` into it does not allocate.
pub struct Line<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> Default for Line<N> {
    fn default() -> Self {
        Self { buf: [0; N], len: 0 }
    }
}

impl<const N: usize> Line<N> {
    pub fn clear(&mut self) {
        self.len = 0;
    }
    pub fn as_str(&self) -> &str {
        // Only whole &str fragments are appended, so this is valid UTF-8.
        std::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl<const N: usize> std::fmt::Write for Line<N> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        // Truncate on a char boundary rather than failing.
        let mut take = s.len().min(N - self.len);
        while !s.is_char_boundary(take) {
            take -= 1;
        }
        self.buf[self.len..self.len + take].copy_from_slice(&s.as_bytes()[..take]);
        self.len += take;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write;

    #[test]
    fn text_merges_runs() {
        let mut b = RectBatch::with_capacity(64);
        // "--" is one row with two separate 3-cell runs.
        let w = b.text(10, 20, 2, [1.0; 4], "--");
        assert_eq!(w, 2 * GLYPH_ADVANCE * 2);
        assert_eq!(
            b.items().iter().map(|(r, _)| *r).collect::<Vec<_>>(),
            vec![Rect { x: 10, y: 24, w: 6, h: 2 }, Rect { x: 18, y: 24, w: 6, h: 2 }]
        );
    }

    #[test]
    fn text_full_row_is_one_run() {
        let mut b = RectBatch::with_capacity(64);
        // '_' bottom row is 3 cells wide; adjacent glyphs leave a gap column.
        b.text(0, 0, 1, [1.0; 4], "_");
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn frame_and_degenerate_rects() {
        let mut b = RectBatch::with_capacity(8);
        b.frame(0, 0, 10, 10, 2, [1.0; 4]);
        assert_eq!(b.len(), 4);
        b.rect(0, 0, 0, 5, [1.0; 4]);
        assert_eq!(b.len(), 4);
    }

    #[test]
    fn line_truncates() {
        let mut l: Line<4> = Line::default();
        write!(l, "abcdef").unwrap();
        assert_eq!(l.as_str(), "abcd");
        l.clear();
        write!(l, "{:.1}", 1.25f32).unwrap();
        assert_eq!(l.as_str(), "1.2");
    }
}
