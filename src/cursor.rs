//! Mouse pointers. Each `Cursor` is drawn here as a small anti-aliased vector
//! image (shapes with a dark outline so they read on any terrain) and the window
//! cursor follows `Game::cursor`, which in turn mirrors what a click would do.

use crate::game::Cursor;
use crate::gfx::{rgba, rgba_image, Rgba};
use crate::scene::View;
use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::window::{CursorIcon, CustomCursor, CustomCursorImage, PrimaryWindow};
use std::collections::HashMap;

pub struct CursorPlugin;

impl Plugin for CursorPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup).add_systems(Update, apply.after(crate::RunGame));
    }
}

const N: usize = 32;
const OUTLINE: Rgba = rgba(0.05, 0.05, 0.06, 0.95);

#[derive(Resource)]
struct Cursors {
    images: HashMap<Cursor, (Handle<Image>, (u16, u16))>,
    shown: Option<Cursor>,
}

/// Signed distance helpers (pixels; negative inside).
fn seg(p: Vec2, a: Vec2, b: Vec2, w: f32) -> f32 {
    let (pa, ba) = (p - a, b - a);
    let h = (pa.dot(ba) / ba.length_squared()).clamp(0.0, 1.0);
    (pa - ba * h).length() - w
}
fn disc(p: Vec2, c: Vec2, r: f32) -> f32 {
    (p - c).length() - r
}
fn ring(p: Vec2, c: Vec2, r: f32, w: f32) -> f32 {
    ((p - c).length() - r).abs() - w
}
/// Ring segment from angle `a0` to `a1` (radians, y down, clockwise positive).
fn arc(p: Vec2, c: Vec2, r: f32, w: f32, a0: f32, a1: f32) -> f32 {
    let d = p - c;
    let a = d.y.atan2(d.x);
    let mut t = a - a0;
    while t < 0.0 {
        t += std::f32::consts::TAU;
    }
    if t <= (a1 - a0).rem_euclid(std::f32::consts::TAU) {
        ring(p, c, r, w)
    } else {
        let e0 = c + Vec2::from_angle(a0) * r;
        let e1 = c + Vec2::from_angle(a1) * r;
        ((p - e0).length()).min((p - e1).length()) - w
    }
}
fn poly(p: Vec2, pts: &[Vec2]) -> f32 {
    let mut d = f32::MAX;
    let mut inside = false;
    for i in 0..pts.len() {
        let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
        d = d.min(seg(p, a, b, 0.0));
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
    }
    if inside {
        -d
    } else {
        d
    }
}
fn v(x: f32, y: f32) -> Vec2 {
    Vec2::new(x, y)
}

/// Accumulates shapes into a 32x32 RGBA image (straight alpha, "over" blending).
struct Canvas(Vec<Rgba>);

impl Canvas {
    fn new() -> Canvas {
        Canvas(vec![Vec4::ZERO; N * N])
    }
    fn fill(&mut self, sd: impl Fn(Vec2) -> f32, c: Rgba) {
        for y in 0..N {
            for x in 0..N {
                let d = sd(v(x as f32 + 0.5, y as f32 + 0.5));
                let a = (0.5 - d).clamp(0.0, 1.0) * c.w;
                if a <= 0.0 {
                    continue;
                }
                let dst = &mut self.0[y * N + x];
                let out_a = a + dst.w * (1.0 - a);
                let rgb = (c.truncate() * a + dst.truncate() * dst.w * (1.0 - a)) / out_a.max(1e-5);
                *dst = rgb.extend(out_a);
            }
        }
    }
    /// Dark outline first, then the shape itself.
    fn shape(&mut self, sd: impl Fn(Vec2) -> f32 + Copy, c: Rgba) {
        self.fill(move |p| sd(p) - 1.6, OUTLINE);
        self.fill(sd, c);
    }
    fn bytes(&self) -> Vec<u8> {
        self.0.iter().flat_map(|c| [c.x, c.y, c.z, c.w].map(|f| (f.clamp(0.0, 1.0) * 255.0).round() as u8)).collect()
    }
}

fn arrowhead(tip: Vec2, dir: Vec2, len: f32, half: f32) -> [Vec2; 3] {
    let side = v(-dir.y, dir.x) * half;
    [tip, tip - dir * len + side, tip - dir * len - side]
}

