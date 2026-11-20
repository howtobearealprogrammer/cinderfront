//! Deterministic game simulation. Integer math only, ordered containers only:
//! every peer runs this with the same commands and must reach the same state.

use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, VecDeque};

pub const TILE: i32 = 256;
pub const MAP_W: i32 = 96;
pub const MAP_H: i32 = 96;
pub const TICKS_PER_SEC: u32 = 20;
pub const HARVEST_CAP: i32 = 500;
pub const START_CREDITS: i32 = 5000;
/// Max empty tiles between a new structure and an existing own structure.
pub const BUILD_GAP: i32 = 3;
const BUILD_RATE: i32 = 6;

pub type Tile = (i32, i32);
pub type Pos = (i32, i32);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Terrain {
    Dust,
    Rock,
    Ore,
    Cliff,
    /// Steep edge of a plateau: blocks ground units and building.
    Bank,
    /// Slope between two levels: walkable, only walls can be built on it.
    Ramp,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum UnitKind {
    Trooper,
    Rocketeer,
    Buggy,
    Tank,
    Harvester,
    Carrier,
    Gunship,
    Explorer,
    Mcv,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum StructKind {
    Yard,
    Slab,
    Slab4,
    Power,
    Refinery,
    Barracks,
    Factory,
    Airpad,
    Wall,
    Turret,
    Armory,
    TechLab,
    RocketTurret,
}

pub struct UnitDef {
    pub name: &'static str,
    pub cost: i32,
    pub hp: i32,
    pub speed: i32,
    pub range: i32,
    pub damage: i32,
    pub cooldown: u16,
    pub hits_air: bool,
    pub air: bool,
    pub vs_struct: i32,
    pub made_at: StructKind,
    /// Tech building that must also stand before this can be trained.
    pub requires: Option<StructKind>,
}

pub struct StructDef {
    pub name: &'static str,
    pub cost: i32,
    pub hp: i32,
    pub w: i32,
    pub h: i32,
    pub power: i32,
    pub requires: Option<StructKind>,
}

impl UnitKind {
    pub const ALL: [UnitKind; 9] = [
        UnitKind::Trooper,
        UnitKind::Rocketeer,
        UnitKind::Buggy,
        UnitKind::Explorer,
        UnitKind::Tank,
        UnitKind::Harvester,
        UnitKind::Mcv,
        UnitKind::Carrier,
        UnitKind::Gunship,
    ];

    /// How far this unit lifts the fog of war, in tiles.
    pub fn sight(self) -> i32 {
        match self {
            UnitKind::Explorer => 11,
            UnitKind::Gunship => 8,
            UnitKind::Harvester | UnitKind::Mcv => 4,
            UnitKind::Trooper => 5,
            _ => 6,
        }
    }

    /// Supply pool this unit counts against and the points it takes. The MCV is exempt.
    pub fn supply(self) -> Option<(Pool, i32)> {
        match self {
            UnitKind::Trooper => Some((Pool::Infantry, 1)),
            UnitKind::Rocketeer => Some((Pool::Infantry, 3)),
            UnitKind::Explorer => Some((Pool::Ground, 1)),
            UnitKind::Buggy => Some((Pool::Ground, 3)),
            UnitKind::Tank => Some((Pool::Ground, 5)),
            UnitKind::Harvester => Some((Pool::Harvest, 1)),
            UnitKind::Carrier | UnitKind::Gunship => Some((Pool::Air, 5)),
            UnitKind::Mcv => None,
        }
    }

    pub fn def(self) -> UnitDef {
        use StructKind::*;
        let d = |name, cost, hp, speed, range, damage, cooldown, hits_air, air, vs_struct, made_at, requires| UnitDef {
            name,
            cost,
            hp,
            speed,
            range,
            damage,
            cooldown,
            hits_air,
            air,
            vs_struct,
            made_at,
            requires,
        };
        match self {
            UnitKind::Trooper => d("Trooper", 60, 60, 20, 4, 6, 10, true, false, 40, Barracks, None),
            UnitKind::Rocketeer => d("Rocketeer", 120, 70, 18, 6, 30, 40, true, false, 100, Barracks, Some(Armory)),
            UnitKind::Buggy => d("Buggy", 250, 150, 56, 4, 8, 6, true, false, 40, Factory, None),
            UnitKind::Tank => d("Tank", 600, 450, 30, 5, 45, 35, false, false, 100, Factory, Some(TechLab)),
            UnitKind::Harvester => d("Harvester", 700, 500, 24, 0, 0, 0, false, false, 0, Factory, Some(Refinery)),
            UnitKind::Carrier => d("Carrier", 500, 250, 72, 0, 0, 0, false, true, 0, Airpad, None),
            UnitKind::Gunship => d("Gunship", 800, 220, 80, 4, 12, 6, true, true, 60, Airpad, Some(TechLab)),
            UnitKind::Explorer => d("Explorer", 200, 100, 70, 0, 0, 0, false, false, 0, Factory, None),
            UnitKind::Mcv => d("MCV", 2500, 800, 16, 0, 0, 0, false, false, 0, Factory, Some(TechLab)),
        }
    }
}

/// Supply pools. Every unit kind except the MCV draws points from one pool, and each
/// standing building of the pool's kind raises that pool's cap.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pool {
    Infantry,
    Ground,
    Air,
    Harvest,
}

impl Pool {
    /// The building that raises this pool's cap, and the points each one adds.
    pub fn source(self) -> (StructKind, i32) {
        match self {
            Pool::Infantry => (StructKind::Barracks, 50),
            Pool::Ground => (StructKind::Factory, 50),
            Pool::Air => (StructKind::Airpad, 20),
            Pool::Harvest => (StructKind::Refinery, 2),
        }
    }
}

/// Credits a train queue spends per tick with `n` production buildings of its kind:
/// each building after the first cuts the time by a third (rate x 3/2).
pub fn train_rate(base: i32, n: usize) -> i32 {
    // Past ten buildings the bonus stops growing, which also keeps the maths in range.
    let k = n.clamp(1, 10) as u32 - 1;
    base * 3i32.pow(k) / 2i32.pow(k)
}

pub const TRAIN_AT: [StructKind; 3] = [StructKind::Barracks, StructKind::Factory, StructKind::Airpad];

impl StructKind {
    /// Everything the player can build, in sidebar order.
    pub const BUILDABLE: [StructKind; 12] = [
        StructKind::Slab,
        StructKind::Slab4,
        StructKind::Power,
        StructKind::Refinery,
        StructKind::Barracks,
        StructKind::Armory,
        StructKind::Factory,
        StructKind::TechLab,
        StructKind::Airpad,
        StructKind::Wall,
        StructKind::Turret,
        StructKind::RocketTurret,
    ];

    pub fn def(self) -> StructDef {
        use StructKind::*;
        let d = |name, cost, hp, w, h, power, requires| StructDef { name, cost, hp, w, h, power, requires };
        match self {
            Yard => d("Command Yard", 0, 3000, 3, 3, 30, None),
            Slab => d("Slab", 20, 0, 1, 1, 0, None),
            Slab4 => d("Slab 2x2", 70, 0, 2, 2, 0, None),
            Power => d("Reactor", 300, 800, 2, 2, 100, None),
            Refinery => d("Refinery", 1200, 1500, 3, 2, -30, Some(Power)),
            Barracks => d("Barracks", 400, 1000, 2, 2, -20, Some(Power)),
            Factory => d("Factory", 1000, 1600, 3, 2, -40, Some(Refinery)),
            Airpad => d("Airpad", 900, 1000, 2, 2, -40, Some(Factory)),
            Wall => d("Wall", 40, 600, 1, 1, 0, Some(Barracks)),
            Turret => d("Turret", 500, 900, 1, 1, -20, Some(Armory)),
            Armory => d("Armory", 600, 1000, 2, 2, -20, Some(Barracks)),
            TechLab => d("Tech Lab", 1500, 1200, 2, 2, -50, Some(Factory)),
            RocketTurret => d("Rocket Turret", 900, 1000, 1, 1, -40, Some(TechLab)),
        }
    }

    pub fn is_slab(self) -> bool {
        matches!(self, StructKind::Slab | StructKind::Slab4)
    }

    /// Defensive weapon, if any: (range in tiles, damage, cooldown in ticks).
    pub fn weapon(self) -> Option<(i32, i32, u16)> {
        match self {
            StructKind::Turret => Some((6, 25, 12)),
            StructKind::RocketTurret => Some((9, 70, 30)),
            _ => None,
        }
    }

    /// How far this building lifts the fog of war, in tiles.
    pub fn sight(self) -> i32 {
        match self {
            StructKind::Yard => 8,
            StructKind::Turret => 8,
            StructKind::RocketTurret => 10,
            StructKind::Wall => 2,
            _ => 6,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Target {
    Unit(u32),
    Struct(u32),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Command {
    StartBuild(StructKind),
    CancelBuild,
    Place { kind: StructKind, x: i32, y: i32 },
    Train(UnitKind),
    CancelTrain(UnitKind),
    Move { units: Vec<u32>, x: i32, y: i32, attack: bool },
    Attack { units: Vec<u32>, target: Target },
    Harvest { units: Vec<u32>, x: i32, y: i32 },
    Return { units: Vec<u32> },
    /// First free carrier in `units` picks up `cargo`.
    Pickup { units: Vec<u32>, cargo: u32 },
    /// Free carriers fetch each of `units` and drop them around (x, y).
    Airlift { units: Vec<u32>, x: i32, y: i32 },
    Stop { units: Vec<u32> },
    /// Toggle repairs on an own structure.
    Repair { id: u32 },
    Sell { id: u32 },
    /// Set the gather point for units produced by this kind of building.
    Rally { id: u32, x: i32, y: i32 },
    /// Unpack MCVs into Command Yards where they stand.
    Deploy { units: Vec<u32> },
    /// Attack-move back and forth between where each unit stands and (x, y).
    Patrol { units: Vec<u32>, x: i32, y: i32 },
    /// Move keeping the group's shape, at the speed of its slowest member.
    Formation { units: Vec<u32>, x: i32, y: i32, attack: bool },
    /// Append a step to each unit's order queue (shift-click). Idle units start it at once.
    Queue { units: Vec<u32>, step: Step },
}

/// One queued order.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Step {
    Move(Tile),
    AttackMove(Tile),
    Attack(Target),
    Patrol(Tile),
    Harvest(Tile),
}

/// Most queued steps a unit holds.
pub const MAX_QUEUE: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Order {
    Idle,
    Move(Tile),
    AttackMove(Tile),
    Attack(Target),
    Harvest(Option<Tile>),
    Return,
    Pickup { cargo: u32, drop: Option<Tile> },
    Drop(Tile),
    /// Attack-moving towards `to`; on arrival it swaps ends.
    Patrol { from: Tile, to: Tile },
}

#[derive(Clone, Debug)]
pub struct Unit {
    pub id: u32,
    pub owner: u8,
    pub kind: UnitKind,
    pub pos: Pos,
    /// Position at the previous tick, for render interpolation.
    pub prev: Pos,
    pub tile: Tile,
    pub face: (i32, i32),
    pub hp: i32,
    pub order: Order,
    /// Reversed: last element is the next step.
    pub path: Vec<Tile>,
    pub cooldown: u16,
    blocked: u8,
    stuck: u8,
    hold_until: u32,
    pub cargo: Option<u32>,
    pub carried: bool,
    pub load: i32,
    /// Ore patch a harvester was sent to; it keeps returning there until the patch runs dry.
    pub home_ore: Option<Tile>,
    /// Orders to carry out after the current one (shift-click waypoints).
    pub queue: VecDeque<Step>,
    /// Formation moves hold everyone to the slowest member's speed (0 = no cap).
    pub speed_cap: i32,
}

#[derive(Clone, Debug)]
pub struct Structure {
    pub id: u32,
    pub owner: u8,
    pub kind: StructKind,
    pub x: i32,
    pub y: i32,
    pub hp: i32,
    pub cooldown: u16,
    pub aim: (i32, i32),
    pub repairing: bool,
    /// Fractional credits owed for repairs so far.
    pub repair_acc: i32,
    /// Gather point: new units head here.
    pub rally: Option<Tile>,
}

#[derive(Clone, Debug)]
pub struct Construction {
    pub kind: StructKind,
    pub spent: i32,
    pub ready: bool,
}

#[derive(Clone, Debug, Default)]
pub struct TrainQueue {
    pub items: VecDeque<UnitKind>,
    pub spent: i32,
}

#[derive(Clone, Debug)]
pub struct Player {
    pub credits: i32,
    pub power_made: i32,
    pub power_used: i32,
    pub construction: Option<Construction>,
    pub queues: [TrainQueue; 3],
    pub defeated: bool,
}

/// Transient, render-only. Cleared at the start of every tick.
#[derive(Clone, Copy, Debug)]
pub enum Event {
    Shot { from: Pos, to: Pos, from_air: bool, to_air: bool, heavy: bool },
    Boom { pos: Pos, big: bool, air: bool },
}

pub struct World {
    pub tick: u32,
    pub terrain: Vec<Terrain>,
    /// Height level per tile (0 low .. 2 high). Banks carry their plateau's level,
    /// ramps the lower level they lead to. Fixed at map generation.
    pub level: Vec<u8>,
    pub ore: Vec<u16>,
    pub slab: Vec<bool>,
    pub structure_at: Vec<u32>,
    pub occ: Vec<u32>,
    pub units: BTreeMap<u32, Unit>,
    pub structs: BTreeMap<u32, Structure>,
    pub players: Vec<Player>,
    pub starts: Vec<Tile>,
    pub events: Vec<Event>,
    pub winner: Option<u8>,
    next_id: u32,
}

pub fn idx(x: i32, y: i32) -> usize {
    (y * MAP_W + x) as usize
}
pub fn inb(x: i32, y: i32) -> bool {
    x >= 0 && y >= 0 && x < MAP_W && y < MAP_H
}
pub fn center(t: Tile) -> Pos {
    (t.0 * TILE + TILE / 2, t.1 * TILE + TILE / 2)
}
fn d2(a: Pos, b: Pos) -> i64 {
    let (dx, dy) = ((a.0 - b.0) as i64, (a.1 - b.1) as i64);
    dx * dx + dy * dy
}
fn range2(tiles: i32) -> i64 {
    let r = (tiles * TILE + TILE / 2) as i64;
    r * r
}

fn h2(seed: u32, x: i32, y: i32) -> u32 {
    let mut h = seed ^ (x as u32).wrapping_mul(0x9E37_79B1) ^ (y as u32).wrapping_mul(0x85EB_CA77);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    h
}

fn noise(seed: u32, x: i32, y: i32, cell: i32) -> i32 {
    let (cx, cy, fx, fy) = (x / cell, y / cell, x % cell, y % cell);
    let v = |a: i32, b: i32| (h2(seed, a, b) & 255) as i32;
    let top = v(cx, cy) * (cell - fx) + v(cx + 1, cy) * fx;
    let bot = v(cx, cy + 1) * (cell - fx) + v(cx + 1, cy + 1) * fx;
    (top * (cell - fy) + bot * fy) / (cell * cell)
}

/// One quadrant is generated from noise and mirrored four ways, so every start is equal.
///
/// Height: each start sits on a middle-level plateau with two ramps (east and south
/// in quadrant terms, toward the ore fields); the map centre is a high plateau with
/// one ramp facing each base. Every level change is a ring of `Bank` tiles except
/// at ramps, so pathfinding needs no height rule of its own.
fn gen_map(seed: u32) -> (Vec<Terrain>, Vec<u16>, Vec<u8>) {
    let q = |v: i32| if v < MAP_W / 2 { v } else { MAP_W - 1 - v };
    let n = (MAP_W * MAP_H) as usize;
    // Levels first: they only depend on the seed.
    let mut level = vec![0u8; n];
    for y in 0..MAP_H {
        for x in 0..MAP_W {
            let (qx, qy) = (q(x), q(y));
            let (cx, cy) = (2 * qx - (MAP_W - 1), 2 * qy - (MAP_H - 1));
            let home_r = 9 + noise(seed + 21, qx, qy, 4) * 3 / 256;
            let mid_r = 7 + noise(seed + 22, qx, qy, 4) * 3 / 256;
            // The home plateau runs out to the map edges; only its inner sides drop.
            level[idx(x, y)] = if qx <= 12 + home_r && qy <= 12 + home_r {
                1
            } else if cx * cx + cy * cy <= 4 * mid_r * mid_r {
                2
            } else {
                0
            };
        }
    }
    let lower_neighbour = |x: i32, y: i32| -> Option<u8> {
        let l = level[idx(x, y)];
        let mut low: Option<u8> = None;
        for dy in -1..=1 {
            for dx in -1..=1 {
                if inb(x + dx, y + dy) && level[idx(x + dx, y + dy)] < l {
                    let m = level[idx(x + dx, y + dy)];
                    low = Some(low.map_or(m, |v: u8| v.min(m)));
                }
            }
        }
        low
    };
    // Low ground within 2 tiles of a plateau stays clear of outcrops, so ramps and
    // the walk around plateaus can never be blocked.
    let near_high = |x: i32, y: i32| {
        (-2..=2).any(|dy| (-2..=2).any(|dx| inb(x + dx, y + dy) && level[idx(x + dx, y + dy)] > 0))
    };
    let mut cliff_t = 200;
    loop {
        let mut terrain = vec![Terrain::Dust; n];
        let mut ore = vec![0u16; n];
        let mut lv = level.clone();
        for y in 0..MAP_H {
            for x in 0..MAP_W {
                let (qx, qy) = (q(x), q(y));
                let i = idx(x, y);
                let ds = (qx - 12).abs().max((qy - 12).abs());
                let near = |cx: i32, cy: i32, r2: i32| (qx - cx) * (qx - cx) + (qy - cy) * (qy - cy) <= r2;
                let rock = noise(seed, qx, qy, 8) * 3 / 4 + noise(seed + 1, qx, qy, 3) / 4 > 138;
                let t = if let Some(low) = lower_neighbour(x, y) {
                    let ramp = match level[i] {
                        1 => ((qy - 12).abs() <= 1 && qx > 12) || ((qx - 12).abs() <= 1 && qy > 12),
                        _ => (qx - qy).abs() <= 1,
                    };
                    if ramp {
                        lv[i] = low;
                        Terrain::Ramp
                    } else {
                        Terrain::Bank
                    }
                } else if level[i] == 1 {
                    if ds <= 7 || rock {
                        Terrain::Rock
                    } else {
                        Terrain::Dust
                    }
                } else if level[i] == 2 {
                    Terrain::Rock
                } else if near(26, 13, 14) || near(13, 26, 14) || near(35, 35, 10) {
                    Terrain::Ore
                } else if !near_high(x, y) && ds > 13 && noise(seed + 13, qx, qy, 5) > cliff_t {
                    Terrain::Cliff
                } else if rock {
                    Terrain::Rock
                } else if noise(seed + 7, qx, qy, 6) > 178 {
                    Terrain::Ore
                } else {
                    Terrain::Dust
                };
                let edge = x == 0 || y == 0 || x == MAP_W - 1 || y == MAP_H - 1;
                terrain[i] = if edge && t == Terrain::Cliff { Terrain::Dust } else { t };
                if terrain[i] == Terrain::Ore {
                    ore[i] = 400 + (h2(seed + 99, qx, qy) % 600) as u16;
                }
            }
        }
        // Start must reach map centre; by symmetry that connects all starts.
        let mut seen = vec![false; n];
        let mut stack = vec![(12, 12)];
        seen[idx(12, 12)] = true;
        while let Some((x, y)) = stack.pop() {
            for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                let (nx, ny) = (x + dx, y + dy);
                if inb(nx, ny) && !seen[idx(nx, ny)] && !matches!(terrain[idx(nx, ny)], Terrain::Cliff | Terrain::Bank) {
                    seen[idx(nx, ny)] = true;
                    stack.push((nx, ny));
                }
            }
        }
        if seen[idx(MAP_W / 2, MAP_H / 2)] || cliff_t > 255 {
            return (terrain, ore, lv);
        }
        cliff_t += 10;
    }
}

impl World {
    pub fn new(seed: u32, num_players: u8) -> World {
        let (terrain, ore, level) = gen_map(seed);
        let n = terrain.len();
        let (a, b) = (12, MAP_W - 13);
        let mut w = World {
            tick: 0,
            terrain,
            level,
            ore,
            slab: vec![false; n],
            structure_at: vec![0; n],
            occ: vec![0; n],
            units: BTreeMap::new(),
            structs: BTreeMap::new(),
            players: Vec::new(),
            starts: vec![(a, a), (b, b), (b, a), (a, b)],
            events: Vec::new(),
            winner: None,
            next_id: 1,
        };
        for p in 0..num_players.min(4) {
            w.players.push(Player {
                credits: START_CREDITS,
                power_made: 0,
                power_used: 0,
                construction: None,
                queues: Default::default(),
                defeated: false,
            });
            let (cx, cy) = w.starts[p as usize];
            for y in cy - 3..=cy + 3 {
                for x in cx - 3..=cx + 3 {
                    w.slab[idx(x, y)] = true;
                }
            }
            w.create_struct(p, StructKind::Yard, cx - 1, cy - 1, StructKind::Yard.def().hp);
            w.new_unit(p, UnitKind::Trooper, (cx - 2, cy + 2));
            w.new_unit(p, UnitKind::Trooper, (cx, cy + 2));
            w.new_unit(p, UnitKind::Buggy, (cx + 2, cy + 2));
        }
        w
    }

    // ---------- queries ----------

    pub fn walkable(&self, x: i32, y: i32) -> bool {
        inb(x, y) && !matches!(self.terrain[idx(x, y)], Terrain::Cliff | Terrain::Bank) && self.structure_at[idx(x, y)] == 0
    }

    /// Height level of the tile under `p`.
    pub fn level_at(&self, p: Pos) -> i32 {
        let (x, y) = (p.0.div_euclid(TILE), p.1.div_euclid(TILE));
        if inb(x, y) {
            self.level[idx(x, y)] as i32
        } else {
            0
        }
    }

    /// Extra range in tiles for shooting down from higher ground (none involving aircraft).
    pub fn height_bonus(&self, from: Pos, from_air: bool, to: Pos, to_air: bool) -> i32 {
        if from_air || to_air {
            0
        } else {
            (self.level_at(from) - self.level_at(to)).max(0)
        }
    }

    pub fn has_struct(&self, p: u8, kind: StructKind) -> bool {
        self.first_struct(p, kind).is_some()
    }

    fn first_struct(&self, p: u8, kind: StructKind) -> Option<u32> {
        self.structs.values().find(|s| s.owner == p && s.kind == kind).map(|s| s.id)
    }

    pub fn can_build(&self, p: u8, kind: StructKind) -> bool {
        kind != StructKind::Yard
            && self.has_struct(p, StructKind::Yard)
            && kind.def().requires.map_or(true, |r| self.has_struct(p, r))
    }

    pub fn can_train(&self, p: u8, kind: UnitKind) -> bool {
        let d = kind.def();
        self.has_struct(p, d.made_at) && d.requires.map_or(true, |r| self.has_struct(p, r))
    }

    /// Points committed in a pool (living units plus everything queued) and the pool's cap.
    pub fn supply(&self, p: u8, pool: Pool) -> (i32, i32) {
        let pts = |k: UnitKind| k.supply().filter(|s| s.0 == pool).map_or(0, |s| s.1);
        let live: i32 = self.units.values().filter(|u| u.owner == p && u.hp > 0).map(|u| pts(u.kind)).sum();
        let queued: i32 = self.players[p as usize].queues.iter().flat_map(|q| q.items.iter()).map(|k| pts(*k)).sum();
        let (kind, per) = pool.source();
        let n = self.structs.values().filter(|s| s.owner == p && s.kind == kind).count() as i32;
        (live + queued, n * per)
    }

    /// Whether one more `kind` fits under its pool's cap.
    pub fn has_supply(&self, p: u8, kind: UnitKind) -> bool {
        kind.supply().map_or(true, |(pool, pts)| {
            let (used, cap) = self.supply(p, pool);
            used + pts <= cap
        })
    }

    pub fn place_tile_ok(&self, kind: StructKind, x: i32, y: i32) -> bool {
        if !inb(x, y) {
            return false;
        }
        let i = idx(x, y);
        (self.terrain[i] == Terrain::Rock || (self.terrain[i] == Terrain::Ramp && kind == StructKind::Wall))
            && self.structure_at[i] == 0
            && self.occ[i] == 0
            && !(kind.is_slab() && self.slab[i])
    }

    pub fn near_own(&self, p: u8, kind: StructKind, x: i32, y: i32) -> bool {
        let d = kind.def();
        self.structs.values().any(|s| {
            let sd = s.kind.def();
            s.owner == p
                && s.kind != StructKind::Wall
                && (s.x - (x + d.w)).max(x - (s.x + sd.w)).max(s.y - (y + d.h)).max(y - (s.y + sd.h)) <= BUILD_GAP
        })
    }

    pub fn can_place(&self, p: u8, kind: StructKind, x: i32, y: i32) -> bool {
        let d = kind.def();
        if !self.near_own(p, kind, x, y) {
            return false;
        }
        let mut tiles = (0..d.h).flat_map(|dy| (0..d.w).map(move |dx| (x + dx, y + dy)));
        if kind == StructKind::Slab4 {
            tiles.any(|(tx, ty)| self.place_tile_ok(kind, tx, ty))
        } else {
            tiles.all(|(tx, ty)| self.place_tile_ok(kind, tx, ty))
        }
    }

    /// (nearest point on target, aim point, owner, airborne, tile)
    fn target_info(&self, from: Pos, t: Target) -> Option<(Pos, Pos, u8, bool, Tile)> {
        match t {
            Target::Unit(id) => {
                let u = self.units.get(&id)?;
                if u.hp <= 0 || u.carried {
                    return None;
                }
                Some((u.pos, u.pos, u.owner, u.kind.def().air, u.tile))
            }
            Target::Struct(id) => {
                let s = self.structs.get(&id)?;
                if s.hp <= 0 {
                    return None;
                }
                let d = s.kind.def();
                let near = (
                    from.0.clamp(s.x * TILE, (s.x + d.w) * TILE - 1),
                    from.1.clamp(s.y * TILE, (s.y + d.h) * TILE - 1),
                );
                let mid = (s.x * TILE + d.w * TILE / 2, s.y * TILE + d.h * TILE / 2);
                Some((near, mid, s.owner, false, (s.x + d.w / 2, s.y + d.h / 2)))
            }
        }
    }

    fn acquire(&self, owner: u8, from: Pos, from_air: bool, range: i32, hits_air: bool) -> Option<Target> {
        let mut best: Option<(i64, Target)> = None;
        for u in self.units.values() {
            let air = u.kind.def().air;
            if u.owner == owner || u.hp <= 0 || u.carried || (air && !hits_air) {
                continue;
            }
            let d = d2(from, u.pos);
            if d <= range2(range + self.height_bonus(from, from_air, u.pos, air)) && best.map_or(true, |b| d < b.0) {
                best = Some((d, Target::Unit(u.id)));
            }
        }
        if best.is_none() {
            for s in self.structs.values() {
                if s.owner == owner || s.hp <= 0 || s.kind == StructKind::Wall {
                    continue;
                }
                let (near, ..) = self.target_info(from, Target::Struct(s.id))?;
                let d = d2(from, near);
                if d <= range2(range + self.height_bonus(from, from_air, near, false)) && best.map_or(true, |b| d < b.0) {
                    best = Some((d, Target::Struct(s.id)));
                }
            }
        }
        best.map(|b| b.1)
    }

    fn free_tile_near(&self, t: Tile, max_r: i32) -> Option<Tile> {
        for r in 0..=max_r {
            for dy in -r..=r {
                for dx in -r..=r {
                    if dx.abs().max(dy.abs()) != r {
                        continue;
                    }
                    let (x, y) = (t.0 + dx, t.1 + dy);
                    if self.walkable(x, y) && self.occ[idx(x, y)] == 0 {
                        return Some((x, y));
                    }
                }
            }
        }
        None
    }

    fn spread(&self, t: Tile, n: usize) -> Vec<Tile> {
        let mut out = Vec::with_capacity(n);
        for r in 0..=12i32 {
            for dy in -r..=r {
                for dx in -r..=r {
                    if dx.abs().max(dy.abs()) != r {
                        continue;
                    }
                    if out.len() >= n {
                        return out;
                    }
                    if self.walkable(t.0 + dx, t.1 + dy) {
                        out.push((t.0 + dx, t.1 + dy));
                    }
                }
            }
        }
        out
    }

    fn nearest_ore(&self, from: Tile, id: u32) -> Option<Tile> {
        let mut best: Option<(i32, Tile)> = None;
        for y in 0..MAP_H {
            for x in 0..MAP_W {
                let i = idx(x, y);
                if self.terrain[i] == Terrain::Ore && (self.occ[i] == 0 || self.occ[i] == id) {
                    let d = (x - from.0) * (x - from.0) + (y - from.1) * (y - from.1);
                    if best.map_or(true, |b| d < b.0) {
                        best = Some((d, (x, y)));
                    }
                }
            }
        }
        best.map(|b| b.1)
    }

    /// A* over tiles. Returns the path reversed (last = first step), excluding `start`.
    /// If the goal is unreachable, leads to the reachable tile closest to it.
    pub fn find_path(&self, start: Tile, goal: Tile, avoid_units: bool) -> Vec<Tile> {
        if start == goal || !inb(goal.0, goal.1) || !inb(start.0, start.1) {
            return Vec::new();
        }
        let n = (MAP_W * MAP_H) as usize;
        let heur = |x: i32, y: i32| {
            let (dx, dy) = ((x - goal.0).abs(), (y - goal.1).abs());
            (10 * dx.max(dy) + 4 * dx.min(dy)) as u32
        };
        let mut g = vec![u32::MAX; n];
        let mut from = vec![u32::MAX; n];
        let mut closed = vec![false; n];
        let mut heap = BinaryHeap::new();
        let si = idx(start.0, start.1);
        let gi = idx(goal.0, goal.1);
        g[si] = 0;
        let h0 = heur(start.0, start.1);
        heap.push(Reverse((h0, h0, si as u32)));
        let mut best = (h0, si);
        while let Some(Reverse((_, hh, i))) = heap.pop() {
            let i = i as usize;
            if closed[i] {
                continue;
            }
            closed[i] = true;
            if i == gi {
                best = (0, i);
                break;
            }
            if hh < best.0 {
                best = (hh, i);
            }
            let (x, y) = (i as i32 % MAP_W, i as i32 / MAP_W);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let (nx, ny) = (x + dx, y + dy);
                    if !self.walkable(nx, ny) {
                        continue;
                    }
                    if dx != 0 && dy != 0 && !(self.walkable(x + dx, y) && self.walkable(x, y + dy)) {
                        continue;
                    }
                    let ni = idx(nx, ny);
                    if closed[ni] || (avoid_units && self.occ[ni] != 0) {
                        continue;
                    }
                    let ng = g[i] + if dx != 0 && dy != 0 { 14 } else { 10 };
                    if ng < g[ni] {
                        g[ni] = ng;
                        from[ni] = i as u32;
                        let h = heur(nx, ny);
                        heap.push(Reverse((ng + h, h, ni as u32)));
                    }
                }
            }
        }
        let mut path = Vec::new();
        let mut i = best.1;
        while i != si {
            path.push((i as i32 % MAP_W, i as i32 / MAP_W));
            i = from[i] as usize;
        }
        path
    }

    pub fn hash(&self) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        let mut add = |v: i64| {
            h ^= v as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        };
        add(self.tick as i64);
        for u in self.units.values() {
            for v in [u.id as i64, u.pos.0 as i64, u.pos.1 as i64, u.hp as i64, u.load as i64, u.queue.len() as i64, u.speed_cap as i64] {
                add(v);
            }
        }
        for s in self.structs.values() {
            add(s.id as i64);
            add(s.hp as i64);
        }
        for p in &self.players {
            add(p.credits as i64);
        }
        h
    }

    // ---------- mutation helpers ----------

    pub fn create_struct(&mut self, owner: u8, kind: StructKind, x: i32, y: i32, hp: i32) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        let d = kind.def();
        for ty in y..y + d.h {
            for tx in x..x + d.w {
                self.structure_at[idx(tx, ty)] = id;
            }
        }
        self.structs.insert(id, Structure { id, owner, kind, x, y, hp, cooldown: 0, aim: (0, 1), repairing: false, repair_acc: 0, rally: None });
        id
    }

    pub fn new_unit(&mut self, owner: u8, kind: UnitKind, tile: Tile) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        let d = kind.def();
        if !d.air {
            self.occ[idx(tile.0, tile.1)] = id;
        }
        let pos = center(tile);
        self.units.insert(
            id,
            Unit {
                id,
                owner,
                kind,
                pos,
                prev: pos,
                tile,
                face: (0, 1),
                hp: d.hp,
                order: if kind == UnitKind::Harvester { Order::Harvest(None) } else { Order::Idle },
                path: Vec::new(),
                cooldown: 0,
                blocked: 0,
                stuck: 0,
                hold_until: 0,
                cargo: None,
                carried: false,
                load: 0,
                home_ore: None,
                queue: VecDeque::new(),
                speed_cap: 0,
            },
        );
        id
    }

    /// A detached unit for UI previews; never enters the world.
    pub fn preview_unit(kind: UnitKind, owner: u8) -> Unit {
        Unit {
            id: 0,
            owner,
            kind,
            pos: (0, 0),
            prev: (0, 0),
            tile: (0, 0),
            face: (1, 1),
            hp: kind.def().hp,
            order: Order::Idle,
            path: Vec::new(),
            cooldown: 0,
            blocked: 0,
            stuck: 0,
            hold_until: 0,
            cargo: None,
            carried: false,
            load: 0,
            home_ore: None,
            queue: VecDeque::new(),
            speed_cap: 0,
        }
    }

    fn spawn_at_struct(&mut self, owner: u8, kind: UnitKind, sid: u32) -> Option<u32> {
        let s = self.structs.get(&sid)?;
        let d = s.kind.def();
        let rally = s.rally;
        let air = kind.def().air;
        let tile = if air { (s.x + d.w / 2, s.y + d.h / 2) } else { self.free_tile_near((s.x + d.w / 2, s.y + d.h), 6)? };
        let id = self.new_unit(owner, kind, tile);
        if let Some(r) = rally {
            let ore = self.terrain[idx(r.0, r.1)] == Terrain::Ore;
            let path = if air || kind == UnitKind::Harvester { Vec::new() } else { self.find_path(tile, r, false) };
            let u = self.units.get_mut(&id).unwrap();
            if kind != UnitKind::Harvester {
                u.order = Order::Move(r);
                u.path = path;
            } else if ore {
                u.order = Order::Harvest(Some(r));
            }
        }
        Some(id)
    }

    fn place(&mut self, p: u8, kind: StructKind, x: i32, y: i32) {
        let d = kind.def();
        if kind.is_slab() {
            for ty in y..y + d.h {
                for tx in x..x + d.w {
                    if self.place_tile_ok(kind, tx, ty) {
                        self.slab[idx(tx, ty)] = true;
                    }
                }
            }
            return;
        }
        let mut slabbed = 0;
        for ty in y..y + d.h {
            for tx in x..x + d.w {
                slabbed += self.slab[idx(tx, ty)] as i32;
            }
        }
        // Bare rock is a poor footing: structures start damaged without slabs.
        let hp = d.hp * (50 + 50 * slabbed / (d.w * d.h)) / 100;
        let sid = self.create_struct(p, kind, x, y, hp);
        // Each refinery brings a harvester, unless a rebuilt one would exceed the harvester cap.
        if kind == StructKind::Refinery && self.has_supply(p, UnitKind::Harvester) {
            self.spawn_at_struct(p, UnitKind::Harvester, sid);
        }
    }

    fn own(&self, p: u8, ids: &[u32]) -> Vec<u32> {
        let mut out: Vec<u32> = Vec::new();
        for id in ids.iter().take(300) {
            if let Some(u) = self.units.get(id) {
                if u.owner == p && u.hp > 0 && !u.carried && !out.contains(id) {
                    out.push(*id);
                }
            }
        }
        out
    }

    fn halt(u: &mut Unit) {
        u.order = Order::Idle;
        u.path.clear();
    }

    // ---------- commands ----------

    pub fn apply(&mut self, p: u8, cmd: &Command) {
        let pi = p as usize;
        if pi >= self.players.len() || self.players[pi].defeated {
            return;
        }
        // A fresh (non-queued) order replaces whatever the units had lined up.
        let fresh = match cmd {
            Command::Move { units, .. }
            | Command::Attack { units, .. }
            | Command::Harvest { units, .. }
            | Command::Return { units }
            | Command::Stop { units }
            | Command::Patrol { units, .. }
            | Command::Formation { units, .. }
            | Command::Airlift { units, .. } => Some(units),
            _ => None,
        };
        if let Some(units) = fresh {
            for id in self.own(p, units) {
                let u = self.units.get_mut(&id).unwrap();
                u.queue.clear();
                u.speed_cap = 0;
            }
        }
        match cmd {
            Command::StartBuild(kind) => {
                if self.players[pi].construction.is_none() && self.can_build(p, *kind) {
                    self.players[pi].construction = Some(Construction { kind: *kind, spent: 0, ready: false });
                }
            }
            Command::CancelBuild => {
                if let Some(c) = self.players[pi].construction.take() {
                    self.players[pi].credits += c.spent;
                }
            }
            Command::Place { kind, x, y } => {
                let ready = matches!(&self.players[pi].construction, Some(c) if c.ready && c.kind == *kind);
                if ready && self.can_place(p, *kind, *x, *y) {
                    self.players[pi].construction = None;
                    self.place(p, *kind, *x, *y);
                }
            }
            Command::Train(kind) => {
                let q = TRAIN_AT.iter().position(|s| *s == kind.def().made_at).unwrap();
                if self.can_train(p, *kind) && self.has_supply(p, *kind) && self.players[pi].queues[q].items.len() < 12 {
                    self.players[pi].queues[q].items.push_back(*kind);
                }
            }
            Command::CancelTrain(kind) => {
                let q = TRAIN_AT.iter().position(|s| *s == kind.def().made_at).unwrap();
                let pl = &mut self.players[pi];
                if let Some(i) = pl.queues[q].items.iter().rposition(|k| k == kind) {
                    if i == 0 {
                        pl.credits += pl.queues[q].spent;
                        pl.queues[q].spent = 0;
                    }
                    pl.queues[q].items.remove(i);
                }
            }
            Command::Move { units, x, y, attack } => {
                if !inb(*x, *y) {
                    return;
                }
                let ids = self.own(p, units);
                let spots = self.spread((*x, *y), ids.len());
                for (i, id) in ids.iter().enumerate() {
                    let dest = spots.get(i).copied().unwrap_or((*x, *y));
                    let (tile, air, loaded) = {
                        let u = &self.units[id];
                        (u.tile, u.kind.def().air, u.cargo.is_some())
                    };
                    let path = if air { Vec::new() } else { self.find_path(tile, dest, false) };
                    let u = self.units.get_mut(id).unwrap();
                    u.path = path;
                    u.stuck = 0;
                    u.blocked = 0;
                    u.order = if loaded {
                        Order::Drop(dest)
                    } else if *attack {
                        Order::AttackMove(dest)
                    } else {
                        Order::Move(dest)
                    };
                }
            }
            Command::Attack { units, target } => {
                let Some((_, _, owner, ..)) = self.target_info((0, 0), *target) else { return };
                if owner == p {
                    return;
                }
                for id in self.own(p, units) {
                    let u = self.units.get_mut(&id).unwrap();
                    if u.kind.def().damage > 0 {
                        u.order = Order::Attack(*target);
                        u.path.clear();
                        u.stuck = 0;
                    }
                }
            }
            Command::Harvest { units, x, y } => {
                if !inb(*x, *y) {
                    return;
                }
                for id in self.own(p, units) {
                    let u = self.units.get_mut(&id).unwrap();
                    if u.kind == UnitKind::Harvester {
                        u.order = Order::Harvest(Some((*x, *y)));
                        u.path.clear();
                    }
                }
            }
            Command::Return { units } => {
                for id in self.own(p, units) {
                    let u = self.units.get_mut(&id).unwrap();
                    if u.kind == UnitKind::Harvester {
                        u.order = Order::Return;
                        u.path.clear();
                    }
                }
            }
            Command::Pickup { units, cargo } => {
                let ok = self.own(p, &[*cargo]).len() == 1 && !self.units[cargo].kind.def().air;
                if !ok {
                    return;
                }
                for id in self.own(p, units) {
                    let c = self.units.get_mut(&id).unwrap();
                    if c.kind == UnitKind::Carrier && c.cargo.is_none() {
                        c.order = Order::Pickup { cargo: *cargo, drop: None };
                        Self::halt(self.units.get_mut(cargo).unwrap());
                        break;
                    }
                }
            }
            Command::Airlift { units, x, y } => {
                if !inb(*x, *y) {
                    return;
                }
                let ids: Vec<u32> =
                    self.own(p, units).into_iter().filter(|id| !self.units[id].kind.def().air).collect();
                let spots = self.spread((*x, *y), ids.len());
                let mut used: Vec<u32> = Vec::new();
                for (i, id) in ids.iter().enumerate() {
                    let upos = self.units[id].pos;
                    let mut best: Option<(i64, u32)> = None;
                    for c in self.units.values() {
                        let free = c.owner == p
                            && c.kind == UnitKind::Carrier
                            && c.hp > 0
                            && c.cargo.is_none()
                            && !matches!(c.order, Order::Pickup { .. })
                            && !used.contains(&c.id);
                        if free {
                            let d = d2(c.pos, upos);
                            if best.map_or(true, |b| d < b.0) {
                                best = Some((d, c.id));
                            }
                        }
                    }
                    let Some((_, cid)) = best else { break };
                    used.push(cid);
                    let dest = spots.get(i).copied().unwrap_or((*x, *y));
                    self.units.get_mut(&cid).unwrap().order = Order::Pickup { cargo: *id, drop: Some(dest) };
                    Self::halt(self.units.get_mut(id).unwrap());
                }
            }
            Command::Repair { id } => {
                if let Some(s) = self.structs.get_mut(id) {
                    if s.owner == p && s.hp < s.kind.def().hp {
                        s.repairing = !s.repairing;
                    }
                }
            }
            Command::Sell { id } => {
                if self.structs.get(id).is_some_and(|s| s.owner == p) {
                    let s = self.structs.remove(id).unwrap();
                    let d = s.kind.def();
                    for ty in s.y..s.y + d.h {
                        for tx in s.x..s.x + d.w {
                            self.structure_at[idx(tx, ty)] = 0;
                        }
                    }
                    // Half the price back, scaled by condition.
                    self.players[pi].credits += d.cost / 2 * s.hp / d.hp;
                }
            }
            Command::Rally { id, x, y } => {
                let kind = self.structs.get(id).filter(|s| s.owner == p).map(|s| s.kind);
                if let Some(kind) = kind.filter(|k| TRAIN_AT.contains(k) && inb(*x, *y)) {
                    // Units come out of the first building of a kind, so all of them share the point.
                    for s in self.structs.values_mut().filter(|s| s.owner == p && s.kind == kind) {
                        s.rally = Some((*x, *y));
                    }
                }
            }
            Command::Deploy { units } => {
                for id in self.own(p, units) {
                    let u = &self.units[&id];
                    if u.kind != UnitKind::Mcv || u.pos != center(u.tile) {
                        continue;
                    }
                    let (tile, x, y) = (u.tile, u.tile.0 - 1, u.tile.1 - 1);
                    let spots = || (0..3).flat_map(move |dy| (0..3).map(move |dx| (x + dx, y + dy)));
                    let clear = spots().all(|(tx, ty)| {
                        inb(tx, ty) && {
                            let i = idx(tx, ty);
                            self.terrain[i] == Terrain::Rock && self.structure_at[i] == 0 && (self.occ[i] == 0 || self.occ[i] == id)
                        }
                    });
                    if !clear {
                        continue;
                    }
                    self.units.remove(&id);
                    self.occ[idx(tile.0, tile.1)] = 0;
                    let slabbed = spots().filter(|(tx, ty)| self.slab[idx(*tx, *ty)]).count() as i32;
                    let hp = StructKind::Yard.def().hp * (50 + 50 * slabbed / 9) / 100;
                    self.create_struct(p, StructKind::Yard, x, y, hp);
                }
            }
            Command::Stop { units } => {
                for id in self.own(p, units) {
                    Self::halt(self.units.get_mut(&id).unwrap());
                }
            }
            Command::Patrol { units, x, y } => {
                if !inb(*x, *y) {
                    return;
                }
                let ids = self.own(p, units);
                let spots = self.spread((*x, *y), ids.len());
                for (i, id) in ids.iter().enumerate() {
                    let dest = spots.get(i).copied().unwrap_or((*x, *y));
                    self.start_step(*id, Step::Patrol(dest));
                }
            }
            Command::Formation { units, x, y, attack } => {
                if !inb(*x, *y) {
                    return;
                }
                let ids = self.own(p, units);
                if ids.is_empty() {
                    return;
                }
                let n = ids.len() as i32;
                let (sx, sy) = ids.iter().fold((0, 0), |a, id| (a.0 + self.units[id].tile.0, a.1 + self.units[id].tile.1));
                let (cx, cy) = (sx.div_euclid(n), sy.div_euclid(n));
                let slowest = |air: bool| {
                    ids.iter().map(|id| self.units[id].kind.def()).filter(|d| d.air == air).map(|d| d.speed).min().unwrap_or(0)
                };
                let (ground_cap, air_cap) = (slowest(false), slowest(true));
                let mut used: Vec<Tile> = Vec::new();
                for id in &ids {
                    let (tile, air) = (self.units[id].tile, self.units[id].kind.def().air);
                    // Keep each unit's offset from the group centre, within reason.
                    let off = ((tile.0 - cx).clamp(-5, 5), (tile.1 - cy).clamp(-5, 5));
                    let want = ((x + off.0).clamp(0, MAP_W - 1), (y + off.1).clamp(0, MAP_H - 1));
                    let ok = |t: Tile, used: &Vec<Tile>| (air || self.walkable(t.0, t.1)) && !used.contains(&t);
                    let dest = if ok(want, &used) {
                        want
                    } else {
                        (1..=4)
                            .flat_map(|r| (-r..=r).flat_map(move |dy| (-r..=r).map(move |dx| (want.0 + dx, want.1 + dy))))
                            .find(|t| inb(t.0, t.1) && ok(*t, &used))
                            .unwrap_or((*x, *y))
                    };
                    used.push(dest);
                    self.start_step(*id, if *attack { Step::AttackMove(dest) } else { Step::Move(dest) });
                    let cap = if air { air_cap } else { ground_cap };
                    if let Some(u) = self.units.get_mut(id) {
                        u.speed_cap = if cap < u.kind.def().speed { cap } else { 0 };
                    }
                }
            }
            Command::Queue { units, step } => {
                let ids = self.own(p, units);
                // Spread ground destinations so a queued group does not pile onto one tile.
                let spots = match step {
                    Step::Move(t) | Step::AttackMove(t) | Step::Patrol(t) if inb(t.0, t.1) => self.spread(*t, ids.len()),
                    _ => Vec::new(),
                };
                for (i, id) in ids.iter().enumerate() {
                    let step = match (*step, spots.get(i)) {
                        (Step::Move(_), Some(t)) => Step::Move(*t),
                        (Step::AttackMove(_), Some(t)) => Step::AttackMove(*t),
                        (Step::Patrol(_), Some(t)) => Step::Patrol(*t),
                        (s, _) => s,
                    };
                    let u = &self.units[id];
                    let idle = u.order == Order::Idle && u.queue.is_empty();
                    if idle {
                        self.start_step(*id, step);
                    } else if u.queue.len() < MAX_QUEUE {
                        self.units.get_mut(id).unwrap().queue.push_back(step);
                    }
                }
            }
        }
    }

    /// Give one unit an order from a step (fresh or from its queue). Steps a unit
    /// cannot carry out (a harvest for a tank, an attack on a friend) are skipped.
    fn start_step(&mut self, id: u32, step: Step) {
        let Some(u) = self.units.get(&id) else { return };
        let (tile, d, loaded, owner) = (u.tile, u.kind.def(), u.cargo.is_some(), u.owner);
        let order = match step {
            Step::Move(t) | Step::AttackMove(t) | Step::Patrol(t) if !inb(t.0, t.1) => return,
            Step::Move(t) if loaded => Order::Drop(t),
            Step::Move(t) => Order::Move(t),
            Step::AttackMove(t) => Order::AttackMove(t),
            Step::Patrol(t) => Order::Patrol { from: tile, to: t },
            Step::Attack(target) => {
                let enemy = self.target_info((0, 0), target).is_some_and(|i| i.2 != owner);
                if !enemy || d.damage == 0 {
                    return;
                }
                Order::Attack(target)
            }
            Step::Harvest(t) => {
                if u.kind != UnitKind::Harvester || !inb(t.0, t.1) {
                    return;
                }
                Order::Harvest(Some(t))
            }
        };
        let path = match order {
            Order::Move(t) | Order::AttackMove(t) | Order::Patrol { to: t, .. } if !d.air => self.find_path(tile, t, false),
            _ => Vec::new(),
        };
        let u = self.units.get_mut(&id).unwrap();
        u.order = order;
        u.path = path;
        u.stuck = 0;
        u.blocked = 0;
    }

    // ---------- tick ----------

    pub fn tick(&mut self) {
        self.events.clear();
        self.tick += 1;
        self.production();
        self.repairs();
        self.turrets();
        let ids: Vec<u32> = self.units.keys().copied().collect();
        for id in ids {
            // Detach the unit so it can freely read and mutate the rest of the world.
            let Some(mut u) = self.units.remove(&id) else { continue };
            if u.hp > 0 && !u.carried {
                self.update_unit(&mut u);
            }
            self.units.insert(id, u);
        }
        self.next_steps();
        self.cleanup();
    }

    /// Units that finished their order move on to the next queued one.
    fn next_steps(&mut self) {
        let ready: Vec<u32> = self
            .units
            .values()
            .filter(|u| u.order == Order::Idle && u.hp > 0 && !u.carried)
            .map(|u| u.id)
            .collect();
        for id in ready {
            let u = self.units.get_mut(&id).unwrap();
            u.speed_cap = 0;
            while let Some(step) = self.units.get_mut(&id).and_then(|u| u.queue.pop_front()) {
                self.start_step(id, step);
                if self.units[&id].order != Order::Idle {
                    break;
                }
            }
        }
    }

    fn production(&mut self) {
        for p in 0..self.players.len() {
            let (mut made, mut used) = (0, 0);
            for s in self.structs.values().filter(|s| s.owner as usize == p) {
                let pw = s.kind.def().power;
                if pw > 0 {
                    made += pw
                } else {
                    used -= pw
                }
            }
            let pl = &mut self.players[p];
            pl.power_made = made;
            pl.power_used = used;
            let rate = if used > made { BUILD_RATE / 2 } else { BUILD_RATE };
            if let Some(c) = &mut pl.construction {
                if !c.ready {
                    let cost = c.kind.def().cost;
                    let pay = rate.min(cost - c.spent).min(pl.credits);
                    c.spent += pay;
                    pl.credits -= pay;
                    c.ready = c.spent >= cost;
                }
            }
            for q in 0..3 {
                let Some(&kind) = self.players[p].queues[q].items.front() else { continue };
                let Some(sid) = self.first_struct(p as u8, TRAIN_AT[q]) else {
                    let pl = &mut self.players[p];
                    pl.credits += pl.queues[q].spent;
                    pl.queues[q].spent = 0;
                    pl.queues[q].items.clear();
                    continue;
                };
                // A lost building can leave the queue over the cap: hold until it fits again.
                if kind.supply().is_some_and(|(pool, _)| {
                    let (used, cap) = self.supply(p as u8, pool);
                    used > cap
                }) {
                    continue;
                }
                let cost = kind.def().cost;
                let rate = train_rate(rate, self.structs.values().filter(|s| s.owner as usize == p && s.kind == TRAIN_AT[q]).count());
                let pl = &mut self.players[p];
                let pay = rate.min(cost - pl.queues[q].spent).min(pl.credits);
                pl.queues[q].spent += pay;
                pl.credits -= pay;
                if pl.queues[q].spent >= cost && self.spawn_at_struct(p as u8, kind, sid).is_some() {
                    let pl = &mut self.players[p];
                    pl.queues[q].items.pop_front();
                    pl.queues[q].spent = 0;
                }
            }
        }
    }

    /// A full repair costs half the build price and takes about 15 seconds.
    fn repairs(&mut self) {
        for s in self.structs.values_mut().filter(|s| s.repairing) {
            let d = s.kind.def();
            if s.hp >= d.hp {
                s.repairing = false;
                continue;
            }
            let heal = (d.hp / 300).max(1).min(d.hp - s.hp);
            let owed = s.repair_acc + d.cost * heal;
            let charge = owed / (2 * d.hp);
            let pl = &mut self.players[s.owner as usize];
            if pl.credits < charge {
                continue;
            }
            pl.credits -= charge;
            s.repair_acc = owed - charge * 2 * d.hp;
            s.hp += heal;
        }
    }

    fn turrets(&mut self) {
        let ids: Vec<u32> = self.structs.values().filter(|s| s.kind.weapon().is_some()).map(|s| s.id).collect();
        for id in ids {
            let s = self.structs.get_mut(&id).unwrap();
            if s.cooldown > 0 {
                s.cooldown -= 1;
                continue;
            }
            let (owner, pos) = (s.owner, center((s.x, s.y)));
            let (range, dmg, cd) = s.kind.weapon().unwrap();
            let pl = &self.players[owner as usize];
            if pl.power_used > pl.power_made {
                continue;
            }
            if let Some(t) = self.acquire(owner, pos, false, range, true) {
                if let Some(to) = self.hit(pos, false, t, dmg, 100, true) {
                    let s = self.structs.get_mut(&id).unwrap();
                    s.aim = (to.0 - pos.0, to.1 - pos.1);
                    s.cooldown = cd;
                }
            }
        }
    }

    fn hit(&mut self, from: Pos, from_air: bool, t: Target, dmg: i32, vs_struct: i32, heavy: bool) -> Option<Pos> {
        let (to, to_air) = match t {
            Target::Unit(id) => {
                let u = self.units.get_mut(&id)?;
                u.hp -= dmg;
                (u.pos, u.kind.def().air)
            }
            Target::Struct(id) => {
                let (_, mid, ..) = self.target_info(from, t)?;
                self.structs.get_mut(&id)?.hp -= (dmg * vs_struct / 100).max(1);
                (mid, false)
            }
        };
        self.events.push(Event::Shot { from, to, from_air, to_air, heavy });
        Some(to)
    }

    fn fire(&mut self, u: &mut Unit, t: Target) {
        let d = u.kind.def();
        if let Some(to) = self.hit(u.pos, d.air, t, d.damage, d.vs_struct, d.damage >= 25) {
            if to != u.pos {
                u.face = (to.0 - u.pos.0, to.1 - u.pos.1);
            }
        }
        u.cooldown = d.cooldown;
    }

    fn auto_fire(&mut self, u: &mut Unit) -> bool {
        let d = u.kind.def();
        if d.damage == 0 || u.cooldown > 0 {
            return false;
        }
        match self.acquire(u.owner, u.pos, d.air, d.range, d.hits_air) {
            Some(t) => {
                self.fire(u, t);
                true
            }
            None => false,
        }
    }

    fn update_unit(&mut self, u: &mut Unit) {
        u.prev = u.pos;
        if u.cooldown > 0 {
            u.cooldown -= 1;
        }
        if u.kind.def().air {
            self.update_air(u);
            u.tile = ((u.pos.0 / TILE).clamp(0, MAP_W - 1), (u.pos.1 / TILE).clamp(0, MAP_H - 1));
        } else {
            self.update_ground(u);
        }
    }

    /// Returns true when the order should be dropped.
    fn attack_order(&mut self, u: &mut Unit, t: Target) -> Option<(bool, Pos, Tile)> {
        let d = u.kind.def();
        let (near, aim, owner, air, tile) = self.target_info(u.pos, t)?;
        if owner == u.owner || d.damage == 0 || (air && !d.hits_air) {
            return None;
        }
        let in_range = d2(u.pos, near) <= range2(d.range + self.height_bonus(u.pos, d.air, near, air));
        if in_range && u.cooldown == 0 {
            self.fire(u, t);
        }
        Some((in_range, aim, tile))
    }

    fn update_ground(&mut self, u: &mut Unit) {
        let d = u.kind.def();
        let at_center = u.pos == center(u.tile);
        let stagger = (self.tick + u.id) % 20 == 0;
        match u.order {
            Order::Idle => {
                self.auto_fire(u);
            }
            Order::Move(_) => {
                if u.path.is_empty() && at_center {
                    u.order = Order::Idle;
                }
            }
            Order::AttackMove(_) => {
                if self.auto_fire(u) {
                    u.hold_until = self.tick + d.cooldown as u32 + 2;
                }
                if u.path.is_empty() && at_center && self.tick >= u.hold_until {
                    u.order = Order::Idle;
                }
            }
            Order::Patrol { from, to } => {
                if self.auto_fire(u) {
                    u.hold_until = self.tick + d.cooldown as u32 + 2;
                }
                if u.path.is_empty() && at_center && self.tick >= u.hold_until {
                    // Turn round. If the far end is unreachable, patrol in place.
                    u.order = Order::Patrol { from: to, to: from };
                    u.path = self.find_path(u.tile, from, false);
                }
            }
            Order::Attack(t) => match self.attack_order(u, t) {
                None => Self::halt(u),
                Some((true, ..)) => u.path.clear(),
                Some((false, _, tile)) => {
                    if at_center && (u.path.is_empty() || stagger) {
                        u.path = self.find_path(u.tile, tile, false);
                        if u.path.is_empty() {
                            Self::halt(u);
                        }
                    }
                }
            },
            Order::Harvest(pref) => {
                if u.load >= HARVEST_CAP {
                    u.order = Order::Return;
                    u.path.clear();
                } else if at_center && u.path.is_empty() {
                    let i = idx(u.tile.0, u.tile.1);
                    if self.terrain[i] == Terrain::Ore {
                        let take = 5.min(self.ore[i] as i32).min(HARVEST_CAP - u.load);
                        self.ore[i] -= take as u16;
                        u.load += take;
                        if self.ore[i] == 0 {
                            self.terrain[i] = Terrain::Dust;
                        }
                    } else {
                        // Prefer the assigned patch; forget it once nothing is left around it.
                        let mut target = None;
                        if let Some(h) = pref.or(u.home_ore) {
                            let near = self.nearest_ore(h, u.id).filter(|t| (t.0 - h.0).pow(2) + (t.1 - h.1).pow(2) <= 49);
                            u.home_ore = near.map(|_| h);
                            target = near;
                        }
                        match target.or_else(|| self.nearest_ore(u.tile, u.id)) {
                            Some(t) => {
                                u.path = self.find_path(u.tile, t, false);
                                u.order = Order::Harvest(None);
                            }
                            None => u.order = if u.load > 0 { Order::Return } else { Order::Idle },
                        }
                    }
                }
            }
            Order::Return => {
                let mut best: Option<(i64, i32, i32)> = None;
                for s in self.structs.values().filter(|s| s.owner == u.owner && s.kind == StructKind::Refinery) {
                    let dd = d2(u.pos, center((s.x + 1, s.y)));
                    if best.map_or(true, |b| dd < b.0) {
                        best = Some((dd, s.x, s.y));
                    }
                }
                if let Some((_, sx, sy)) = best {
                    let sd = StructKind::Refinery.def();
                    let gap = (sx - u.tile.0 - 1).max(u.tile.0 - (sx + sd.w)).max(sy - u.tile.1 - 1).max(u.tile.1 - (sy + sd.h));
                    if gap <= 0 && at_center {
                        u.path.clear();
                        let give = 25.min(u.load);
                        u.load -= give;
                        self.players[u.owner as usize].credits += give;
                        if u.load == 0 {
                            u.order = Order::Harvest(None);
                        }
                    } else if at_center && (u.path.is_empty() || stagger) {
                        u.path = self.find_path(u.tile, (sx + 1, sy + 1), false);
                    }
                }
            }
            Order::Pickup { .. } | Order::Drop(_) => u.order = Order::Idle,
        }
        self.step_ground(u);
    }

    fn step_ground(&mut self, u: &mut Unit) {
        let speed = Self::speed(u);
        if u.pos == center(u.tile) {
            if self.tick < u.hold_until {
                return;
            }
            let Some(&next) = u.path.last() else { return };
            if !self.walkable(next.0, next.1) {
                // Something was built on the route.
                let goal = u.path[0];
                u.path = self.find_path(u.tile, goal, false);
                return;
            }
            if self.occ[idx(next.0, next.1)] != 0 {
                u.blocked += 1;
                if u.blocked > 8 {
                    u.blocked = 0;
                    u.stuck += 1;
                    if u.stuck > 3 {
                        u.stuck = 0;
                        u.path.clear();
                    } else {
                        let goal = u.path[0];
                        u.path = self.find_path(u.tile, goal, true);
                    }
                }
                return;
            }
            u.path.pop();
            self.occ[idx(u.tile.0, u.tile.1)] = 0;
            self.occ[idx(next.0, next.1)] = u.id;
            u.face = (next.0 - u.tile.0, next.1 - u.tile.1);
            u.tile = next;
            u.blocked = 0;
            u.stuck = 0;
        }
        let c = center(u.tile);
        let (dx, dy) = (c.0 - u.pos.0, c.1 - u.pos.1);
        let sp = if dx != 0 && dy != 0 { speed * 181 / 256 } else { speed };
        u.pos.0 += dx.clamp(-sp, sp);
        u.pos.1 += dy.clamp(-sp, sp);
    }

    fn speed(u: &Unit) -> i32 {
        let base = u.kind.def().speed;
        if u.speed_cap > 0 {
            base.min(u.speed_cap)
        } else {
            base
        }
    }

    fn fly(u: &mut Unit, to: Pos) -> bool {
        let sp = Self::speed(u) as i64;
        let (dx, dy) = ((to.0 - u.pos.0) as i64, (to.1 - u.pos.1) as i64);
        let dd = dx * dx + dy * dy;
        if dd <= sp * sp {
            u.pos = to;
            return true;
        }
        let dist = dd.isqrt();
        u.pos.0 += (dx * sp / dist) as i32;
        u.pos.1 += (dy * sp / dist) as i32;
        u.face = (dx as i32, dy as i32);
        false
    }

    fn update_air(&mut self, u: &mut Unit) {
        match u.order {
            Order::Idle => {
                self.auto_fire(u);
            }
            Order::Move(t) | Order::AttackMove(t) => {
                self.auto_fire(u);
                if Self::fly(u, center(t)) {
                    u.order = Order::Idle;
                }
            }
            Order::Patrol { from, to } => {
                self.auto_fire(u);
                if Self::fly(u, center(to)) {
                    u.order = Order::Patrol { from: to, to: from };
                }
            }
            Order::Attack(t) => match self.attack_order(u, t) {
                None => u.order = Order::Idle,
                Some((true, ..)) => {}
                Some((false, aim, _)) => {
                    Self::fly(u, aim);
                }
            },
            Order::Pickup { cargo, drop } => {
                let target = self.units.get(&cargo).filter(|t| {
                    t.owner == u.owner && t.hp > 0 && !t.carried && !t.kind.def().air && u.cargo.is_none()
                });
                let Some(t) = target else {
                    u.order = Order::Idle;
                    return;
                };
                let (tpos, settled, ttile) = (t.pos, t.pos == center(t.tile) && t.path.is_empty(), t.tile);
                if Self::fly(u, tpos) && settled {
                    let t = self.units.get_mut(&cargo).unwrap();
                    t.carried = true;
                    Self::halt(t);
                    self.occ[idx(ttile.0, ttile.1)] = 0;
                    u.cargo = Some(cargo);
                    u.order = drop.map_or(Order::Idle, Order::Drop);
                }
            }
            Order::Drop(t) => {
                let Some(cargo) = u.cargo else {
                    u.order = Order::Idle;
                    return;
                };
                if Self::fly(u, center(t)) {
                    match self.free_tile_near(t, 8) {
                        Some(f) if f == t => {
                            if let Some(c) = self.units.get_mut(&cargo) {
                                c.carried = false;
                                c.tile = t;
                                c.pos = center(t);
                                c.prev = c.pos;
                                self.occ[idx(t.0, t.1)] = cargo;
                            }
                            u.cargo = None;
                            u.order = Order::Idle;
                        }
                        Some(f) => u.order = Order::Drop(f),
                        None => u.order = Order::Idle,
                    }
                }
            }
            Order::Harvest(_) | Order::Return => u.order = Order::Idle,
        }
    }

    fn cleanup(&mut self) {
        let mut dead: Vec<u32> = self.units.values().filter(|u| u.hp <= 0).map(|u| u.id).collect();
        let mut i = 0;
        while i < dead.len() {
            if let Some(u) = self.units.remove(&dead[i]) {
                let air = u.kind.def().air;
                if !air && !u.carried && self.occ[idx(u.tile.0, u.tile.1)] == u.id {
                    self.occ[idx(u.tile.0, u.tile.1)] = 0;
                }
                if let Some(c) = u.cargo {
                    if !dead.contains(&c) {
                        dead.push(c);
                    }
                }
                if !u.carried {
                    self.events.push(Event::Boom { pos: u.pos, big: false, air });
                }
            }
            i += 1;
        }
        let gone: Vec<u32> = self.structs.values().filter(|s| s.hp <= 0).map(|s| s.id).collect();
        for id in gone {
            let s = self.structs.remove(&id).unwrap();
            let d = s.kind.def();
            for ty in s.y..s.y + d.h {
                for tx in s.x..s.x + d.w {
                    self.structure_at[idx(tx, ty)] = 0;
                }
            }
            let pos = (s.x * TILE + d.w * TILE / 2, s.y * TILE + d.h * TILE / 2);
            self.events.push(Event::Boom { pos, big: true, air: false });
        }
        for p in 0..self.players.len() {
            if !self.players[p].defeated
                && !self.structs.values().any(|s| s.owner as usize == p && s.kind != StructKind::Wall)
            {
                self.players[p].defeated = true;
                self.players[p].construction = None;
            }
        }
        if self.winner.is_none() && self.players.len() > 1 {
            let mut alive = self.players.iter().enumerate().filter(|(_, p)| !p.defeated);
            if let (Some((w, _)), None) = (alive.next(), alive.next()) {
                self.winner = Some(w as u8);
            }
        }
    }
}
