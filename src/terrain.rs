//! Terrain renderer. The map is baked per-pixel into chunk textures: terrain
//! types blend along noisy borders, a height field gives lighting, cliffs cast
//! shadows. Chunks re-bake when their tiles change (ore mined, slabs laid).

use crate::gfx::{fbm, hash2, TH, TW};
use cinderfront::sim::{idx, inb, Terrain, World, MAP_H, MAP_W};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use macroquad::prelude::*;

const CT: i32 = 8;
const NX: i32 = MAP_W / CT;
const NY: i32 = MAP_H / CT;
const PW: usize = (CT as usize) * TW as usize;
const PH: usize = (CT as usize) * TH as usize;
/// Margin baked around each chunk so lighting and shadows can look at neighbours.
const MG: usize = 8;
const SLOPE: f32 = 7.0;

/// The slice of world state the terrain look depends on, copied for the bake threads.
struct Snap {
    terrain: Vec<Terrain>,
    ore: Vec<u16>,
    slab: Vec<bool>,
}

#[derive(Clone, Copy, Default)]
struct S {
    h: f32,
    rm: f32,
    cm: f32,
    om: f32,
    lo: f32,
    hi: f32,
    crack: f32,
    ripple: f32,
}

fn ss(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn ore_level(w: &Snap, i: usize) -> f32 {
    ((w.ore[i] / 150) as f32 * 0.3).clamp(0.3, 1.0)
}

fn sample(w: &Snap, gx: f32, gy: f32) -> S {
    let (u, v) = (gx / TW, gy / TH);
    // Warp the lookup so borders between terrain types wander organically.
    let wu = u + (fbm(u * 1.3, v * 1.3, 11, 3) - 0.5) * 0.9;
    let wv = v + (fbm(u * 1.3 + 40.0, v * 1.3, 23, 3) - 0.5) * 0.9;
    let (fu, fv) = (wu - 0.5, wv - 0.5);
    let (iu, iv) = (fu.floor(), fv.floor());
    let (tx, ty) = (ss(0.0, 1.0, fu - iu), ss(0.0, 1.0, fv - iv));
    let (mut r, mut o, mut c) = (0.0, 0.0, 0.0);
    for (dx, dy, wt) in [(0, 0, (1.0 - tx) * (1.0 - ty)), (1, 0, tx * (1.0 - ty)), (0, 1, (1.0 - tx) * ty), (1, 1, tx * ty)] {
        let i = idx((iu as i32 + dx).clamp(0, MAP_W - 1), (iv as i32 + dy).clamp(0, MAP_H - 1));
        match w.terrain[i] {
            Terrain::Rock => r += wt,
            Terrain::Cliff => c += wt,
            Terrain::Ore => o += wt * ore_level(w, i),
            Terrain::Dust => {}
        }
    }
    let lo = fbm(u * 0.35, v * 0.35, 5, 3);
    let hi = fbm(u * 4.0, v * 4.0, 7, 3);
    let cm = ss(0.40, 0.60, c + (hi - 0.5) * 0.25);
    let rm = ss(0.40, 0.60, r + (hi - 0.5) * 0.3) * (1.0 - cm);
    let om = ss(0.25, 0.7, o + (hi - 0.5) * 0.3);
    let ridged = 1.0 - (2.0 * fbm(u * 2.2, v * 2.2, 31, 3) - 1.0).abs();
    let crack = ss(0.90, 0.98, ridged) * rm;
    let dust = (1.0 - rm - cm).max(0.0);
    let ripple = ((u * 1.5 + v * 6.0 + fbm(u * 0.6, v * 0.6, 9, 2) * 9.0).sin() * 0.5 + 0.5) * dust;
    let h = cm * (0.7 + 0.5 * lo + 0.25 * hi) + rm * (0.10 + 0.05 * hi) - crack * 0.06 + ripple * 0.07 + hi * 0.035;
    S { h, rm, cm, om, lo, hi, crack, ripple }
}

fn bake(w: &Snap, cx: i32, cy: i32) -> Vec<u8> {
    let (bw, bh) = (PW + 2 * MG, PH + 2 * MG);
    let (ox, oy) = ((cx as usize * PW) as f32 - MG as f32, (cy as usize * PH) as f32 - MG as f32);
    let mut buf = vec![S::default(); bw * bh];
    for y in 0..bh {
        for x in 0..bw {
            buf[y * bw + x] = sample(w, ox + x as f32 + 0.5, oy + y as f32 + 0.5);
        }
    }
    let light = vec3(-0.55, -0.65, 0.52).normalize();
    let mut bytes = vec![255u8; PW * PH * 4];
    for y in 0..PH {
        for x in 0..PW {
            let (bx, by) = (x + MG, y + MG);
            let s = buf[by * bw + bx];
            let dhx = buf[by * bw + bx + 1].h - buf[by * bw + bx - 1].h;
            let dhy = buf[(by + 1) * bw + bx].h - buf[(by - 1) * bw + bx].h;
            let n = vec3(-dhx * SLOPE, -dhy * SLOPE, 1.0).normalize();
            let mut lum = 0.28 + 0.98 * n.dot(light).max(0.0);
            // Cast shadow: is something taller standing between us and the sun?
            let up = buf[(by - 5) * bw + bx - 4].h.max(buf[(by - 8) * bw + bx - 8].h);
            let sh = ((up - s.h - 0.12) * 1.6).clamp(0.0, 0.5);

            let (gx, gy) = (cx * PW as i32 + x as i32, cy * PH as i32 + y as i32);
            let grain = hash2(gx, gy, 3) - 0.5;
            let dust = vec3(0.60, 0.45, 0.31).lerp(vec3(0.74, 0.59, 0.41), s.lo) * (0.9 + 0.2 * s.hi + 0.06 * s.ripple);
            let rock = vec3(0.35, 0.33, 0.35).lerp(vec3(0.48, 0.45, 0.43), s.lo) * (0.82 + 0.36 * s.hi) * (1.0 - 0.55 * s.crack);
            let strata = 0.88 + 0.12 * (s.h * 38.0).sin();
            let cliff = vec3(0.30, 0.23, 0.21).lerp(vec3(0.47, 0.37, 0.30), s.hi) * strata;
            let mut col = dust.lerp(rock, s.rm).lerp(cliff, s.cm);
            let glow = s.om * (0.45 + 0.55 * s.hi);
            col = col.lerp(vec3(0.80, 0.34, 0.07), glow * 0.6);

            let (tx, ty) = (gx / TW as i32, gy / TH as i32);
            if inb(tx, ty) && w.slab[idx(tx, ty)] {
                let (lx, ly) = (gx % TW as i32, gy % TH as i32);
                let (u, v) = (gx as f32 / TW, gy as f32 / TH);
                let mut g = 0.60 + (s.hi - 0.5) * 0.10 + (s.lo - 0.5) * 0.06;
                if fbm(u * 1.7, v * 1.7, 77, 2) > 0.60 {
                    g *= 0.92;
                }
                if lx < 2 || ly < 2 {
                    g *= 1.2;
                } else if lx >= TW as i32 - 2 || ly >= TH as i32 - 2 {
                    g *= 0.66;
                }
                for (px, py) in [(6, 5), (33, 5), (6, 24), (33, 24)] {
                    if (lx - px).abs() <= 1 && (ly - py).abs() <= 1 {
                        g *= 0.62;
                    }
                }
                col = vec3(g, g, g * 1.03);
                lum = 0.92;
            }
            lum *= 1.0 - sh;
            let out = (col * lum * (1.0 + grain * 0.07) + vec3(0.28, 0.09, 0.0) * glow * 0.5).clamp(Vec3::ZERO, Vec3::ONE);
            let i = (y * PW + x) * 4;
            bytes[i] = (out.x * 255.0) as u8;
            bytes[i + 1] = (out.y * 255.0) as u8;
            bytes[i + 2] = (out.z * 255.0) as u8;
        }
    }
    bytes
}

/// Everything in and around a chunk that affects how it looks.
fn signature(w: &Snap, cx: i32, cy: i32) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for y in cy * CT - 1..=cy * CT + CT {
        for x in cx * CT - 1..=cx * CT + CT {
            if inb(x, y) {
                let i = idx(x, y);
                let v = w.terrain[i] as u64 | (w.slab[i] as u64) << 4 | ((w.ore[i] / 150) as u64) << 8;
                h = (h ^ v).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }
    h
}

type Job = (i32, i32, Arc<Snap>);
type Done = (i32, i32, u64, Vec<u8>);

pub struct TerrainGfx {
    tex: Vec<Option<Texture2D>>,
    /// Signature each chunk texture was baked from (0 = never baked).
    sig: Vec<u64>,
    pending: Vec<bool>,
    in_flight: usize,
    timer: f32,
    jobs: Sender<Job>,
    done: Receiver<Done>,
}

impl TerrainGfx {
    pub fn new() -> TerrainGfx {
        let n = (NX * NY) as usize;
        let (jobs, job_rx) = channel::<Job>();
        let (done_tx, done) = channel::<Done>();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let workers = std::thread::available_parallelism().map_or(2, |p| p.get().saturating_sub(1).clamp(1, 4));
        for _ in 0..workers {
            let (job_rx, done_tx) = (job_rx.clone(), done_tx.clone());
            std::thread::spawn(move || loop {
                let job = job_rx.lock().unwrap().recv();
                let Ok((cx, cy, snap)) = job else { break };
                let out = (cx, cy, signature(&snap, cx, cy), bake(&snap, cx, cy));
                if done_tx.send(out).is_err() {
                    break;
                }
            });
        }
        TerrainGfx { tex: vec![None; n], sig: vec![0; n], pending: vec![false; n], in_flight: 0, timer: 0.0, jobs, done }
    }

    fn accept(&mut self, (cx, cy, sig, bytes): Done) {
        let i = (cy * NX + cx) as usize;
        let img = Image { bytes, width: PW as u16, height: PH as u16 };
        match &self.tex[i] {
            Some(t) => t.update(&img),
            None => {
                let t = Texture2D::from_image(&img);
                t.set_filter(FilterMode::Linear);
                self.tex[i] = Some(t);
            }
        }
        self.sig[i] = sig;
        self.pending[i] = false;
        self.in_flight -= 1;
    }

    pub fn draw(&mut self, w: &World, cam: Vec2, zoom: f32, vw: f32, vh: f32, dt: f32) {
        let x0 = ((cam.x / PW as f32).floor() as i32).max(0);
        let y0 = (((cam.y - 20.0) / PH as f32).floor() as i32).max(0);
        let x1 = (((cam.x + vw / zoom) / PW as f32).floor() as i32).min(NX - 1);
        let y1 = (((cam.y + vh / zoom) / PH as f32).floor() as i32).min(NY - 1);
        let visible = |i: usize| {
            let (cx, cy) = (i as i32 % NX, i as i32 / NX);
            cx >= x0 && cx <= x1 && cy >= y0 && cy <= y1
        };

        while let Ok(d) = self.done.try_recv() {
            self.accept(d);
        }

        // Queue stale chunks for the bake threads: what is on screen first, then the
        // rest of the map in the background. Checked every frame while anything on
        // screen is missing, otherwise a few times a second.
        self.timer -= dt;
        let missing = (0..self.tex.len()).any(|i| visible(i) && self.tex[i].is_none());
        if missing || self.timer <= 0.0 {
            self.timer = 0.2;
            let snap = Snap { terrain: w.terrain.clone(), ore: w.ore.clone(), slab: w.slab.clone() };
            let mut order: Vec<usize> = (0..self.tex.len()).filter(|i| !self.pending[*i]).collect();
            order.sort_by_key(|i| !visible(*i));
            let stale: Vec<usize> =
                order.into_iter().filter(|i| self.sig[*i] != signature(&snap, *i as i32 % NX, *i as i32 / NX)).collect();
            let snap = Arc::new(snap);
            for i in stale {
                if self.in_flight >= 8 && !(visible(i) && self.tex[i].is_none()) {
                    break;
                }
                self.pending[i] = true;
                self.in_flight += 1;
                let _ = self.jobs.send((i as i32 % NX, i as i32 / NX, snap.clone()));
            }
        }
        // Never show an unbaked hole: on the first frame (or after a camera jump) wait for it.
        while (0..self.tex.len()).any(|i| visible(i) && self.tex[i].is_none() && self.pending[i]) {
            match self.done.recv() {
                Ok(d) => self.accept(d),
                Err(_) => break,
            }
        }

        for cy in y0..=y1 {
            for cx in x0..=x1 {
                let x = ((cx as usize * PW) as f32 - cam.x) * zoom;
                let y = ((cy as usize * PH) as f32 - cam.y) * zoom;
                let size = vec2(PW as f32 * zoom + 0.6, PH as f32 * zoom + 0.6);
                match &self.tex[(cy * NX + cx) as usize] {
                    Some(t) => {
                        draw_texture_ex(t, x, y, WHITE, DrawTextureParams { dest_size: Some(size), ..Default::default() })
                    }
                    None => draw_rectangle(x, y, size.x, size.y, Color::new(0.52, 0.41, 0.30, 1.0)),
                }
            }
        }
    }
}
