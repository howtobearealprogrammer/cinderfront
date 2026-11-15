//! Shared drawing helpers: font, soft sprites, gradient quads, noise.

use macroquad::prelude::*;
use std::cell::RefCell;

pub const TW: f32 = 40.0;
pub const TH: f32 = 30.0;
/// Vertical squash that gives the tilted, semi-top-down look.
pub const YS: f32 = TH / TW;

struct Gfx {
    font: Font,
    glow: Texture2D,
    puff: Texture2D,
}

thread_local! {
    static G: RefCell<Option<Gfx>> = const { RefCell::new(None) };
}

/// Call once after the window exists.
pub fn init() {
    let font = load_ttf_font_from_bytes(include_bytes!("../assets/DejaVuSans-Bold.ttf")).expect("font");
    let n = 64u16;
    let mut glow = Image::gen_image_color(n, n, WHITE);
    let mut puff = Image::gen_image_color(n, n, WHITE);
    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = ((x as f32 + 0.5) / n as f32 * 2.0 - 1.0, (y as f32 + 0.5) / n as f32 * 2.0 - 1.0);
            let a = (1.0 - (fx * fx + fy * fy).sqrt()).clamp(0.0, 1.0);
            let soft = a * a * (3.0 - 2.0 * a);
            glow.set_pixel(x as u32, y as u32, Color::new(1.0, 1.0, 1.0, soft));
            let cloud = fbm(x as f32 * 0.09, y as f32 * 0.09, 3, 3);
            let pa = (soft * (0.35 + 1.3 * cloud)).clamp(0.0, 1.0);
            let lum = 0.75 + 0.25 * fbm(x as f32 * 0.15 + 9.0, y as f32 * 0.15, 5, 2);
            puff.set_pixel(x as u32, y as u32, Color::new(lum, lum, lum, pa));
        }
    }
    let tex = |img: &Image| {
        let t = Texture2D::from_image(img);
        t.set_filter(FilterMode::Linear);
        t
    };
    let g = Gfx { font, glow: tex(&glow), puff: tex(&puff) };
    G.with(|c| *c.borrow_mut() = Some(g));
}

fn with<R>(f: impl FnOnce(&Gfx) -> R) -> R {
    G.with(|c| f(c.borrow().as_ref().expect("gfx::init not called")))
}

pub fn text(s: &str, x: f32, y: f32, size: f32, color: Color) {
    with(|g| {
        let p = TextParams { font: Some(&g.font), font_size: size.round().max(1.0) as u16, color, ..Default::default() };
        draw_text_ex(s, x.round(), y.round(), p);
    });
}

pub fn text_w(s: &str, size: f32) -> f32 {
    with(|g| measure_text(s, Some(&g.font), size.round().max(1.0) as u16, 1.0).width)
}

/// Text with a drop shadow, for anything drawn over the battlefield.
pub fn text_sh(s: &str, x: f32, y: f32, size: f32, color: Color) {
    text(s, x + 1.5, y + 1.5, size, Color::new(0.0, 0.0, 0.0, 0.7 * color.a));
    text(s, x, y, size, color);
}

/// Soft radial sprite: glows, flashes, soft shadows.
pub fn glow(c: Vec2, rx: f32, ry: f32, color: Color) {
    with(|g| {
        let p = DrawTextureParams { dest_size: Some(vec2(rx * 2.0, ry * 2.0)), ..Default::default() };
        draw_texture_ex(&g.glow, c.x - rx, c.y - ry, color, p);
    });
}

/// Cloudy sprite: smoke and dust.
pub fn puff(c: Vec2, r: f32, rot: f32, color: Color) {
    with(|g| {
        let p = DrawTextureParams { dest_size: Some(vec2(r * 2.0, r * 2.0)), rotation: rot, ..Default::default() };
        draw_texture_ex(&g.puff, c.x - r, c.y - r, color, p);
    });
}

/// Quad with a colour per corner.
pub fn quad(p: [Vec2; 4], c: [Color; 4]) {
    let v = |i: usize| Vertex::new(p[i].x, p[i].y, 0.0, 0.0, 0.0, c[i]);
    draw_mesh(&Mesh { vertices: vec![v(0), v(1), v(2), v(3)], indices: vec![0, 1, 2, 0, 2, 3], texture: None });
}

pub fn flat(p: [Vec2; 4], c: Color) {
    quad(p, [c; 4]);
}

pub fn vgrad(x: f32, y: f32, w: f32, h: f32, top: Color, bot: Color) {
    quad([vec2(x, y), vec2(x + w, y), vec2(x + w, y + h), vec2(x, y + h)], [top, top, bot, bot]);
}

pub fn hgrad(x: f32, y: f32, w: f32, h: f32, left: Color, right: Color) {
    quad([vec2(x, y), vec2(x + w, y), vec2(x + w, y + h), vec2(x, y + h)], [left, right, right, left]);
}

pub fn ellipse(c: Vec2, rx: f32, ry: f32, col: Color) {
    let n = 18;
    for i in 0..n {
        let (a0, a1) = (i as f32 / n as f32 * 6.2832, (i + 1) as f32 / n as f32 * 6.2832);
        draw_triangle(c, c + vec2(a0.cos() * rx, a0.sin() * ry), c + vec2(a1.cos() * rx, a1.sin() * ry), col);
    }
}

pub fn ellipse_lines(c: Vec2, rx: f32, ry: f32, th: f32, col: Color) {
    let n = 22;
    for i in 0..n {
        let (a0, a1) = (i as f32 / n as f32 * 6.2832, (i + 1) as f32 / n as f32 * 6.2832);
        let (p, q) = (c + vec2(a0.cos() * rx, a0.sin() * ry), c + vec2(a1.cos() * rx, a1.sin() * ry));
        draw_line(p.x, p.y, q.x, q.y, th, col);
    }
}

pub fn rgba(r: f32, g: f32, b: f32, a: f32) -> Color {
    Color::new(r, g, b, a)
}
pub fn shade(c: Color, k: f32) -> Color {
    Color::new((c.r * k).min(1.0), (c.g * k).min(1.0), (c.b * k).min(1.0), c.a)
}
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    Color::new(a.r + (b.r - a.r) * t, a.g + (b.g - a.g) * t, a.b + (b.b - a.b) * t, a.a + (b.a - a.a) * t)
}
pub fn fade(c: Color, a: f32) -> Color {
    Color::new(c.r, c.g, c.b, c.a * a)
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
