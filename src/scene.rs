//! The 3D view: camera, sun, unit and building models (glTF made in Blender, see
//! tools/blender), ore crystals, particles and ground overlays. Everything here
//! mirrors `Game` each frame; nothing here feeds back into the sim.

use crate::game::{fpos, struct_height, Fx, Game, Mode, AIR_Z, PCOL, PITCH_C, PITCH_S, PX};
use crate::gfx::{fade, rgba, sprite_images, Rgba};
use crate::terrain::{self, TerrainView};
use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::camera::{ScalingMode, Viewport};
use bevy::gltf::{GltfAssetLabel, GltfMaterialName};
use bevy::light::CascadeShadowConfigBuilder;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::world_serialization::{WorldAsset, WorldAssetRoot, WorldInstanceReady};
use cinderfront::sim::{idx, StructKind, Terrain, UnitKind, MAP_H, MAP_W, TRAIN_AT};
use std::collections::HashMap;

/// Units are drawn a bit larger than life so they read at RTS zoom.
const UNIT_SCALE: f32 = 1.3;

pub struct ScenePlugin;

impl Plugin for ScenePlugin {
    fn build(&self, app: &mut App) {
        app.init_gizmo_group::<OverlayGizmos>()
            .add_systems(Startup, setup)
            .add_systems(PostStartup, configure_gizmos)
            .add_observer(on_model_ready)
            .add_systems(
                Update,
                (sync_camera, sync_terrain, sync_units, sync_structs, animate_parts, sync_ore, sync_fx, draw_overlays)
                    .chain()
                    .after(crate::RunGame),
            );
    }
}

/// Lines drawn over everything (selection, rally points, placement).
#[derive(Default, Reflect, GizmoConfigGroup)]
pub struct OverlayGizmos;

/// The game currently shown, if any. Owned here so render systems can read it
/// while the session logic (which holds non-Sync sockets) lives elsewhere.
#[derive(Resource, Default)]
pub struct View(pub Option<Game>);

#[derive(Resource)]
struct Assets3d {
    units: HashMap<UnitKind, Handle<WorldAsset>>,
    structs: HashMap<StructKind, Handle<WorldAsset>>,
    team: Vec<Handle<StandardMaterial>>,
    crystal: Handle<Mesh>,
    crystal_mat: Handle<StandardMaterial>,
    batches: [Handle<Mesh>; 4],
}

#[derive(Component)]
struct MainCam;

#[derive(Component)]
struct UnitView(u32);

#[derive(Component)]
struct StructView(u32);

/// Model instance, waiting to have its team colour applied.
#[derive(Component)]
struct Team(u8);

#[derive(Clone, Copy, PartialEq)]
enum PartKind {
    Turret,
    Rotor(f32),
    Load,
}

/// A moving piece of a model, with its rest transform and the view root it belongs to.
#[derive(Component)]
struct Part {
    kind: PartKind,
    rest: Transform,
    root: Entity,
}

#[derive(Resource, Default)]
struct Spawned {
    units: HashMap<u32, Entity>,
    structs: HashMap<u32, (Entity, u8)>,
    /// Per tile: crystal entities and the ore level they show.
    ore: HashMap<usize, (u16, Vec<Entity>)>,
    ore_timer: f32,
    terrain: Option<TerrainView>,
}

pub fn unit_file(k: UnitKind) -> &'static str {
    match k {
        UnitKind::Trooper => "trooper",
        UnitKind::Rocketeer => "rocketeer",
        UnitKind::Buggy => "buggy",
        UnitKind::Tank => "tank",
        UnitKind::Harvester => "harvester",
        UnitKind::Carrier => "carrier",
        UnitKind::Gunship => "gunship",
        UnitKind::Explorer => "explorer",
        UnitKind::Mcv => "mcv",
    }
}

pub fn struct_file(k: StructKind) -> Option<&'static str> {
    Some(match k {
        StructKind::Yard => "yard",
        StructKind::Power => "power",
        StructKind::Refinery => "refinery",
        StructKind::Barracks => "barracks",
        StructKind::Factory => "factory",
        StructKind::Airpad => "airpad",
        StructKind::Wall => "wall",
        StructKind::Turret => "turret",
        StructKind::Armory => "armory",
        StructKind::TechLab => "tech_lab",
        StructKind::RocketTurret => "rocket_turret",
        StructKind::Slab | StructKind::Slab4 => return None,
    })
}

