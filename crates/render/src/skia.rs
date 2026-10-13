//! Skia (Ganesh/GL) backend for [`Renderer`]. Shares the platform's GL
//! context; draws straight into the default framebuffer, or into a smaller
//! offscreen target that is upscaled when `render_scale < 1` (spec 6.4).
//!
//! Hot-path rules (spec 6.1): no saveLayer except explicit `push_opacity`,
//! no per-frame blurs (shadows are baked once into textures), text is
//! shaped once and cached as a TextBlob.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::ffi::c_void;
use std::hash::{Hash, Hasher};

use skia_safe as sk;
use skia_safe::gpu::{self, DirectContext, SurfaceOrigin, gl as skgl};

use crate::text::{FONTS, Shaper};
use crate::{Color, Gradient, ImageId, Point, Rect, RenderStats, Renderer, TextMetrics, TextStyle};

const GL_RGBA8: u32 = 0x8058;
/// Text cache size before stale entries are evicted.
const TEXT_CACHE_CAP: usize = 4096;
/// Entries unused for this many frames are evicted when the cache is full.
const TEXT_CACHE_KEEP_FRAMES: u64 = 120;
/// Baked shadow textures kept before the cache is reset.
const SHADOW_CACHE_CAP: usize = 64;

#[derive(Copy, Clone, Debug)]
pub struct SkiaOptions {
    /// UI render scale (0.5..=1.0). Below 1 the UI is drawn offscreen at
    /// reduced resolution and upscaled.
    pub render_scale: f32,
    /// Stencil bits of the default framebuffer.
    pub stencil_bits: usize,
}

impl Default for SkiaOptions {
    fn default() -> Self {
        Self { render_scale: 1.0, stencil_bits: 8 }
    }
}

struct TextEntry {
    text: Box<str>,
    style: TextStyle,
    blob: Option<sk::TextBlob>,
    metrics: TextMetrics,
    last_used: u64,
}

struct ImageEntry {
    image: sk::Image,
    bytes: u64,
    /// The pooled texture behind `image`, returned to the pool on release.
    surface: Option<(PoolKey, sk::Surface)>,
}

/// Textures are reused by (width, height, opaque) (D43): on the HD 530,
/// writing into an existing texture took 0.7 ms for a 4K poster and 1.1 ms
/// for a backdrop, against 1.0 and 6.1 ms for a new texture each time.
type PoolKey = (i32, i32, bool);
/// Images smaller than this skip the pool: creating them is as cheap.
const POOL_MIN_BYTES: usize = 64 << 10;
/// Spare textures kept per size.
const POOL_SPARES: usize = 48;

/// Shadows are baked once per (radius, blur, colour) as a small nine-patch:
/// fixed corners, stretchable edges and a transparent centre. Any card size
/// reuses the same texture, and the part hidden under the card is never
/// drawn (spec 6.1: no per-frame blur, minimal fill).
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
struct ShadowKey {
    radius: u32,
    blur: u32,
    argb: u32,
}

impl ShadowKey {
    /// Margin around the shape: 3 sigma of the blur (sigma = blur / 2).
    fn pad(&self) -> u32 {
        (self.blur * 3).div_ceil(2)
    }
    /// Corner cell size: margin plus corner radius.
    fn corner(&self) -> u32 {
        self.pad() + self.radius + 1
    }
    /// Baked texture side: two corners and a 2 px stretchable middle.
    fn side(&self) -> u32 {
        2 * self.corner() + 2
    }
}

pub struct SkiaRenderer {
    ctx: DirectContext,
    opts: SkiaOptions,
    fb: (u32, u32),
    main: Option<sk::Surface>,
    offscreen: Option<sk::Surface>,
    /// Device pixels per logical px of the surface being drawn.
    scale: f32,
    logical: (f32, f32),
    shaper: Shaper,
    typefaces: Vec<sk::Typeface>,
    text_cache: HashMap<u64, TextEntry>,
    shadows: HashMap<ShadowKey, sk::Image>,
    images: Vec<Option<ImageEntry>>,
    free_images: Vec<u32>,
    pool: HashMap<PoolKey, Vec<sk::Surface>>,
    frame: u64,
    stats: RenderStats,
}

