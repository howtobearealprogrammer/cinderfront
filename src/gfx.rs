//! Shared helpers: colour maths, noise, soft sprite images.

use bevy::asset::RenderAssetUsages;
use bevy::image::{Image, ImageSampler};
use bevy::math::Vec4;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

/// Plain sRGB-ish colour with alpha, used for particles and procedural pixels.
pub type Rgba = Vec4;

pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Rgba {
    Vec4::new(r, g, b, a)
}
pub fn shade(c: Rgba, k: f32) -> Rgba {
    Vec4::new((c.x * k).min(1.0), (c.y * k).min(1.0), (c.z * k).min(1.0), c.w)
}
pub fn fade(c: Rgba, a: f32) -> Rgba {
    Vec4::new(c.x, c.y, c.z, c.w * a)
}
pub fn to_color(c: Rgba) -> bevy::color::Color {
    bevy::color::Color::srgba(c.x, c.y, c.z, c.w)
}

pub fn hash2(x: i32, y: i32, s: u32) -> f32 {
    let mut h = s ^ (x as u32).wrapping_mul(0x9E37_79B1) ^ (y as u32).wrapping_mul(0x85EB_CA77);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    (h >> 8) as f32 / 16_777_216.0
}

pub fn vnoise(x: f32, y: f32, s: u32) -> f32 {
    let (xi, yi) = (x.floor(), y.floor());
    let (fx, fy) = (x - xi, y - yi);
    let (sx, sy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
    let (xi, yi) = (xi as i32, yi as i32);
    let top = hash2(xi, yi, s) + (hash2(xi + 1, yi, s) - hash2(xi, yi, s)) * sx;
    let bot = hash2(xi, yi + 1, s) + (hash2(xi + 1, yi + 1, s) - hash2(xi, yi + 1, s)) * sx;
    top + (bot - top) * sy
}

pub fn fbm(x: f32, y: f32, s: u32, oct: u32) -> f32 {
    let (mut sum, mut amp, mut f, mut norm) = (0.0, 0.5, 1.0, 0.0);
    for o in 0..oct {
        sum += vnoise(x * f, y * f, s + o * 17) * amp;
        norm += amp;
        amp *= 0.5;
        f *= 2.03;
    }
    sum / norm
}

/// Small deterministic RNG for cosmetic randomness (never touches the sim).
pub struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Rng {
        Rng(seed.max(1))
    }
    pub fn next(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x >> 8) as f32 / 16_777_216.0
    }
    pub fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.next()
    }
}

pub fn rgba_image(w: u32, h: u32, bytes: Vec<u8>, usage: RenderAssetUsages) -> Image {
    let mut img = Image::new(
        Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        TextureDimension::D2,
        bytes,
        TextureFormat::Rgba8UnormSrgb,
        usage,
    );
    img.sampler = ImageSampler::linear();
    img
}

/// Soft round glow (white, alpha falloff) and a cloudy smoke puff.
pub fn sprite_images() -> (Image, Image) {
    let n = 64u32;
    let mut glow = vec![0u8; (n * n * 4) as usize];
    let mut puff = vec![0u8; (n * n * 4) as usize];
    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = ((x as f32 + 0.5) / n as f32 * 2.0 - 1.0, (y as f32 + 0.5) / n as f32 * 2.0 - 1.0);
            let a = (1.0 - (fx * fx + fy * fy).sqrt()).clamp(0.0, 1.0);
            let soft = a * a * (3.0 - 2.0 * a);
            let i = ((y * n + x) * 4) as usize;
            glow[i..i + 4].copy_from_slice(&[255, 255, 255, (soft * 255.0) as u8]);
            let cloud = fbm(x as f32 * 0.09, y as f32 * 0.09, 3, 3);
            let pa = (soft * (0.35 + 1.3 * cloud)).clamp(0.0, 1.0);
            let lum = 0.75 + 0.25 * fbm(x as f32 * 0.15 + 9.0, y as f32 * 0.15, 5, 2);
            let l = (lum * 255.0) as u8;
            puff[i..i + 4].copy_from_slice(&[l, l, l, (pa * 255.0) as u8]);
        }
    }
    let usage = RenderAssetUsages::RENDER_WORLD;
    (rgba_image(n, n, glow, usage), rgba_image(n, n, puff, usage))
}