fn draw(kind: Cursor) -> (Canvas, (u16, u16)) {
    let mut c = Canvas::new();
    let mid = v(16.0, 16.0);
    let center = (16, 16);
    let red = rgba(1.0, 0.22, 0.18, 1.0);
    let green = rgba(0.35, 1.0, 0.45, 1.0);
    let gold = rgba(1.0, 0.78, 0.22, 1.0);
    let blue = rgba(0.4, 0.75, 1.0, 1.0);
    let pointer = [v(3.0, 2.0), v(3.0, 24.0), v(8.5, 18.8), v(12.5, 27.5), v(16.0, 26.0), v(12.0, 17.5), v(19.5, 17.5)];
    let inward = |c: &mut Canvas, col: Rgba, r: f32| {
        for d in [v(0.0, 1.0), v(0.0, -1.0), v(1.0, 0.0), v(-1.0, 0.0)] {
            let pts = arrowhead(mid - d * (r - 6.0), d, 6.0, 4.5);
            c.shape(move |p| poly(p, &pts), col);
        }
    };
    let hotspot = match kind {
        Cursor::Normal => {
            c.shape(|p| poly(p, &pointer), rgba(0.97, 0.97, 0.97, 1.0));
            (3, 2)
        }
        Cursor::Select => {
            c.shape(|p| poly(p, &pointer), rgba(0.65, 1.0, 0.72, 1.0));
            (3, 2)
        }
        Cursor::Move => {
            inward(&mut c, green, 14.0);
            c.shape(|p| disc(p, mid, 2.2), green);
            center
        }
        Cursor::Attack => {
            c.shape(|p| ring(p, mid, 8.5, 1.3), red);
            for d in [v(0.0, 1.0), v(0.0, -1.0), v(1.0, 0.0), v(-1.0, 0.0)] {
                c.shape(move |p| seg(p, mid + d * 5.0, mid + d * 13.0, 1.2), red);
            }
            c.shape(|p| disc(p, mid, 1.8), red);
            center
        }
        Cursor::AttackMove => {
            let orange = rgba(1.0, 0.5, 0.12, 1.0);
            c.shape(|p| ring(p, mid, 11.0, 1.2), orange);
            inward(&mut c, orange, 11.5);
            c.shape(|p| disc(p, mid, 1.8), red);
            center
        }
        Cursor::Patrol => {
            let cyan = rgba(0.3, 0.95, 1.0, 1.0);
            let pi = std::f32::consts::PI;
            c.shape(|p| arc(p, mid, 9.5, 1.5, pi * 1.15, pi * 1.85), cyan);
            c.shape(|p| arc(p, mid, 9.5, 1.5, pi * 0.15, pi * 0.85), cyan);
            let a = arrowhead(mid + Vec2::from_angle(pi * 1.85) * 9.5 + v(1.5, 2.5), v(0.35, 1.0).normalize(), 6.0, 4.0);
            let b = arrowhead(mid + Vec2::from_angle(pi * 0.85) * 9.5 - v(1.5, 2.5), v(-0.35, -1.0).normalize(), 6.0, 4.0);
            c.shape(move |p| poly(p, &a), cyan);
            c.shape(move |p| poly(p, &b), cyan);
            center
        }
        Cursor::Harvest => {
            // Pickaxe.
            c.shape(|p| seg(p, v(9.0, 27.0), v(21.0, 10.0), 1.6), rgba(0.62, 0.42, 0.24, 1.0));
            let pi = std::f32::consts::PI;
            c.shape(|p| arc(p, v(24.0, 17.0), 11.5, 2.0, pi * 1.1, pi * 1.62), rgba(1.0, 0.6, 0.15, 1.0));
            (13, 7)
        }
        Cursor::Return => {
            // Down arrow into a tray: "deliver here".
            c.shape(|p| seg(p, v(16.0, 3.0), v(16.0, 14.0), 2.0), gold);
            let h = arrowhead(v(16.0, 21.0), v(0.0, 1.0), 7.0, 6.0);
            c.shape(move |p| poly(p, &h), gold);
            let tray = |p: Vec2| seg(p, v(5.0, 20.0), v(5.0, 27.0), 1.4).min(seg(p, v(5.0, 27.0), v(27.0, 27.0), 1.4)).min(seg(p, v(27.0, 27.0), v(27.0, 20.0), 1.4));
            c.shape(tray, gold);
            center
        }
        Cursor::Pickup => {
            // Up arrow with a hook: "lift this".
            c.shape(|p| seg(p, v(16.0, 10.0), v(16.0, 21.0), 1.8), blue);
            let h = arrowhead(v(16.0, 3.0), v(0.0, -1.0), 7.0, 6.0);
            c.shape(move |p| poly(p, &h), blue);
            let pi = std::f32::consts::PI;
            c.shape(|p| arc(p, v(12.5, 22.0), 3.5, 1.6, 0.0, pi * 1.05), blue);
            center
        }
        Cursor::Airlift => {
            // Double chevron down onto a bar: "drop here".
            for y in [6.0, 13.0] {
                c.shape(move |p| seg(p, v(8.0, y), v(16.0, y + 7.0), 1.8).min(seg(p, v(16.0, y + 7.0), v(24.0, y), 1.8)), blue);
            }
            c.shape(|p| seg(p, v(7.0, 26.0), v(25.0, 26.0), 1.8), blue);
            (16, 26)
        }
        Cursor::Repair => {
            // Wrench.
            let pi = std::f32::consts::PI;
            c.shape(|p| seg(p, v(8.0, 25.0), v(18.0, 14.0), 2.4), green);
            c.shape(|p| arc(p, v(21.0, 11.0), 5.5, 2.0, pi * 0.0, pi * 1.5), green);
            center
        }
        Cursor::Sell => {
            // Coin with a dollar sign.
            c.shape(|p| disc(p, mid, 11.0), gold);
            let ink = rgba(0.35, 0.22, 0.02, 1.0);
            let pi = std::f32::consts::PI;
            c.fill(|p| arc(p, v(16.0, 12.5), 3.6, 1.3, pi * 0.25, pi * 1.5), ink);
            c.fill(|p| arc(p, v(16.0, 19.5), 3.6, 1.3, pi * 1.25, pi * 0.5), ink);
            c.fill(|p| seg(p, v(16.0, 6.0), v(16.0, 26.0), 1.0), ink);
            center
        }
        Cursor::Rally => {
            c.shape(|p| seg(p, v(9.0, 4.0), v(9.0, 28.0), 1.3), rgba(0.95, 0.95, 0.95, 1.0));
            c.shape(|p| poly(p, &[v(10.0, 4.0), v(26.0, 9.0), v(10.0, 15.0)]), green);
            (9, 28)
        }
        Cursor::Deny => {
            c.shape(|p| ring(p, mid, 10.0, 1.8).min(seg(p, mid + v(-7.0, -7.0), mid + v(7.0, 7.0), 1.8)), red);
            center
        }
    };
    (c, hotspot)
}