fn sk_color(c: Color) -> sk::Color4f {
    sk::Color4f::new(c.r, c.g, c.b, c.a)
}

fn sk_rect(r: Rect) -> sk::Rect {
    sk::Rect::from_xywh(r.x, r.y, r.w, r.h)
}

fn rrect(r: Rect, radius: f32) -> sk::RRect {
    sk::RRect::new_rect_xy(sk_rect(r), radius, radius)
}

fn paint(c: Color) -> sk::Paint {
    let mut p = sk::Paint::new(sk_color(c), None);
    p.set_anti_alias(true);
    p
}

fn text_key(text: &str, style: &TextStyle) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    style.size.to_bits().hash(&mut h);
    style.weight.hash(&mut h);
    style.max_width.map(f32::to_bits).hash(&mut h);
    h.finish()
}

impl SkiaRenderer {
    /// A spare texture of this size, or a new one.
    fn pooled_surface(&mut self, key: PoolKey, info: &sk::ImageInfo) -> Option<sk::Surface> {
        if let Some(s) = self.pool.get_mut(&key).and_then(Vec::pop) {
            return Some(s);
        }
        gpu::surfaces::render_target(
            &mut self.ctx,
            gpu::Budgeted::No,
            info,
            None,
            SurfaceOrigin::TopLeft,
            None,
            false,
            None,
        )
    }

    /// `--bench-upload`: times ways of getting a `w`x`h` RGBA image onto the
    /// GPU, `n` times each, to tell what an upload costs on this driver.
    /// Returns (method, mean ms per call, mean ms of the flush after it).
    pub fn bench_uploads(&mut self, w: u32, h: u32, n: usize) -> Vec<(&'static str, f64, f64)> {
        let info = sk::ImageInfo::new(
            (w as i32, h as i32),
            sk::ColorType::RGBA8888,
            sk::AlphaType::Premul,
            None,
        );
        let row = w as usize * 4;
        // Two different images, so no call can be skipped as a no-op.
        let mut px: Vec<Vec<u8>> = (0..2u8)
            .map(|k| {
                (0..row * h as usize)
                    .map(|i| if i % 4 == 3 { 255 } else { (i as u8).wrapping_mul(31) ^ k })
                    .collect()
            })
            .collect();
        let ms = |t: std::time::Instant| t.elapsed().as_secs_f64() * 1000.0;
        let mut out = Vec::new();

        // A: what the app does: a new texture per image.
        let (mut op, mut fl) = (0.0, 0.0);
        let mut keep = Vec::with_capacity(n);
        for i in 0..n {
            let t = std::time::Instant::now();
            let raster = sk::images::raster_from_data(&info, sk::Data::new_copy(&px[i % 2]), row);
            let img = raster.and_then(|r| {
                gpu::images::texture_from_image(
                    &mut self.ctx,
                    &r,
                    gpu::Mipmapped::No,
                    gpu::Budgeted::No,
                )
            });
            op += ms(t);
            let t = std::time::Instant::now();
            self.ctx.flush_and_submit();
            fl += ms(t);
            keep.extend(img);
        }
        out.push(("A new texture per image (current)", op / n as f64, fl / n as f64));
        drop(keep);
        self.ctx.flush_and_submit();

        // B: new pixels into one existing texture.
        let (mut op, mut fl) = (0.0, 0.0);
        if let Some(mut surf) = gpu::surfaces::render_target(
            &mut self.ctx,
            gpu::Budgeted::No,
            &info,
            None,
            SurfaceOrigin::TopLeft,
            None,
            false,
            None,
        ) {
            for i in 0..n {
                let t = std::time::Instant::now();
                if let Some(pm) = sk::Pixmap::new(&info, &mut px[i % 2], row) {
                    surf.write_pixels_from_pixmap(&pm, (0, 0));
                }
                op += ms(t);
                let t = std::time::Instant::now();
                self.ctx.flush_and_submit();
                fl += ms(t);
            }
            out.push(("B write into an existing texture", op / n as f64, fl / n as f64));
        }

        // C: allocation alone: a new empty texture, no pixels.
        let (mut op, mut fl) = (0.0, 0.0);
        let mut keep = Vec::with_capacity(n);
        for _ in 0..n {
            let t = std::time::Instant::now();
            let surf = gpu::surfaces::render_target(
                &mut self.ctx,
                gpu::Budgeted::No,
                &info,
                None,
                SurfaceOrigin::TopLeft,
                None,
                false,
                None,
            );
            op += ms(t);
            let t = std::time::Instant::now();
            self.ctx.flush_and_submit();
            fl += ms(t);
            keep.extend(surf);
        }
        out.push(("C new empty texture, no pixels", op / n as f64, fl / n as f64));
        drop(keep);
        self.ctx.flush_and_submit();

        // D: the app's path (create_image/release_image, pooled when
        // large), for opaque images (backdrops) and with alpha (posters).
        for (opaque, name) in [
            (true, "D the app's path, opaque (backdrops)"),
            (false, "D the app's path, with alpha (posters)"),
        ] {
            let (mut op, mut fl) = (0.0, 0.0);
            let mut prev = None;
            for i in 0..n {
                let t = std::time::Instant::now();
                let id = self.create_image(w, h, &px[i % 2], opaque);
                op += ms(t);
                let t = std::time::Instant::now();
                self.ctx.flush_and_submit();
                fl += ms(t);
                if let Some(p) = prev.take() {
                    self.release_image(p);
                }
                prev = id;
            }
            if let Some(p) = prev {
                self.release_image(p);
            }
            out.push((name, op / n as f64, fl / n as f64));
        }
        out
    }