fn lin(c: Rgba) -> [f32; 4] {
    [c.x.powf(2.2), c.y.powf(2.2), c.z.powf(2.2), c.w]
}

fn setup(
    mut commands: Commands,
    server: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((
        MainCam,
        Camera3d::default(),
        Camera { order: 0, ..default() },
        Projection::from(OrthographicProjection {
            scaling_mode: ScalingMode::Fixed { width: 26.5, height: 20.0 },
            near: -200.0,
            far: 400.0,
            ..OrthographicProjection::default_3d()
        }),
        Transform::from_xyz(48.0, 40.0, 60.0).looking_at(Vec3::new(48.0, 0.0, 48.0), Vec3::Y),
    ));
    // Sun from the top-left of the screen (north-west), fairly high.
    commands.spawn((
        DirectionalLight { illuminance: 5000.0, shadow_maps_enabled: true, ..default() },
        Transform::from_xyz(-0.6, 1.25, -0.75).looking_at(Vec3::ZERO, Vec3::Y),
        CascadeShadowConfigBuilder {
            num_cascades: 1,
            minimum_distance: 0.1,
            maximum_distance: 160.0,
            first_cascade_far_bound: 160.0,
            ..default()
        }
        .build(),
    ));
    commands.insert_resource(GlobalAmbientLight { color: Color::srgb(1.0, 0.92, 0.85), brightness: 600.0, ..default() });

    let model = |name: &str| server.load(GltfAssetLabel::Scene(0).from_asset(format!("embedded://models/{name}.glb")));
    let units = UnitKind::ALL.iter().map(|k| (*k, model(unit_file(*k)))).collect();
    let structs = StructKind::BUILDABLE
        .iter()
        .chain([StructKind::Yard].iter())
        .filter_map(|k| struct_file(*k).map(|f| (*k, model(f))))
        .collect();
    let team = PCOL
        .iter()
        .map(|c| {
            materials.add(StandardMaterial {
                base_color: Color::srgb(c.x, c.y, c.z),
                perceptual_roughness: 0.55,
                ..default()
            })
        })
        .collect();
    let crystal = meshes.add(Cone::new(0.06, 0.2).mesh().resolution(4));
    let crystal_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.62, 0.2),
        emissive: LinearRgba::rgb(1.6, 0.55, 0.08),
        perceptual_roughness: 0.3,
        ..default()
    });

    // Immediate-mode quad batches, rebuilt every frame: glow (additive), soft shade,
    // smoke and solid fills.
    let (glow, puff) = sprite_images();
    let (glow, puff) = (images.add(glow), images.add(puff));
    let mat = |tex: Option<Handle<Image>>, alpha: AlphaMode| StandardMaterial {
        base_color_texture: tex,
        unlit: true,
        alpha_mode: alpha,
        double_sided: true,
        cull_mode: None,
        ..default()
    };
    let mats = [
        mat(Some(glow.clone()), AlphaMode::Add),
        mat(Some(glow), AlphaMode::Blend),
        mat(Some(puff), AlphaMode::Blend),
        mat(None, AlphaMode::Blend),
    ];
    let batches = mats.map(|m| {
        let mesh = meshes.add(empty_batch());
        commands.spawn((
            Mesh3d(mesh.clone()),
            MeshMaterial3d(materials.add(m)),
            Transform::IDENTITY,
            NoFrustumCulling,
            bevy::light::NotShadowCaster,
        ));
        mesh
    });
    commands.insert_resource(Assets3d { units, structs, team, crystal, crystal_mat, batches });
    commands.insert_resource(Spawned::default());
}

fn configure_gizmos(mut store: ResMut<GizmoConfigStore>) {
    let (c, _) = store.config_mut::<OverlayGizmos>();
    c.line.width = 2.0;
    c.depth_bias = -0.2;
    let (c, _) = store.config_mut::<DefaultGizmoConfigGroup>();
    c.line.width = 2.0;
}

