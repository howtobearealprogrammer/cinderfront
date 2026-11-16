//! Terrain renderer. The map is split into chunks; each chunk gets a baked colour
//! texture (terrain types blend along noisy borders, fine relief is baked in as
//! lighting) on a mesh whose vertices rise where there are cliffs, so cliffs are
//! real geometry that catches the sun and casts shadows. Fog of war is written
//! into the mesh vertex colours. Chunks re-bake on worker threads when their
//! tiles change (ore mined, slabs laid).

use crate::game::Game;
use crate::gfx::{fbm, hash2, rgba_image};
use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::platform::cell::SyncCell;
use bevy::prelude::*;
use cinderfront::sim::{idx, inb, Terrain, World, MAP_H, MAP_W};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

const CT: i32 = 8;
const NX: i32 = MAP_W / CT;
const NY: i32 = MAP_H / CT;
/// Texture pixels per tile.
const TPX: usize = 32;
const PW: usize = CT as usize * TPX;
/// Mesh subdivisions per tile.
const SUB: usize = 4;
const VN: usize = CT as usize * SUB + 1;
/// Margin baked around each chunk so lighting can look at neighbours.
const MG: usize = 2;
const SLOPE: f32 = 7.0;
/// World height of a full cliff, in tiles.
pub const CLIFF_H: f32 = 1.15;

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

impl S {
    /// The part of the height that is modelled as geometry.
    fn macro_h(&self) -> f32 {
        self.cm * (0.7 + 0.5 * self.lo)
    }
}