    /// `load` resolves GL symbols (e.g. SDL_GL_GetProcAddress). The GL
    /// context must be current.
    pub fn new(load: impl Fn(&str) -> *const c_void, opts: SkiaOptions) -> Result<Self, String> {
        let interface = skgl::Interface::new_load_with(|name| {
            // Skia probes EGL through the same loader; SDL owns EGL, and
            // resolving it here confuses Skia's GL/GLES detection.
            if name == "eglGetCurrentDisplay" { std::ptr::null() } else { load(name) }
        })
        .ok_or("Skia could not load the GL interface")?;
        let ctx = gpu::direct_contexts::make_gl(interface, None)
            .ok_or("Skia could not create a GL context")?;

        let mgr = sk::FontMgr::custom_empty().unwrap_or_default();
        let mut typefaces = Vec::with_capacity(FONTS.len());
        for (name, bytes) in FONTS {
            let tf = mgr
                .new_from_data(sk::Data::new_copy(bytes), None)
                .ok_or_else(|| format!("Skia could not load font {name}"))?;
            typefaces.push(tf);
        }

        Ok(Self {
            ctx,
            opts: SkiaOptions { render_scale: opts.render_scale.clamp(0.5, 1.0), ..opts },
            fb: (0, 0),
            main: None,
            offscreen: None,
            scale: 1.0,
            logical: (0.0, 0.0),
            shaper: Shaper::new(),
            typefaces,
            text_cache: HashMap::new(),
            shadows: HashMap::new(),
            images: Vec::new(),
            free_images: Vec::new(),
            pool: HashMap::new(),
            frame: 0,
            stats: RenderStats::default(),
        })
    }

    fn make_surfaces(&mut self, fb: (u32, u32)) -> bool {
        let (w, h) = (fb.0 as i32, fb.1 as i32);
        let info = skgl::FramebufferInfo { fboid: 0, format: GL_RGBA8, ..Default::default() };
        let target = gpu::backend_render_targets::make_gl((w, h), 0, self.opts.stencil_bits, info);
        self.main = gpu::surfaces::wrap_backend_render_target(
            &mut self.ctx,
            &target,
            SurfaceOrigin::BottomLeft,
            sk::ColorType::RGBA8888,
            None,
            None,
        );
        self.offscreen = None;
        if self.opts.render_scale < 1.0 {
            let sw = ((w as f32) * self.opts.render_scale).round() as i32;
            let sh = ((h as f32) * self.opts.render_scale).round() as i32;
            let ii = sk::ImageInfo::new_n32_premul((sw.max(1), sh.max(1)), None);
            self.offscreen = gpu::surfaces::render_target(
                &mut self.ctx,
                gpu::Budgeted::Yes,
                &ii,
                None,
                SurfaceOrigin::TopLeft,
                None,
                false,
                None,
            );
            if self.offscreen.is_none() {
                tracing::warn!("offscreen UI target failed; rendering at full resolution");
            }
        }
        self.fb = fb;
        self.main.is_some()
    }

