//! Client side: camera, input, sidebar and rendering. Never mutates the sim
//! directly - everything the player does leaves as a `Command`.

use crate::gfx::{self, ellipse, ellipse_lines, fade, hash2, mix, rgba, shade, TH, TW, YS};
use crate::terrain::TerrainGfx;
use cinderfront::sim::{
    center, idx, inb, Command, Event, StructKind, Structure, Target, Terrain, Unit, UnitKind, World, HARVEST_CAP, MAP_H,
    MAP_W, TILE, TRAIN_AT,
};
use macroquad::prelude::*;
use macroquad::rand::gen_range;
use std::collections::HashMap;

const SIDE: f32 = 220.0;
const AIR_Z: f32 = 44.0;
const MM: f32 = 200.0;
const TOOL_Y: f32 = 264.0;
const ICON_Y: f32 = 290.0;
const ICON_H: f32 = 41.0;
const ICON_PITCH: f32 = 44.0;
/// Direction towards the sun on the ground plane (top-left).
const SUN: Vec2 = Vec2::new(-0.65, -0.76);

pub const PCOL: [Color; 4] = [
    Color::new(0.22, 0.52, 0.98, 1.0),
    Color::new(0.93, 0.25, 0.20, 1.0),
    Color::new(0.28, 0.80, 0.36, 1.0),
    Color::new(0.95, 0.80, 0.22, 1.0),
];
const GLASS: Color = Color::new(0.35, 0.78, 0.95, 1.0);
const STEEL: Color = Color::new(0.16, 0.16, 0.18, 1.0);

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Normal,
    AttackMove,
    Airlift,
    Repair,
    Sell,
}

enum Fx {
    Tracer { a: Vec2, az: f32, b: Vec2, bz: f32, t: f32 },
    Shell { a: Vec2, az: f32, b: Vec2, bz: f32, t: f32, life: f32 },
    Ring { at: Vec2, z: f32, t: f32, life: f32, r: f32 },
}

/// One particle. Positions are ground-plane pixels plus a height.
struct Part {
    p: Vec2,
    z: f32,
    v: Vec2,
    vz: f32,
    grav: f32,
    age: f32,
    life: f32,
    s0: f32,
    s1: f32,
    c0: Color,
    c1: Color,
    smoke: bool,
    rot: f32,
}

pub struct Game {
    pub world: World,
    pub me: u8,
    cam: Vec2,
    zoom: f32,
    alpha: f32,
    time: f32,
    sel: Vec<u32>,
    sel_struct: Option<u32>,
    drag: Option<Vec2>,
    last_mouse: Vec2,
    placing: Option<StructKind>,
    mode: Mode,
    fx: Vec<Fx>,
    parts: Vec<Part>,
    decals: Vec<(Vec2, f32, f32)>,
    shake: f32,
    shown_credits: f32,
    groups: Vec<Vec<u32>>,
    angles: HashMap<u32, f32>,
    terrain: TerrainGfx,
    /// Fog of war, per tile. Purely a view filter: the sim knows nothing about it.
    visible: Vec<bool>,
    explored: Vec<bool>,
    /// Enemy structures as last seen.
    ghosts: HashMap<u32, Structure>,
    fog_img: Image,
    fog_tex: Texture2D,
    mm_img: Image,
    mm_tex: Texture2D,
    mm_timer: f32,
}

fn fpos(p: (i32, i32)) -> Vec2 {
    vec2(p.0 as f32, p.1 as f32) * (TW / TILE as f32)
}
fn rdir() -> Vec2 {
    let a = gen_range(0.0, 6.2832f32);
    vec2(a.cos(), a.sin())
}

fn hp_bar(c: Vec2, w: f32, frac: f32) {
    let frac = frac.clamp(0.0, 1.0);
    let col = if frac > 0.5 {
        rgba(0.3, 0.95, 0.35, 1.0)
    } else if frac > 0.25 {
        rgba(1.0, 0.8, 0.15, 1.0)
    } else {
        rgba(1.0, 0.25, 0.15, 1.0)
    };
    let (x, y) = ((c.x - w / 2.0).round(), c.y.round());
    draw_rectangle(x - 1.0, y - 1.0, w + 2.0, 6.0, rgba(0.0, 0.0, 0.0, 0.8));
    gfx::vgrad(x, y, w * frac, 4.0, shade(col, 1.15), shade(col, 0.6));
    let mut tx = 6.0;
    while tx < w {
        draw_line(x + tx, y, x + tx, y + 4.0, 1.0, rgba(0.0, 0.0, 0.0, 0.55));
        tx += 6.0;
    }
}

impl Game {
    pub fn new(world: World, me: u8) -> Game {
        let mm_img = Image::gen_image_color(MAP_W as u16, MAP_H as u16, BLACK);
        let mm_tex = Texture2D::from_image(&mm_img);
        mm_tex.set_filter(FilterMode::Linear);
        let fog_img = Image::gen_image_color(MAP_W as u16, MAP_H as u16, BLACK);
        let fog_tex = Texture2D::from_image(&fog_img);
        fog_tex.set_filter(FilterMode::Linear);
        let tiles = (MAP_W * MAP_H) as usize;
        let credits = world.players[me as usize].credits as f32;
        let mut g = Game {
            world,
            me,
            cam: Vec2::ZERO,
            zoom: 1.0,
            alpha: 0.0,
            time: 0.0,
            sel: Vec::new(),
            sel_struct: None,
            drag: None,
            last_mouse: Vec2::ZERO,
            placing: None,
            mode: Mode::Normal,
            fx: Vec::new(),
            parts: Vec::new(),
            decals: Vec::new(),
            shake: 0.0,
            shown_credits: credits,
            groups: vec![Vec::new(); 9],
            angles: HashMap::new(),
            terrain: TerrainGfx::new(),
            visible: vec![false; tiles],
            explored: vec![false; tiles],
            ghosts: HashMap::new(),
            fog_img,
            fog_tex,
            mm_img,
            mm_tex,
            mm_timer: 0.0,
        };
        g.center_on(g.world.starts[me as usize]);
        g.update_vis();
        g
    }

    // ---------- fog of war ----------

    fn unit_visible(&self, u: &Unit) -> bool {
        u.owner == self.me || self.visible[idx(u.tile.0, u.tile.1)]
    }

    fn struct_visible(&self, s: &Structure) -> bool {
        let d = s.kind.def();
        s.owner == self.me || (s.y..s.y + d.h).any(|y| (s.x..s.x + d.w).any(|x| self.visible[idx(x, y)]))
    }

    fn pos_visible(&self, p: (i32, i32)) -> bool {
        let (x, y) = (p.0.div_euclid(TILE), p.1.div_euclid(TILE));
        inb(x, y) && self.visible[idx(x, y)]
    }

    /// Recompute what this player can see right now. Runs after every sim tick.
    fn update_vis(&mut self) {
        let me = self.me;
        let all = self.world.players[me as usize].defeated || self.world.winner.is_some();
        self.visible.iter_mut().for_each(|v| *v = all);
        let mut eyes: Vec<((i32, i32), i32)> = Vec::new();
        for u in self.world.units.values().filter(|u| u.owner == me) {
            eyes.push((u.tile, u.kind.sight()));
        }
        for s in self.world.structs.values().filter(|s| s.owner == me) {
            let d = s.kind.def();
            eyes.push(((s.x + d.w / 2, s.y + d.h / 2), s.kind.sight()));
        }
        for ((cx, cy), r) in eyes {
            for dy in -r..=r {
                for dx in -r..=r {
                    if dx * dx + dy * dy <= r * r + r && inb(cx + dx, cy + dy) {
                        self.visible[idx(cx + dx, cy + dy)] = true;
                    }
                }
            }
        }
        for y in 0..MAP_H {
            for x in 0..MAP_W {
                let i = idx(x, y);
                self.explored[i] |= self.visible[i];
                let a = if self.visible[i] { 0.0 } else if self.explored[i] { 0.5 } else { 1.0 };
                self.fog_img.set_pixel(x as u32, y as u32, rgba(0.0, 0.0, 0.0, a));
            }
        }
        self.fog_tex.update(&self.fog_img);

        // Remember enemy buildings we can see; forget destroyed ones once we look again.
        let seen: Vec<Structure> =
            self.world.structs.values().filter(|s| s.owner != me && self.struct_visible(s)).cloned().collect();
        for s in seen {
            self.ghosts.insert(s.id, s);
        }
        let Game { ghosts, world, visible, .. } = self;
        ghosts.retain(|id, g| {
            let d = g.kind.def();
            world.structs.contains_key(id) || !(g.y..g.y + d.h).any(|y| (g.x..g.x + d.w).any(|x| visible[idx(x, y)]))
        });
    }

    // ---------- effects ----------

    #[allow(clippy::too_many_arguments)]
    fn part(&mut self, p: Vec2, z: f32, v: Vec2, vz: f32, life: f32, s0: f32, s1: f32, c0: Color, c1: Color, smoke: bool) {
        if self.parts.len() < 2500 {
            let rot = gen_range(0.0, 6.28);
            self.parts.push(Part { p, z, v, vz, grav: 0.0, age: 0.0, life, s0, s1, c0, c1, smoke, rot });
        }
    }

    fn explode(&mut self, at: Vec2, z: f32, scale: f32) {
        let n = |k: f32| (k * scale).ceil() as i32;
        let sz = scale.sqrt();
        let (hot, ember) = (rgba(1.0, 0.95, 0.75, 1.0), rgba(1.0, 0.6, 0.2, 0.0));
        self.part(at, z + 4.0, Vec2::ZERO, 0.0, 0.14, 26.0 * sz, 44.0 * sz, hot, ember, false);
        for _ in 0..n(6.0) {
            let v = rdir() * gen_range(10.0, 55.0) * sz;
            let life = gen_range(0.3, 0.65);
            let (c0, c1) = (rgba(1.0, 0.78, 0.3, 0.95), rgba(0.75, 0.15, 0.03, 0.0));
            self.part(at + v * 0.1, z + 5.0, v, gen_range(10.0, 40.0), life, 9.0 * sz, 20.0 * sz, c0, c1, false);
        }
        for _ in 0..n(5.0) {
            let v = rdir() * gen_range(4.0, 22.0) * sz;
            let life = gen_range(0.9, 2.0);
            let g = gen_range(0.1, 0.22);
            let (c0, c1) = (rgba(g, g, g, 0.75), rgba(0.3, 0.3, 0.3, 0.0));
            self.part(at + v * 0.3, z + 6.0, v, gen_range(14.0, 34.0), life, 10.0 * sz, 30.0 * sz, c0, c1, true);
        }
        for _ in 0..n(7.0) {
            let v = rdir() * gen_range(60.0, 170.0);
            let (c0, c1) = (rgba(1.0, 0.85, 0.4, 1.0), rgba(1.0, 0.4, 0.1, 0.0));
            self.part(at, z + 5.0, v, gen_range(60.0, 150.0), gen_range(0.35, 0.7), 3.0, 1.5, c0, c1, false);
            if let Some(p) = self.parts.last_mut() {
                p.grav = 320.0;
            }
        }
        self.fx.push(Fx::Ring { at, z, t: 0.0, life: 0.3 * sz, r: 30.0 * scale });
        if z < 20.0 && scale >= 0.9 {
            self.decals.push((at, 15.0 * sz, 0.0));
            if self.decals.len() > 250 {
                self.decals.remove(0);
            }
        }
        if scale >= 3.0 {
            self.shake = 7.0;
        }
    }

    /// Collect render effects from the tick that just ran.
    pub fn after_tick(&mut self) {
        self.update_vis();
        for e in std::mem::take(&mut self.world.events) {
            let seen = match e {
                Event::Shot { from, to, .. } => self.pos_visible(from) || self.pos_visible(to),
                Event::Boom { pos, .. } => self.pos_visible(pos),
            };
            if !seen {
                continue;
            }
            let z = |air: bool| if air { AIR_Z } else { 9.0 };
            match e {
                Event::Shot { from, to, from_air, to_air, heavy } => {
                    let (a, b, az, bz) = (fpos(from), fpos(to), z(from_air), z(to_air));
                    let dir = (b - a).normalize_or_zero();
                    let muzzle = a + dir * 12.0;
                    let (hot, ember) = (rgba(1.0, 0.93, 0.6, 1.0), rgba(1.0, 0.55, 0.15, 0.0));
                    if heavy {
                        let life = (a.distance(b) / 650.0).clamp(0.08, 0.4);
                        self.fx.push(Fx::Shell { a: muzzle, az, b, bz, t: 0.0, life });
                        self.part(muzzle, az, Vec2::ZERO, 0.0, 0.1, 10.0, 20.0, hot, ember, false);
                        let grey = rgba(0.6, 0.6, 0.6, 0.5);
                        self.part(muzzle, az, dir * 20.0, 8.0, 0.5, 5.0, 14.0, grey, fade(grey, 0.0), true);
                    } else {
                        self.fx.push(Fx::Tracer { a: muzzle, az, b, bz, t: 0.0 });
                        self.part(muzzle, az, Vec2::ZERO, 0.0, 0.06, 6.0, 10.0, hot, ember, false);
                        let hit = b + rdir() * gen_range(0.0, 5.0);
                        self.part(hit, bz, Vec2::ZERO, 0.0, 0.1, 3.0, 7.0, fade(hot, 0.9), ember, false);
                    }
                }
                Event::Boom { pos, big, air } => self.explode(fpos(pos), z(air), if big { 3.2 } else { 1.0 }),
            }
        }
    }