fn ss(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn ore_level(w: &Snap, i: usize) -> f32 {
    ((w.ore[i] / 150) as f32 * 0.3).clamp(0.3, 1.0)
}

/// `u`, `v` in tiles.
fn sample(w: &Snap, u: f32, v: f32) -> S {
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

struct Baked {
    cx: i32,
    cy: i32,
    sig: u64,
    bytes: Vec<u8>,
    heights: Vec<f32>,
}

fn bake(w: &Snap, cx: i32, cy: i32) -> Baked {
    let bw = PW + 2 * MG;
    let (ox, oy) = ((cx as usize * PW) as f32 - MG as f32, (cy as usize * PW) as f32 - MG as f32);
    let mut buf = vec![S::default(); bw * bw];
    for y in 0..bw {
        for x in 0..bw {
            buf[y * bw + x] = sample(w, (ox + x as f32 + 0.5) / TPX as f32, (oy + y as f32 + 0.5) / TPX as f32);
        }
    }
    let fine = |s: &S| s.h - s.macro_h();
    let light = Vec3::new(-0.55, -0.65, 0.52).normalize();
    let mut bytes = vec![255u8; PW * PW * 4];
    let px_scale = 40.0 / TPX as f32;
    for y in 0..PW {
        for x in 0..PW {
            let (bx, by) = (x + MG, y + MG);
            let s = buf[by * bw + bx];
            let dhx = (fine(&buf[by * bw + bx + 1]) - fine(&buf[by * bw + bx - 1])) * px_scale;
            let dhy = (fine(&buf[(by + 1) * bw + bx]) - fine(&buf[(by - 1) * bw + bx])) * px_scale;
            let n = Vec3::new(-dhx * SLOPE, -dhy * SLOPE, 1.0).normalize();
            // Real sun lighting handles the big shapes; bake only the fine relief.
            let mut lum = (1.0 + 0.9 * (n.dot(light) - light.z)).clamp(0.55, 1.35);

            let (gx, gy) = (cx * PW as i32 + x as i32, cy * PW as i32 + y as i32);
            let grain = hash2(gx, gy, 3) - 0.5;
            let dust = Vec3::new(0.60, 0.45, 0.31).lerp(Vec3::new(0.74, 0.59, 0.41), s.lo) * (0.9 + 0.2 * s.hi + 0.06 * s.ripple);
            let rock =
                Vec3::new(0.35, 0.33, 0.35).lerp(Vec3::new(0.48, 0.45, 0.43), s.lo) * (0.82 + 0.36 * s.hi) * (1.0 - 0.55 * s.crack);
            let strata = 0.88 + 0.12 * (s.h * 38.0).sin();
            let cliff = Vec3::new(0.30, 0.23, 0.21).lerp(Vec3::new(0.47, 0.37, 0.30), s.hi) * strata;
            let mut col = dust.lerp(rock, s.rm).lerp(cliff, s.cm);
            let glow = s.om * (0.45 + 0.55 * s.hi);
            col = col.lerp(Vec3::new(0.80, 0.34, 0.07), glow * 0.6);

            let (tx, ty) = (gx / TPX as i32, gy / TPX as i32);
            if inb(tx, ty) && w.slab[idx(tx, ty)] {
                let (lx, ly) = ((gx % TPX as i32) as f32 / TPX as f32, (gy % TPX as i32) as f32 / TPX as f32);
                let (u, v) = (gx as f32 / TPX as f32, gy as f32 / TPX as f32);
                let mut g = 0.60 + (s.hi - 0.5) * 0.10 + (s.lo - 0.5) * 0.06;
                if fbm(u * 1.7, v * 1.7, 77, 2) > 0.60 {
                    g *= 0.92;
                }
                let edge = 2.0 / TPX as f32;
                if lx < edge || ly < edge {
                    g *= 1.2;
                } else if lx >= 1.0 - edge || ly >= 1.0 - edge {
                    g *= 0.66;
                }
                for (px, py) in [(0.16, 0.16), (0.84, 0.16), (0.16, 0.84), (0.84, 0.84)] {
                    if (lx - px).abs() <= 1.5 / TPX as f32 && (ly - py).abs() <= 1.5 / TPX as f32 {
                        g *= 0.62;
                    }
                }
                col = Vec3::new(g, g, g * 1.03);
                lum = 1.0;
            }
            let out = (col * lum * (1.0 + grain * 0.07) + Vec3::new(0.28, 0.09, 0.0) * glow * 0.5).clamp(Vec3::ZERO, Vec3::ONE);
            let i = (y * PW + x) * 4;
            bytes[i] = (out.x * 255.0) as u8;
            bytes[i + 1] = (out.y * 255.0) as u8;
            bytes[i + 2] = (out.z * 255.0) as u8;
        }
    }
    let mut heights = vec![0.0; VN * VN];
    for vy in 0..VN {
        for vx in 0..VN {
            let u = (cx * CT) as f32 + vx as f32 / SUB as f32;
            let v = (cy * CT) as f32 + vy as f32 / SUB as f32;
            heights[vy * VN + vx] = sample(w, u, v).macro_h() * CLIFF_H;
        }
    }
    Baked { cx, cy, sig: signature(w, cx, cy), bytes, heights }
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

/// Per-tile fog brightness: 1 visible, 0.45 explored, 0 never seen.
fn fog_level(g: &Game, x: i32, y: i32) -> f32 {
    let (x, y) = (x.clamp(0, MAP_W - 1), y.clamp(0, MAP_H - 1));
    let i = idx(x, y);
    if g.visible[i] {
        1.0
    } else if g.explored[i] {
        0.45
    } else {
        0.0
    }
}

fn chunk_colors(g: &Game, cx: i32, cy: i32) -> Vec<[f32; 4]> {
    let mut out = Vec::with_capacity(VN * VN);
    for vy in 0..VN {
        for vx in 0..VN {
            // Bilinear between tile centres so the fog edge is soft.
            let u = (cx * CT) as f32 + vx as f32 / SUB as f32 - 0.5;
            let v = (cy * CT) as f32 + vy as f32 / SUB as f32 - 0.5;
            let (x0, y0) = (u.floor(), v.floor());
            let (fx, fy) = (u - x0, v - y0);
            let (x0, y0) = (x0 as i32, y0 as i32);
            let top = fog_level(g, x0, y0) * (1.0 - fx) + fog_level(g, x0 + 1, y0) * fx;
            let bot = fog_level(g, x0, y0 + 1) * (1.0 - fx) + fog_level(g, x0 + 1, y0 + 1) * fx;
            let f = top * (1.0 - fy) + bot * fy;
            out.push([f, f, f, 1.0]);
        }
    }
    out
}

fn chunk_mesh(b: &Baked, colors: Vec<[f32; 4]>) -> Mesh {
    let h = |x: usize, y: usize| b.heights[y.min(VN - 1) * VN + x.min(VN - 1)];
    let step = 1.0 / SUB as f32;
    let mut pos = Vec::with_capacity(VN * VN);
    let mut nrm = Vec::with_capacity(VN * VN);
    let mut uv = Vec::with_capacity(VN * VN);
    for y in 0..VN {
        for x in 0..VN {
            pos.push([(b.cx * CT) as f32 + x as f32 * step, h(x, y), (b.cy * CT) as f32 + y as f32 * step]);
            let dx = (h(x + 1, y) - h(x.saturating_sub(1), y)) / (2.0 * step);
            let dz = (h(x, y + 1) - h(x, y.saturating_sub(1))) / (2.0 * step);
            nrm.push(Vec3::new(-dx, 1.0, -dz).normalize().to_array());
            uv.push([x as f32 / (VN - 1) as f32, y as f32 / (VN - 1) as f32]);
        }
    }
    let mut ind = Vec::with_capacity((VN - 1) * (VN - 1) * 6);
    for y in 0..VN - 1 {
        for x in 0..VN - 1 {
            let i = (y * VN + x) as u32;
            let (r, d) = (i + 1, i + VN as u32);
            ind.extend_from_slice(&[i, d, r, r, d, d + 1]);
        }
    }
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD)
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, nrm)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uv)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors)
        .with_inserted_indices(Indices::U32(ind))
}

