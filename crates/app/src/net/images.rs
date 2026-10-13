//! Image decode on the worker pool (spec 4, 9): server bytes (WebP, JPEG,
//! PNG) to RGBA8 at the exact on-screen pixel size, card chrome baked in,
//! so the render thread only uploads. Also BlurHash placeholders (spec 8).

use fast_image_resize as fr;

use crate::posters;

/// What to bake into the pixels after resizing.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Finish {
    /// Rounded corners of this radius (pixels).
    Card { radius: f32 },
    /// Legibility scrim toward the background color (hero backdrop).
    Backdrop { bg: [u8; 3] },
}

pub struct Decoded {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// Decodes, center-crops to the target aspect, resizes to exactly `w`×`h`
/// and applies `finish`.
pub fn decode(bytes: &[u8], w: u32, h: u32, finish: Finish) -> Result<Decoded, String> {
    if w == 0 || h == 0 {
        return Err("zero target size".into());
    }
    let img = image::load_from_memory(bytes).map_err(|e| e.to_string())?.to_rgba8();
    let (sw, sh) = img.dimensions();
    if sw == 0 || sh == 0 {
        return Err("empty image".into());
    }
    let src = fr::images::Image::from_vec_u8(sw, sh, img.into_raw(), fr::PixelType::U8x4)
        .map_err(|e| e.to_string())?;
    let mut dst = fr::images::Image::new(w, h, fr::PixelType::U8x4);
    let opts = fr::ResizeOptions::new()
        .resize_alg(fr::ResizeAlg::Convolution(fr::FilterType::CatmullRom))
        .fit_into_destination(Some((0.5, 0.5)));
    fr::Resizer::new().resize(&src, &mut dst, &opts).map_err(|e| e.to_string())?;
    let mut rgba = dst.into_vec();
    apply(&mut rgba, w, h, finish);
    Ok(Decoded { w, h, rgba })
}

fn apply(rgba: &mut [u8], w: u32, h: u32, finish: Finish) {
    match finish {
        Finish::Card { radius } => posters::round_corners(rgba, w, h, radius),
        Finish::Backdrop { bg } => posters::apply_scrim(rgba, w, h, bg, 0.92, 0.95),
    }
}

// ---- BlurHash ----------------------------------------------------------------
// https://github.com/woltapp/blurhash (algorithm description and reference
// decoder). Tiny placeholders: a few cosine components, decoded at ~32 px
// and upscaled by the GPU while the real image loads.

const BASE83: &[u8] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz#$%*+,-.:;=?@[]^_{|}~";

fn decode83(s: &[u8]) -> Option<u32> {
    s.iter().try_fold(0u32, |acc, &c| {
        let d = BASE83.iter().position(|&b| b == c)? as u32;
        Some(acc * 83 + d)
    })
}

fn srgb_to_linear(v: u32) -> f32 {
    let v = v as f32 / 255.0;
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb(v: f32) -> u8 {
    let v = v.clamp(0.0, 1.0);
    let s = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
    (s * 255.0 + 0.5) as u8
}

fn sign_pow(v: f32, e: f32) -> f32 {
    v.abs().powf(e).copysign(v)
}

/// Decodes a BlurHash to `w`×`h` opaque RGBA8.
pub fn blurhash(hash: &str, w: u32, h: u32) -> Result<Decoded, String> {
    let b = hash.as_bytes();
    if b.len() < 6 || w == 0 || h == 0 {
        return Err("blurhash too short".into());
    }
    let size = decode83(&b[..1]).ok_or("bad blurhash")?;
    let (nx, ny) = ((size % 9 + 1) as usize, (size / 9 + 1) as usize);
    if b.len() != 4 + 2 * nx * ny {
        return Err(format!("blurhash length {} != {}", b.len(), 4 + 2 * nx * ny));
    }
    let max = (decode83(&b[1..2]).ok_or("bad blurhash")? + 1) as f32 / 166.0;
    let mut colors = Vec::with_capacity(nx * ny);
    let dc = decode83(&b[2..6]).ok_or("bad blurhash")?;
    colors.push([
        srgb_to_linear(dc >> 16),
        srgb_to_linear((dc >> 8) & 255),
        srgb_to_linear(dc & 255),
    ]);
    for i in 1..nx * ny {
        let v = decode83(&b[4 + i * 2..6 + i * 2]).ok_or("bad blurhash")?;
        let q = |x: u32| sign_pow((x as f32 - 9.0) / 9.0, 2.0) * max;
        colors.push([q(v / (19 * 19)), q((v / 19) % 19), q(v % 19)]);
    }
    let (wf, hf) = (w as f32, h as f32);
    let cos_x: Vec<f32> = (0..w as usize * nx)
        .map(|k| (std::f32::consts::PI * (k / nx) as f32 * (k % nx) as f32 / wf).cos())
        .collect();
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let mut c = [0.0f32; 3];
            for j in 0..ny {
                let by = (std::f32::consts::PI * y as f32 * j as f32 / hf).cos();
                for i in 0..nx {
                    let basis = cos_x[x * nx + i] * by;
                    let col = colors[i + j * nx];
                    for k in 0..3 {
                        c[k] += col[k] * basis;
                    }
                }
            }
            let o = (y * w as usize + x) * 4;
            rgba[o] = linear_to_srgb(c[0]);
            rgba[o + 1] = linear_to_srgb(c[1]);
            rgba[o + 2] = linear_to_srgb(c[2]);
            rgba[o + 3] = 255;
        }
    }
    Ok(Decoded { w, h, rgba })
}