    fn on_screen(&self, p: Vec2) -> bool {
        let s = self.proj(p, 0.0);
        s.x > -60.0 && s.y > -60.0 && s.x < self.view_w() + 60.0 && s.y < screen_height() + 60.0
    }

    /// Smoke, dust and other ambience that is not driven by sim events.
    fn ambient(&mut self, dt: f32) {
        let mut spawn: Vec<(Vec2, f32, u8)> = Vec::new();
        for u in self.world.units.values() {
            let on_foot = matches!(u.kind, UnitKind::Trooper | UnitKind::Rocketeer);
            if u.carried || u.pos == u.prev || u.kind.def().air || on_foot {
                continue;
            }
            if !self.unit_visible(u) {
                continue;
            }
            let at = self.upos(u);
            let loose = self.world.terrain[idx(u.tile.0, u.tile.1)] != Terrain::Rock;
            if loose && self.on_screen(at) && gen_range(0.0, 1.0) < dt * 14.0 {
                spawn.push((at, 2.0, 0));
            }
        }
        for s in self.world.structs.values() {
            let d = s.kind.def();
            let mid = vec2((s.x as f32 + d.w as f32 / 2.0) * TW, (s.y as f32 + d.h as f32 / 2.0) * TW);
            if !self.on_screen(mid) || !self.struct_visible(s) {
                continue;
            }
            let frac = s.hp as f32 / d.hp as f32;
            if s.kind != StructKind::Wall && frac < 0.5 && gen_range(0.0, 1.0) < dt * (9.0 - 12.0 * frac) {
                let off = vec2(gen_range(-0.3, 0.3) * d.w as f32 * TW, gen_range(-0.3, 0.3) * d.h as f32 * TW);
                spawn.push((mid + off, 18.0, if frac < 0.25 { 2 } else { 1 }));
            }
            if s.kind == StructKind::Refinery && gen_range(0.0, 1.0) < dt * 5.0 {
                spawn.push((vec2((s.x as f32 + 0.45) * TW, (s.y as f32 + 0.45) * TW), 46.0, 3));
            }
        }
        for (at, z, kind) in spawn {
            let drift = vec2(gen_range(4.0, 12.0), gen_range(-3.0, 3.0));
            match kind {
                0 => {
                    let c = rgba(0.74, 0.60, 0.44, 0.34);
                    self.part(at + rdir() * 5.0, z, rdir() * 6.0, 7.0, gen_range(0.6, 1.1), 5.0, 17.0, c, fade(c, 0.0), true)
                }
                3 => {
                    let c = rgba(0.84, 0.84, 0.86, 0.4);
                    self.part(at, z, drift, 16.0, gen_range(1.6, 2.6), 4.0, 16.0, c, fade(c, 0.0), true)
                }
                k => {
                    let (c0, c1) = (rgba(0.1, 0.1, 0.1, 0.6), rgba(0.25, 0.25, 0.25, 0.0));
                    self.part(at, z, drift, 22.0, gen_range(1.2, 2.2), 7.0, 24.0, c0, c1, true);
                    if k == 2 {
                        let (c0, c1) = (rgba(1.0, 0.7, 0.25, 0.9), rgba(1.0, 0.3, 0.05, 0.0));
                        self.part(at, z - 2.0, Vec2::ZERO, 12.0, 0.35, 9.0, 4.0, c0, c1, false);
                    }
                }
            }
        }
    }

    fn update_fx(&mut self, dt: f32) {
        let mut trail: Vec<(Vec2, f32)> = Vec::new();
        let mut burst: Vec<(Vec2, f32)> = Vec::new();
        for f in &mut self.fx {
            match f {
                Fx::Tracer { t, .. } | Fx::Ring { t, .. } => *t += dt,
                Fx::Shell { a, az, b, bz, t, life } => {
                    *t += dt;
                    let k = (*t / *life).min(1.0);
                    trail.push((a.lerp(*b, k), *az + (*bz - *az) * k));
                    if *t >= *life {
                        burst.push((*b, *bz));
                    }
                }
            }
        }
        self.fx.retain(|f| match f {
            Fx::Tracer { t, .. } => *t < 0.1,
            Fx::Shell { t, life, .. } => t < life,
            Fx::Ring { t, life, .. } => t < life,
        });
        for (p, z) in trail {
            let c = rgba(0.75, 0.75, 0.75, 0.4);
            self.part(p, z, rdir() * 3.0, 4.0, 0.45, 3.0, 9.0, c, fade(c, 0.0), true);
        }
        for (p, z) in burst {
            self.explode(p, z, 0.4);
        }
        for p in &mut self.parts {
            p.age += dt;
            p.p += p.v * dt;
            p.vz -= p.grav * dt;
            p.z = (p.z + p.vz * dt).max(0.0);
            p.v *= 1.0 - (1.8 * dt).min(1.0);
            p.rot += dt * 0.4;
        }
        self.parts.retain(|p| p.age < p.life);
        for d in &mut self.decals {
            d.2 += dt;
        }
        self.decals.retain(|d| d.2 < 90.0);
        self.shake = (self.shake - dt * 18.0).max(0.0);
        let credits = self.world.players[self.me as usize].credits as f32;
        let gap = credits - self.shown_credits;
        self.shown_credits += gap.signum() * gap.abs().min((gap.abs() * 6.0 + 60.0) * dt);
    }

    // ---------- projection ----------

    fn view_w(&self) -> f32 {
        screen_width() - SIDE
    }
    /// `s` is in ground-plane pixels (square tiles); the y squash happens here.
    fn proj(&self, s: Vec2, z: f32) -> Vec2 {
        vec2((s.x - self.cam.x) * self.zoom, (s.y * YS - self.cam.y - z) * self.zoom)
    }
    fn upos(&self, u: &Unit) -> Vec2 {
        fpos(u.prev).lerp(fpos(u.pos), self.alpha.clamp(0.0, 1.0))
    }
    fn uz(u: &Unit) -> f32 {
        if u.kind.def().air {
            AIR_Z
        } else {
            0.0
        }
    }
    fn mouse_tile(&self, m: Vec2) -> (i32, i32) {
        (((m.x / self.zoom + self.cam.x) / TW).floor() as i32, ((m.y / self.zoom + self.cam.y) / TH).floor() as i32)
    }
    fn center_on(&mut self, t: (i32, i32)) {
        self.cam = vec2(
            (t.0 as f32 + 0.5) * TW - self.view_w() / self.zoom / 2.0,
            (t.1 as f32 + 0.5) * TH - screen_height() / self.zoom / 2.0,
        );
    }
    fn mm_rect(&self) -> Rect {
        Rect::new(self.view_w() + 10.0, 10.0, MM, MM)
    }
    fn icon_rect(&self, i: usize, units: bool) -> Rect {
        let y = ICON_Y + if units { 6.0 * ICON_PITCH + 10.0 } else { 0.0 };
        Rect::new(self.view_w() + 8.0 + (i % 2) as f32 * 104.0, y + (i / 2) as f32 * ICON_PITCH, 100.0, ICON_H)
    }

    fn pick_unit(&self, m: Vec2, pred: impl Fn(&Unit) -> bool) -> Option<u32> {
        let mut best: Option<(f32, u32)> = None;
        for u in self.world.units.values().filter(|u| !u.carried && pred(u) && self.unit_visible(u)) {
            let d = self.proj(self.upos(u), Self::uz(u) + 5.0).distance(m);
            if d < 15.0 * self.zoom && best.map_or(true, |b| d < b.0) {
                best = Some((d, u.id));
            }
        }
        best.map(|b| b.1)
    }

    fn own_struct_at(&self, tile: (i32, i32)) -> Option<u32> {
        if !inb(tile.0, tile.1) {
            return None;
        }
        let id = self.world.structure_at[idx(tile.0, tile.1)];
        self.world.structs.get(&id).filter(|s| s.owner == self.me).map(|s| s.id)
    }

    /// Repair / sell buttons, in a slim row above the build grid.
    fn tool_rect(&self, i: usize) -> Rect {
        Rect::new(self.view_w() + 8.0 + i as f32 * 104.0, TOOL_Y, 100.0, 22.0)
    }

    fn ghost_origin(&self, kind: StructKind, m: Vec2) -> (i32, i32) {
        let d = kind.def();
        let t = self.mouse_tile(m);
        (t.0 - (d.w - 1) / 2, t.1 - (d.h - 1) / 2)
    }

    fn ready(&self, kind: StructKind) -> bool {
        matches!(&self.world.players[self.me as usize].construction, Some(c) if c.ready && c.kind == kind)
    }

    // ---------- frame ----------

