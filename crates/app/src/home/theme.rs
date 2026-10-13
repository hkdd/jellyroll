//! Shared colors (spec 10: dark theme, high contrast at 10 feet).

use jellyroll_render::Color;

pub const BG_RGB: [u8; 3] = [12, 14, 20];
pub const BG: Color = Color::rgb(BG_RGB[0], BG_RGB[1], BG_RGB[2]);
pub const TEXT: Color = Color::rgb(235, 235, 240);
pub const TEXT_DIM: Color = Color::rgb(170, 175, 190);
pub const NOTICE: Color = Color::rgb(240, 180, 60);
pub const ACCENT: Color = Color::rgb(0, 164, 220);
