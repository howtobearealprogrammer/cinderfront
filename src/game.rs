//! Client side state: camera, input, selection, fog of war and cosmetic effects.
//! Never mutates the sim directly - everything the player does leaves as a `Command`.
//! Rendering lives in `scene.rs` (3D) and `hud.rs` (UI); both only read this.
//!
//! Effect positions use "ground pixels": 40 per tile on both axes, heights in the
//! same unit. The renderer divides by `PX` to get tiles (= world units).

use crate::gfx::{fade, rgba, Rgba, Rng};
use bevy::input::keyboard::KeyCode;
use bevy::math::{Rect, Vec2, Vec3};
use cinderfront::sim::{
    center, idx, inb, Command, Step, Event, StructKind, Structure, Target, Terrain, Unit, UnitKind, World, MAP_H, MAP_W, TILE,
    TRAIN_AT,
};
use std::collections::HashMap;

/// Ground pixels per tile (the old 2D renderer's tile width).
pub const PX: f32 = 40.0;
pub const SIDE: f32 = 220.0;
pub const AIR_Z: f32 = 56.0;
pub const MM: f32 = 200.0;
const TOOL_Y: f32 = 264.0;
const ICON_Y: f32 = 290.0;
const ICON_H: f32 = 41.0;
const ICON_PITCH: f32 = 44.0;
/// Camera pitch: sin gives the ground squash on screen, cos the height lift.
pub const PITCH_S: f32 = 0.75;
pub const PITCH_C: f32 = 0.661_437_8;

pub const PCOL: [Rgba; 4] = [
    rgba(0.22, 0.52, 0.98, 1.0),
    rgba(0.93, 0.25, 0.20, 1.0),
    rgba(0.28, 0.80, 0.36, 1.0),
    rgba(0.95, 0.80, 0.22, 1.0),
];

#[derive(PartialEq, Clone, Copy)]
pub enum Mode {
    Normal,
    AttackMove,
    Patrol,
    Airlift,
    Repair,
    Sell,
}

/// The Esc menu. `paused` freezes a solo game; network games keep running.
#[derive(Clone, Copy)]
pub struct PauseMenu {
    pub sel: usize,
    pub can_restart: bool,
    pub paused: bool,
}

pub const PAUSE_ITEMS: [&str; 4] = ["Resume", "Restart map", "Exit to main menu", "Quit to desktop"];

impl PauseMenu {
    pub fn enabled(&self, i: usize) -> bool {
        i != 1 || self.can_restart
    }
}

/// What a right click would do right now (see `Game::context`).
#[derive(Clone, Copy, PartialEq)]
pub enum Ctx {
    None,
    Select,
    Move,
    Attack(Target),
    Pickup(u32),
    Return,
    Harvest,
    Rally,
}

/// Mouse pointer shapes; drawn in `cursor.rs`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Cursor {
    Normal,
    Select,
    Move,
    Attack,
    AttackMove,
    Patrol,
    Harvest,
    Return,
    Pickup,
    Airlift,
    Repair,
    Sell,
    Rally,
    Deny,
}

pub enum Fx {
    Tracer { a: Vec2, az: f32, b: Vec2, bz: f32, t: f32 },
    Shell { a: Vec2, az: f32, b: Vec2, bz: f32, t: f32, life: f32 },
    Ring { at: Vec2, z: f32, t: f32, life: f32, r: f32 },
}

/// One particle. Positions are ground pixels plus a height.
pub struct Part {
    pub p: Vec2,
    pub z: f32,
    v: Vec2,
    vz: f32,
    grav: f32,
    pub age: f32,
    pub life: f32,
    pub s0: f32,
    pub s1: f32,
    pub c0: Rgba,
    pub c1: Rgba,
    pub smoke: bool,
    pub rot: f32,
}

/// Input for one frame, gathered from Bevy so the game logic stays engine-agnostic.
#[derive(Default)]
pub struct InputSnap {
    pub mouse: Vec2,
    pub screen: Vec2,
    pub wheel: f32,
    pub keys_down: Vec<KeyCode>,
    pub keys_pressed: Vec<KeyCode>,
    /// left, right, middle
    pub mouse_down: [bool; 3],
    pub mouse_pressed: [bool; 3],
    pub mouse_released: [bool; 3],
    /// Window has focus and the cursor is over it (edge scrolling only then).
    pub hover: bool,
}

impl InputSnap {
    pub fn down(&self, k: KeyCode) -> bool {
        self.keys_down.contains(&k)
    }
    pub fn pressed(&self, k: KeyCode) -> bool {
        self.keys_pressed.contains(&k)
    }
}

pub struct Game {
    pub world: World,
    pub me: u8,
    /// Ground point (in tiles) at the centre of the view.
    pub focus: Vec2,
    pub zoom: f32,
    pub alpha: f32,
    pub time: f32,
    pub sel: Vec<u32>,
    pub sel_structs: Vec<u32>,
    pub cursor: Cursor,
    last_click: Option<(Vec2, f32)>,
    last_group_tap: Option<(usize, f32)>,
    pub drag: Option<Vec2>,
    pub mouse: Vec2,
    last_mouse: Vec2,
    pub screen: Vec2,
    pub placing: Option<StructKind>,
    pub mode: Mode,
    pub fx: Vec<Fx>,
    pub parts: Vec<Part>,
    pub decals: Vec<(Vec2, f32, f32)>,
    pub shake: f32,
    pub shake_off: Vec2,
    pub shown_credits: f32,
    /// Control groups: units and buildings.
    groups: Vec<(Vec<u32>, Vec<u32>)>,
    pub angles: HashMap<u32, f32>,
    /// Fog of war, per tile. Purely a view filter: the sim knows nothing about it.
    pub visible: Vec<bool>,
    pub explored: Vec<bool>,
    /// Terrain height at tile corners (see `corner_heights`). Fixed for a map.
    pub corners: Vec<f32>,
    /// Bumped whenever `visible`/`explored` change.
    pub fog_gen: u32,
    /// Enemy structures as last seen.
    pub ghosts: HashMap<u32, Structure>,
    pub show_tech: bool,
    /// Seconds until the finished match closes (set by the session).
    pub menu_in: Option<f32>,
    /// Esc was pressed with nothing to cancel.
    pub menu_request: bool,
    /// The in-game (Esc) menu, while open.
    pub pause: Option<PauseMenu>,
    rng: Rng,
}