    pub fn frame(&mut self, alpha: f32, dt: f32, status: &str) -> Vec<Command> {
        self.alpha = alpha;
        self.time += dt;
        let mut out = Vec::new();
        let (w, me) = (&self.world, self.me);
        self.sel.retain(|id| w.units.get(id).is_some_and(|u| u.owner == me && !u.carried));
        self.sel_struct = self.sel_struct.filter(|id| w.structs.contains_key(id) && self.sel.is_empty());
        for u in w.units.values() {
            if u.face != (0, 0) {
                let want = (u.face.1 as f32).atan2(u.face.0 as f32);
                let a = self.angles.entry(u.id).or_insert(want);
                let diff = (want - *a + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
                *a += diff * (dt * 12.0).min(1.0);
            }
        }
        self.angles.retain(|id, _| w.units.contains_key(id));
        self.input(dt, &mut out);
        self.ambient(dt);
        self.update_fx(dt);
        self.draw(status, dt);
        out
    }

    fn input(&mut self, dt: f32, out: &mut Vec<Command>) {
        let m: Vec2 = mouse_position().into();
        let (sw, sh, vw) = (screen_width(), screen_height(), self.view_w());
        let shift = is_key_down(KeyCode::LeftShift) || is_key_down(KeyCode::RightShift);
        let ctrl = is_key_down(KeyCode::LeftControl) || is_key_down(KeyCode::RightControl);

        // Camera.
        let sp = 900.0 * dt / self.zoom;
        let inside = m.x >= 0.0 && m.y >= 0.0 && m.x <= sw && m.y <= sh;
        if is_mouse_button_down(MouseButton::Middle) {
            self.cam -= (m - self.last_mouse) / self.zoom;
        }
        self.last_mouse = m;
        if is_key_down(KeyCode::Left) || is_key_down(KeyCode::A) || (inside && m.x <= 2.0) {
            self.cam.x -= sp;
        }
        if is_key_down(KeyCode::Right) || is_key_down(KeyCode::D) || (inside && m.x >= sw - 3.0) {
            self.cam.x += sp;
        }
        if is_key_down(KeyCode::Up) || is_key_down(KeyCode::W) || (inside && m.y <= 2.0) {
            self.cam.y -= sp;
        }
        if is_key_down(KeyCode::Down) || is_key_down(KeyCode::S) || (inside && m.y >= sh - 3.0) {
            self.cam.y += sp;
        }
        let wheel = mouse_wheel().1;
        if wheel != 0.0 && m.x < vw {
            let before = m / self.zoom + self.cam;
            self.zoom = (self.zoom * if wheel > 0.0 { 1.1 } else { 1.0 / 1.1 }).clamp(0.45, 2.2);
            self.cam = before - m / self.zoom;
        }
        if is_key_pressed(KeyCode::Space) {
            let yard = self.world.structs.values().find(|s| s.owner == self.me && s.kind == StructKind::Yard);
            let t = yard.map_or(self.world.starts[self.me as usize], |s| (s.x + 1, s.y + 1));
            self.center_on(t);
        }
        if self.drag.is_none() && is_mouse_button_down(MouseButton::Left) && self.mm_rect().contains(m) {
            let r = self.mm_rect();
            self.center_on((((m.x - r.x) / MM * MAP_W as f32) as i32, ((m.y - r.y) / MM * MAP_H as f32) as i32));
        }
        let (vwz, vhz) = (vw / self.zoom, sh / self.zoom);
        self.cam.x = self.cam.x.clamp(-200.0, (MAP_W as f32 * TW - vwz + 200.0).max(-200.0));
        self.cam.y = self.cam.y.clamp(-200.0, (MAP_H as f32 * TH - vhz + 200.0).max(-200.0));

        // Keys.
        if !self.sel.is_empty() {
            if is_key_pressed(KeyCode::Q) {
                self.mode = Mode::AttackMove;
            }
            if is_key_pressed(KeyCode::T) {
                self.mode = Mode::Airlift;
            }
            if is_key_pressed(KeyCode::F) {
                out.push(Command::Deploy { units: self.sel.clone() });
            }
            if is_key_pressed(KeyCode::X) {
                out.push(Command::Stop { units: self.sel.clone() });
            }
        }
        for (key, mode) in [(KeyCode::R, Mode::Repair), (KeyCode::V, Mode::Sell)] {
            if is_key_pressed(key) {
                self.mode = if self.mode == mode { Mode::Normal } else { mode };
                self.placing = None;
            }
        }
        if is_key_pressed(KeyCode::Escape) {
            if self.placing.is_some() || self.mode != Mode::Normal {
                self.placing = None;
                self.mode = Mode::Normal;
            } else {
                self.sel.clear();
                self.sel_struct = None;
            }
        }
        const KEYS: [KeyCode; 9] = [
            KeyCode::Key1,
            KeyCode::Key2,
            KeyCode::Key3,
            KeyCode::Key4,
            KeyCode::Key5,
            KeyCode::Key6,
            KeyCode::Key7,
            KeyCode::Key8,
            KeyCode::Key9,
        ];
        for (i, k) in KEYS.iter().enumerate() {
            if is_key_pressed(*k) {
                if ctrl {
                    self.groups[i] = self.sel.clone();
                } else if !self.groups[i].is_empty() {
                    self.sel = self.groups[i].clone();
                }
            }
        }

        let (lp, rp) = (is_mouse_button_pressed(MouseButton::Left), is_mouse_button_pressed(MouseButton::Right));
        if m.x >= vw {
            if lp || rp {
                self.sidebar_click(m, lp, out);
            }
            if is_mouse_button_released(MouseButton::Left) {
                self.drag = None;
            }
            return;
        }

        let tile = self.mouse_tile(m);
        if lp {
            if let Some(kind) = self.placing {
                let (x, y) = self.ghost_origin(kind, m);
                if self.ready(kind) && self.world.can_place(self.me, kind, x, y) {
                    out.push(Command::Place { kind, x, y });
                    if shift {
                        out.push(Command::StartBuild(kind));
                    } else {
                        self.placing = None;
                    }
                }
            } else if matches!(self.mode, Mode::Repair | Mode::Sell) {
                // Stays armed so several buildings can be clicked in a row.
                if let Some(id) = self.own_struct_at(tile) {
                    out.push(if self.mode == Mode::Repair { Command::Repair { id } } else { Command::Sell { id } });
                }
            } else if self.mode != Mode::Normal {
                if inb(tile.0, tile.1) && !self.sel.is_empty() {
                    let units = self.sel.clone();
                    out.push(match self.mode {
                        Mode::Airlift => Command::Airlift { units, x: tile.0, y: tile.1 },
                        _ => Command::Move { units, x: tile.0, y: tile.1, attack: true },
                    });
                    self.ack(m);
                }
                self.mode = Mode::Normal;
            } else {
                self.drag = Some(m);
            }
        }
        if is_mouse_button_released(MouseButton::Left) {
            if let Some(d0) = self.drag.take() {
                let me = self.me;
                if !shift {
                    self.sel.clear();
                }
                self.sel_struct = None;
                if d0.distance(m) < 6.0 {
                    if let Some(id) = self.pick_unit(m, |u| u.owner == me) {
                        if let Some(i) = self.sel.iter().position(|s| *s == id) {
                            self.sel.remove(i);
                        } else {
                            self.sel.push(id);
                        }
                    } else if self.sel.is_empty() {
                        self.sel_struct = self.own_struct_at(self.mouse_tile(m));
                    }
                } else {
                    let r = Rect::new(d0.x.min(m.x), d0.y.min(m.y), (d0.x - m.x).abs(), (d0.y - m.y).abs());
                    for u in self.world.units.values().filter(|u| u.owner == me && !u.carried) {
                        if r.contains(self.proj(self.upos(u), Self::uz(u))) && !self.sel.contains(&u.id) {
                            self.sel.push(u.id);
                        }
                    }
                }
            }
        }
        if rp {
            if self.placing.is_some() || self.mode != Mode::Normal {
                self.placing = None;
                self.mode = Mode::Normal;
            } else if !self.sel.is_empty() && inb(tile.0, tile.1) {
                self.context_order(m, tile, out);
                self.ack(m);
            } else if let Some(s) = self.sel_struct.and_then(|id| self.world.structs.get(&id)) {
                if TRAIN_AT.contains(&s.kind) && inb(tile.0, tile.1) {
                    out.push(Command::Rally { id: s.id, x: tile.0, y: tile.1 });
                    self.ack(m);
                }
            }
        }
    }

    /// Order acknowledged: a quick ring where the click landed.
    fn ack(&mut self, m: Vec2) {
        let at = vec2(m.x / self.zoom + self.cam.x, (m.y / self.zoom + self.cam.y) / YS);
        self.fx.push(Fx::Ring { at, z: 0.0, t: 0.0, life: 0.25, r: 12.0 });
    }

    fn context_order(&mut self, m: Vec2, tile: (i32, i32), out: &mut Vec<Command>) {
        let (w, me) = (&self.world, self.me);
        let units = self.sel.clone();
        let of_kind = |k: UnitKind| -> Vec<u32> { units.iter().copied().filter(|id| w.units[id].kind == k).collect() };
        let (harvs, carriers) = (of_kind(UnitKind::Harvester), of_kind(UnitKind::Carrier));
        let st = w.structs.get(&w.structure_at[idx(tile.0, tile.1)]);

        if let Some(id) = self.pick_unit(m, |u| u.owner != me) {
            out.push(Command::Attack { units, target: Target::Unit(id) });
        } else if let Some(s) = st.filter(|s| s.owner != me && self.ghosts.contains_key(&s.id)) {
            out.push(Command::Attack { units, target: Target::Struct(s.id) });
        } else if let Some(cargo) =
            self.pick_unit(m, |u| u.owner == me && !u.kind.def().air).filter(|_| !carriers.is_empty())
        {
            out.push(Command::Pickup { units: carriers, cargo });
        } else if st.is_some_and(|s| s.kind == StructKind::Refinery) && !harvs.is_empty() {
            out.push(Command::Return { units: harvs });
        } else if w.terrain[idx(tile.0, tile.1)] == Terrain::Ore && !harvs.is_empty() {
            let rest: Vec<u32> = units.iter().copied().filter(|id| !harvs.contains(id)).collect();
            out.push(Command::Harvest { units: harvs, x: tile.0, y: tile.1 });
            if !rest.is_empty() {
                out.push(Command::Move { units: rest, x: tile.0, y: tile.1, attack: false });
            }
        } else {
            out.push(Command::Move { units, x: tile.0, y: tile.1, attack: false });
        }
    }

    fn sidebar_click(&mut self, m: Vec2, left: bool, out: &mut Vec<Command>) {
        let (w, me) = (&self.world, self.me);
        for (i, kind) in StructKind::BUILDABLE.iter().enumerate() {
            if !self.icon_rect(i, false).contains(m) {
                continue;
            }
            let current = w.players[me as usize].construction.as_ref().map(|c| c.kind);
            if !left {
                if current == Some(*kind) {
                    out.push(Command::CancelBuild);
                    self.placing = None;
                }
            } else if current.is_none() && w.can_build(me, *kind) {
                out.push(Command::StartBuild(*kind));
            } else if current == Some(*kind) && self.ready(*kind) {
                self.placing = Some(*kind);
                self.mode = Mode::Normal;
            }
        }
        for (i, mode) in [Mode::Repair, Mode::Sell].into_iter().enumerate() {
            if left && self.tool_rect(i).contains(m) {
                self.mode = if self.mode == mode { Mode::Normal } else { mode };
                self.placing = None;
            }
        }
        for (i, kind) in UnitKind::ALL.iter().enumerate() {
            if self.icon_rect(i, true).contains(m) {
                if !left {
                    out.push(Command::CancelTrain(*kind));
                } else if w.can_train(me, *kind) {
                    out.push(Command::Train(*kind));
                }
            }
        }
    }

    // ---------- drawing: primitives ----------

    /// Oriented box on the ground plane, extruded from height z0 to z1, lit from SUN.
    #[allow(clippy::too_many_arguments)]
    fn obox(&self, c: Vec2, l: f32, w: f32, ang: f32, z0: f32, z1: f32, col: Color) {
        let (s, co) = ang.sin_cos();
        let (f, r) = (vec2(co, s) * l, vec2(-s, co) * w);
        let cs = [c + f + r, c + f - r, c - f - r, c - f + r];
        let b = cs.map(|p| self.proj(p, z0));
        let t = cs.map(|p| self.proj(p, z1));
        for i in 0..4 {
            let j = (i + 1) % 4;
            let n = ((cs[i] + cs[j]) * 0.5 - c).normalize_or_zero();
            if n.y < -0.05 {
                continue; // faces away from the camera
            }
            let k = 0.56 + 0.26 * n.dot(SUN);
            let (hi, lo) = (shade(col, k), shade(col, k * 0.72));
            gfx::quad([t[i], t[j], b[j], b[i]], [hi, hi, lo, lo]);
        }
        let lit = |p: Vec2| shade(col, 1.0 + 0.16 * (p - c).normalize_or_zero().dot(SUN));
        gfx::quad(t, [lit(cs[0]), lit(cs[1]), lit(cs[2]), lit(cs[3])]);
    }

    fn rotor(&self, c: Vec2, z: f32, r: f32, phase: f32) {
        let zm = self.zoom;
        let p = self.proj(c, z);
        ellipse(p, r * zm, r * YS * zm, rgba(0.85, 0.9, 0.95, 0.16));
        ellipse_lines(p, r * zm, r * YS * zm, 1.0, rgba(0.1, 0.1, 0.12, 0.35));
        for k in 0..2 {
            let a = self.time * 38.0 + phase + k as f32 * 1.5708;
            let d = vec2(a.cos(), a.sin() * YS) * r * zm;
            draw_line(p.x - d.x, p.y - d.y, p.x + d.x, p.y + d.y, 1.6 * zm, rgba(0.08, 0.08, 0.1, 0.55));
        }
        draw_circle(p.x, p.y, 1.8 * zm, STEEL);
    }

    /// Axis-aligned building block. (x, y, w, h) is the footprint on screen, `ht` its height.
    #[allow(clippy::too_many_arguments)]
    fn block(&self, x: f32, y: f32, w: f32, h: f32, ht: f32, roof: Color, wall: Color) {
        let top = y - ht;
        gfx::vgrad(x, top + h, w, ht, shade(wall, 0.80), shade(wall, 0.42));
        gfx::quad(
            [vec2(x, top), vec2(x + w, top), vec2(x + w, top + h), vec2(x, top + h)],
            [shade(roof, 1.16), shade(roof, 1.0), shade(roof, 0.80), shade(roof, 0.96)],
        );
        draw_line(x, top + h, x + w, top + h, 1.0, shade(roof, 1.3));
        draw_line(x, top, x + w, top, 1.5, shade(roof, 1.4));
        draw_line(x, top, x, top + h, 1.5, shade(roof, 1.3));
        draw_line(x + w, top, x + w, top + h + ht, 1.0, shade(wall, 0.3));
        draw_line(x, top + h + ht, x + w, top + h + ht, 1.0, shade(wall, 0.25));
    }

    fn block_shadow(&self, x: f32, y: f32, w: f32, h: f32, ht: f32) {
        let s = vec2(ht * 0.75, ht * 0.5);
        let c = rgba(0.0, 0.0, 0.0, 0.34);
        gfx::flat([vec2(x + w, y), vec2(x + w, y) + s, vec2(x + w, y + h) + s, vec2(x + w, y + h)], c);
        gfx::flat([vec2(x, y + h), vec2(x + w, y + h), vec2(x + w, y + h) + s, vec2(x, y + h) + s], c);
    }

    /// Upright cylinder standing at `base` (screen), for silos, stacks and domes.
    fn cyl(&self, base: Vec2, rx: f32, ry: f32, ht: f32, col: Color) {
        ellipse(base, rx, ry, shade(col, 0.45));
        gfx::hgrad(base.x - rx, base.y - ht, rx * 0.7, ht, shade(col, 0.85), shade(col, 1.12));
        gfx::hgrad(base.x - rx * 0.3, base.y - ht, rx * 1.3, ht, shade(col, 1.12), shade(col, 0.42));
        let top = base - vec2(0.0, ht);
        ellipse(top, rx, ry, shade(col, 1.08));
        ellipse_lines(top, rx, ry, 1.0, shade(col, 1.4));
    }

    // ---------- drawing: units ----------

    fn draw_unit(&self, u: &Unit, at: Vec2, z: f32, ang: f32, in_world: bool) {
        const CORNERS: [(f32, f32); 4] = [(1.0, 1.0), (1.0, -1.0), (-1.0, 1.0), (-1.0, -1.0)];
        let zm = self.zoom;
        let col = PCOL[u.owner as usize % 4];
        let f = vec2(ang.cos(), ang.sin());
        let r = vec2(-f.y, f.x);
        let air = u.kind.def().air;
        let moving = u.pos != u.prev;
        if in_world {
            if air {
                let sp = self.proj(at + vec2(AIR_Z * 0.55, AIR_Z * 0.55), 0.0);
                gfx::glow(sp, 20.0 * zm, 13.0 * zm, rgba(0.0, 0.0, 0.0, 0.42));
            } else {
                let k = match u.kind {
                    UnitKind::Tank | UnitKind::Harvester => 1.35,
                    UnitKind::Mcv => 1.6,
                    UnitKind::Buggy => 1.0,
                    _ => 0.55,
                };
                gfx::glow(self.proj(at + vec2(3.0, 3.0), z), 17.0 * k * zm, 11.0 * k * zm, rgba(0.0, 0.0, 0.0, 0.5));
            }
        }
        let tracks = |len: f32, off: f32| {
            for s in [-1.0, 1.0] {
                self.obox(at + r * off * s, len, 2.6, ang, z, z + 4.5, rgba(0.13, 0.12, 0.12, 1.0));
                // Tread marks that scroll as the vehicle moves.
                let mut d = -len + if moving { (self.time * 30.0) % 4.0 } else { 0.0 };
                while d < len {
                    let p = self.proj(at + r * (off * s - 2.2) + f * d, z + 4.6);
                    let q = self.proj(at + r * (off * s + 2.2) + f * d, z + 4.6);
                    draw_line(p.x, p.y, q.x, q.y, 1.0, rgba(0.3, 0.29, 0.28, 0.9));
                    d += 4.0;
                }
            }
        };
        match u.kind {
            UnitKind::Trooper | UnitKind::Rocketeer => {
                let step = self.time * 13.0 + u.id as f32;
                let bob = if moving { step.sin().abs() * 1.6 } else { 0.0 };
                let stride = if moving { step.sin() * 2.2 } else { 0.0 };
                let rocket = u.kind == UnitKind::Rocketeer;
                for s in [-1.0, 1.0] {
                    let hip = self.proj(at + r * 1.8 * s, z + 5.0);
                    let foot = self.proj(at + r * 1.8 * s + f * stride * s, z);
                    draw_line(hip.x, hip.y, foot.x, foot.y, 1.8 * zm, rgba(0.15, 0.15, 0.17, 1.0));
                }
                let body = self.proj(at, z + 6.0 + bob);
                ellipse(body, 3.6 * zm, 4.2 * zm, shade(col, 0.62));
                ellipse(body - vec2(0.8 * zm, 0.8 * zm), 2.4 * zm, 3.0 * zm, shade(col, 0.9));
                if rocket {
                    let pack = self.proj(at - f * 2.5, z + 6.5 + bob);
                    draw_circle(pack.x, pack.y, 2.4 * zm, rgba(0.25, 0.3, 0.22, 1.0));
                }
                let head = self.proj(at, z + 11.0 + bob);
                draw_circle(head.x, head.y, 2.5 * zm, rgba(0.86, 0.70, 0.56, 1.0));
                ellipse(head - vec2(0.0, 0.9 * zm), 2.8 * zm, 2.0 * zm, shade(col, 1.05));
                let g0 = self.proj(at + f * 1.0, z + 8.0 + bob);
                let g1 = self.proj(at + f * if rocket { 11.0 } else { 8.0 }, z + 8.5 + bob);
                let (gw, gc) = if rocket { (2.8, rgba(0.3, 0.36, 0.26, 1.0)) } else { (1.5, rgba(0.08, 0.08, 0.09, 1.0)) };
                draw_line(g0.x, g0.y, g1.x, g1.y, gw * zm, gc);
            }
            UnitKind::Buggy => {
                for (a, b) in CORNERS {
                    self.obox(at + f * 6.5 * a + r * 5.8 * b, 3.2, 1.7, ang, z, z + 4.5, rgba(0.1, 0.1, 0.11, 1.0));
                }
                self.obox(at, 9.5, 4.6, ang, z + 2.0, z + 6.0, col);
                self.obox(at + f * 6.5, 2.4, 3.6, ang, z + 6.0, z + 7.0, shade(col, 0.7));
                self.obox(at - f * 2.5, 3.6, 3.8, ang, z + 6.0, z + 9.5, shade(col, 0.55));
                self.obox(at - f * 0.2, 1.0, 3.2, ang, z + 6.5, z + 9.0, GLASS);
                self.obox(at + f * 2.5, 5.5, 0.8, ang, z + 9.5, z + 10.5, STEEL);
            }
            UnitKind::Tank => {
                tracks(13.0, 8.2);
                self.obox(at, 12.0, 6.6, ang, z + 2.0, z + 7.0, shade(col, 0.72));
                self.obox(at - f * 8.5, 2.2, 5.6, ang, z + 7.0, z + 8.2, shade(col, 0.5));
                self.obox(at - f * 0.5, 6.4, 5.0, ang, z + 7.0, z + 11.5, col);
                self.obox(at + f * 13.0, 8.5, 1.3, ang, z + 8.6, z + 10.8, shade(col, 0.42));
                self.obox(at + f * 21.0, 1.6, 1.9, ang, z + 8.3, z + 11.1, STEEL);
                let hatch = self.proj(at - f * 2.5 + r * 1.5, z + 11.6);
                ellipse(hatch, 2.2 * zm, 1.7 * zm, shade(col, 0.5));
            }
            UnitKind::Harvester => {
                tracks(15.0, 9.4);
                let hull = mix(col, rgba(0.82, 0.66, 0.24, 1.0), 0.62);
                self.obox(at + f * 17.0, 1.6, 9.5, ang, z, z + 4.5, rgba(0.2, 0.2, 0.22, 1.0));
                self.obox(at - f * 2.5, 12.0, 8.4, ang, z + 2.0, z + 11.0, hull);
                self.obox(at - f * 2.5, 10.0, 6.6, ang, z + 11.0, z + 11.6, shade(hull, 0.35));
                if u.load > 0 {
                    let k = (u.load as f32 / HARVEST_CAP as f32).clamp(0.12, 1.0);
                    self.obox(at - f * 2.5, 9.6 * k, 6.2, ang, z + 11.6, z + 12.0 + 2.5 * k, rgba(1.0, 0.56, 0.12, 1.0));
                    gfx::glow(self.proj(at - f * 2.5, z + 13.0), 14.0 * k * zm, 9.0 * k * zm, rgba(1.0, 0.5, 0.1, 0.3));
                }
                self.obox(at + f * 11.5, 3.2, 6.8, ang, z + 2.0, z + 13.5, shade(col, 0.9));
                self.obox(at + f * 13.6, 1.3, 5.6, ang, z + 9.0, z + 13.0, GLASS);
                let lamp = self.proj(at + f * 10.5, z + 15.0);
                let on = moving && (self.time * 3.0 + u.id as f32).sin() > 0.0;
                draw_circle(lamp.x, lamp.y, 1.6 * zm, if on { rgba(1.0, 0.75, 0.2, 1.0) } else { rgba(0.4, 0.3, 0.1, 1.0) });
                if on {
                    gfx::glow(lamp, 9.0 * zm, 9.0 * zm, rgba(1.0, 0.7, 0.2, 0.5));
                }
            }
            UnitKind::Explorer => {
                for (a, b) in CORNERS {
                    self.obox(at + f * 5.0 * a + r * 4.4 * b, 2.6, 1.4, ang, z, z + 3.6, rgba(0.1, 0.1, 0.11, 1.0));
                }
                self.obox(at, 7.5, 3.4, ang, z + 1.5, z + 5.0, col);
                self.obox(at + f * 2.5, 1.6, 2.8, ang, z + 5.0, z + 7.5, GLASS);
                // Scanner dish on a mast, sweeping.
                let base = self.proj(at - f * 3.0, z + 5.0);
                let top = self.proj(at - f * 3.0, z + 13.0);
                draw_line(base.x, base.y, top.x, top.y, 1.3 * zm, rgba(0.25, 0.25, 0.28, 1.0));
                let sweep = (self.time * 3.0 + u.id as f32).cos().abs();
                ellipse(top, (sweep * 5.0 + 1.5) * zm, 3.6 * zm, rgba(0.85, 0.87, 0.9, 1.0));
                ellipse_lines(top, (sweep * 5.0 + 1.5) * zm, 3.6 * zm, 1.0, rgba(0.3, 0.3, 0.33, 1.0));
                if (self.time * 2.5 + u.id as f32).sin() > 0.2 {
                    gfx::glow(top, 8.0 * zm, 8.0 * zm, rgba(0.4, 1.0, 0.6, 0.5));
                }
            }
            UnitKind::Mcv => {
                tracks(17.0, 10.5);
                let hull = mix(col, rgba(0.6, 0.6, 0.64, 1.0), 0.55);
                self.obox(at - f * 2.0, 15.0, 9.4, ang, z + 2.0, z + 12.0, hull);
                self.obox(at - f * 5.0, 9.0, 7.0, ang, z + 12.0, z + 16.0, shade(hull, 1.1));
                self.obox(at - f * 5.0, 6.0, 4.5, ang, z + 16.0, z + 17.0, col);
                self.obox(at + f * 12.5, 3.6, 8.0, ang, z + 2.0, z + 15.0, col);
                self.obox(at + f * 15.0, 1.3, 6.6, ang, z + 10.0, z + 14.5, GLASS);
                // Folded crane boom.
                self.obox(at - f * 4.0 + r * 6.0, 11.0, 1.2, ang, z + 16.0, z + 18.5, rgba(0.95, 0.75, 0.15, 1.0));
                let lamp = self.proj(at + f * 11.0, z + 16.5);
                let on = (self.time * 3.0 + u.id as f32).sin() > 0.0;
                draw_circle(lamp.x, lamp.y, 1.8 * zm, if on { rgba(1.0, 0.75, 0.2, 1.0) } else { rgba(0.4, 0.3, 0.1, 1.0) });
                if on {
                    gfx::glow(lamp, 10.0 * zm, 10.0 * zm, rgba(1.0, 0.7, 0.2, 0.5));
                }
            }
            UnitKind::Carrier => {
                if in_world {
                    if let Some(c) = u.cargo.and_then(|c| self.world.units.get(&c)) {
                        self.draw_unit(c, at, z - 24.0, ang, false);
                        for (a, b) in CORNERS {
                            let p = self.proj(at + f * 6.0 * a + r * 4.0 * b, z);
                            let q = self.proj(at + f * 5.0 * a + r * 4.0 * b, z - 12.0);
                            draw_line(p.x, p.y, q.x, q.y, 1.0 * zm, rgba(0.1, 0.1, 0.1, 0.9));
                        }
                    }
                }
                for s in [-1.0, 1.0] {
                    self.obox(at + f * 9.0 * s, 1.6, 13.0, ang, z + 2.0, z + 4.0, rgba(0.22, 0.22, 0.25, 1.0));
                }
                self.obox(at, 14.0, 6.0, ang, z, z + 6.0, col);
                self.obox(at, 8.0, 3.0, ang, z + 6.0, z + 7.5, shade(col, 0.6));
                self.obox(at + f * 12.0, 2.0, 4.6, ang, z + 3.0, z + 7.0, GLASS);
                for (i, (a, b)) in CORNERS.into_iter().enumerate() {
                    let c = at + f * 9.0 * a + r * 13.0 * b;
                    self.obox(c, 2.2, 2.2, ang, z + 2.0, z + 6.0, STEEL);
                    self.rotor(c, z + 7.0, 8.5, i as f32 * 0.8 + u.id as f32);
                }
            }
            UnitKind::Gunship => {
                self.obox(at - f * 14.0, 6.0, 1.1, ang, z + 2.0, z + 4.5, shade(col, 0.7));
                self.obox(at - f * 19.5, 1.2, 4.0, ang, z + 3.0, z + 4.5, shade(col, 0.55));
                self.obox(at - f * 19.5, 1.6, 0.6, ang, z + 4.5, z + 9.0, col);
                self.obox(at + f * 1.0, 2.6, 10.0, ang, z + 1.0, z + 3.0, shade(col, 0.6));
                for s in [-1.0, 1.0] {
                    self.obox(at + f * 2.5 + r * 9.5 * s, 4.0, 1.4, ang, z - 1.0, z + 1.5, STEEL);
                }
                self.obox(at, 11.0, 3.4, ang, z, z + 6.5, col);
                self.obox(at + f * 8.0, 3.6, 2.6, ang, z + 4.5, z + 8.0, GLASS);
                self.obox(at + f * 12.5, 1.8, 0.7, ang, z + 0.5, z + 2.0, STEEL);
                self.rotor(at + f * 1.0, z + 9.5, 16.0, u.id as f32);
            }
        }
    }

    fn unit_overlay(&self, u: &Unit, at: Vec2) {
        let zm = self.zoom;
        let z = Self::uz(u);
        let frac = u.hp as f32 / u.kind.def().hp as f32;
        if self.sel.contains(&u.id) {
            let p = self.proj(at, z);
            let k = if matches!(u.kind, UnitKind::Trooper | UnitKind::Rocketeer) { 0.65 } else { 1.15 };
            ellipse_lines(p, 15.0 * k * zm, 10.0 * k * zm, 3.0, rgba(0.2, 1.0, 0.4, 0.25));
            ellipse_lines(p, 15.0 * k * zm, 10.0 * k * zm, 1.2, rgba(0.75, 1.0, 0.8, 0.95));
            hp_bar(self.proj(at, z + 24.0), 26.0 * zm.max(0.8), frac);
        } else if frac < 1.0 {
            hp_bar(self.proj(at, z + 24.0), 22.0 * zm.max(0.8), frac);
        }
    }

    // ---------- drawing: structures ----------

    /// Footprint on screen plus height: (x, y, w, h, ht).
    fn struct_rect(&self, s: &Structure) -> (f32, f32, f32, f32, f32) {
        let zm = self.zoom;
        let d = s.kind.def();
        let inset = if s.kind == StructKind::Wall { 0.0 } else { 2.5 };
        let x = (s.x as f32 * TW - self.cam.x + inset) * zm;
        let y = (s.y as f32 * TH - self.cam.y + inset) * zm;
        let (w, h) = ((d.w as f32 * TW - 2.0 * inset) * zm, (d.h as f32 * TH - 2.0 * inset) * zm);
        let ht = zm * match s.kind {
            StructKind::Yard => 22.0,
            StructKind::Power => 10.0,
            StructKind::Refinery => 13.0,
            StructKind::Barracks => 14.0,
            StructKind::Factory => 20.0,
            StructKind::Airpad => 4.0,
            StructKind::Wall => 11.0,
            StructKind::Armory => 12.0,
            StructKind::TechLab => 15.0,
            _ => 7.0,
        };
        (x, y, w, h, ht)
    }

    fn draw_struct(&self, s: &Structure) {
        let zm = self.zoom;
        let t = self.time;
        let col = PCOL[s.owner as usize % 4];
        let roof = mix(col, rgba(0.56, 0.57, 0.62, 1.0), 0.74);
        let wall = mix(col, rgba(0.42, 0.42, 0.46, 1.0), 0.80);
        let (x, y, w, h, ht) = self.struct_rect(s);
        let top = y - ht;
        let (cx, cy) = (x + w / 2.0, top + h / 2.0);
        let blink = |phase: f32| (t * 2.2 + phase + s.id as f32).sin() > 0.3;
        let lamp = |p: Vec2, c: Color, on: bool| {
            draw_circle(p.x, p.y, 1.8 * zm, if on { c } else { shade(c, 0.3) });
            if on {
                gfx::glow(p, 10.0 * zm, 10.0 * zm, fade(c, 0.55));
            }
        };
        let stripe = |yy: f32, th: f32| gfx::hgrad(x, yy, w, th, shade(col, 1.15), shade(col, 0.75));
        let amber = rgba(1.0, 0.7, 0.18, 1.0);
        match s.kind {
            StructKind::Yard => {
                self.block(x, y, w, h, ht, roof, wall);
                stripe(top, 5.0 * zm);
                for i in 0..5 {
                    let wx = x + w * (0.1 + 0.18 * i as f32);
                    draw_rectangle(wx, top + h + ht * 0.3, w * 0.08, ht * 0.3, rgba(0.95, 0.85, 0.5, 0.9));
                }
                let (tx, ty, tw, th2, tht) = (x + w * 0.14, y + h * 0.2 - ht, w * 0.5, h * 0.56, 13.0 * zm);
                self.block_shadow(tx, ty, tw, th2, tht * 0.6);
                self.block(tx, ty, tw, th2, tht, shade(roof, 1.12), shade(wall, 1.1));
                for i in 0..4 {
                    let wx = tx + tw * (0.1 + 0.22 * i as f32);
                    draw_rectangle(wx, ty + th2 - tht + tht * 0.3, tw * 0.13, tht * 0.38, fade(GLASS, 0.9));
                }
                let (px, py, pw, ph) = (tx + tw * 0.2, ty - tht + th2 * 0.2, tw * 0.6, th2 * 0.5);
                gfx::vgrad(px, py, pw, ph, shade(roof, 0.95), shade(roof, 0.76));
                draw_rectangle_lines(px, py, pw, ph, 1.5, shade(roof, 0.45));
                // Radar mast.
                let base = vec2(x + w * 0.82, top + h * 0.4);
                let mast = base - vec2(0.0, 26.0 * zm);
                draw_line(base.x, base.y, mast.x, mast.y, 2.0 * zm, rgba(0.25, 0.25, 0.28, 1.0));
                let a = t * 1.6 + s.id as f32;
                let rx = (a.cos().abs() * 9.0 + 2.0) * zm;
                ellipse(mast, rx, 6.0 * zm, rgba(0.82, 0.84, 0.88, 1.0));
                ellipse_lines(mast, rx, 6.0 * zm, 1.0, rgba(0.3, 0.3, 0.33, 1.0));
                lamp(mast - vec2(0.0, 7.0 * zm), rgba(1.0, 0.2, 0.15, 1.0), blink(0.0));
            }
            StructKind::Power => {
                self.block(x, y, w, h, ht, shade(roof, 0.85), wall);
                stripe(top, 4.0 * zm);
                draw_line(x + w * 0.27, cy + 4.0 * zm, x + w * 0.73, cy + 4.0 * zm, 4.0 * zm, rgba(0.25, 0.26, 0.3, 1.0));
                let pulse = 0.6 + 0.4 * (t * 3.0 + s.id as f32).sin();
                for dx in [0.27, 0.73] {
                    let base = vec2(x + w * dx, cy + 5.0 * zm);
                    self.cyl(base, 13.0 * zm, 9.5 * zm, 15.0 * zm, rgba(0.62, 0.64, 0.7, 1.0));
                    let c = base - vec2(0.0, 15.0 * zm);
                    ellipse(c, 9.0 * zm, 6.5 * zm, rgba(0.05, 0.12, 0.16, 1.0));
                    ellipse(c, 7.5 * zm, 5.2 * zm, rgba(0.3, 0.9 * pulse + 0.1, 1.0, 1.0));
                    gfx::glow(c, 24.0 * zm, 17.0 * zm, rgba(0.3, 0.9, 1.0, 0.35 * pulse));
                }
            }
            StructKind::Refinery => {
                self.block(x, y, w, h, ht, roof, wall);
                stripe(top, 4.0 * zm);
                // Ore hopper.
                let (hx, hy, hw, hh) = (x + w * 0.08, top + h * 0.38, w * 0.4, h * 0.5);
                draw_rectangle(hx, hy, hw, hh, rgba(0.08, 0.07, 0.07, 1.0));
                let (o0, o1) = (rgba(1.0, 0.62, 0.15, 1.0), rgba(0.75, 0.3, 0.05, 1.0));
                gfx::vgrad(hx + 2.0 * zm, hy + 4.0 * zm, hw - 4.0 * zm, hh - 6.0 * zm, o0, o1);
                gfx::glow(vec2(hx + hw / 2.0, hy + hh / 2.0), hw * 0.8, hh * 0.9, rgba(1.0, 0.5, 0.1, 0.22));
                draw_rectangle_lines(hx, hy, hw, hh, 2.0, rgba(0.2, 0.2, 0.22, 1.0));
                // Chimney, pipe, silo.
                self.cyl(vec2(x + w * 0.15, top + h * 0.26), 4.5 * zm, 3.4 * zm, 26.0 * zm, rgba(0.45, 0.42, 0.42, 1.0));
                draw_line(x + w * 0.5, cy, x + w * 0.68, cy, 4.0 * zm, rgba(0.3, 0.3, 0.34, 1.0));
                let silo = vec2(x + w * 0.78, cy + 6.0 * zm);
                self.cyl(silo, 17.0 * zm, 12.5 * zm, 24.0 * zm, rgba(0.7, 0.71, 0.76, 1.0));
                gfx::hgrad(silo.x - 17.0 * zm, silo.y - 15.0 * zm, 34.0 * zm, 4.0 * zm, shade(col, 1.1), shade(col, 0.5));
                lamp(silo - vec2(0.0, 24.0 * zm), amber, blink(1.0));
            }
            StructKind::Barracks => {
                self.block(x, y, w, h, ht, roof, wall);
                stripe(top, 4.0 * zm);
                for i in 0..6 {
                    let rx = x + w * (0.08 + 0.15 * i as f32);
                    gfx::hgrad(rx, top + h * 0.2, w * 0.1, h * 0.7, shade(roof, 1.2), shade(roof, 0.7));
                }
                let (dw, dh) = (14.0 * zm, ht * 0.72);
                draw_rectangle(cx - dw / 2.0, top + h + ht - dh, dw, dh, rgba(0.07, 0.07, 0.08, 1.0));
                draw_rectangle(cx - dw / 2.0, top + h + ht - dh - 2.0 * zm, dw, 2.0 * zm, rgba(1.0, 0.9, 0.6, 1.0));
                gfx::glow(vec2(cx, top + h + ht - dh), 14.0 * zm, 9.0 * zm, rgba(1.0, 0.85, 0.5, 0.3));
                // Flag.
                let pole = vec2(x + w * 0.88, top + h * 0.3);
                let tip = pole - vec2(0.0, 24.0 * zm);
                draw_line(pole.x, pole.y, tip.x, tip.y, 1.5 * zm, rgba(0.8, 0.8, 0.82, 1.0));
                let wave = (t * 5.0 + s.id as f32).sin() * 2.0 * zm;
                let far = tip + vec2(-13.0 * zm, 2.0 * zm + wave);
                gfx::quad(
                    [tip, far, far + vec2(0.0, 7.0 * zm), tip + vec2(0.0, 8.0 * zm)],
                    [col, shade(col, 0.8), shade(col, 0.7), shade(col, 0.9)],
                );
            }
            StructKind::Factory => {
                self.block(x, y, w, h, ht, roof, wall);
                stripe(top, 5.0 * zm);
                let (dx, dw, dh) = (x + w * 0.18, w * 0.64, ht * 0.8);
                let dy = top + h + ht - dh;
                gfx::vgrad(dx, dy, dw, dh, rgba(0.04, 0.04, 0.05, 1.0), rgba(0.16, 0.15, 0.15, 1.0));
                let n = 12;
                for i in 0..n {
                    let c = if i % 2 == 0 { rgba(0.95, 0.78, 0.1, 1.0) } else { rgba(0.1, 0.1, 0.1, 1.0) };
                    draw_rectangle(dx + dw * i as f32 / n as f32, dy - 3.0 * zm, dw / n as f32 + 0.5, 3.0 * zm, c);
                }
                let (px, py, pw, ph) = (x + w * 0.1, y + h * 0.22 - ht, w * 0.55, h * 0.5);
                self.block(px, py, pw, ph, 6.0 * zm, shade(roof, 1.1), shade(wall, 1.05));
                for i in 0..2 {
                    let v = vec2(x + w * (0.76 + 0.13 * i as f32), top + h * 0.45);
                    ellipse(v, 7.0 * zm, 5.2 * zm, rgba(0.1, 0.1, 0.11, 1.0));
                    for k in 0..2 {
                        let a = t * 6.0 + i as f32 + k as f32 * 1.5708;
                        let d = vec2(a.cos() * 6.5, a.sin() * 4.8) * zm;
                        draw_line(v.x - d.x, v.y - d.y, v.x + d.x, v.y + d.y, 1.5 * zm, rgba(0.5, 0.5, 0.55, 1.0));
                    }
                    ellipse_lines(v, 7.0 * zm, 5.2 * zm, 1.5, rgba(0.6, 0.6, 0.65, 1.0));
                }
                lamp(vec2(dx - 4.0 * zm, dy - 6.0 * zm), amber, blink(0.0));
                lamp(vec2(dx + dw + 4.0 * zm, dy - 6.0 * zm), amber, blink(3.1));
            }
            StructKind::Airpad => {
                self.block(x, y, w, h, ht, rgba(0.24, 0.25, 0.28, 1.0), wall);
                draw_rectangle_lines(x + 3.0 * zm, top + 3.0 * zm, w - 6.0 * zm, h - 6.0 * zm, 2.0 * zm, fade(col, 0.9));
                let paint = rgba(0.95, 0.95, 0.9, 0.95);
                ellipse_lines(vec2(cx, cy), w * 0.34, h * 0.34, 2.5 * zm, paint);
                for (ax, ay, bx, by) in [(-7.0, -6.0, -7.0, 6.0), (7.0, -6.0, 7.0, 6.0), (-7.0, 0.0, 7.0, 0.0)] {
                    draw_line(cx + ax * zm, cy + ay * zm, cx + bx * zm, cy + by * zm, 3.0 * zm, paint);
                }
                for (i, (a, b)) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].into_iter().enumerate() {
                    let p = vec2(x + 5.0 * zm + (w - 10.0 * zm) * a, top + 5.0 * zm + (h - 10.0 * zm) * b);
                    let on = ((t * 3.0) as usize + s.id as usize) % 4 == i;
                    lamp(p, rgba(0.3, 1.0, 0.45, 1.0), on);
                }
            }
            StructKind::Wall => {
                let c = mix(col, rgba(0.62, 0.61, 0.6, 1.0), 0.86);
                self.block(x, y, w, h, ht, c, shade(c, 0.9));
                draw_rectangle(x + w * 0.2, top + h * 0.25, w * 0.6, h * 0.5, shade(c, 0.82));
            }
            StructKind::Turret => {
                self.block(x, y, w, h, ht, shade(roof, 0.8), wall);
                let a = (s.aim.1 as f32).atan2(s.aim.0 as f32);
                let g = vec2((s.x as f32 + 0.5) * TW, (s.y as f32 + 0.5) * TW);
                let f = vec2(a.cos(), a.sin());
                ellipse(self.proj(g, 7.0), 12.0 * zm, 9.0 * zm, rgba(0.2, 0.2, 0.23, 1.0));
                let recoil = if s.cooldown >= 10 { 3.0 } else { 0.0 };
                self.obox(g - f * 1.0, 8.0, 6.5, a, 8.0, 15.0, col);
                for sgn in [-1.0, 1.0] {
                    self.obox(g + f * (13.0 - recoil) + vec2(-f.y, f.x) * 2.6 * sgn, 8.0, 1.2, a, 10.5, 13.0, STEEL);
                }
                if s.cooldown >= 11 {
                    gfx::glow(self.proj(g + f * 22.0, 12.0), 14.0 * zm, 14.0 * zm, rgba(1.0, 0.85, 0.5, 0.9));
                }
            }
            StructKind::RocketTurret => {
                self.block(x, y, w, h, ht, shade(roof, 0.7), wall);
                let a = (s.aim.1 as f32).atan2(s.aim.0 as f32);
                let g = vec2((s.x as f32 + 0.5) * TW, (s.y as f32 + 0.5) * TW);
                let f = vec2(a.cos(), a.sin());
                let side = vec2(-f.y, f.x);
                ellipse(self.proj(g, 7.0), 12.0 * zm, 9.0 * zm, rgba(0.2, 0.2, 0.23, 1.0));
                self.obox(g, 4.5, 4.5, a, 7.0, 12.0, STEEL);
                // Twin launcher pods, two tubes each.
                for sgn in [-1.0, 1.0] {
                    self.obox(g + side * 5.6 * sgn - f * 1.0, 10.0, 3.4, a, 12.0, 19.0, col);
                    for k in [-1.0, 1.0] {
                        let tube = self.proj(g + side * (5.6 * sgn + 1.5 * k) + f * 9.2, 15.5);
                        draw_circle(tube.x, tube.y, 1.5 * zm, rgba(0.05, 0.05, 0.06, 1.0));
                    }
                }
                if s.cooldown >= 28 {
                    gfx::glow(self.proj(g + f * 14.0, 16.0), 20.0 * zm, 20.0 * zm, rgba(1.0, 0.8, 0.45, 0.9));
                }
            }
            StructKind::Armory => {
                self.block(x, y, w, h, ht, shade(roof, 0.9), wall);
                stripe(top, 4.0 * zm);
                // Ammo crates on the roof, blast door below.
                for (i, (ax, ay)) in [(0.12, 0.3), (0.42, 0.3), (0.12, 0.62), (0.42, 0.62)].into_iter().enumerate() {
                    let c = if i % 2 == 0 { rgba(0.38, 0.44, 0.3, 1.0) } else { rgba(0.5, 0.45, 0.32, 1.0) };
                    self.block(x + w * ax, top + h * ay + 5.0 * zm, w * 0.24, h * 0.22, 5.0 * zm, c, shade(c, 0.9));
                }
                let m0 = vec2(x + w * 0.82, top + h * 0.55);
                self.cyl(m0, 5.0 * zm, 3.8 * zm, 13.0 * zm, rgba(0.85, 0.3, 0.2, 1.0));
                let (dw, dh) = (w * 0.4, ht * 0.75);
                gfx::vgrad(cx - dw / 2.0, top + h + ht - dh, dw, dh, rgba(0.2, 0.2, 0.22, 1.0), rgba(0.08, 0.08, 0.09, 1.0));
                draw_line(cx, top + h + ht - dh, cx, top + h + ht, 1.5, rgba(0.4, 0.4, 0.44, 1.0));
                lamp(vec2(cx, top + h + ht - dh - 3.0 * zm), amber, blink(0.5));
            }
            StructKind::TechLab => {
                self.block(x, y, w, h, ht, shade(roof, 1.05), wall);
                stripe(top, 4.0 * zm);
                for i in 0..5 {
                    let wx = x + w * (0.08 + 0.18 * i as f32);
                    draw_rectangle(wx, top + h + ht * 0.25, w * 0.1, ht * 0.4, fade(GLASS, 0.9));
                }
                // Glowing research dome and an antenna.
                let pulse = 0.6 + 0.4 * (t * 2.0 + s.id as f32).sin();
                let dome = vec2(x + w * 0.4, cy + 4.0 * zm);
                self.cyl(dome, 17.0 * zm, 12.5 * zm, 6.0 * zm, rgba(0.7, 0.72, 0.78, 1.0));
                let c = dome - vec2(0.0, 8.0 * zm);
                ellipse(c, 14.0 * zm, 11.0 * zm, rgba(0.25, 0.85 * pulse + 0.1, 0.45, 1.0));
                ellipse(c - vec2(3.0 * zm, 3.0 * zm), 6.0 * zm, 4.0 * zm, rgba(0.8, 1.0, 0.85, 0.5));
                gfx::glow(c, 30.0 * zm, 24.0 * zm, rgba(0.3, 1.0, 0.5, 0.3 * pulse));
                let base = vec2(x + w * 0.84, top + h * 0.5);
                let tip = base - vec2(0.0, 28.0 * zm);
                draw_line(base.x, base.y, tip.x, tip.y, 2.0 * zm, rgba(0.25, 0.25, 0.28, 1.0));
                for k in 1..4 {
                    let yy = base.y - 7.0 * zm * k as f32;
                    draw_line(base.x - 4.0 * zm, yy, base.x + 4.0 * zm, yy, 1.5 * zm, rgba(0.6, 0.6, 0.65, 1.0));
                }
                lamp(tip, rgba(0.3, 1.0, 0.5, 1.0), blink(2.0));
            }
            StructKind::Slab | StructKind::Slab4 => {}
        }
    }