/// Give freshly spawned model instances their team colour and find their moving parts.
fn on_model_ready(
    ev: On<WorldInstanceReady>,
    mut commands: Commands,
    teams: Query<(&Team, &ChildOf)>,
    children: Query<&Children>,
    mats: Query<&GltfMaterialName>,
    names: Query<(&Name, &Transform)>,
    assets: Res<Assets3d>,
) {
    let Ok((team, parent)) = teams.get(ev.entity) else { return };
    let root = parent.parent();
    // glTF root nodes are named after the model (the `turret` building's root is
    // called `turret` too), so moving parts are only looked for below them.
    let model_roots: Vec<Entity> = children.get(ev.entity).map(|c| c.iter().collect()).unwrap_or_default();
    let mut part_level: Vec<Entity> = Vec::new();
    for r in &model_roots {
        for c in children.iter_descendants(*r) {
            part_level.push(c);
        }
    }
    for e in children.iter_descendants(ev.entity) {
        let can_move = part_level.contains(&e) && !model_roots.contains(&e);
        if mats.get(e).is_ok_and(|m| m.0 == "Team") {
            commands.entity(e).insert(MeshMaterial3d(assets.team[team.0 as usize % 4].clone()));
        }
        if let Ok((name, t)) = names.get(e) {
            let kind = match name.as_str() {
                "turret" => Some(PartKind::Turret),
                "rotor" => Some(PartKind::Rotor(0.0)),
                "rotor2" => Some(PartKind::Rotor(0.8)),
                "load" => Some(PartKind::Load),
                _ => None,
            };
            if let Some(kind) = kind.filter(|_| can_move) {
                commands.entity(e).insert(Part { kind, rest: *t, root });
            }
        }
    }
}

fn sync_camera(
    view: Res<View>,
    windows: Query<&Window>,
    mut cam: Query<(&mut Camera, &mut Projection, &mut Transform), With<MainCam>>,
) {
    let (Some(g), Ok(win), Ok((mut camera, mut proj, mut tf))) = (&view.0, windows.single(), cam.single_mut()) else {
        if let Ok((mut camera, _, _)) = cam.single_mut() {
            camera.is_active = false;
        }
        return;
    };
    camera.is_active = true;
    let sf = win.scale_factor();
    let (vw, vh) = (g.view_w().max(1.0), g.screen.y.max(1.0));
    let size = UVec2::new((vw * sf) as u32, (vh * sf) as u32).max(UVec2::ONE);
    camera.viewport = Some(Viewport { physical_position: UVec2::ZERO, physical_size: size, ..default() });
    if let Projection::Orthographic(o) = proj.as_mut() {
        o.scaling_mode = ScalingMode::Fixed { width: vw / g.k(), height: vh / g.k() };
    }
    let f = g.focus + g.shake_off;
    let target = Vec3::new(f.x, 0.0, f.y);
    *tf = Transform::from_translation(target + Vec3::new(0.0, PITCH_S, PITCH_C) * 80.0).looking_at(target, Vec3::Y);
}

#[allow(clippy::too_many_arguments)]
fn sync_terrain(
    view: Res<View>,
    time: Res<Time>,
    mut spawned: ResMut<Spawned>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let Some(g) = &view.0 else {
        if let Some(mut tv) = spawned.terrain.take() {
            tv.clear(&mut commands);
        }
        return;
    };
    let tv = spawned.terrain.get_or_insert_with(TerrainView::new);
    terrain::update(tv, g, time.delta_secs(), &mut commands, &mut meshes, &mut images, &mut materials);
}

fn world_pos(p: Vec2, z: f32) -> Vec3 {
    Vec3::new(p.x / PX, z / PX, p.y / PX)
}

