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
}