/// BlurHash placeholder size for a card of aspect `w`:`h`: the long side is
/// 32 px (the hash has at most 9 components per axis).
pub fn blurhash_size(w: f32, h: f32) -> (u32, u32) {
    if w >= h {
        (32, ((32.0 * h / w).round() as u32).max(1))
    } else {
        (((32.0 * w / h).round() as u32).max(1), 32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode83(mut v: u32, len: usize) -> String {
        let mut s = vec![b'0'; len];
        for i in (0..len).rev() {
            s[i] = BASE83[(v % 83) as usize];
            v /= 83;
        }
        String::from_utf8(s).unwrap()
    }

    #[test]
    fn dc_only_hash_is_a_solid_color() {
        // 1×1 components, quantized max 0, DC = sRGB (200, 100, 50).
        let hash = format!("00{}", encode83((200 << 16) | (100 << 8) | 50, 4));
        let d = blurhash(&hash, 4, 3).unwrap();
        assert_eq!((d.w, d.h, d.rgba.len()), (4, 3, 48));
        for p in d.rgba.as_chunks::<4>().0 {
            assert_eq!(*p, [200, 100, 50, 255]);
        }
    }

    #[test]
    fn reference_hash_decodes() {
        // The example hash from the BlurHash README (4×3 components).
        let d = blurhash("LEHV6nWB2yk8pyo0adR*.7kCMdnj", 32, 32).unwrap();
        assert_eq!(d.rgba.len(), 32 * 32 * 4);
        assert!(blurhash("LEHV6nWB2yk8pyo0adR*.7kCMdn", 32, 32).is_err(), "wrong length");
        assert!(blurhash("", 32, 32).is_err());
    }

    #[test]
    fn decode_crops_and_resizes_exactly() {
        // A 40×20 PNG: left half red, right half blue.
        let mut img = image::RgbaImage::new(40, 20);
        for (x, _, p) in img.enumerate_pixels_mut() {
            *p = if x < 20 { image::Rgba([255, 0, 0, 255]) } else { image::Rgba([0, 0, 255, 255]) };
        }
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        // Target is square: center crop keeps both halves, resized to 10×10.
        let d = decode(&png, 10, 10, Finish::Card { radius: 0.0 }).unwrap();
        assert_eq!((d.w, d.h), (10, 10));
        assert_eq!(&d.rgba[..4], &[255, 0, 0, 255]);
        assert_eq!(&d.rgba[(10 * 5 + 9) * 4..(10 * 5 + 9) * 4 + 4], &[0, 0, 255, 255]);
        assert!(decode(b"not an image", 10, 10, Finish::Card { radius: 0.0 }).is_err());
    }

    #[test]
    fn placeholder_sizes() {
        assert_eq!(blurhash_size(160.0, 240.0), (21, 32));
        assert_eq!(blurhash_size(300.0, 169.0), (32, 18));
    }
}
