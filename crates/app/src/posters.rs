//! Procedural stand-in artwork for the M2 benchmark scene: deterministic
//! "posters" and "backdrops", so rendering can be measured before the
//! Jellyfin client and image pipeline exist (M5 replaces this).

use jellyroll_render::Color;

fn hsv(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h = h.rem_euclid(1.0) * 6.0;
    let i = h.floor();
    let f = h - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match i as u32 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

fn hue(i: usize) -> f32 {
    (i as f32 * 0.618_034).fract()
}

/// Flat colour shown until a poster's texture is uploaded (stands in for a
/// BlurHash placeholder).
pub fn placeholder(i: usize) -> Color {
    let [r, g, b] = hsv(hue(i), 0.45, 0.35);
    Color { r, g, b, a: 1.0 }
}

/// Fills `out` with a `w`x`h` RGBA8 poster: a two-tone gradient, a diagonal
/// band and a disc, varied by `i`.
pub fn poster(i: usize, w: u32, h: u32, out: &mut Vec<u8>) {
    let h0 = hue(i);
    let top = hsv(h0, 0.55, 0.30);
    let bottom = hsv(h0 + 0.08, 0.70, 0.85);
    let band = hsv(h0 + 0.5, 0.50, 0.95);
    let (cx, cy, rad) = (
        w as f32 * (0.3 + 0.4 * hue(i + 7)),
        h as f32 * (0.25 + 0.3 * hue(i + 3)),
        w as f32 * (0.18 + 0.12 * hue(i + 11)),
    );
    out.clear();
    out.reserve(w as usize * h as usize * 4);
    for y in 0..h {
        let t = y as f32 / h as f32;
        for x in 0..w {
            let mut c = [
                top[0] + (bottom[0] - top[0]) * t,
                top[1] + (bottom[1] - top[1]) * t,
                top[2] + (bottom[2] - top[2]) * t,
            ];
            let d = (x as f32 - y as f32 * 0.5 + h as f32 * 0.1) / w as f32;
            if (0.45..0.55).contains(&d.fract().abs()) {
                c = [
                    c[0] * 0.5 + band[0] * 0.5,
                    c[1] * 0.5 + band[1] * 0.5,
                    c[2] * 0.5 + band[2] * 0.5,
                ];
            }
            let (dx, dy) = (x as f32 - cx, y as f32 - cy);
            if dx * dx + dy * dy < rad * rad {
                c = [c[0] * 0.3 + 0.7, c[1] * 0.3 + 0.7, c[2] * 0.3 + 0.65];
            }
            out.extend_from_slice(&[
                (c[0] * 255.0) as u8,
                (c[1] * 255.0) as u8,
                (c[2] * 255.0) as u8,
                255,
            ]);
        }
    }
}

/// Fills `out` with a soft landscape-ish backdrop.
pub fn backdrop(i: usize, w: u32, h: u32, out: &mut Vec<u8>) {
    let h0 = hue(i * 5 + 2);
    let sky = hsv(h0, 0.5, 0.55);
    let ground = hsv(h0 + 0.12, 0.6, 0.25);
    out.clear();
    out.reserve(w as usize * h as usize * 4);
    for y in 0..h {
        for x in 0..w {
            let fx = x as f32 / w as f32;
            let horizon = 0.55 + 0.08 * (fx * 6.0 + i as f32).sin();
            let t = y as f32 / h as f32;
            let c = if t < horizon {
                let k = t / horizon;
                [sky[0] * (1.0 - 0.4 * k), sky[1] * (1.0 - 0.3 * k), sky[2]]
            } else {
                ground
            };
            out.extend_from_slice(&[
                (c[0] * 255.0) as u8,
                (c[1] * 255.0) as u8,
                (c[2] * 255.0) as u8,
                255,
            ]);
        }
    }
}

/// Bakes the legibility scrims into a backdrop (spec 6.5: precomputed
/// gradient scrims), so drawing it costs one full-screen pass instead of
/// three: darken toward `bg` from the left edge (`left_alpha` at x = 0,
/// clear at 75% width) and toward the bottom (clear at 35% height,
/// `bottom_alpha` at the bottom edge).
pub fn apply_scrim(px: &mut [u8], w: u32, h: u32, bg: [u8; 3], left_alpha: f32, bottom_alpha: f32) {
    let (wf, hf) = (w as f32, h as f32);
    for y in 0..h {
        let ty = ((y as f32 / hf - 0.35) / 0.65).clamp(0.0, 1.0);
        let ab = bottom_alpha * ty;
        for x in 0..w {
            let al = left_alpha * (1.0 - (x as f32 / (wf * 0.75)).min(1.0));
            let i = ((y * w + x) * 4) as usize;
            for (k, &b) in bg.iter().enumerate() {
                let v = px[i + k] as f32;
                let v = v + (b as f32 - v) * al;
                let v = v + (b as f32 - v) * ab;
                px[i + k] = v.round() as u8;
            }
        }
    }
}

/// Cuts anti-aliased rounded corners into RGBA8 pixels (alpha only), so a
/// card can be drawn as a plain image (spec 6.5: card chrome baked into
/// cached assets). Plain image draws batch in the renderer; rounded-rect
/// image fills do not.
/// Premultiplies alpha in place (what `Renderer::create_image` takes).
/// Only pixels that aren't opaque change, so for posters with rounded
/// corners this touches the corners only. Done on the decoding thread:
/// left to Skia, the conversion ran on the render thread during the upload.
pub fn premultiply(px: &mut [u8]) {
    for p in px.as_chunks_mut::<4>().0 {
        let a = p[3] as u32;
        if a < 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * a + 127) / 255) as u8;
            }
        }
    }
}