    /// What is still missing before this can be built / trained, if anything.
    fn struct_needs(&self, kind: StructKind) -> Option<&'static str> {
        let w = &self.world;
        if !w.has_struct(self.me, StructKind::Yard) {
            return Some(StructKind::Yard.def().name);
        }
        kind.def().requires.filter(|r| !w.has_struct(self.me, *r)).map(|r| r.def().name)
    }

    fn unit_needs(&self, kind: UnitKind) -> Option<&'static str> {
        let (w, d) = (&self.world, kind.def());
        [Some(d.made_at), d.requires].into_iter().flatten().find(|r| !w.has_struct(self.me, *r)).map(|r| r.def().name)
    }

    /// Hold Tab: every building and what it unlocks.
    fn draw_tech_tree(&self) {
        let (vw, sh) = (self.view_w(), screen_height());
        let mut rows: Vec<(StructKind, String)> = Vec::new();
        let kinds = std::iter::once(StructKind::Yard).chain(StructKind::BUILDABLE.into_iter().filter(|k| !k.is_slab()));
        for k in kinds {
            let mut unlocks: Vec<String> = Vec::new();
            for b in StructKind::BUILDABLE {
                let root = k == StructKind::Yard && b.def().requires.is_none();
                if root || b.def().requires == Some(k) {
                    unlocks.push(b.def().name.to_string());
                }
            }
            for u in UnitKind::ALL {
                let d = u.def();
                if d.requires == Some(k) {
                    unlocks.push(d.name.to_string());
                } else if d.made_at == k {
                    match d.requires {
                        None => unlocks.push(d.name.to_string()),
                        Some(r) => unlocks.push(format!("{} (with {})", d.name, r.def().name)),
                    }
                }
            }
            if !unlocks.is_empty() {
                rows.push((k, unlocks.join(",  ")));
            }
        }
        let (pw, ph) = (760.0f32.min(vw - 40.0), 70.0 + rows.len() as f32 * 30.0);
        let (px, py) = ((vw - pw) / 2.0, (sh - ph) / 2.0);
        gfx::vgrad(px, py, pw, ph, rgba(0.10, 0.11, 0.15, 0.95), rgba(0.04, 0.04, 0.06, 0.95));
        draw_rectangle_lines(px, py, pw, ph, 2.0, PCOL[self.me as usize % 4]);
        gfx::text("TECH TREE", px + 20.0, py + 32.0, 20.0, rgba(1.0, 0.84, 0.32, 1.0));
        gfx::text("green = you have it", px + pw - 20.0 - gfx::text_w("green = you have it", 11.0), py + 30.0, 11.0, rgba(0.5, 1.0, 0.6, 1.0));
        for (i, (k, unlocks)) in rows.iter().enumerate() {
            let y = py + 66.0 + i as f32 * 30.0;
            let have = self.world.has_struct(self.me, *k);
            let c = if have { rgba(0.5, 1.0, 0.6, 1.0) } else { rgba(0.75, 0.77, 0.82, 1.0) };
            gfx::text(k.def().name, px + 20.0, y, 14.0, c);
            gfx::text("unlocks", px + 150.0, y, 11.0, rgba(0.5, 0.52, 0.58, 1.0));
            gfx::text(unlocks, px + 205.0, y, 13.0, if have { WHITE } else { rgba(0.6, 0.62, 0.68, 1.0) });
        }
    }

    // ---------- drawing: scene ----------

    fn draw_ore(&self) {
        let zm = self.zoom;
        let w = &self.world;
        let (x0, y0) = self.mouse_tile(vec2(0.0, 0.0));
        let (x1, y1) = self.mouse_tile(vec2(self.view_w(), screen_height()));
        for ty in y0.max(0)..=y1.min(MAP_H - 1) {
            for tx in x0.max(0)..=x1.min(MAP_W - 1) {
                let i = idx(tx, ty);
                if w.terrain[i] != Terrain::Ore {
                    continue;
                }
                let amount = (w.ore[i] as f32 / 800.0).clamp(0.2, 1.0);
                let n = 2 + (w.ore[i] / 200).min(4) as i32;
                for j in 0..n {
                    let (hx, hy, hs) = (hash2(tx * 7 + j, ty, 1), hash2(tx, ty * 5 + j, 2), hash2(tx + j, ty - j, 3));
                    let c = vec2(
                        ((tx as f32 + 0.1 + 0.8 * hx) * TW - self.cam.x) * zm,
                        ((ty as f32 + 0.25 + 0.65 * hy) * TH - self.cam.y) * zm,
                    );
                    let s = (2.6 + 3.2 * hs) * zm * (0.6 + 0.4 * amount);
                    let tip = c + vec2((hx - 0.5) * s, -s * 2.0);
                    let (l, r, b) = (c + vec2(-s, 0.0), c + vec2(s, 0.0), c + vec2(0.0, s * 0.55));
                    gfx::glow(c, s * 3.2, s * 2.2, rgba(1.0, 0.5, 0.1, 0.22));
                    draw_triangle(tip, l, b, rgba(1.0, 0.72, 0.25, 1.0));
                    draw_triangle(tip, b, r, rgba(0.78, 0.33, 0.06, 1.0));
                    draw_line(tip.x, tip.y, b.x, b.y, 1.0, rgba(1.0, 0.92, 0.6, 0.9));
                    let tw = (self.time * 1.7 + hx * 40.0 + hy * 17.0).sin();
                    if tw > 0.96 {
                        gfx::glow(tip, 7.0 * zm, 7.0 * zm, rgba(1.0, 1.0, 0.85, (tw - 0.96) * 22.0));
                    }
                }
            }
        }
    }

    fn draw(&mut self, status: &str, dt: f32) {
        let real_cam = self.cam;
        if self.shake > 0.0 {
            self.cam += vec2(gen_range(-1.0, 1.0), gen_range(-1.0, 1.0)) * self.shake;
        }
        let zm = self.zoom;
        let m: Vec2 = mouse_position().into();
        let (vw, sh) = (self.view_w(), screen_height());
        self.terrain.draw(&self.world, self.cam, zm, vw, sh, dt);
        for (at, r, age) in &self.decals {
            let a = 0.6 * (1.0 - age / 90.0);
            gfx::glow(self.proj(*at, 0.0), r * 1.5 * zm, r * 1.5 * YS * zm, rgba(0.02, 0.02, 0.02, a));
        }
        self.draw_ore();

        // Ground things, back to front. Shadows first so they never fall on a neighbour's roof.
        enum D<'a> {
            S(&'a Structure),
            U(&'a Unit),
        }
        let w = &self.world;
        let mut items: Vec<(f32, D)> = Vec::new();
        let shown: Vec<&Structure> = w.structs.values().filter(|s| s.owner == self.me).chain(self.ghosts.values()).collect();
        for s in shown.iter().copied() {
            let (x, y, bw, bh, ht) = self.struct_rect(s);
            self.block_shadow(x, y, bw, bh, ht);
            items.push(((s.y + s.kind.def().h) as f32 * TW - 1.0, D::S(s)));
        }
        for u in w.units.values().filter(|u| !u.carried && !u.kind.def().air && self.unit_visible(u)) {
            items.push((self.upos(u).y, D::U(u)));
        }
        items.sort_by(|a, b| a.0.total_cmp(&b.0));
        let ang = |u: &Unit| self.angles.get(&u.id).copied().unwrap_or(1.57);
        for (_, d) in &items {
            match d {
                D::S(s) => self.draw_struct(s),
                D::U(u) => self.draw_unit(u, self.upos(u), 0.0, ang(u), true),
            }
        }
        for s in shown.iter().copied() {
            let frac = s.hp as f32 / s.kind.def().hp as f32;
            if frac < 1.0 {
                let (x, y, bw, _, ht) = self.struct_rect(s);
                hp_bar(vec2(x + bw / 2.0, y - ht - 9.0), (bw * 0.7).max(20.0), frac);
            }
        }
        if let Some(s) = self.sel_struct.and_then(|id| w.structs.get(&id)) {
            let (x, y, bw, bh, ht) = self.struct_rect(s);
            let c = rgba(0.75, 1.0, 0.8, 0.95);
            draw_rectangle_lines(x - 3.0, y - ht - 3.0, bw + 6.0, bh + ht + 6.0, 5.0, rgba(0.2, 1.0, 0.4, 0.25));
            draw_rectangle_lines(x - 3.0, y - ht - 3.0, bw + 6.0, bh + ht + 6.0, 2.0, c);
            if let Some(r) = s.rally {
                // Gather point: dashed line from the building to a flag.
                let from = vec2(x + bw / 2.0, y + bh);
                let foot = self.proj(vec2((r.0 as f32 + 0.5) * TW, (r.1 as f32 + 0.5) * TW), 0.0);
                let n = (from.distance(foot) / 12.0).max(1.0) as i32;
                for i in (0..n).step_by(2) {
                    let (a, b) = (from.lerp(foot, i as f32 / n as f32), from.lerp(foot, (i + 1) as f32 / n as f32));
                    draw_line(a.x, a.y, b.x, b.y, 2.0, fade(c, 0.7));
                }
                let team = PCOL[s.owner as usize % 4];
                let tip = foot - vec2(0.0, 26.0 * zm);
                ellipse_lines(foot, 10.0 * zm, 7.0 * zm, 2.0, c);
                draw_line(foot.x, foot.y, tip.x, tip.y, 2.0 * zm, WHITE);
                let wave = (self.time * 5.0).sin() * 2.0 * zm;
                let far = tip + vec2(14.0 * zm, 3.0 * zm + wave);
                gfx::quad([tip, far, far + vec2(0.0, 8.0 * zm), tip + vec2(0.0, 9.0 * zm)], [team, shade(team, 0.8), shade(team, 0.7), shade(team, 0.9)]);
            }
        }
        for s in shown.iter().copied().filter(|s| s.repairing) {
            let (x, y, bw, bh, ht) = self.struct_rect(s);
            let c = vec2(x + bw / 2.0, y - ht + bh / 2.0);
            let k = 0.55 + 0.45 * (self.time * 7.0).sin();
            let green = rgba(0.35, 1.0, 0.45, k);
            gfx::glow(c, 22.0 * zm, 22.0 * zm, fade(green, 0.5));
            draw_rectangle(c.x - 9.0 * zm, c.y - 2.5 * zm, 18.0 * zm, 5.0 * zm, green);
            draw_rectangle(c.x - 2.5 * zm, c.y - 9.0 * zm, 5.0 * zm, 18.0 * zm, green);
        }
        if matches!(self.mode, Mode::Repair | Mode::Sell) && m.x < vw {
            if let Some(s) = self.own_struct_at(self.mouse_tile(m)).and_then(|id| w.structs.get(&id)) {
                let (x, y, bw, bh, ht) = self.struct_rect(s);
                let c = if self.mode == Mode::Repair { rgba(0.35, 1.0, 0.45, 1.0) } else { rgba(1.0, 0.82, 0.3, 1.0) };
                draw_rectangle(x, y - ht, bw, bh + ht, fade(c, 0.18));
                draw_rectangle_lines(x, y - ht, bw, bh + ht, 3.0, c);
                if self.mode == Mode::Sell {
                    let d = s.kind.def();
                    gfx::text_sh(&format!("+${}", d.cost / 2 * s.hp / d.hp), x + 4.0, y - ht - 6.0, 14.0, c);
                }
            }
        }
        for u in w.units.values().filter(|u| u.kind.def().air && self.unit_visible(u)) {
            self.draw_unit(u, self.upos(u), AIR_Z, ang(u), true);
        }

        // Particles and shots.
        for p in &self.parts {
            let k = p.age / p.life;
            let c = mix(p.c0, p.c1, k);
            let size = (p.s0 + (p.s1 - p.s0) * k) * zm;
            let at = self.proj(p.p, p.z);
            if p.smoke {
                gfx::puff(at, size, p.rot, c);
            } else {
                gfx::glow(at, size, size, c);
                if size > 6.0 * zm {
                    gfx::glow(at, size * 0.45, size * 0.45, rgba(1.0, 1.0, 0.9, c.a));
                }
            }
        }
        for f in &self.fx {
            match f {
                Fx::Tracer { a, az, b, bz, t } => {
                    let k = t / 0.1;
                    let (pa, pb) = (self.proj(*a, *az), self.proj(*b, *bz));
                    let (p, q) = (pa.lerp(pb, (k * 1.3 - 0.3).clamp(0.0, 1.0)), pa.lerp(pb, (k * 1.3).min(1.0)));
                    draw_line(p.x, p.y, q.x, q.y, 3.5 * zm, rgba(1.0, 0.7, 0.2, 0.3));
                    draw_line(p.x, p.y, q.x, q.y, 1.2 * zm, rgba(1.0, 0.97, 0.75, 0.95));
                }
                Fx::Shell { a, az, b, bz, t, life } => {
                    let k = (t / life).min(1.0);
                    let p = self.proj(a.lerp(*b, k), az + (bz - az) * k);
                    gfx::glow(p, 9.0 * zm, 9.0 * zm, rgba(1.0, 0.6, 0.2, 0.8));
                    draw_circle(p.x, p.y, 2.0 * zm, rgba(1.0, 0.97, 0.8, 1.0));
                }
                Fx::Ring { at, z, t, life, r } => {
                    let k = t / life;
                    let rr = r * (0.25 + 0.75 * k) * zm;
                    let th = 2.5 * zm * (1.0 - k) + 0.5;
                    ellipse_lines(self.proj(*at, *z), rr, rr * YS, th, rgba(1.0, 0.9, 0.7, 0.8 * (1.0 - k)));
                }
            }
        }
        // Fog: one texel per tile, stretched over the map; linear filtering softens the edge.
        let fog_size = vec2(MAP_W as f32 * TW * zm, MAP_H as f32 * TH * zm);
        let fog_at = -self.cam * zm;
        draw_texture_ex(&self.fog_tex, fog_at.x, fog_at.y, WHITE, DrawTextureParams { dest_size: Some(fog_size), ..Default::default() });
        for u in w.units.values().filter(|u| !u.carried && self.unit_visible(u)) {
            self.unit_overlay(u, self.upos(u));
        }

        // Placement ghost.
        if let Some(kind) = self.placing.filter(|k| self.ready(*k) && m.x < vw) {
            let d = kind.def();
            let (gx, gy) = self.ghost_origin(kind, m);
            let near = self.world.near_own(self.me, kind, gx, gy);
            let pulse = 0.42 + 0.1 * (self.time * 6.0).sin();
            for ty in gy..gy + d.h {
                for tx in gx..gx + d.w {
                    let ok = near && self.world.place_tile_ok(kind, tx, ty);
                    let bare = ok && !kind.is_slab() && !self.world.slab[idx(tx, ty)];
                    let c = if !ok {
                        rgba(1.0, 0.15, 0.1, pulse)
                    } else if bare {
                        rgba(1.0, 0.8, 0.1, pulse)
                    } else {
                        rgba(0.2, 1.0, 0.35, pulse)
                    };
                    let (x, y) = ((tx as f32 * TW - self.cam.x) * zm, (ty as f32 * TH - self.cam.y) * zm);
                    draw_rectangle(x + 1.0, y + 1.0, TW * zm - 2.0, TH * zm - 2.0, c);
                    draw_rectangle_lines(x + 1.0, y + 1.0, TW * zm - 2.0, TH * zm - 2.0, 1.5, fade(c, 2.0));
                }
            }
        }
        if let Some(d0) = self.drag {
            if d0.distance(m) >= 6.0 {
                let r = Rect::new(d0.x.min(m.x), d0.y.min(m.y), (d0.x - m.x).abs(), (d0.y - m.y).abs());
                draw_rectangle(r.x, r.y, r.w, r.h, rgba(0.3, 1.0, 0.5, 0.08));
                draw_rectangle_lines(r.x, r.y, r.w, r.h, 1.5, rgba(0.7, 1.0, 0.8, 0.9));
            }
        }
        self.cam = real_cam;

        // Vignette.
        let (edge, dark, clear) = (130.0, rgba(0.0, 0.0, 0.0, 0.38), rgba(0.0, 0.0, 0.0, 0.0));
        gfx::vgrad(0.0, 0.0, vw, edge, dark, clear);
        gfx::vgrad(0.0, sh - edge, vw, edge, clear, dark);
        gfx::hgrad(0.0, 0.0, edge, sh, dark, clear);
        gfx::hgrad(vw - edge, 0.0, edge, sh, clear, dark);

        self.draw_sidebar(dt);

        // HUD text.
        let hint = match self.mode {
            Mode::AttackMove => "ATTACK-MOVE  -  left click destination".to_string(),
            Mode::Airlift => "AIRLIFT  -  left click drop zone".to_string(),
            Mode::Repair => "REPAIR  -  left click own buildings to start / stop  -  right click when done".to_string(),
            Mode::Sell => "SELL  -  left click own buildings  -  right click when done".to_string(),
            Mode::Normal if self.placing.is_some_and(|k| self.ready(k)) => {
                "Left click to place  -  Shift: build another  -  Yellow = no slab, half strength".to_string()
            }
            Mode::Normal if !self.sel.is_empty() => {
                let mcv = self.sel.iter().any(|id| self.world.units.get(id).is_some_and(|u| u.kind == UnitKind::Mcv));
                let extra = if mcv { "   -   F deploy MCV (needs 3x3 clear rock)" } else { "" };
                format!("{} selected   -   Q attack-move   -   T airlift   -   X stop{extra}", self.sel.len())
            }
            Mode::Normal => match self.sel_struct.and_then(|id| self.world.structs.get(&id)) {
                Some(s) if TRAIN_AT.contains(&s.kind) => {
                    format!("{}   -   right click the map to set the gather point", s.kind.def().name)
                }
                Some(s) => s.kind.def().name.to_string(),
                None => String::new(),
            },
        };
        if !hint.is_empty() {
            let tw = gfx::text_w(&hint, 15.0);
            draw_rectangle(10.0, sh - 34.0, tw + 20.0, 26.0, rgba(0.03, 0.04, 0.06, 0.72));
            draw_rectangle(10.0, sh - 34.0, 3.0, 26.0, PCOL[self.me as usize % 4]);
            gfx::text(&hint, 22.0, sh - 16.0, 15.0, rgba(0.92, 0.94, 0.97, 1.0));
        }
        if is_key_down(KeyCode::Tab) {
            self.draw_tech_tree();
        }
        if !status.is_empty() {
            gfx::text_sh(status, 14.0, 30.0, 22.0, rgba(1.0, 0.3, 0.25, 1.0));
        }
        let over = if self.world.winner == Some(self.me) {
            Some(("VICTORY", rgba(1.0, 0.84, 0.3, 1.0)))
        } else if self.world.players[self.me as usize].defeated {
            Some(("DEFEATED", rgba(1.0, 0.25, 0.2, 1.0)))
        } else {
            None
        };
        if let Some((txt, col)) = over {
            let tw = gfx::text_w(txt, 96.0);
            draw_rectangle(0.0, sh * 0.4 - 90.0, vw, 120.0, rgba(0.0, 0.0, 0.0, 0.55));
            gfx::glow(vec2(vw / 2.0, sh * 0.4 - 30.0), tw * 0.8, 90.0, fade(col, 0.25));
            gfx::text_sh(txt, (vw - tw) / 2.0, sh * 0.4, 96.0, col);
        }
    }

    // ---------- drawing: sidebar ----------

    /// Draw a structure or unit portrait centred at `c`, by briefly pointing the camera at a fake one.
    fn portrait(&mut self, c: Vec2, st: Option<StructKind>, un: Option<UnitKind>) {
        let (cam, zoom) = (self.cam, self.zoom);
        if let Some(kind) = st {
            let d = kind.def();
            self.zoom = (44.0 / (d.w as f32 * TW)).min(30.0 / (d.h as f32 * TH + 14.0));
            let (fw, fh) = (d.w as f32 * TW, d.h as f32 * TH);
            self.cam = vec2(fw / 2.0 - c.x / self.zoom, fh / 2.0 - (c.y + 5.0) / self.zoom);
            if kind.is_slab() {
                for ty in 0..d.h {
                    for tx in 0..d.w {
                        let x = (tx as f32 * TW - self.cam.x) * self.zoom;
                        let y = (ty as f32 * TH - self.cam.y) * self.zoom;
                        let (w, h) = (TW * self.zoom - 1.0, TH * self.zoom - 1.0);
                        gfx::vgrad(x, y, w, h, rgba(0.72, 0.72, 0.74, 1.0), rgba(0.54, 0.54, 0.57, 1.0));
                        draw_rectangle_lines(x, y, w, h, 1.5, rgba(0.36, 0.36, 0.4, 1.0));
                    }
                }
            } else {
                let s = Structure { id: 0, owner: self.me, kind, x: 0, y: 0, hp: d.hp, cooldown: 0, aim: (3, 2), repairing: false, repair_acc: 0, rally: None };
                self.draw_struct(&s);
            }
        } else if let Some(kind) = un {
            self.zoom = if matches!(kind, UnitKind::Trooper | UnitKind::Rocketeer) { 1.5 } else { 1.0 };
            self.cam = vec2(-c.x / self.zoom, -(c.y + 5.0) / self.zoom);
            let u = World::preview_unit(kind, self.me);
            self.draw_unit(&u, Vec2::ZERO, 0.0, 0.5, false);
        }
        self.cam = cam;
        self.zoom = zoom;
    }

    #[allow(clippy::too_many_arguments)]
    fn icon(
        &mut self,
        r: Rect,
        name: &str,
        cost: i32,
        enabled: bool,
        progress: Option<f32>,
        tag: &str,
        st: Option<StructKind>,
        un: Option<UnitKind>,
    ) {
        let needs = match (st, un) {
            (Some(k), _) => self.struct_needs(k),
            (_, Some(k)) => self.unit_needs(k),
            _ => None,
        };
        let m: Vec2 = mouse_position().into();
        let hover = r.contains(m) && enabled;
        let team = PCOL[self.me as usize % 4];
        let k = if hover { 1.35 } else { 1.0 };
        gfx::vgrad(r.x, r.y, r.w, r.h, shade(rgba(0.23, 0.25, 0.30, 1.0), k), shade(rgba(0.12, 0.13, 0.16, 1.0), k));
        if let Some(p) = progress {
            let pw = r.w * p.clamp(0.0, 1.0);
            gfx::vgrad(r.x, r.y, pw, r.h, rgba(0.25, 0.75, 0.4, 0.55), rgba(0.1, 0.4, 0.2, 0.55));
            draw_rectangle(r.x + pw - 2.0, r.y, 2.0, r.h, rgba(0.6, 1.0, 0.7, 0.9));
        }
        self.portrait(vec2(r.x + 72.0, r.y + 22.0), st, un);
        if !enabled {
            draw_rectangle(r.x, r.y, r.w, r.h, rgba(0.05, 0.05, 0.07, 0.72));
        }
        let ready = tag == "READY";
        let border = if ready {
            fade(rgba(0.4, 1.0, 0.55, 1.0), 0.6 + 0.4 * (self.time * 6.0).sin())
        } else if hover {
            team
        } else {
            rgba(0.36, 0.38, 0.44, 1.0)
        };
        draw_rectangle_lines(r.x, r.y, r.w, r.h, if ready || hover { 3.0 } else { 1.5 }, border);
        draw_line(r.x + 1.0, r.y + 1.0, r.x + r.w - 1.0, r.y + 1.0, 1.0, rgba(1.0, 1.0, 1.0, 0.10));
        let tc = if enabled { rgba(0.94, 0.95, 0.98, 1.0) } else { rgba(0.5, 0.5, 0.54, 1.0) };
        let gold = if enabled { rgba(1.0, 0.82, 0.3, 1.0) } else { rgba(0.5, 0.45, 0.3, 1.0) };
        gfx::text_sh(name, r.x + 5.0, r.y + 14.0, 11.0, tc);
        match needs {
            Some(n) => gfx::text_sh(&format!("needs {n}"), r.x + 5.0, r.y + r.h - 6.0, 10.0, rgba(1.0, 0.45, 0.4, 1.0)),
            None => gfx::text_sh(&format!("${cost}"), r.x + 5.0, r.y + r.h - 6.0, 11.0, gold),
        }
        if !tag.is_empty() {
            gfx::text_sh(tag, r.x + 5.0, r.y + 24.0, 10.0, rgba(0.5, 1.0, 0.6, 1.0));
        }
    }

    fn draw_sidebar(&mut self, dt: f32) {
        let (vw, sh) = (self.view_w(), screen_height());
        let team = PCOL[self.me as usize % 4];
        gfx::vgrad(vw, 0.0, SIDE, sh, rgba(0.13, 0.14, 0.18, 1.0), rgba(0.06, 0.06, 0.08, 1.0));
        gfx::hgrad(vw - 14.0, 0.0, 14.0, sh, rgba(0.0, 0.0, 0.0, 0.0), rgba(0.0, 0.0, 0.0, 0.5));
        draw_line(vw, 0.0, vw, sh, 2.0, fade(team, 0.8));

        // Minimap: terrain texture refreshed twice a second, live dots on top.
        self.mm_timer -= dt;
        if self.mm_timer <= 0.0 {
            self.mm_timer = 0.5;
            for y in 0..MAP_H {
                for x in 0..MAP_W {
                    let i = idx(x, y);
                    let k = 0.9 + 0.2 * hash2(x, y, 9);
                    let c = match self.world.terrain[i] {
                        Terrain::Dust => rgba(0.50 * k, 0.39 * k, 0.28 * k, 1.0),
                        Terrain::Rock if self.world.slab[i] => rgba(0.62, 0.62, 0.65, 1.0),
                        Terrain::Rock => rgba(0.33 * k, 0.32 * k, 0.34 * k, 1.0),
                        Terrain::Ore => rgba(1.0, 0.58, 0.12, 1.0),
                        Terrain::Cliff => rgba(0.13, 0.10, 0.10, 1.0),
                    };
                    let c = if !self.explored[i] {
                        rgba(0.02, 0.02, 0.03, 1.0)
                    } else if !self.visible[i] {
                        shade(c, 0.5)
                    } else {
                        c
                    };
                    self.mm_img.set_pixel(x as u32, y as u32, c);
                }
            }
            self.mm_tex.update(&self.mm_img);
        }
        let r = self.mm_rect();
        let k = MM / MAP_W as f32;
        draw_rectangle(r.x - 3.0, r.y - 3.0, MM + 6.0, MM + 6.0, rgba(0.02, 0.02, 0.03, 1.0));
        let params = DrawTextureParams { dest_size: Some(vec2(MM, MM)), ..Default::default() };
        draw_texture_ex(&self.mm_tex, r.x, r.y, WHITE, params);
        for s in self.world.structs.values().filter(|s| s.owner == self.me).chain(self.ghosts.values()) {
            let d = s.kind.def();
            let c = PCOL[s.owner as usize % 4];
            draw_rectangle(r.x + s.x as f32 * k, r.y + s.y as f32 * k, d.w as f32 * k, d.h as f32 * k, c);
        }
        for u in self.world.units.values().filter(|u| !u.carried && self.unit_visible(u)) {
            let c = center(u.tile);
            let p = vec2(c.0 as f32, c.1 as f32) / TILE as f32 * k;
            draw_rectangle(r.x + p.x - 1.5, r.y + p.y - 1.5, 3.0, 3.0, shade(PCOL[u.owner as usize % 4], 1.25));
        }
        let (vx, vy) = (r.x + self.cam.x / TW * k, r.y + self.cam.y / TH * k);
        let (vx0, vy0) = (vx.max(r.x), vy.max(r.y));
        let vx1 = (vx + vw / self.zoom / TW * k).min(r.x + MM);
        let vy1 = (vy + sh / self.zoom / TH * k).min(r.y + MM);
        draw_rectangle_lines(vx0, vy0, (vx1 - vx0).max(0.0), (vy1 - vy0).max(0.0), 1.5, rgba(1.0, 1.0, 1.0, 0.9));
        draw_rectangle_lines(r.x - 3.0, r.y - 3.0, MM + 6.0, MM + 6.0, 2.0, rgba(0.4, 0.42, 0.48, 1.0));

        // Credits and power.
        let pl = &self.world.players[self.me as usize];
        let x = vw + 10.0;
        let label = rgba(0.6, 0.62, 0.68, 1.0);
        gfx::text("CREDITS", x, 233.0, 10.0, label);
        let cr = format!("{}", self.shown_credits.round() as i32);
        gfx::text(&cr, vw + SIDE - 10.0 - gfx::text_w(&cr, 20.0), 236.0, 20.0, rgba(1.0, 0.84, 0.32, 1.0));
        let low = pl.power_used > pl.power_made;
        gfx::text("POWER", x, 254.0, 10.0, label);
        let (bx, bw) = (x + 50.0, SIDE - 70.0);
        draw_rectangle(bx, 244.0, bw, 11.0, rgba(0.02, 0.02, 0.03, 1.0));
        let cap = pl.power_made.max(pl.power_used).max(1) as f32;
        let pc = if low {
            fade(rgba(1.0, 0.25, 0.2, 1.0), 0.6 + 0.4 * (self.time * 8.0).sin())
        } else {
            rgba(0.3, 0.85, 1.0, 1.0)
        };
        gfx::vgrad(bx + 1.0, 245.0, (bw - 2.0) * pl.power_used as f32 / cap, 9.0, pc, shade(pc, 0.55));
        let mx = bx + 1.0 + (bw - 2.0) * pl.power_made as f32 / cap;
        draw_line(mx - 1.0, 243.0, mx - 1.0, 256.0, 2.0, WHITE);
        let pt = format!("{} / {}", pl.power_used, pl.power_made);
        gfx::text_sh(&pt, bx + (bw - gfx::text_w(&pt, 9.0)) / 2.0, 253.0, 9.0, WHITE);

        let me = self.me;
        for (i, kind) in StructKind::BUILDABLE.iter().enumerate() {
            let d = kind.def();
            let pl = &self.world.players[me as usize];
            let cur = pl.construction.as_ref().filter(|c| c.kind == *kind);
            let enabled = self.world.can_build(me, *kind) && (pl.construction.is_none() || cur.is_some());
            let progress = cur.map(|c| c.spent as f32 / d.cost as f32);
            let tag = if cur.is_some_and(|c| c.ready) { "READY" } else { "" };
            self.icon(self.icon_rect(i, false), d.name, d.cost, enabled, progress, tag, Some(*kind), None);
        }
        let m: Vec2 = mouse_position().into();
        for (i, (mode, name, col)) in
            [(Mode::Repair, "REPAIR  [R]", rgba(0.35, 1.0, 0.45, 1.0)), (Mode::Sell, "SELL  [V]", rgba(1.0, 0.82, 0.3, 1.0))]
                .into_iter()
                .enumerate()
        {
            let r = self.tool_rect(i);
            let on = self.mode == mode;
            let k = if on { 1.0 } else if r.contains(m) { 0.5 } else { 0.28 };
            gfx::vgrad(r.x, r.y, r.w, r.h, fade(shade(col, 0.6), k), fade(shade(col, 0.3), k));
            draw_rectangle_lines(r.x, r.y, r.w, r.h, if on { 3.0 } else { 1.5 }, if on { col } else { rgba(0.36, 0.38, 0.44, 1.0) });
            gfx::text_sh(name, r.x + (r.w - gfx::text_w(name, 11.0)) / 2.0, r.y + r.h / 2.0 + 4.0, 11.0, WHITE);
        }
        for (i, kind) in UnitKind::ALL.iter().enumerate() {
            let d = kind.def();
            let q = &self.world.players[me as usize].queues[TRAIN_AT.iter().position(|s| *s == d.made_at).unwrap()];
            let n = q.items.iter().filter(|k| *k == kind).count();
            let progress = (q.items.front() == Some(kind)).then(|| q.spent as f32 / d.cost as f32);
            let tag = if n > 0 { format!("x{n}") } else { String::new() };
            let enabled = self.world.can_train(me, *kind);
            self.icon(self.icon_rect(i, true), d.name, d.cost, enabled, progress, &tag, None, Some(*kind));
        }
    }
}