fn sync_units(
    view: Res<View>,
    assets: Res<Assets3d>,
    mut spawned: ResMut<Spawned>,
    mut commands: Commands,
    mut q: Query<(&mut Transform, &mut Visibility), With<UnitView>>,
) {
    let Some(g) = &view.0 else {
        for (_, e) in spawned.units.drain() {
            commands.entity(e).despawn();
        }
        return;
    };
    let w = &g.world;
    spawned.units.retain(|id, e| {
        let keep = w.units.contains_key(id);
        if !keep {
            commands.entity(*e).despawn();
        }
        keep
    });
    for u in w.units.values() {
        let e = *spawned.units.entry(u.id).or_insert_with(|| {
            commands
                .spawn((UnitView(u.id), Transform::default(), Visibility::Hidden))
                .with_child((WorldAssetRoot(assets.units[&u.kind].clone()), Team(u.owner)))
                .id()
        });
        let Ok((mut tf, mut vis)) = q.get_mut(e) else { continue };
        let ang = g.angles.get(&u.id).copied().unwrap_or(1.57);
        let (at, z, show) = if u.carried {
            // Hanging under whichever carrier holds it.
            match w.units.values().find(|c| c.cargo == Some(u.id)) {
                Some(c) => (g.upos(c), AIR_Z - 16.0, g.unit_visible(c)),
                None => (g.upos(u), 0.0, false),
            }
        } else {
            (g.upos(u), Game::uz(u), g.unit_visible(u))
        };
        *tf = Transform::from_translation(world_pos(at, z))
            .with_rotation(Quat::from_rotation_y(-ang))
            .with_scale(Vec3::splat(UNIT_SCALE));
        *vis = if show { Visibility::Inherited } else { Visibility::Hidden };
    }
}

fn sync_structs(
    view: Res<View>,
    assets: Res<Assets3d>,
    mut spawned: ResMut<Spawned>,
    mut commands: Commands,
) {
    let Some(g) = &view.0 else {
        for (_, (e, _)) in spawned.structs.drain() {
            commands.entity(e).despawn();
        }
        return;
    };
    let shown: HashMap<u32, (StructKind, i32, i32, u8)> =
        g.shown_structs().map(|s| (s.id, (s.kind, s.x, s.y, s.owner))).collect();
    spawned.structs.retain(|id, (e, _)| {
        let keep = shown.contains_key(id);
        if !keep {
            commands.entity(*e).despawn();
        }
        keep
    });
    for (id, (kind, x, y, owner)) in shown {
        if spawned.structs.contains_key(&id) {
            continue;
        }
        let Some(model) = assets.structs.get(&kind) else { continue };
        let d = kind.def();
        let at = Vec3::new(x as f32 + d.w as f32 / 2.0, 0.0, y as f32 + d.h as f32 / 2.0);
        let e = commands
            .spawn((StructView(id), Transform::from_translation(at), Visibility::Inherited))
            .with_child((WorldAssetRoot(model.clone()), Team(owner)))
            .id();
        spawned.structs.insert(id, (e, owner));
    }
}

fn animate_parts(
    view: Res<View>,
    units: Query<&UnitView>,
    structs: Query<&StructView>,
    mut parts: Query<(&Part, &mut Transform)>,
) {
    let Some(g) = &view.0 else { return };
    for (p, mut tf) in &mut parts {
        match p.kind {
            PartKind::Rotor(phase) => {
                let id = units.get(p.root).map_or(0, |u| u.0);
                let a = g.time * 38.0 + phase + id as f32;
                *tf = p.rest.with_rotation(p.rest.rotation * Quat::from_rotation_y(a));
            }
            PartKind::Turret => {
                // Buildings aim at their target; vehicle turrets follow the hull.
                let aim = structs.get(p.root).ok().and_then(|s| {
                    g.world.structs.get(&s.0).or_else(|| g.ghosts.get(&s.0)).map(|s| s.aim)
                });
                if let Some(aim) = aim.filter(|a| *a != (0, 0)) {
                    let a = (aim.1 as f32).atan2(aim.0 as f32);
                    *tf = p.rest.with_rotation(Quat::from_rotation_y(-a) * p.rest.rotation);
                }
            }
            PartKind::Load => {
                let load = units.get(p.root).ok().and_then(|u| g.world.units.get(&u.0)).map_or(0, |u| u.load);
                let k = load as f32 / cinderfront::sim::HARVEST_CAP as f32;
                *tf = p.rest.with_scale(p.rest.scale * Vec3::new(1.0, k.clamp(0.0, 1.0).max(0.001), 1.0));
            }
        }
    }
}