/// World height of one terrain level, in tiles.
pub const LEVEL_H: f32 = 0.8;

/// Ground height (tiles) at every tile corner, `(MAP_W + 1) * (MAP_H + 1)` values.
/// A corner takes the highest level of the plain tiles around it, so plateaus and
/// low ground are flat and each bank or ramp tile is the slope between them.
pub fn corner_heights(w: &World) -> Vec<f32> {
    let mut out = Vec::with_capacity(((MAP_W + 1) * (MAP_H + 1)) as usize);
    for cy in 0..=MAP_H {
        for cx in 0..=MAP_W {
            let tiles = [(cx - 1, cy - 1), (cx, cy - 1), (cx - 1, cy), (cx, cy)];
            let (mut top, mut sum, mut n) = (None::<u8>, 0.0f32, 0.0f32);
            for (x, y) in tiles.into_iter().filter(|(x, y)| inb(*x, *y)) {
                let i = idx(x, y);
                let l = w.level[i];
                match w.terrain[i] {
                    Terrain::Bank => sum += l as f32 - 0.5,
                    Terrain::Ramp => sum += l as f32 + 0.5,
                    _ => top = Some(top.map_or(l, |t| t.max(l))),
                }
                n += 1.0;
            }
            out.push(top.map_or(sum / n.max(1.0), |t| t as f32) * LEVEL_H);
        }
    }
    out
}

/// Ground height in tiles at tile coordinates (`u`, `v`), bilinear over the corners.
pub fn ground_h(corners: &[f32], u: f32, v: f32) -> f32 {
    let u = u.clamp(0.0, MAP_W as f32 - 0.001);
    let v = v.clamp(0.0, MAP_H as f32 - 0.001);
    let (x0, y0) = (u.floor(), v.floor());
    let (fx, fy) = (u - x0, v - y0);
    let at = |x: i32, y: i32| corners[(y * (MAP_W + 1) + x) as usize];
    let (x0, y0) = (x0 as i32, y0 as i32);
    let top = at(x0, y0) * (1.0 - fx) + at(x0 + 1, y0) * fx;
    let bot = at(x0, y0 + 1) * (1.0 - fx) + at(x0 + 1, y0 + 1) * fx;
    top * (1.0 - fy) + bot * fy
}

pub fn fpos(p: (i32, i32)) -> Vec2 {
    Vec2::new(p.0 as f32, p.1 as f32) * (PX / TILE as f32)
}

impl Game {
    pub fn new(world: World, me: u8) -> Game {
        let tiles = (MAP_W * MAP_H) as usize;
        let credits = world.players[me as usize].credits as f32;
        let mut g = Game {
            world,
            me,
            focus: Vec2::ZERO,
            zoom: 1.0,
            alpha: 0.0,
            time: 0.0,
            sel: Vec::new(),
            sel_structs: Vec::new(),
            cursor: Cursor::Normal,
            last_click: None,
            last_group_tap: None,
            drag: None,
            mouse: Vec2::ZERO,
            last_mouse: Vec2::ZERO,
            screen: Vec2::new(1280.0, 800.0),
            placing: None,
            mode: Mode::Normal,
            fx: Vec::new(),
            parts: Vec::new(),
            decals: Vec::new(),
            shake: 0.0,
            shake_off: Vec2::ZERO,
            shown_credits: credits,
            groups: vec![(Vec::new(), Vec::new()); 9],
            angles: HashMap::new(),
            visible: vec![false; tiles],
            explored: vec![false; tiles],
            corners: Vec::new(),
            fog_gen: 0,
            ghosts: HashMap::new(),
            show_tech: false,
            menu_in: None,
            menu_request: false,
            pause: None,
            rng: Rng::new(0x1234_5678 ^ me as u32),
        };
        g.corners = corner_heights(&g.world);
        g.center_on(g.world.starts[me as usize]);
        g.update_vis();
        g
    }

    fn rdir(&mut self) -> Vec2 {
        let a = self.rng.range(0.0, std::f32::consts::TAU);
        Vec2::new(a.cos(), a.sin())
    }
    fn rr(&mut self, a: f32, b: f32) -> f32 {
        self.rng.range(a, b)
    }

    // ---------- fog of war ----------

    pub fn unit_visible(&self, u: &Unit) -> bool {
        u.owner == self.me || self.visible[idx(u.tile.0, u.tile.1)]
    }

