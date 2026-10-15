//! 2D rendering (spec 6). Everything the UI draws goes through [`Renderer`]:
//! rects, rounded rects, gradients, pre-baked shadows, images, text, clips,
//! transforms and layer opacity. The default backend is Skia on the shared
//! GL context ([`skia::SkiaRenderer`]); the trait keeps a lean custom GL
//! backend a contained swap if Skia misses the budgets (spec 6.1).
//!
//! Coordinates are logical units: 1080 logical px = output height
//! (spec 6.4). The backend applies the scale.
//!
//! Callers are generic over `R: Renderer` (no dynamic dispatch in the
//! render path).

#![forbid(unsafe_code)]

pub mod skia;
pub mod text;

/// Straight (non-premultiplied) RGBA, 0..=1.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const WHITE: Color = Color::rgb(255, 255, 255);
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const TRANSPARENT: Color = Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::rgba(r, g, b, 255)
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a: a as f32 / 255.0 }
    }

    pub fn with_alpha(self, a: f32) -> Self {
        Self { a, ..self }
    }

    /// Packed 0xAARRGGBB, for cache keys.
    pub fn to_argb8(self) -> u32 {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
        (q(self.a) << 24) | (q(self.r) << 16) | (q(self.g) << 8) | q(self.b)
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn inset(&self, d: f32) -> Self {
        Self::new(self.x + d, self.y + d, self.w - 2.0 * d, self.h - 2.0 * d)
    }

    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Weight {
    Regular,
    Bold,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct TextStyle {
    /// Font size in logical px.
    pub size: f32,
    pub weight: Weight,
    /// Truncate with an ellipsis beyond this width (logical px).
    pub max_width: Option<f32>,
}

impl TextStyle {
    pub const fn new(size: f32, weight: Weight) -> Self {
        Self { size, weight, max_width: None }
    }

    pub const fn max_width(self, w: f32) -> Self {
        Self { max_width: Some(w), ..self }
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct TextMetrics {
    pub width: f32,
    /// Line height; text drawn at `y` occupies `y..y + height`.
    pub height: f32,
}

/// Two-stop linear gradient across a rect.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Gradient {
    pub from: Color,
    pub to: Color,
    /// true: top to bottom; false: left to right.
    pub vertical: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ImageId(pub u32);

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct RenderStats {
    pub texture_bytes: u64,
    pub images: u32,
    pub text_cache_entries: u32,
    pub text_cache_hits: u64,
    pub text_cache_misses: u64,
    pub shadow_cache_entries: u32,
    /// Skia's own GPU resource cache: in use and its budget.
    pub gpu_cache_bytes: u64,
    pub gpu_cache_limit: u64,
    /// The UI's render scale, percent (100: native resolution).
    pub render_scale_pct: u32,
    /// Image uploads so far and the time spent in each step (ns): getting
    /// a texture (pooled or new) and writing the pixels into it.
    pub uploads: u64,
    pub upload_texture_ns: u64,
    pub upload_write_ns: u64,
}

pub trait Renderer {
    /// Starts a frame for a framebuffer of `fb` pixels. Returns false if
    /// there is nothing to draw into (e.g. surface lost).
    fn begin_frame(&mut self, fb: (u32, u32)) -> bool;
    /// Submits all recorded work to the GPU (does not swap).
    fn end_frame(&mut self);
    /// Another user of the GL context (mpv) changed GL state; forget any
    /// cached state before the next draw (spec 6.3).
    fn reset_gl_state(&mut self);

    /// Logical size of the current frame.
    fn logical_size(&self) -> (f32, f32);

    fn clear(&mut self, c: Color);
    fn fill_rect(&mut self, r: Rect, c: Color);
    fn fill_rrect(&mut self, r: Rect, radius: f32, c: Color);
    fn stroke_rrect(&mut self, r: Rect, radius: f32, width: f32, c: Color);
    /// Connected line segments with round ends and joins (icons such as a
    /// check mark).
    fn polyline(&mut self, pts: &[Point], width: f32, c: Color);
    fn fill_gradient(&mut self, r: Rect, radius: f32, g: &Gradient);
    /// Soft drop shadow under a rounded rect. Pre-baked into a cached
    /// texture; never a per-frame blur.
    fn shadow(&mut self, r: Rect, radius: f32, blur: f32, c: Color);
    fn image(&mut self, img: ImageId, dst: Rect, radius: f32, opacity: f32);
    /// Draws the `src` part of an image (in image pixels) into `dst`, e.g.
    /// one tile of a trickplay sheet.
    fn image_region(&mut self, img: ImageId, src: Rect, dst: Rect);
    /// Draws text with its top-left at `origin`. Shaping is cached by
    /// (text, style); static labels are never re-shaped.
    fn text(&mut self, text: &str, style: &TextStyle, origin: Point, c: Color) -> TextMetrics;
    fn measure_text(&mut self, text: &str, style: &TextStyle) -> TextMetrics;

    fn push_clip(&mut self, r: Rect, radius: f32);
    /// Translates, then scales about the new origin.
    fn push_transform(&mut self, translate: Point, scale: f32);
    /// Group opacity. Uses an offscreen layer: avoid in hot paths (spec 6.1).
    fn push_opacity(&mut self, alpha: f32);
    /// Draws what follows (until `pop`) for a TV in HDR10 mode: sRGB
    /// colours are converted to PQ/BT.2020 with sRGB white at
    /// `white_nits` cd/m² (BT.2408 reference white: 203), instead of
    /// landing at the TV's peak. Uses an offscreen layer.
    fn push_hdr(&mut self, white_nits: f32);
    fn pop(&mut self);

    /// Uploads premultiplied-alpha RGBA8 pixels as a GPU texture (the GPU's
    /// native form: nothing to convert on the render thread). `opaque`:
    /// every pixel's alpha is 255 (drawn without blending); the caller
    /// knows it from producing the pixels, so the render thread needn't
    /// scan them.
    fn create_image(
        &mut self,
        width: u32,
        height: u32,
        rgba: &[u8],
        opaque: bool,
    ) -> Option<ImageId>;

    /// Draws the UI at this fraction of the output resolution from the next
    /// frame on (0.5..=1.0; below 1 it is upscaled).
    fn set_render_scale(&mut self, scale: f32);
    fn release_image(&mut self, img: ImageId);

    fn stats(&self) -> RenderStats;
}