/// Ore crystals: a few glowing spikes per ore tile, fewer as the tile is mined out.
fn sync_ore(
    view: Res<View>,
    time: Res<Time>,
    assets: Res<Assets3d>,
    mut spawned: ResMut<Spawned>,
    mut commands: Commands,
) {
    let Some(g) = &view.0 else {
        for (_, (_, es)) in spawned.ore.drain() {
            es.into_iter().for_each(|e| commands.entity(e).despawn());
        }
        return;
    };
    spawned.ore_timer -= time.delta_secs();
    if spawned.ore_timer > 0.0 {
        return;
    }
    spawned.ore_timer = 0.5;
    let w = &g.world;
    for ty in 0..MAP_H {
        for tx in 0..MAP_W {
            let i = idx(tx, ty);
            let level = if w.terrain[i] == Terrain::Ore && g.explored[i] { 1 + (w.ore[i] / 200).min(4) } else { 0 };
            if spawned.ore.get(&i).map_or(0, |o| o.0) == level {
                continue;
            }
            if let Some((_, es)) = spawned.ore.remove(&i) {
                es.into_iter().for_each(|e| commands.entity(e).despawn());
            }
            if level == 0 {
                continue;
            }
            let amount = (w.ore[i] as f32 / 800.0).clamp(0.2, 1.0);
            let mut es = Vec::new();
            for j in 0..(level as i32 + 1) {
                let h = |a: i32, b: i32, s: u32| crate::gfx::hash2(a, b, s);
                let (hx, hy, hs) = (h(tx * 7 + j, ty, 1), h(tx, ty * 5 + j, 2), h(tx + j, ty - j, 3));
                let p = Vec3::new(tx as f32 + 0.1 + 0.8 * hx, 0.0, ty as f32 + 0.1 + 0.8 * hy);
                let s = (0.7 + 0.8 * hs) * (0.6 + 0.4 * amount);
                let tilt = Quat::from_rotation_z((hx - 0.5) * 0.6) * Quat::from_rotation_x((hy - 0.5) * 0.6);
                let tf = Transform::from_translation(p + Vec3::Y * 0.08 * s)
                    .with_rotation(Quat::from_rotation_y(hs * 3.0) * tilt)
                    .with_scale(Vec3::splat(s));
                es.push(commands.spawn((Mesh3d(assets.crystal.clone()), MeshMaterial3d(assets.crystal_mat.clone()), tf)).id());
            }
            spawned.ore.insert(i, (level, es));
        }
    }
}

// ---------- particle / overlay batches ----------

#[derive(Default)]
struct Batch {
    pos: Vec<[f32; 3]>,
    col: Vec<[f32; 4]>,
    uv: Vec<[f32; 2]>,
    ind: Vec<u32>,
}

impl Batch {
    fn quad(&mut self, corners: [Vec3; 4], c: Rgba) {
        let base = self.pos.len() as u32;
        let c = lin(c);
        for (p, uv) in corners.iter().zip([[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]) {
            self.pos.push(p.to_array());
            self.col.push(c);
            self.uv.push(uv);
        }
        self.ind.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    /// Camera-facing square of half-size `r` (world units), rotated by `rot`.
    fn billboard(&mut self, c: Vec3, r: f32, rot: f32, col: Rgba) {
        let (s, co) = rot.sin_cos();
        let (right, up) = (Vec3::X, Vec3::new(0.0, PITCH_C, -PITCH_S));
        let (a, b) = ((right * co + up * s) * r, (up * co - right * s) * r);
        self.quad([c - a + b, c + a + b, c + a - b, c - a - b], col);
    }
    /// Flat on the ground (slightly raised), half-size `r`.
    fn flat(&mut self, c: Vec3, rx: f32, rz: f32, col: Rgba) {
        let (a, b) = (Vec3::X * rx, Vec3::Z * rz);
        self.quad([c - a - b, c + a - b, c + a + b, c - a + b], col);
    }
    fn into_mesh(mut self) -> Mesh {
        if self.ind.is_empty() {
            self.quad([Vec3::ZERO; 4], rgba(0.0, 0.0, 0.0, 0.0));
        }
        let n = self.pos.len();
        Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD)
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.pos)
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; n])
            .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, self.uv)
            .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, self.col)
            .with_inserted_indices(Indices::U32(self.ind))
    }
}

