//! Text shaping with cosmic-text over the bundled fonts (spec 10.4).
//! Backend-neutral: produces glyph ids and positions per bundled font; the
//! renderer turns them into GPU text (Skia rasterizes the glyphs).
//!
//! Only the embedded fonts are loaded (no system font scan), so startup is
//! fast and rendering is identical on every machine.

use std::collections::HashMap;
use std::sync::Arc;

use cosmic_text::fontdb;
use cosmic_text::{
    Attrs, Buffer, Ellipsize, EllipsizeHeightLimit, Family, FontSystem, Metrics, Shaping, Wrap,
};

use crate::{TextStyle, Weight};

/// Bundled fonts, subset by scripts/subset-fonts.sh (SIL OFL 1.1, see
/// assets/fonts/OFL.txt).
pub const FONTS: [(&str, &[u8]); 4] = [
    ("NotoSans-Regular", include_bytes!("../../../assets/fonts/NotoSans-Regular.ttf")),
    ("NotoSans-Bold", include_bytes!("../../../assets/fonts/NotoSans-Bold.ttf")),
    ("NotoSansJP-Regular", include_bytes!("../../../assets/fonts/NotoSansJP-Regular.otf")),
    ("NotoSansJP-Bold", include_bytes!("../../../assets/fonts/NotoSansJP-Bold.otf")),
];

const LATIN_FAMILY: &str = "Noto Sans";
const JP_FAMILY: &str = "Noto Sans JP";

/// Fallback restricted to the bundled families: Latin first, then JP for
/// anything it lacks (kana, kanji, full-width forms).
struct BundledFallback;

impl cosmic_text::Fallback for BundledFallback {
    fn common_fallback(&self) -> &[&'static str] {
        &[LATIN_FAMILY, JP_FAMILY]
    }

    fn forbidden_fallback(&self) -> &[&'static str] {
        &[]
    }

    fn script_fallback(&self, _script: unicode_script::Script, _locale: &str) -> &[&'static str] {
        &[JP_FAMILY]
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ShapedGlyph {
    /// Index into [`FONTS`].
    pub font: u16,
    pub id: u16,
    /// Position of the glyph origin (baseline) relative to the text's
    /// top-left corner, logical px.
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShapedText {
    pub glyphs: Vec<ShapedGlyph>,
    /// Font size the glyphs were shaped at.
    pub size: f32,
    pub width: f32,
    pub height: f32,
}

pub struct Shaper {
    system: FontSystem,
    buffer: Buffer,
    font_index: HashMap<fontdb::ID, u16>,
}

/// Line height as a multiple of font size.
const LINE_HEIGHT: f32 = 1.3;

impl Default for Shaper {
    fn default() -> Self {
        Self::new()
    }
}

impl Shaper {
    pub fn new() -> Self {
        let mut db = fontdb::Database::new();
        let mut font_index = HashMap::new();
        for (i, (_, bytes)) in FONTS.iter().enumerate() {
            let data: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(*bytes);
            for id in db.load_font_source(fontdb::Source::Binary(data)) {
                font_index.insert(id, i as u16);
            }
        }
        db.set_sans_serif_family(LATIN_FAMILY);
        let mut system =
            FontSystem::new_with_locale_and_db_and_fallback("en-US".into(), db, BundledFallback);
        let buffer = Buffer::new(&mut system, Metrics::new(16.0, 16.0 * LINE_HEIGHT));
        Self { system, buffer, font_index }
    }

    pub fn shape(&mut self, text: &str, style: &TextStyle) -> ShapedText {
        let size = style.size.max(1.0);
        let fs = &mut self.system;
        let buf = &mut self.buffer;
        buf.set_metrics(Metrics::new(size, size * LINE_HEIGHT));
        buf.set_wrap(Wrap::None);
        buf.set_size(style.max_width, None);
        buf.set_ellipsize(match style.max_width {
            Some(_) => Ellipsize::End(EllipsizeHeightLimit::Lines(1)),
            None => Ellipsize::None,
        });
        let weight = match style.weight {
            Weight::Regular => fontdb::Weight::NORMAL,
            Weight::Bold => fontdb::Weight::BOLD,
        };
        let attrs = Attrs::new().family(Family::Name(LATIN_FAMILY)).weight(weight);
        buf.set_text(text, &attrs, Shaping::Advanced, None);
        buf.shape_until_scroll(fs, false);

        let mut out = ShapedText { size, ..Default::default() };
        for run in buf.layout_runs() {
            out.width = out.width.max(run.line_w);
            out.height = out.height.max(run.line_top + run.line_height);
            for g in run.glyphs {
                let Some(&font) = self.font_index.get(&g.font_id) else { continue };
                out.glyphs.push(ShapedGlyph {
                    font,
                    id: g.glyph_id,
                    x: g.x + g.font_size * g.x_offset,
                    y: run.line_y + g.y - g.font_size * g.y_offset,
                });
            }
        }
        if out.height == 0.0 {
            out.height = size * LINE_HEIGHT;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(size: f32) -> TextStyle {
        TextStyle::new(size, Weight::Regular)
    }

    #[test]
    fn latin_uses_noto_sans() {
        let mut s = Shaper::new();
        let t = s.shape("Hello", &style(20.0));
        assert_eq!(t.glyphs.len(), 5);
        assert!(t.glyphs.iter().all(|g| g.font == 0), "{t:?}");
        assert!(t.width > 30.0 && t.width < 80.0, "{}", t.width);
        assert!(t.glyphs.windows(2).all(|w| w[1].x > w[0].x));
    }

    #[test]
    fn bold_uses_bold_face() {
        let mut s = Shaper::new();
        let t = s.shape("Hi", &TextStyle::new(20.0, Weight::Bold));
        assert!(t.glyphs.iter().all(|g| g.font == 1), "{t:?}");
    }

    #[test]
    fn japanese_falls_back_to_noto_sans_jp() {
        let mut s = Shaper::new();
        let t = s.shape("映画とテレビ", &style(20.0));
        assert_eq!(t.glyphs.len(), 6);
        assert!(t.glyphs.iter().all(|g| g.font == 2 && g.id != 0), "{t:?}");
    }

    #[test]
    fn mixed_script_and_cyrillic() {
        let mut s = Shaper::new();
        let t = s.shape("Кино 映画 Movie", &style(20.0));
        let fonts: Vec<u16> = t.glyphs.iter().map(|g| g.font).collect();
        assert!(fonts.contains(&0) && fonts.contains(&2), "{fonts:?}");
        assert!(t.glyphs.iter().all(|g| g.id != 0), "missing glyph: {t:?}");
    }

    #[test]
    fn ellipsizes_to_max_width() {
        let mut s = Shaper::new();
        let long = "An Extremely Long Movie Title That Will Not Fit";
        let full = s.shape(long, &style(20.0));
        let cut = s.shape(long, &style(20.0).max_width(150.0));
        assert!(full.width > 150.0);
        assert!(cut.width <= 150.5, "{}", cut.width);
        assert!(cut.glyphs.len() < full.glyphs.len());
    }

    #[test]
    fn empty_text_has_line_height() {
        let mut s = Shaper::new();
        let t = s.shape("", &style(20.0));
        assert!(t.glyphs.is_empty());
        assert!((t.height - 26.0).abs() < 0.01);
    }
}