    /// The surface the UI is drawn into this frame.
    fn canvas(&mut self) -> &sk::Canvas {
        let s = match self.offscreen.as_mut() {
            Some(s) => s,
            None => self.main.as_mut().expect("begin_frame ensured a surface"),
        };
        s.canvas()
    }

    fn shaped(&mut self, text: &str, style: &TextStyle) -> &TextEntry {
        let key = text_key(text, style);
        let hit = self.text_cache.get(&key).is_some_and(|e| &*e.text == text && e.style == *style);
        if hit {
            self.stats.text_cache_hits += 1;
        } else {
            self.stats.text_cache_misses += 1;
            if self.text_cache.len() >= TEXT_CACHE_CAP {
                let frame = self.frame;
                self.text_cache.retain(|_, e| e.last_used + TEXT_CACHE_KEEP_FRAMES > frame);
                if self.text_cache.len() >= TEXT_CACHE_CAP {
                    self.text_cache.clear();
                }
            }
            let shaped = self.shaper.shape(text, style);
            let blob = build_blob(&self.typefaces, &shaped);
            let metrics = TextMetrics { width: shaped.width, height: shaped.height };
            self.text_cache.insert(
                key,
                TextEntry { text: text.into(), style: *style, blob, metrics, last_used: 0 },
            );
        }
        let e = self.text_cache.get_mut(&key).expect("present");
        e.last_used = self.frame;
        e
    }

    fn baked_shadow(&mut self, key: ShadowKey) -> Option<sk::Image> {
        if let Some(img) = self.shadows.get(&key) {
            return Some(img.clone());
        }
        if self.shadows.len() >= SHADOW_CACHE_CAP {
            self.shadows.clear();
        }
        let (pad, side) = (key.pad() as f32, key.side() as i32);
        let mut surf = sk::surfaces::raster_n32_premul((side, side))?;
        let c = surf.canvas();
        c.clear(sk::Color::TRANSPARENT);
        let mut p = sk::Paint::default();
        p.set_anti_alias(true);
        p.set_color(sk::Color::new(key.argb));
        p.set_mask_filter(sk::MaskFilter::blur(
            sk::BlurStyle::Normal,
            key.blur as f32 / 2.0,
            false,
        ));
        let inner = side as f32 - 2.0 * pad;
        let r = sk::Rect::from_xywh(pad, pad, inner, inner);
        c.draw_rrect(sk::RRect::new_rect_xy(r, key.radius as f32, key.radius as f32), &p);
        let raster = surf.image_snapshot();
        let tex = gpu::images::texture_from_image(
            &mut self.ctx,
            &raster,
            gpu::Mipmapped::No,
            gpu::Budgeted::Yes,
        )
        .unwrap_or(raster);
        self.shadows.insert(key, tex.clone());
        Some(tex)
    }
}

fn build_blob(typefaces: &[sk::Typeface], t: &crate::text::ShapedText) -> Option<sk::TextBlob> {
    if t.glyphs.is_empty() {
        return None;
    }
    let mut builder = sk::TextBlobBuilder::new();
    let mut start = 0;
    while start < t.glyphs.len() {
        let font_idx = t.glyphs[start].font;
        let end = t.glyphs[start..]
            .iter()
            .position(|g| g.font != font_idx)
            .map_or(t.glyphs.len(), |n| start + n);
        let mut font = sk::Font::from_typeface(typefaces[font_idx as usize].clone(), t.size);
        font.set_edging(sk::font::Edging::AntiAlias);
        font.set_subpixel(true);
        font.set_hinting(sk::FontHinting::None);
        font.set_linear_metrics(true);
        let (ids, pos) = builder.alloc_run_pos(&font, end - start, None);
        for (i, g) in t.glyphs[start..end].iter().enumerate() {
            ids[i] = g.id;
            pos[i] = sk::Point::new(g.x, g.y);
        }
        start = end;
    }
    builder.make()
}