const ALL: [Cursor; 14] = [
    Cursor::Normal,
    Cursor::Select,
    Cursor::Move,
    Cursor::Attack,
    Cursor::AttackMove,
    Cursor::Patrol,
    Cursor::Harvest,
    Cursor::Return,
    Cursor::Pickup,
    Cursor::Airlift,
    Cursor::Repair,
    Cursor::Sell,
    Cursor::Rally,
    Cursor::Deny,
];

fn setup(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let usage = RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD;
    let images = ALL
        .iter()
        .map(|k| {
            let (canvas, hot) = draw(*k);
            (*k, (images.add(rgba_image(N as u32, N as u32, canvas.bytes(), usage)), hot))
        })
        .collect();
    commands.insert_resource(Cursors { images, shown: None });
}

fn apply(view: Res<View>, mut cursors: ResMut<Cursors>, mut commands: Commands, window: Query<Entity, With<PrimaryWindow>>) {
    let want = view.0.as_ref().map_or(Cursor::Normal, |g| g.cursor);
    if cursors.shown == Some(want) {
        return;
    }
    let Ok(win) = window.single() else { return };
    let (handle, hotspot) = cursors.images[&want].clone();
    commands.entity(win).insert(CursorIcon::Custom(CustomCursor::Image(CustomCursorImage { handle, hotspot, ..default() })));
    cursors.shown = Some(want);
}

/// Every cursor side by side (CF_CURSORS=path.png dumps it), for checking the art.
pub fn sheet() -> (u32, u32, Vec<u8>) {
    let (cols, scale) = (ALL.len(), 3usize);
    let (w, h) = (cols * N * scale, N * scale);
    let mut out = vec![0u8; w * h * 4];
    for (i, k) in ALL.iter().enumerate() {
        let (c, hot) = draw(*k);
        for y in 0..h {
            for x in 0..N * scale {
                let mut px = c.0[(y / scale) * N + x / scale];
                // Checker backdrop so outlines and alpha are visible; hotspot in magenta.
                let bg = if ((x / 12) + (y / 12)) % 2 == 0 { 0.55 } else { 0.35 };
                let a = px.w;
                px = (px.truncate() * a + Vec3::splat(bg) * (1.0 - a)).extend(1.0);
                if (x / scale) as u16 == hot.0 && (y / scale) as u16 == hot.1 {
                    px = Vec4::new(1.0, 0.0, 1.0, 1.0);
                }
                let o = (y * w + i * N * scale + x) * 4;
                out[o..o + 4].copy_from_slice(&[px.x, px.y, px.z, px.w].map(|f| (f.clamp(0.0, 1.0) * 255.0) as u8));
            }
        }
    }
    (w as u32, h as u32, out)
}

/// Minimal PNG writer (stored, uncompressed deflate) for the debug sheet.
pub fn write_png(path: &std::path::Path, w: u32, h: u32, rgba: &[u8]) -> std::io::Result<()> {
    fn crc(data: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for b in data {
            c ^= *b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend((data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend(data);
        out.extend(&body);
        out.extend(crc(&body).to_be_bytes());
    }
    let mut raw = Vec::new();
    for y in 0..h as usize {
        raw.push(0u8);
        raw.extend(&rgba[y * w as usize * 4..(y + 1) * w as usize * 4]);
    }
    let mut z = vec![0x78, 0x01];
    for (i, block) in raw.chunks(65535).enumerate() {
        let last = (i + 1) * 65535 >= raw.len();
        z.push(last as u8);
        z.extend((block.len() as u16).to_le_bytes());
        z.extend((!(block.len() as u16)).to_le_bytes());
        z.extend(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for x in &raw {
        a = (a + *x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend(((b << 16) | a).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut ihdr = Vec::new();
    ihdr.extend(w.to_be_bytes());
    ihdr.extend(h.to_be_bytes());
    ihdr.extend([8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    std::fs::write(path, out)
}