type Job = (i32, i32, Arc<Snap>);

struct Chunk {
    entity: Entity,
    mesh: Handle<Mesh>,
    image: Handle<Image>,
}

#[derive(Resource)]
pub struct TerrainView {
    chunks: Vec<Option<Chunk>>,
    /// Signature each chunk was baked from (0 = never baked).
    sig: Vec<u64>,
    pending: Vec<bool>,
    in_flight: usize,
    timer: f32,
    jobs: SyncCell<Sender<Job>>,
    done: SyncCell<Receiver<Baked>>,
    fog_gen: Option<u32>,
    /// Fog level per tile as last written to the meshes.
    fog_seen: Vec<f32>,
}

impl TerrainView {
    pub fn new() -> TerrainView {
        let n = (NX * NY) as usize;
        let (jobs, job_rx) = channel::<Job>();
        let (done_tx, done) = channel::<Baked>();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let workers = std::thread::available_parallelism().map_or(2, |p| p.get().saturating_sub(1).clamp(1, 4));
        for _ in 0..workers {
            let (job_rx, done_tx) = (job_rx.clone(), done_tx.clone());
            std::thread::spawn(move || loop {
                let job = job_rx.lock().unwrap().recv();
                let Ok((cx, cy, snap)) = job else { break };
                if done_tx.send(bake(&snap, cx, cy)).is_err() {
                    break;
                }
            });
        }
        TerrainView {
            chunks: (0..n).map(|_| None).collect(),
            sig: vec![0; n],
            pending: vec![false; n],
            in_flight: 0,
            timer: 0.0,
            jobs: SyncCell::new(jobs),
            done: SyncCell::new(done),
            fog_gen: None,
            fog_seen: vec![-1.0; (MAP_W * MAP_H) as usize],
        }
    }

    /// Despawn every chunk (leaving a game).
    pub fn clear(&mut self, commands: &mut Commands) {
        for c in self.chunks.iter_mut().filter_map(Option::take) {
            commands.entity(c.entity).despawn();
        }
        self.sig.iter_mut().for_each(|s| *s = 0);
        self.fog_gen = None;
        self.fog_seen.iter_mut().for_each(|f| *f = -1.0);
    }
}

#[allow(clippy::too_many_arguments)]
fn accept(
    tv: &mut TerrainView,
    mut b: Baked,
    g: &Game,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    images: &mut Assets<Image>,
    materials: &mut Assets<StandardMaterial>,
) {
    let i = (b.cy * NX + b.cx) as usize;
    let img = rgba_image(PW as u32, PW as u32, std::mem::take(&mut b.bytes), RenderAssetUsages::RENDER_WORLD);
    let mesh = chunk_mesh(&b, chunk_colors(g, b.cx, b.cy));
    match &tv.chunks[i] {
        Some(c) => {
            let _ = images.insert(&c.image, img);
            let _ = meshes.insert(&c.mesh, mesh);
        }
        None => {
            let image = images.add(img);
            let mesh = meshes.add(mesh);
            let mat = materials.add(StandardMaterial {
                base_color_texture: Some(image.clone()),
                // The bake already holds the terrain's brightness; lift it back to
                // where it was meant to be under the (gentler) model lighting.
                base_color: Color::LinearRgba(LinearRgba::rgb(1.95, 1.95, 1.95)),
                perceptual_roughness: 0.95,
                reflectance: 0.2,
                ..default()
            });
            let mut e = commands.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(mat), Transform::IDENTITY));
            // Flat ground cannot shadow anything; only chunks with cliffs go into the shadow map.
            if b.heights.iter().all(|h| *h < 0.02) {
                e.insert(bevy::light::NotShadowCaster);
            }
            let entity = e.id();
            tv.chunks[i] = Some(Chunk { entity, mesh, image });
        }
    }
    tv.sig[i] = b.sig;
    tv.pending[i] = false;
    tv.in_flight -= 1;
}