fn empty_batch() -> Mesh {
    Batch::default().into_mesh()
}

fn sync_fx(view: Res<View>, assets: Res<Assets3d>, mut meshes: ResMut<Assets<Mesh>>) {
    let (mut add, mut shade, mut smoke, mut solid) = (Batch::default(), Batch::default(), Batch::default(), Batch::default());
    if let Some(g) = &view.0 {
        for (at, r, age) in &g.decals {
            let a = 0.6 * (1.0 - age / 90.0);
            shade.flat(world_pos(*at, 0.4), r * 1.5 / PX, r * 1.5 / PX, rgba(0.02, 0.02, 0.02, a));
        }
        for p in &g.parts {
            let k = p.age / p.life;
            let c = p.c0 + (p.c1 - p.c0) * k;
            let size = (p.s0 + (p.s1 - p.s0) * k) / PX;
            let at = world_pos(p.p, p.z);
            if p.smoke {
                smoke.billboard(at, size, p.rot, c);
            } else {
                add.billboard(at, size, 0.0, c);
                if size > 6.0 / PX {
                    add.billboard(at, size * 0.45, 0.0, rgba(1.0, 1.0, 0.9, c.w));
                }
            }
        }
        for f in &g.fx {
            if let Fx::Shell { a, az, b, bz, t, life } = f {
                let k = (t / life).min(1.0);
                let at = world_pos(a.lerp(*b, k), az + (bz - az) * k);
                add.billboard(at, 9.0 / PX, 0.0, rgba(1.0, 0.6, 0.2, 0.8));
                add.billboard(at, 2.5 / PX, 0.0, rgba(1.0, 0.97, 0.8, 1.0));
            }
        }
        // Placement ghost: tinted tiles under the cursor.
        if let Some(kind) = g.placing.filter(|k| g.ready(*k) && g.mouse.x < g.view_w()) {
            let d = kind.def();
            let (gx, gy) = g.ghost_origin(kind, g.mouse);
            let near = g.world.near_own(g.me, kind, gx, gy);
            let pulse = 0.42 + 0.1 * (g.time * 6.0).sin();
            for ty in gy..gy + d.h {
                for tx in gx..gx + d.w {
                    let ok = near && g.world.place_tile_ok(kind, tx, ty);
                    let bare = ok && !kind.is_slab() && !g.world.slab[idx(tx.clamp(0, MAP_W - 1), ty.clamp(0, MAP_H - 1))];
                    let c = if !ok {
                        rgba(1.0, 0.15, 0.1, pulse)
                    } else if bare {
                        rgba(1.0, 0.8, 0.1, pulse)
                    } else {
                        rgba(0.2, 1.0, 0.35, pulse)
                    };
                    solid.flat(Vec3::new(tx as f32 + 0.5, 0.03, ty as f32 + 0.5), 0.46, 0.46, c);
                }
            }
        }
    }
    for (h, b) in assets.batches.iter().zip([add, shade, smoke, solid]) {
        let _ = meshes.insert(h, b.into_mesh());
    }
}

fn ground_iso(p: Vec3) -> Isometry3d {
    Isometry3d::new(p, Quat::from_rotation_arc(Vec3::Z, Vec3::Y))
}