impl Renderer for SkiaRenderer {
    fn begin_frame(&mut self, fb: (u32, u32)) -> bool {
        self.frame += 1;
        if (fb != self.fb || self.main.is_none()) && !self.make_surfaces(fb) {
            return false;
        }
        let device_scale = fb.1 as f32 / 1080.0;
        self.scale = if self.offscreen.is_some() {
            device_scale * self.opts.render_scale
        } else {
            device_scale
        };
        self.logical = (fb.0 as f32 / device_scale, 1080.0);
        let s = self.scale;
        let c = self.canvas();
        c.restore_to_count(1);
        c.reset_matrix();
        c.save();
        c.scale((s, s));
        true
    }

    fn end_frame(&mut self) {
        self.canvas().restore_to_count(1);
        if let Some(off) = self.offscreen.as_mut() {
            let img = off.image_snapshot();
            if let Some(main) = self.main.as_mut() {
                let (w, h) = (self.fb.0 as f32, self.fb.1 as f32);
                let mut p = sk::Paint::default();
                p.set_blend_mode(sk::BlendMode::Src);
                main.canvas().draw_image_rect_with_sampling_options(
                    &img,
                    None,
                    sk::Rect::from_wh(w, h),
                    sk::SamplingOptions::new(sk::FilterMode::Linear, sk::MipmapMode::None),
                    &p,
                );
            }
        }
        self.ctx.flush_and_submit();
    }

    fn reset_gl_state(&mut self) {
        self.ctx.reset(None);
    }

    fn logical_size(&self) -> (f32, f32) {
        self.logical
    }

    fn clear(&mut self, c: Color) {
        self.canvas().clear(sk_color(c));
    }

    fn fill_rect(&mut self, r: Rect, c: Color) {
        let mut p = paint(c);
        p.set_anti_alias(false);
        self.canvas().draw_rect(sk_rect(r), &p);
    }

    fn fill_rrect(&mut self, r: Rect, radius: f32, c: Color) {
        self.canvas().draw_rrect(rrect(r, radius), &paint(c));
    }

    fn stroke_rrect(&mut self, r: Rect, radius: f32, width: f32, c: Color) {
        let mut p = paint(c);
        p.set_style(sk::PaintStyle::Stroke);
        p.set_stroke_width(width);
        self.canvas().draw_rrect(rrect(r, radius), &p);
    }

    fn fill_gradient(&mut self, r: Rect, radius: f32, g: &Gradient) {
        let end = if g.vertical { (r.x, r.bottom()) } else { (r.right(), r.y) };
        let colors = [sk_color(g.from), sk_color(g.to)];
        let grad = sk::gradient::Gradient::new(
            sk::gradient::Colors::new_evenly_spaced(&colors, sk::TileMode::Clamp, None),
            sk::gradient::Interpolation::default(),
        );
        let shader = sk::gradient::shaders::linear_gradient(((r.x, r.y), end), &grad, None);
        let mut p = sk::Paint::default();
        p.set_anti_alias(radius > 0.0);
        p.set_shader(shader);
        if radius > 0.0 {
            self.canvas().draw_rrect(rrect(r, radius), &p);
        } else {
            self.canvas().draw_rect(sk_rect(r), &p);
        }
    }

    fn shadow(&mut self, r: Rect, radius: f32, blur: f32, c: Color) {
        // Baked and drawn in device pixels so the fixed corner cells map 1:1.
        let s = self.scale;
        let key = ShadowKey {
            radius: (radius * s).round() as u32,
            blur: (blur * s).round().max(1.0) as u32,
            argb: c.to_argb8(),
        };
        let Some(img) = self.baked_shadow(key) else { return };
        let (corner, side) = (key.corner() as i32, key.side() as i32);
        let divs = [corner, side - corner];
        let mut types = [sk::canvas::lattice::RectType::Default; 9];
        types[4] = sk::canvas::lattice::RectType::Transparent;
        let colors = [sk::Color::TRANSPARENT; 9];
        let lattice = sk::canvas::lattice::Lattice {
            x_divs: &divs,
            y_divs: &divs,
            rect_types: Some(&types),
            bounds: None,
            colors: Some(&colors),
        };
        let c = self.canvas();
        let (dev, _) = c.local_to_device_as_3x3().map_rect(sk_rect(r));
        let pad = key.pad() as f32;
        let dst =
            sk::Rect::from_ltrb(dev.left - pad, dev.top - pad, dev.right + pad, dev.bottom + pad);
        c.save();
        c.reset_matrix();
        c.draw_image_lattice(&img, &lattice, dst, sk::FilterMode::Linear, None);
        c.restore();
    }