    pub fn struct_visible(&self, s: &Structure) -> bool {
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
        // CF_REVEAL=1 (debug): no fog of war.
        let reveal = std::env::var_os("CF_REVEAL").is_some();
        let all = reveal || self.world.players[me as usize].defeated || self.world.winner.is_some();
        let before = self.visible.clone();
        self.visible.iter_mut().for_each(|v| *v = all);
        let mut eyes: Vec<((i32, i32), i32)> = Vec::new();
        for u in self.world.units.values().filter(|u| u.owner == me) {
            let up = if u.kind.def().air { 0 } else { self.world.level_at(u.pos) };
            eyes.push((u.tile, u.kind.sight() + up));
        }
        for s in self.world.structs.values().filter(|s| s.owner == me) {
            let d = s.kind.def();
            let mid = (s.x + d.w / 2, s.y + d.h / 2);
            eyes.push((mid, s.kind.sight() + self.world.level[idx(mid.0, mid.1)] as i32));
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
        for i in 0..self.visible.len() {
            self.explored[i] |= self.visible[i];
        }
        if before != self.visible {
            self.fog_gen = self.fog_gen.wrapping_add(1);
        }

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
    fn part(&mut self, p: Vec2, z: f32, v: Vec2, vz: f32, life: f32, s0: f32, s1: f32, c0: Rgba, c1: Rgba, smoke: bool) {
        if self.parts.len() < 2500 {
            let rot = self.rr(0.0, 6.28);
            self.parts.push(Part { p, z, v, vz, grav: 0.0, age: 0.0, life, s0, s1, c0, c1, smoke, rot });
        }
    }

    fn explode(&mut self, at: Vec2, z: f32, scale: f32) {
        let n = |k: f32| (k * scale).ceil() as i32;
        let sz = scale.sqrt();
        let (hot, ember) = (rgba(1.0, 0.95, 0.75, 1.0), rgba(1.0, 0.6, 0.2, 0.0));
        self.part(at, z + 4.0, Vec2::ZERO, 0.0, 0.14, 26.0 * sz, 44.0 * sz, hot, ember, false);
        for _ in 0..n(6.0) {
            let v = self.rdir() * self.rr(10.0, 55.0) * sz;
            let life = self.rr(0.3, 0.65);
            let vz = self.rr(10.0, 40.0);
            let (c0, c1) = (rgba(1.0, 0.78, 0.3, 0.95), rgba(0.75, 0.15, 0.03, 0.0));
            self.part(at + v * 0.1, z + 5.0, v, vz, life, 9.0 * sz, 20.0 * sz, c0, c1, false);
        }
        for _ in 0..n(5.0) {
            let v = self.rdir() * self.rr(4.0, 22.0) * sz;
            let life = self.rr(0.9, 2.0);
            let g = self.rr(0.1, 0.22);
            let vz = self.rr(14.0, 34.0);
            let (c0, c1) = (rgba(g, g, g, 0.75), rgba(0.3, 0.3, 0.3, 0.0));
            self.part(at + v * 0.3, z + 6.0, v, vz, life, 10.0 * sz, 30.0 * sz, c0, c1, true);
        }
        for _ in 0..n(7.0) {
            let v = self.rdir() * self.rr(60.0, 170.0);
            let (vz, life) = (self.rr(60.0, 150.0), self.rr(0.35, 0.7));
            let (c0, c1) = (rgba(1.0, 0.85, 0.4, 1.0), rgba(1.0, 0.4, 0.1, 0.0));
            self.part(at, z + 5.0, v, vz, life, 3.0, 1.5, c0, c1, false);
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
                        let hit = b + self.rdir() * self.rr(0.0, 5.0);
                        self.part(hit, bz, Vec2::ZERO, 0.0, 0.1, 3.0, 7.0, fade(hot, 0.9), ember, false);
                    }
                }
                Event::Boom { pos, big, air } => self.explode(fpos(pos), z(air), if big { 3.2 } else { 1.0 }),
            }
        }
    }

    fn on_screen(&self, p: Vec2) -> bool {
        let s = self.proj(p, 0.0);
        s.x > -60.0 && s.y > -60.0 && s.x < self.view_w() + 60.0 && s.y < self.screen.y + 60.0
    }

    /// Smoke, dust and other ambience that is not driven by sim events.
    fn ambient(&mut self, dt: f32) {
        let mut spawn: Vec<(Vec2, f32, u8)> = Vec::new();
        for u in self.world.units.values() {
            let on_foot = matches!(u.kind, UnitKind::Trooper | UnitKind::Rocketeer);
            if u.carried || u.pos == u.prev || u.kind.def().air || on_foot || !self.unit_visible(u) {
                continue;
            }
            let at = self.upos(u);
            let loose = self.world.terrain[idx(u.tile.0, u.tile.1)] != Terrain::Rock;
            if loose && self.on_screen(at) && self.rng.next() < dt * 14.0 {
                spawn.push((at, 2.0, 0));
            }
        }
        let structs: Vec<Structure> = self.world.structs.values().cloned().collect();
        for s in &structs {
            let d = s.kind.def();
            let mid = Vec2::new((s.x as f32 + d.w as f32 / 2.0) * PX, (s.y as f32 + d.h as f32 / 2.0) * PX);
            if !self.on_screen(mid) || !self.struct_visible(s) {
                continue;
            }
            let frac = s.hp as f32 / d.hp as f32;
            let top = struct_height(s.kind) * PX;
            if s.kind != StructKind::Wall && frac < 0.5 && self.rng.next() < dt * (9.0 - 12.0 * frac) {
                let (ox, oy) = (self.rr(-0.3, 0.3), self.rr(-0.3, 0.3));
                let off = Vec2::new(ox * d.w as f32 * PX, oy * d.h as f32 * PX);
                spawn.push((mid + off, top, if frac < 0.25 { 2 } else { 1 }));
            }
            if s.kind == StructKind::Refinery && self.rng.next() < dt * 5.0 {
                spawn.push((Vec2::new((s.x as f32 + 0.45) * PX, (s.y as f32 + 0.45) * PX), top + 14.0, 3));
            }
        }
        for (at, z, kind) in spawn {
            let drift = Vec2::new(self.rr(4.0, 12.0), self.rr(-3.0, 3.0));
            match kind {
                0 => {
                    let c = rgba(0.74, 0.60, 0.44, 0.34);
                    let (o, v, life) = (self.rdir() * 5.0, self.rdir() * 6.0, self.rr(0.6, 1.1));
                    self.part(at + o, z, v, 7.0, life, 5.0, 17.0, c, fade(c, 0.0), true)
                }
                3 => {
                    let c = rgba(0.84, 0.84, 0.86, 0.4);
                    let life = self.rr(1.6, 2.6);
                    self.part(at, z, drift, 16.0, life, 4.0, 16.0, c, fade(c, 0.0), true)
                }
                k => {
                    let (c0, c1) = (rgba(0.1, 0.1, 0.1, 0.6), rgba(0.25, 0.25, 0.25, 0.0));
                    let life = self.rr(1.2, 2.2);
                    self.part(at, z, drift, 22.0, life, 7.0, 24.0, c0, c1, true);
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
            let v = self.rdir() * 3.0;
            self.part(p, z, v, 4.0, 0.45, 3.0, 9.0, c, fade(c, 0.0), true);
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
        self.shake_off = if self.shake > 0.0 {
            Vec2::new(self.rr(-1.0, 1.0), self.rr(-1.0, 1.0)) * self.shake / PX
        } else {
            Vec2::ZERO
        };
        let credits = self.world.players[self.me as usize].credits as f32;
        let gap = credits - self.shown_credits;
        self.shown_credits += gap.signum() * gap.abs().min((gap.abs() * 6.0 + 60.0) * dt);
    }

    // ---------- projection ----------

    pub fn view_w(&self) -> f32 {
        self.screen.x - SIDE
    }
    /// Screen pixels per tile.
    pub fn k(&self) -> f32 {
        PX * self.zoom
    }
    /// Ground pixels + height to screen position (matches the 3D camera exactly).
    /// `z` is the height above the terrain at `s`.
    pub fn proj(&self, s: Vec2, z: f32) -> Vec2 {
        let k = self.k();
        let rel = s / PX - self.focus;
        let h = z / PX + self.ground(s / PX);
        Vec2::new(self.view_w() / 2.0 + rel.x * k, self.screen.y / 2.0 + (rel.y * PITCH_S - h * PITCH_C) * k)
    }
    /// Screen position to the ground point under it, in tiles (follows terrain height).
    pub fn ground_at(&self, m: Vec2) -> Vec2 {
        let k = self.k();
        let x = self.focus.x + (m.x - self.view_w() / 2.0) / k;
        let flat = |h: f32| Vec2::new(x, self.focus.y + ((m.y - self.screen.y / 2.0) / k + h * PITCH_C) / PITCH_S);
        let mut p = flat(0.0);
        for _ in 0..4 {
            p = flat(self.ground(p));
        }
        p
    }
    /// Terrain height in tiles at a point given in tiles.
    pub fn ground(&self, t: Vec2) -> f32 {
        if self.corners.is_empty() {
            return 0.0;
        }
        ground_h(&self.corners, t.x, t.y)
    }
    /// World position (tiles) of a point in ground pixels, `z` ground pixels above the terrain.
    pub fn wpos(&self, p: Vec2, z: f32) -> Vec3 {
        Vec3::new(p.x / PX, z / PX + self.ground(p / PX), p.y / PX)
    }
    /// World position of a tile-space point lying on the terrain, lifted by `up` tiles.
    pub fn on_ground(&self, t: Vec2, up: f32) -> Vec3 {
        Vec3::new(t.x, self.ground(t) + up, t.y)
    }
    pub fn upos(&self, u: &Unit) -> Vec2 {
        fpos(u.prev).lerp(fpos(u.pos), self.alpha.clamp(0.0, 1.0))
    }
    pub fn uz(u: &Unit) -> f32 {
        if u.kind.def().air {
            AIR_Z
        } else {
            0.0
        }
    }
    pub fn mouse_tile(&self, m: Vec2) -> (i32, i32) {
        let g = self.ground_at(m);
        (g.x.floor() as i32, g.y.floor() as i32)
    }
    fn center_on(&mut self, t: (i32, i32)) {
        self.focus = Vec2::new(t.0 as f32 + 0.5, t.1 as f32 + 0.5);
    }
    pub fn mm_rect(&self) -> Rect {
        Rect::from_corners(Vec2::new(self.view_w() + 10.0, 10.0), Vec2::new(self.view_w() + 10.0 + MM, 10.0 + MM))
    }
    pub fn icon_rect(&self, i: usize, units: bool) -> Rect {
        let y = ICON_Y + if units { 6.0 * ICON_PITCH + 10.0 } else { 0.0 };
        let p = Vec2::new(self.view_w() + 8.0 + (i % 2) as f32 * 104.0, y + (i / 2) as f32 * ICON_PITCH);
        Rect::from_corners(p, p + Vec2::new(100.0, ICON_H))
    }
    /// Repair / sell buttons, in a slim row above the build grid.
    pub fn tool_rect(&self, i: usize) -> Rect {
        let p = Vec2::new(self.view_w() + 8.0 + i as f32 * 104.0, TOOL_Y);
        Rect::from_corners(p, p + Vec2::new(100.0, 22.0))
    }
    /// Screen rectangle of an Esc-menu entry (centred in the map view).
    pub fn pause_rect(&self, i: usize) -> Rect {
        let top = self.screen.y / 2.0 - 110.0;
        let p = Vec2::new(self.view_w() / 2.0 - 150.0, top + 56.0 + i as f32 * 50.0);
        Rect::from_corners(p, p + Vec2::new(300.0, 42.0))
    }

    /// Ground rectangle (tiles) the view currently shows.
    pub fn view_ground(&self) -> Rect {
        Rect::from_corners(self.ground_at(Vec2::ZERO), self.ground_at(Vec2::new(self.view_w(), self.screen.y)))
    }

    fn pick_unit(&self, m: Vec2, pred: impl Fn(&Unit) -> bool) -> Option<u32> {
        let mut best: Option<(f32, u32)> = None;
        for u in self.world.units.values().filter(|u| !u.carried && pred(u) && self.unit_visible(u)) {
            let d = self.proj(self.upos(u), Self::uz(u) + 8.0).distance(m);
            if d < 16.0 * self.zoom.max(0.7) && best.is_none_or(|b| d < b.0) {
                best = Some((d, u.id));
            }
        }
        best.map(|b| b.1)
    }

    pub fn own_struct_at(&self, tile: (i32, i32)) -> Option<u32> {
        if !inb(tile.0, tile.1) {
            return None;
        }
        let id = self.world.structure_at[idx(tile.0, tile.1)];
        self.world.structs.get(&id).filter(|s| s.owner == self.me).map(|s| s.id)
    }

    pub fn ghost_origin(&self, kind: StructKind, m: Vec2) -> (i32, i32) {
        let d = kind.def();
        let t = self.mouse_tile(m);
        (t.0 - (d.w - 1) / 2, t.1 - (d.h - 1) / 2)
    }

    pub fn ready(&self, kind: StructKind) -> bool {
        matches!(&self.world.players[self.me as usize].construction, Some(c) if c.ready && c.kind == kind)
    }

    pub fn struct_needs(&self, kind: StructKind) -> Option<&'static str> {
        let w = &self.world;
        if !w.has_struct(self.me, StructKind::Yard) {
            return Some(StructKind::Yard.def().name);
        }
        kind.def().requires.filter(|r| !w.has_struct(self.me, *r)).map(|r| r.def().name)
    }

    pub fn unit_needs(&self, kind: UnitKind) -> Option<&'static str> {
        let (w, d) = (&self.world, kind.def());
        [Some(d.made_at), d.requires].into_iter().flatten().find(|r| !w.has_struct(self.me, *r)).map(|r| r.def().name)
    }

    /// Own structures plus enemy structures as last seen.
    pub fn shown_structs(&self) -> impl Iterator<Item = &Structure> {
        self.world.structs.values().filter(|s| s.owner == self.me).chain(self.ghosts.values())
    }

    // ---------- frame ----------

    pub fn frame(&mut self, alpha: f32, dt: f32, inp: &InputSnap) -> Vec<Command> {
        self.alpha = alpha;
        self.time += dt;
        self.screen = inp.screen;
        let mut out = Vec::new();
        let (w, me) = (&self.world, self.me);
        self.sel.retain(|id| w.units.get(id).is_some_and(|u| u.owner == me && !u.carried));
        self.sel_structs.retain(|id| w.structs.get(id).is_some_and(|s| s.owner == me));
        for u in w.units.values() {
            if u.face != (0, 0) {
                let want = (u.face.1 as f32).atan2(u.face.0 as f32);
                let a = self.angles.entry(u.id).or_insert(want);
                let diff = (want - *a + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
                *a += diff * (dt * 12.0).min(1.0);
            }
        }
        self.angles.retain(|id, _| w.units.contains_key(id));
        self.input(dt, inp, &mut out);
        self.ambient(dt);
        self.update_fx(dt);
        out
    }

    fn input(&mut self, dt: f32, inp: &InputSnap, out: &mut Vec<Command>) {
        let m = inp.mouse;
        self.mouse = m;
        let (sw, sh, vw) = (inp.screen.x, inp.screen.y, self.view_w());
        let shift = inp.down(KeyCode::ShiftLeft) || inp.down(KeyCode::ShiftRight);
        let ctrl = inp.down(KeyCode::ControlLeft) || inp.down(KeyCode::ControlRight);
        let alt = inp.down(KeyCode::AltLeft) || inp.down(KeyCode::AltRight);
        self.show_tech = inp.down(KeyCode::Tab);

        // Camera: arrows, screen edge, middle-drag, wheel.
        let sp = 22.5 * dt / self.zoom;
        let inside = inp.hover && m.x >= 0.0 && m.y >= 0.0 && m.x <= sw && m.y <= sh;
        if inp.mouse_down[2] {
            let d = m - self.last_mouse;
            self.focus -= Vec2::new(d.x / self.k(), d.y / (self.k() * PITCH_S));
        }
        self.last_mouse = m;
        if inp.down(KeyCode::ArrowLeft) || (inside && m.x <= 2.0) {
            self.focus.x -= sp;
        }
        if inp.down(KeyCode::ArrowRight) || (inside && m.x >= sw - 3.0) {
            self.focus.x += sp;
        }
        if inp.down(KeyCode::ArrowUp) || (inside && m.y <= 2.0) {
            self.focus.y -= sp / PITCH_S;
        }
        if inp.down(KeyCode::ArrowDown) || (inside && m.y >= sh - 3.0) {
            self.focus.y += sp / PITCH_S;
        }
        if inp.wheel != 0.0 && m.x < vw {
            let before = self.ground_at(m);
            self.zoom = (self.zoom * if inp.wheel > 0.0 { 1.1 } else { 1.0 / 1.1 }).clamp(0.45, 2.2);
            self.focus += before - self.ground_at(m);
        }
        if inp.pressed(KeyCode::Space) {
            let yard = self.world.structs.values().find(|s| s.owner == self.me && s.kind == StructKind::Yard);
            let t = yard.map_or(self.world.starts[self.me as usize], |s| (s.x + 1, s.y + 1));
            self.center_on(t);
        }
        if self.drag.is_none() && inp.mouse_down[0] && self.mm_rect().contains(m) {
            let r = self.mm_rect();
            self.center_on((((m.x - r.min.x) / MM * MAP_W as f32) as i32, ((m.y - r.min.y) / MM * MAP_H as f32) as i32));
        }
        self.clamp_focus();

        // Command keys.
        if !self.sel.is_empty() {
            if inp.pressed(KeyCode::KeyA) {
                self.mode = Mode::AttackMove;
                self.placing = None;
            }
            if inp.pressed(KeyCode::KeyQ) {
                self.mode = Mode::Patrol;
                self.placing = None;
            }
            if inp.pressed(KeyCode::KeyT) {
                self.mode = Mode::Airlift;
                self.placing = None;
            }
            if inp.pressed(KeyCode::KeyF) {
                out.push(Command::Deploy { units: self.sel.clone() });
            }
            if inp.pressed(KeyCode::KeyS) || inp.pressed(KeyCode::KeyX) {
                out.push(Command::Stop { units: self.sel.clone() });
                self.mode = Mode::Normal;
            }
        }
        for (key, mode) in [(KeyCode::KeyR, Mode::Repair), (KeyCode::KeyV, Mode::Sell)] {
            if inp.pressed(key) {
                self.mode = if self.mode == mode { Mode::Normal } else { mode };
                self.placing = None;
            }
        }
        if inp.pressed(KeyCode::Escape) {
            if self.placing.is_some() || self.mode != Mode::Normal {
                self.placing = None;
                self.mode = Mode::Normal;
            } else {
                // Nothing to cancel: the session opens the in-game menu.
                self.menu_request = true;
            }
        }
        self.group_keys(inp, ctrl, shift);

        let (lp, rp) = (inp.mouse_pressed[0], inp.mouse_pressed[1]);
        if m.x >= vw {
            self.cursor = Cursor::Normal;
            if lp || rp {
                self.sidebar_click(m, lp, out);
            }
            if inp.mouse_released[0] {
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
                    out.push(match (self.mode, shift) {
                        (Mode::Airlift, _) => Command::Airlift { units, x: tile.0, y: tile.1 },
                        (Mode::Patrol, false) => Command::Patrol { units, x: tile.0, y: tile.1 },
                        (Mode::Patrol, true) => Command::Queue { units, step: Step::Patrol(tile) },
                        (_, false) => Command::Move { units, x: tile.0, y: tile.1, attack: true },
                        (_, true) => Command::Queue { units, step: Step::AttackMove(tile) },
                    });
                    self.ack(m);
                }
                // Shift keeps the command armed for the next waypoint.
                if !shift || self.mode == Mode::Airlift {
                    self.mode = Mode::Normal;
                }
            } else {
                self.drag = Some(m);
            }
        }
        if inp.mouse_released[0] {
            if let Some(d0) = self.drag.take() {
                if d0.distance(m) < 6.0 {
                    self.click_select(m, shift);
                } else {
                    self.box_select(Rect::from_corners(d0, m), shift);
                }
            }
        }
        if rp {
            if self.placing.is_some() || self.mode != Mode::Normal {
                self.placing = None;
                self.mode = Mode::Normal;
            } else if inb(tile.0, tile.1) {
                let ctx = self.context(m);
                self.order(ctx, tile, shift, ctrl, alt, out);
                if !matches!(ctx, Ctx::None | Ctx::Select) {
                    self.ack(m);
                }
            }
        }
        self.cursor = self.cursor_for(m, alt);
    }

    fn clamp_focus(&mut self) {
        let (hx, hy) = (self.view_w() / 2.0 / self.k(), self.screen.y / 2.0 / (self.k() * PITCH_S));
        let clamp = |v: f32, half: f32, size: f32| {
            let (lo, hi) = (half - 5.0, size - half + 5.0);
            if lo > hi {
                size / 2.0
            } else {
                v.clamp(lo, hi)
            }
        };
        self.focus.x = clamp(self.focus.x, hx, MAP_W as f32);
        self.focus.y = clamp(self.focus.y, hy, MAP_H as f32);
    }

    /// 1-9 select a group (twice quickly: jump to it), Ctrl sets it, Shift adds to it.
    fn group_keys(&mut self, inp: &InputSnap, ctrl: bool, shift: bool) {
        const KEYS: [KeyCode; 9] = [
            KeyCode::Digit1,
            KeyCode::Digit2,
            KeyCode::Digit3,
            KeyCode::Digit4,
            KeyCode::Digit5,
            KeyCode::Digit6,
            KeyCode::Digit7,
            KeyCode::Digit8,
            KeyCode::Digit9,
        ];
        for (i, k) in KEYS.iter().enumerate() {
            if !inp.pressed(*k) {
                continue;
            }
            if ctrl {
                self.groups[i] = (self.sel.clone(), self.sel_structs.clone());
            } else if shift {
                let g = &mut self.groups[i];
                g.0.extend(self.sel.iter().filter(|id| !g.0.contains(id)).copied().collect::<Vec<_>>());
                g.1.extend(self.sel_structs.iter().filter(|id| !g.1.contains(id)).copied().collect::<Vec<_>>());
            } else {
                let (w, me) = (&self.world, self.me);
                let g = &mut self.groups[i];
                g.0.retain(|id| w.units.get(id).is_some_and(|u| u.owner == me));
                g.1.retain(|id| w.structs.get(id).is_some_and(|s| s.owner == me));
                if g.0.is_empty() && g.1.is_empty() {
                    continue;
                }
                self.sel = g.0.clone();
                self.sel_structs = g.1.clone();
                self.mode = Mode::Normal;
                let double = self.last_group_tap.is_some_and(|(j, t)| j == i && self.time - t < 0.35);
                if double {
                    if let Some(c) = self.selection_center() {
                        self.focus = c;
                    }
                }
                self.last_group_tap = Some((i, self.time));
            }
        }
    }

    /// Middle of the current selection, in tiles.
    fn selection_center(&self) -> Option<Vec2> {
        let mut pts: Vec<Vec2> = self.sel.iter().filter_map(|id| self.world.units.get(id)).map(|u| self.upos(u) / PX).collect();
        for s in self.sel_structs.iter().filter_map(|id| self.world.structs.get(id)) {
            let d = s.kind.def();
            pts.push(Vec2::new(s.x as f32 + d.w as f32 / 2.0, s.y as f32 + d.h as f32 / 2.0));
        }
        (!pts.is_empty()).then(|| pts.iter().copied().sum::<Vec2>() / pts.len() as f32)
    }

    fn on_view(&self, p: Vec2) -> bool {
        p.x >= 0.0 && p.y >= 0.0 && p.x < self.view_w() && p.y < self.screen.y
    }

    /// Left click: a unit, else a building. Double click: every one of that kind on screen.
    fn click_select(&mut self, m: Vec2, shift: bool) {
        let me = self.me;
        let double = self.last_click.is_some_and(|(p, t)| p.distance(m) < 8.0 && self.time - t < 0.35);
        self.last_click = Some((m, self.time));
        if let Some(id) = self.pick_unit(m, |u| u.owner == me) {
            if !shift {
                self.sel.clear();
                self.sel_structs.clear();
            }
            if double {
                let kind = self.world.units[&id].kind;
                let same: Vec<u32> = self
                    .world
                    .units
                    .values()
                    .filter(|u| u.owner == me && u.kind == kind && !u.carried && self.on_view(self.proj(self.upos(u), Self::uz(u))))
                    .map(|u| u.id)
                    .collect();
                for id in same {
                    if !self.sel.contains(&id) {
                        self.sel.push(id);
                    }
                }
            } else if let Some(i) = self.sel.iter().position(|s| *s == id) {
                self.sel.remove(i);
            } else {
                self.sel.push(id);
            }
            return;
        }
        let st = self.own_struct_at(self.mouse_tile(m));
        if !shift {
            self.sel.clear();
            self.sel_structs.clear();
        }
        let Some(id) = st else { return };
        if double {
            let kind = self.world.structs[&id].kind;
            let same: Vec<u32> = self
                .world
                .structs
                .values()
                .filter(|s| s.owner == me && s.kind == kind && self.on_view(self.proj(Self::struct_mid(s) * PX, 0.0)))
                .map(|s| s.id)
                .collect();
            for id in same {
                if !self.sel_structs.contains(&id) {
                    self.sel_structs.push(id);
                }
            }
        } else if let Some(i) = self.sel_structs.iter().position(|s| *s == id) {
            self.sel_structs.remove(i);
        } else {
            self.sel_structs.push(id);
        }
    }

    /// Drag box: own units inside; if there are none, own buildings inside.
    fn box_select(&mut self, r: Rect, shift: bool) {
        let me = self.me;
        if !shift {
            self.sel.clear();
            self.sel_structs.clear();
        }
        let units: Vec<u32> = self
            .world
            .units
            .values()
            .filter(|u| u.owner == me && !u.carried && r.contains(self.proj(self.upos(u), Self::uz(u))))
            .map(|u| u.id)
            .collect();
        if !units.is_empty() {
            for id in units {
                if !self.sel.contains(&id) {
                    self.sel.push(id);
                }
            }
            return;
        }
        let structs: Vec<u32> = self
            .world
            .structs
            .values()
            .filter(|s| s.owner == me && !s.kind.is_slab() && r.contains(self.proj(Self::struct_mid(s) * PX, 0.0)))
            .map(|s| s.id)
            .collect();
        for id in structs {
            if !self.sel_structs.contains(&id) {
                self.sel_structs.push(id);
            }
        }
    }

    pub fn struct_mid(s: &Structure) -> Vec2 {
        let d = s.kind.def();
        Vec2::new(s.x as f32 + d.w as f32 / 2.0, s.y as f32 + d.h as f32 / 2.0)
    }

    /// Order acknowledged: a quick ring where the click landed.
    fn ack(&mut self, m: Vec2) {
        let at = self.ground_at(m) * PX;
        self.fx.push(Fx::Ring { at, z: 0.0, t: 0.0, life: 0.25, r: 12.0 });
    }

    fn can_hit(&self, u: &Unit, target_air: bool) -> bool {
        let d = u.kind.def();
        d.damage > 0 && (!target_air || d.hits_air)
    }

    /// What a right click at `m` would do with the current selection. The cursor
    /// shows exactly this, and `order` carries it out, so they cannot disagree.
    pub fn context(&self, m: Vec2) -> Ctx {
        let (w, me) = (&self.world, self.me);
        let tile = self.mouse_tile(m);
        let sel: Vec<&Unit> = self.sel.iter().filter_map(|id| w.units.get(id)).collect();
        if sel.is_empty() {
            let trainers = self.sel_structs.iter().filter_map(|id| w.structs.get(id)).any(|s| TRAIN_AT.contains(&s.kind));
            if trainers {
                return if inb(tile.0, tile.1) { Ctx::Rally } else { Ctx::None };
            }
            let own_unit = self.pick_unit(m, |u| u.owner == me).is_some();
            return if own_unit || self.own_struct_at(tile).is_some() { Ctx::Select } else { Ctx::None };
        }
        if !inb(tile.0, tile.1) {
            return Ctx::None;
        }
        if let Some(id) = self.pick_unit(m, |u| u.owner != me) {
            let air = w.units[&id].kind.def().air;
            if sel.iter().any(|u| self.can_hit(u, air)) {
                return Ctx::Attack(Target::Unit(id));
            }
        }
        let st = w.structs.get(&w.structure_at[idx(tile.0, tile.1)]);
        if let Some(s) = st.filter(|s| s.owner != me && self.ghosts.contains_key(&s.id)) {
            if sel.iter().any(|u| self.can_hit(u, false)) {
                return Ctx::Attack(Target::Struct(s.id));
            }
        }
        let has = |k: UnitKind| sel.iter().any(|u| u.kind == k);
        if has(UnitKind::Carrier) {
            if let Some(cargo) = self.pick_unit(m, |u| u.owner == me && !u.kind.def().air) {
                return Ctx::Pickup(cargo);
            }
        }
        if has(UnitKind::Harvester) {
            if st.is_some_and(|s| s.owner == me && s.kind == StructKind::Refinery) {
                return Ctx::Return;
            }
            if w.terrain[idx(tile.0, tile.1)] == Terrain::Ore {
                return Ctx::Harvest;
            }
        }
        Ctx::Move
    }

    /// Right-click orders. Shift queues, Ctrl moves in formation, Alt attack-moves.
    fn order(&mut self, ctx: Ctx, tile: (i32, i32), shift: bool, ctrl: bool, alt: bool, out: &mut Vec<Command>) {
        let w = &self.world;
        let units = self.sel.clone();
        let (x, y) = tile;
        let of_kind = |k: UnitKind| -> Vec<u32> { units.iter().copied().filter(|id| w.units[id].kind == k).collect() };
        match ctx {
            Ctx::None | Ctx::Select => {}
            Ctx::Attack(target) => out.push(if shift {
                Command::Queue { units, step: Step::Attack(target) }
            } else {
                Command::Attack { units, target }
            }),
            Ctx::Pickup(cargo) => out.push(Command::Pickup { units: of_kind(UnitKind::Carrier), cargo }),
            Ctx::Return => out.push(Command::Return { units: of_kind(UnitKind::Harvester) }),
            Ctx::Harvest => {
                let harvs = of_kind(UnitKind::Harvester);
                let rest: Vec<u32> = units.iter().copied().filter(|id| !harvs.contains(id)).collect();
                if shift {
                    out.push(Command::Queue { units: harvs, step: Step::Harvest(tile) });
                    if !rest.is_empty() {
                        out.push(Command::Queue { units: rest, step: Step::Move(tile) });
                    }
                } else {
                    out.push(Command::Harvest { units: harvs, x, y });
                    if !rest.is_empty() {
                        out.push(Command::Move { units: rest, x, y, attack: false });
                    }
                }
            }
            Ctx::Move => out.push(match (shift, ctrl) {
                (true, _) => Command::Queue { units, step: if alt { Step::AttackMove(tile) } else { Step::Move(tile) } },
                (false, true) => Command::Formation { units, x, y, attack: alt },
                (false, false) => Command::Move { units, x, y, attack: alt },
            }),
            Ctx::Rally => {
                // The sim shares one gather point per building kind.
                let mut kinds: Vec<StructKind> = Vec::new();
                for s in self.sel_structs.iter().filter_map(|id| w.structs.get(id)) {
                    if TRAIN_AT.contains(&s.kind) && !kinds.contains(&s.kind) {
                        kinds.push(s.kind);
                        out.push(Command::Rally { id: s.id, x, y });
                    }
                }
            }
        }
    }

    fn cursor_for(&self, m: Vec2, alt: bool) -> Cursor {
        if self.drag.is_some_and(|d| d.distance(m) >= 6.0) || self.placing.is_some_and(|k| self.ready(k)) {
            return Cursor::Normal;
        }
        let tile = self.mouse_tile(m);
        match self.mode {
            Mode::Repair => {
                let damaged = self.own_struct_at(tile).and_then(|id| self.world.structs.get(&id)).is_some_and(|s| s.hp < s.kind.def().hp);
                return if damaged { Cursor::Repair } else { Cursor::Deny };
            }
            Mode::Sell => return if self.own_struct_at(tile).is_some() { Cursor::Sell } else { Cursor::Deny },
            _ if !inb(tile.0, tile.1) && self.mode != Mode::Normal => return Cursor::Deny,
            Mode::AttackMove => return Cursor::AttackMove,
            Mode::Patrol => return Cursor::Patrol,
            Mode::Airlift => return Cursor::Airlift,
            Mode::Normal => {}
        }
        match self.context(m) {
            Ctx::Attack(_) => Cursor::Attack,
            Ctx::Pickup(_) => Cursor::Pickup,
            Ctx::Return => Cursor::Return,
            Ctx::Harvest => Cursor::Harvest,
            Ctx::Move if alt => Cursor::AttackMove,
            Ctx::Move => Cursor::Move,
            Ctx::Rally => Cursor::Rally,
            Ctx::Select => Cursor::Select,
            Ctx::None if !self.sel.is_empty() || !self.sel_structs.is_empty() => Cursor::Deny,
            Ctx::None => Cursor::Normal,
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

    /// Bottom-left hint line for the current mode / selection.
    pub fn hint(&self) -> String {
        match self.mode {
            Mode::AttackMove => "ATTACK-MOVE  -  left click destination  -  Shift: add waypoint".to_string(),
            Mode::Patrol => "PATROL  -  left click the far end  -  Shift: queue it".to_string(),
            Mode::Airlift => "AIRLIFT  -  left click drop zone".to_string(),
            Mode::Repair => "REPAIR  -  left click own buildings to start / stop  -  right click when done".to_string(),
            Mode::Sell => "SELL  -  left click own buildings  -  right click when done".to_string(),
            Mode::Normal if self.placing.is_some_and(|k| self.ready(k)) => {
                "Left click to place  -  Shift: build another  -  Yellow = no slab, half strength".to_string()
            }
            Mode::Normal if !self.sel.is_empty() => {
                let mcv = self.sel.iter().any(|id| self.world.units.get(id).is_some_and(|u| u.kind == UnitKind::Mcv));
                let extra = if mcv { "   F deploy" } else { "" };
                format!(
                    "{} selected   A attack-move   Q patrol   S stop   T airlift{extra}   |   Shift queue   Ctrl formation   Alt attack",
                    self.sel.len()
                )
            }
            Mode::Normal => {
                let structs: Vec<&Structure> = self.sel_structs.iter().filter_map(|id| self.world.structs.get(id)).collect();
                match structs.as_slice() {
                    [] => String::new(),
                    [s] if TRAIN_AT.contains(&s.kind) => {
                        format!("{}   -   right click the map to set the gather point", s.kind.def().name)
                    }
                    [s] => s.kind.def().name.to_string(),
                    many if many.iter().any(|s| TRAIN_AT.contains(&s.kind)) => {
                        format!("{} buildings   -   right click the map to set their gather point", many.len())
                    }
                    many => format!("{} buildings", many.len()),
                }
            }
        }
    }

    /// Unit centre in tiles, for the minimap.
    pub fn unit_tile_pos(u: &Unit) -> Vec2 {
        let c = center(u.tile);
        Vec2::new(c.0 as f32, c.1 as f32) / TILE as f32
    }
}

/// Approximate roof height of each building model, in tiles (for bars and smoke).
pub fn struct_height(kind: StructKind) -> f32 {
    match kind {
        StructKind::Yard => 1.0,
        StructKind::Factory => 0.95,
        StructKind::TechLab => 0.85,
        StructKind::Refinery => 0.8,
        StructKind::Barracks => 0.7,
        StructKind::Power => 0.75,
        StructKind::Armory => 0.65,
        StructKind::Wall => 0.5,
        StructKind::Turret | StructKind::RocketTurret => 0.6,
        StructKind::Airpad => 0.2,
        StructKind::Slab | StructKind::Slab4 => 0.05,
    }
}