#[allow(clippy::too_many_arguments)]
pub fn update(
    tv: &mut TerrainView,
    g: &Game,
    dt: f32,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    images: &mut Assets<Image>,
    materials: &mut Assets<StandardMaterial>,
) {
    let w: &World = &g.world;
    let view = g.view_ground();
    let (x0, y0) = (((view.min.x - 1.0) / CT as f32).floor() as i32, ((view.min.y - 2.0) / CT as f32).floor() as i32);
    let (x1, y1) = (((view.max.x + 1.0) / CT as f32).floor() as i32, ((view.max.y + 1.0) / CT as f32).floor() as i32);
    let visible = |i: usize| {
        let (cx, cy) = (i as i32 % NX, i as i32 / NX);
        cx >= x0 && cx <= x1 && cy >= y0 && cy <= y1
    };

    while let Ok(b) = tv.done.get().try_recv() {
        accept(tv, b, g, commands, meshes, images, materials);
    }

    // Queue stale chunks for the bake threads: what is on screen first, then the
    // rest of the map in the background.
    tv.timer -= dt;
    let missing = (0..tv.chunks.len()).any(|i| visible(i) && tv.chunks[i].is_none() && !tv.pending[i]);
    if missing || tv.timer <= 0.0 {
        tv.timer = 0.2;
        let snap = Snap { terrain: w.terrain.clone(), ore: w.ore.clone(), slab: w.slab.clone() };
        let mut order: Vec<usize> = (0..tv.chunks.len()).filter(|i| !tv.pending[*i]).collect();
        order.sort_by_key(|i| !visible(*i));
        let stale: Vec<usize> =
            order.into_iter().filter(|i| tv.sig[*i] != signature(&snap, *i as i32 % NX, *i as i32 / NX)).collect();
        let snap = Arc::new(snap);
        for i in stale {
            if tv.in_flight >= 8 && !(visible(i) && tv.chunks[i].is_none()) {
                break;
            }
            tv.pending[i] = true;
            tv.in_flight += 1;
            let _ = tv.jobs.get().send((i as i32 % NX, i as i32 / NX, snap.clone()));
        }
    }
    // Never show an unbaked hole on screen: wait for those.
    while (0..tv.chunks.len()).any(|i| visible(i) && tv.chunks[i].is_none() && tv.pending[i]) {
        match tv.done.get().recv() {
            Ok(b) => accept(tv, b, g, commands, meshes, images, materials),
            Err(_) => break,
        }
    }

    // Fog: rewrite vertex colours of chunks whose tiles (or neighbours) changed.
    if tv.fog_gen != Some(g.fog_gen) {
        tv.fog_gen = Some(g.fog_gen);
        let mut dirty = vec![false; tv.chunks.len()];
        for y in 0..MAP_H {
            for x in 0..MAP_W {
                let f = fog_level(g, x, y);
                let i = idx(x, y);
                if tv.fog_seen[i] != f {
                    tv.fog_seen[i] = f;
                    for (dx, dy) in [(-1, -1), (1, -1), (-1, 1), (1, 1), (0, 0)] {
                        let (cx, cy) = ((x + dx).clamp(0, MAP_W - 1) / CT, (y + dy).clamp(0, MAP_H - 1) / CT);
                        dirty[(cy * NX + cx) as usize] = true;
                    }
                }
            }
        }
        for (i, c) in tv.chunks.iter().enumerate() {
            if let (true, Some(c)) = (dirty[i], c) {
                if let Some(mut m) = meshes.get_mut(&c.mesh) {
                    m.insert_attribute(Mesh::ATTRIBUTE_COLOR, chunk_colors(g, i as i32 % NX, i as i32 / NX));
                }
            }
        }
    }
}