    fn image(&mut self, img: ImageId, dst: Rect, radius: f32, opacity: f32) {
        let Some(Some(entry)) = self.images.get(img.0 as usize) else { return };
        let image = entry.image.clone();
        let sampling = sk::SamplingOptions::new(sk::FilterMode::Linear, sk::MipmapMode::None);
        let mut p = sk::Paint::default();
        p.set_alpha_f(opacity.clamp(0.0, 1.0));
        p.set_anti_alias(true);
        if radius <= 0.0 {
            self.canvas().draw_image_rect_with_sampling_options(
                &image,
                None,
                sk_rect(dst),
                sampling,
                &p,
            );
            return;
        }
        // Rounded corners via an image shader on a rounded rect: one draw,
        // no clip or layer.
        let src = sk::Rect::from_wh(image.width() as f32, image.height() as f32);
        let m = sk::Matrix::rect_2_rect(src, sk_rect(dst), sk::matrix::ScaleToFit::Fill)
            .unwrap_or_default();
        p.set_shader(image.to_shader((sk::TileMode::Clamp, sk::TileMode::Clamp), sampling, &m));
        self.canvas().draw_rrect(rrect(dst, radius), &p);
    }

    fn image_region(&mut self, img: ImageId, src: Rect, dst: Rect) {
        let Some(Some(entry)) = self.images.get(img.0 as usize) else { return };
        let image = entry.image.clone();
        let sampling = sk::SamplingOptions::new(sk::FilterMode::Linear, sk::MipmapMode::None);
        let p = sk::Paint::default();
        self.canvas().draw_image_rect_with_sampling_options(
            &image,
            Some((&sk_rect(src), sk::canvas::SrcRectConstraint::Strict)),
            sk_rect(dst),
            sampling,
            &p,
        );
    }

    fn text(&mut self, text: &str, style: &TextStyle, origin: Point, c: Color) -> TextMetrics {
        let e = self.shaped(text, style);
        let (blob, metrics) = (e.blob.clone(), e.metrics);
        if let Some(blob) = blob {
            // Snap the text origin to whole device pixels. Skia reuses a
            // blob's cached glyph runs only when it moves by whole pixels;
            // fractional motion (scrolling, sway) would rebuild every
            // title every frame. Snapping also keeps moving text crisp.
            let canvas = self.canvas();
            let m = canvas.local_to_device_as_3x3();
            let p = m.map_point((origin.x, origin.y));
            // Keep the scale, replace only the translation. (Skia's
            // `set_translate` would reset the whole matrix to a translation.)
            let mut snapped = m;
            snapped.set_translate_x(p.x.round());
            snapped.set_translate_y(p.y.round());
            canvas.save();
            canvas.set_matrix(&sk::M44::from(snapped));
            canvas.draw_text_blob(&blob, (0.0, 0.0), &paint(c));
            canvas.restore();
        }
        metrics
    }

    fn measure_text(&mut self, text: &str, style: &TextStyle) -> TextMetrics {
        self.shaped(text, style).metrics
    }

    fn push_clip(&mut self, r: Rect, radius: f32) {
        let c = self.canvas();
        c.save();
        c.clip_rrect(rrect(r, radius), sk::ClipOp::Intersect, true);
    }

    fn push_transform(&mut self, translate: Point, scale: f32) {
        let c = self.canvas();
        c.save();
        c.translate((translate.x, translate.y));
        c.scale((scale, scale));
    }