pub fn round_corners(px: &mut [u8], w: u32, h: u32, radius: f32) {
    let r = radius.min(w as f32 / 2.0).min(h as f32 / 2.0);
    if r <= 0.0 {
        return;
    }
    let span = r.ceil() as u32;
    for y in 0..span.min(h) {
        for x in 0..span.min(w) {
            // Distance from the corner circle's centre, sampled at the pixel
            // centre; one pixel of linear falloff for anti-aliasing.
            let (dx, dy) = (r - (x as f32 + 0.5), r - (y as f32 + 0.5));
            let d = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt();
            let cover = (r - d + 0.5).clamp(0.0, 1.0);
            if cover >= 1.0 {
                continue;
            }
            for (cx, cy) in [(x, y), (w - 1 - x, y), (x, h - 1 - y), (w - 1 - x, h - 1 - y)] {
                let a = &mut px[((cy * w + cx) * 4 + 3) as usize];
                *a = (*a as f32 * cover).round() as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_determinism() {
        let (mut a, mut b) = (Vec::new(), Vec::new());
        poster(3, 20, 30, &mut a);
        poster(3, 20, 30, &mut b);
        assert_eq!(a.len(), 20 * 30 * 4);
        assert_eq!(a, b);
        poster(4, 20, 30, &mut b);
        assert_ne!(a, b);
        backdrop(1, 16, 9, &mut a);
        assert_eq!(a.len(), 16 * 9 * 4);
        assert!(a.chunks(4).all(|p| p[3] == 255));
    }

    #[test]
    fn rounded_corners_are_transparent_and_centre_is_not() {
        let (w, h) = (40u32, 60u32);
        let mut px = vec![255u8; (w * h * 4) as usize];
        round_corners(&mut px, w, h, 8.0);
        let a = |x: u32, y: u32| px[((y * w + x) * 4 + 3) as usize];
        for (x, y) in [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
            assert_eq!(a(x, y), 0, "corner ({x},{y})");
        }
        assert_eq!(a(w / 2, 0), 255, "top edge middle");
        assert_eq!(a(0, h / 2), 255, "left edge middle");
        assert_eq!(a(w / 2, h / 2), 255, "centre");
        let edge = a(2, 2);
        assert!(edge > 0 && edge < 255 || edge == 0, "anti-aliased falloff");
    }

    #[test]
    fn premultiply_scales_colour_by_alpha() {
        let mut px = [200, 100, 50, 255, 200, 100, 50, 128, 200, 100, 50, 0];
        premultiply(&mut px);
        assert_eq!(px, [200, 100, 50, 255, 100, 50, 25, 128, 0, 0, 0, 0]);
    }

    #[test]
    fn scrim_darkens_left_and_bottom_only() {
        let (w, h) = (100u32, 100u32);
        let mut px = vec![200u8; (w * h * 4) as usize];
        apply_scrim(&mut px, w, h, [0, 0, 0], 0.9, 0.9);
        let at = |x: u32, y: u32| px[((y * w + x) * 4) as usize];
        assert!(at(0, 0) < 30, "left edge darkened");
        assert_eq!(at(99, 0), 200, "top right untouched");
        assert!(at(99, 99) < 30, "bottom darkened");
        assert!(at(50, 20) > at(10, 20), "fades toward the right");
    }
}