fn draw_overlays(view: Res<View>, mut gz: Gizmos<OverlayGizmos>) {
    let Some(g) = &view.0 else { return };
    let col = |c: Rgba| crate::gfx::to_color(c);
    let w = &g.world;
    // Selected units: ring on the ground.
    for u in g.sel.iter().filter_map(|id| w.units.get(id)) {
        let k = if matches!(u.kind, UnitKind::Trooper | UnitKind::Rocketeer) { 0.65 } else { 1.15 };
        let at = world_pos(g.upos(u), Game::uz(u) + 1.0);
        gz.ellipse(ground_iso(at), Vec2::new(0.38, 0.3) * k, col(rgba(0.75, 1.0, 0.8, 0.95)));
    }
    // Selected structures: footprint outline, rally flag.
    for s in g.sel_structs.iter().filter_map(|id| w.structs.get(id)) {
        let d = s.kind.def();
        let c = rgba(0.75, 1.0, 0.8, 0.95);
        let mid = Vec3::new(s.x as f32 + d.w as f32 / 2.0, 0.05, s.y as f32 + d.h as f32 / 2.0);
        gz.rect(ground_iso(mid), Vec2::new(d.w as f32 + 0.1, d.h as f32 + 0.1), col(c));
        if let (Some(r), true) = (s.rally, TRAIN_AT.contains(&s.kind)) {
            let foot = Vec3::new(r.0 as f32 + 0.5, 0.02, r.1 as f32 + 0.5);
            let from = Vec3::new(mid.x, 0.05, s.y as f32 + d.h as f32);
            let n = (from.distance(foot) / 0.3).max(1.0) as i32;
            for i in (0..n).step_by(2) {
                gz.line(from.lerp(foot, i as f32 / n as f32), from.lerp(foot, (i + 1) as f32 / n as f32), col(fade(c, 0.7)));
            }
            let team = col(PCOL[s.owner as usize % 4]);
            let tip = foot + Vec3::Y * 0.9;
            gz.ellipse(ground_iso(foot), Vec2::new(0.25, 0.25), col(c));
            gz.line(foot, tip, Color::WHITE);
            let wave = (g.time * 5.0).sin() * 0.05;
            gz.line(tip, tip + Vec3::new(0.35, -0.08 + wave, 0.0), team);
            gz.line(tip + Vec3::new(0.35, -0.08 + wave, 0.0), tip - Vec3::Y * 0.22, team);
        }
    }
    // Repair / sell hover highlight and repairing markers.
    if matches!(g.mode, Mode::Repair | Mode::Sell) && g.mouse.x < g.view_w() {
        if let Some(s) = g.own_struct_at(g.mouse_tile(g.mouse)).and_then(|id| w.structs.get(&id)) {
            let d = s.kind.def();
            let c = if g.mode == Mode::Repair { rgba(0.35, 1.0, 0.45, 1.0) } else { rgba(1.0, 0.82, 0.3, 1.0) };
            let mid = Vec3::new(s.x as f32 + d.w as f32 / 2.0, 0.05, s.y as f32 + d.h as f32 / 2.0);
            let size = Vec2::new(d.w as f32, d.h as f32);
            gz.rect(ground_iso(mid), size, col(c));
            gz.rect(ground_iso(mid + Vec3::Y * struct_height(s.kind)), size, col(fade(c, 0.6)));
        }
    }
    for s in g.shown_structs().filter(|s| s.repairing) {
        let d = s.kind.def();
        let top = Vec3::new(s.x as f32 + d.w as f32 / 2.0, struct_height(s.kind) + 0.3, s.y as f32 + d.h as f32 / 2.0);
        let k = 0.55 + 0.45 * (g.time * 7.0).sin();
        let green = col(rgba(0.35, 1.0, 0.45, k));
        gz.line(top - Vec3::X * 0.22, top + Vec3::X * 0.22, green);
        gz.line(top - Vec3::Y * 0.22, top + Vec3::Y * 0.22, green);
    }
    // Shots and rings.
    for f in &g.fx {
        match f {
            Fx::Tracer { a, az, b, bz, t } => {
                let k = t / 0.1;
                let (pa, pb) = (world_pos(*a, *az), world_pos(*b, *bz));
                let (p, q) = (pa.lerp(pb, (k * 1.3 - 0.3).clamp(0.0, 1.0)), pa.lerp(pb, (k * 1.3).min(1.0)));
                gz.line(p, q, col(rgba(1.0, 0.97, 0.75, 0.95)));
            }
            Fx::Ring { at, z, t, life, r } => {
                let k = t / life;
                let rr = r * (0.25 + 0.75 * k) / PX;
                gz.ellipse(ground_iso(world_pos(*at, *z + 1.0)), Vec2::splat(rr), col(rgba(1.0, 0.9, 0.7, 0.8 * (1.0 - k))));
            }
            Fx::Shell { .. } => {}
        }
    }
    let _ = fpos;
}