    fn push_opacity(&mut self, alpha: f32) {
        self.canvas().save_layer_alpha_f(None, alpha.clamp(0.0, 1.0));
    }

    fn pop(&mut self) {
        let c = self.canvas();
        if c.save_count() > 2 {
            c.restore();
        }
    }

    fn create_image(
        &mut self,
        width: u32,
        height: u32,
        rgba: &[u8],
        opaque: bool,
    ) -> Option<ImageId> {
        let bytes = width as usize * height as usize * 4;
        if width == 0 || height == 0 || rgba.len() < bytes {
            return None;
        }
        // Opaque images (backdrops, blur placeholders) are tagged as such
        // so Skia can draw them without blending.
        let alpha = if opaque { sk::AlphaType::Opaque } else { sk::AlphaType::Premul };
        let info =
            sk::ImageInfo::new((width as i32, height as i32), sk::ColorType::RGBA8888, alpha, None);
        let t1 = std::time::Instant::now();
        let row = width as usize * 4;
        let key = (width as i32, height as i32, opaque);
        let pooled = if bytes >= POOL_MIN_BYTES { self.pooled_surface(key, &info) } else { None };
        let t2 = std::time::Instant::now();
        let (image, surface) = match pooled {
            Some(mut surf) => {
                // Written in place: the surface holds no other snapshot.
                if !surf.canvas().write_pixels(&info, &rgba[..bytes], row, (0, 0)) {
                    return None;
                }
                (surf.image_snapshot(), Some((key, surf)))
            }
            None => {
                let raster =
                    sk::images::raster_from_data(&info, sk::Data::new_copy(&rgba[..bytes]), row)?;
                let image = gpu::images::texture_from_image(
                    &mut self.ctx,
                    &raster,
                    gpu::Mipmapped::No,
                    // Not counted against Skia's cache budget: the app caps
                    // its own artwork (`[cache] textures_mb`). Counted, it
                    // kept Skia permanently over its 256 MB budget (D42).
                    gpu::Budgeted::No,
                )?;
                (image, None)
            }
        };
        let t3 = std::time::Instant::now();
        self.stats.uploads += 1;
        self.stats.upload_texture_ns += (t2 - t1).as_nanos() as u64;
        self.stats.upload_write_ns += (t3 - t2).as_nanos() as u64;
        let entry = ImageEntry { image, bytes: bytes as u64, surface };
        self.stats.texture_bytes += entry.bytes;
        self.stats.images += 1;
        let id = match self.free_images.pop() {
            Some(i) => {
                self.images[i as usize] = Some(entry);
                i
            }
            None => {
                self.images.push(Some(entry));
                (self.images.len() - 1) as u32
            }
        };
        Some(ImageId(id))
    }

    fn set_render_scale(&mut self, scale: f32) {
        let scale = scale.clamp(0.5, 1.0);
        if scale != self.opts.render_scale {
            self.opts.render_scale = scale;
            // Rebuilt by the next begin_frame; spare textures were sized
            // for the old scale.
            self.main = None;
            self.pool.clear();
        }
    }

    fn release_image(&mut self, img: ImageId) {
        if let Some(slot) = self.images.get_mut(img.0 as usize)
            && let Some(e) = slot.take()
        {
            self.stats.texture_bytes -= e.bytes;
            self.stats.images -= 1;
            self.free_images.push(img.0);
            let ImageEntry { image, surface, .. } = e;
            // The snapshot goes first, so the next write needn't copy.
            drop(image);
            if let Some((key, surf)) = surface {
                let spares = self.pool.entry(key).or_default();
                if spares.len() < POOL_SPARES {
                    spares.push(surf);
                }
            }
        }
    }

    fn stats(&self) -> RenderStats {
        RenderStats {
            text_cache_entries: self.text_cache.len() as u32,
            shadow_cache_entries: self.shadows.len() as u32,
            gpu_cache_bytes: self.ctx.resource_cache_usage().resource_bytes as u64,
            gpu_cache_limit: self.ctx.resource_cache_limit() as u64,
            render_scale_pct: (self.opts.render_scale * 100.0).round() as u32,
            ..self.stats
        }
    }
}
