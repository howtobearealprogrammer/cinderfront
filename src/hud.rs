//! Screen-space UI: menus, sidebar (minimap, credits, power, build and train
//! buttons), hint line, tech tree, Esc menu and end-of-game banner.
//! Layout constants and click handling live in `game.rs`; this only draws.

use crate::game::{Game, Mode, MM, PCOL, SIDE};
use crate::gfx::{fade, hash2, rgba, rgba_image, shade, to_color, Rgba};
use crate::scene::{struct_file, unit_file, View};
use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::text::FontSize;
use cinderfront::sim::{idx, StructKind, Terrain, UnitKind, MAP_H, MAP_W, TRAIN_AT};

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MenuContent>()
            .add_systems(Startup, setup)
            .add_systems(Update, (update_menu, update_hud, update_tools, update_icons, update_pause).after(crate::RunGame));
    }
}

/// Lines for the menu / lobby screens: (text, size, colour). Empty while playing.
#[derive(Resource, Default)]
pub struct MenuContent {
    pub lines: Vec<(String, f32, Rgba)>,
    pub time: f32,
}

#[derive(Resource)]
struct HudRes {
    minimap: Handle<Image>,
    mm_timer: f32,
}

#[derive(Component)]
struct MenuRoot;
#[derive(Component)]
struct MenuLine(usize);
#[derive(Component)]
struct GameRoot;
#[derive(Component)]
struct Panel;
#[derive(Component)]
struct MmImage;
#[derive(Component)]
struct MmView;
#[derive(Component)]
struct CreditsText;
#[derive(Component)]
struct PowerFill;
#[derive(Component)]
struct PowerMark;
#[derive(Component)]
struct PowerText;
#[derive(Component)]
struct ToolBtn(usize);

#[derive(Clone, Copy, PartialEq)]
enum Slot {
    Build(usize),
    Train(usize),
}
#[derive(Clone, Copy, PartialEq)]
enum Role {
    Body,
    Progress,
    Shade,
    Cost,
    Tag,
    Name,
}
#[derive(Component)]
struct IconPart(Slot, Role);

#[derive(Component)]
struct HintBox;
#[derive(Component)]
struct PausePanel;
#[derive(Component)]
struct PauseItem(usize);
#[derive(Component)]
struct HintText;
#[derive(Component)]
struct StatusText;
#[derive(Component)]
struct Banner;
#[derive(Component)]
struct TechPanel;
#[derive(Component)]
struct TechRow(StructKind);
#[derive(Component)]
struct DragRect;

const FONT: &str = "embedded://DejaVuSans-Bold.ttf";

fn abs(x: f32, y: f32, w: f32, h: f32) -> Node {
    Node {
        position_type: PositionType::Absolute,
        left: px(x),
        top: px(y),
        width: px(w),
        height: px(h),
        ..default()
    }
}

fn text(font: &Handle<Font>, s: &str, size: f32, c: Rgba) -> (Text, TextFont, TextColor) {
    (
        Text::new(s),
        TextFont { font: font.clone().into(), font_size: FontSize::Px(size), ..default() },
        TextColor(to_color(c)),
    )
}

fn label(font: &Handle<Font>, s: &str, size: f32, c: Rgba, x: f32, y: f32) -> impl Bundle {
    (text(font, s, size, c), Node { position_type: PositionType::Absolute, left: px(x), top: px(y), ..default() })
}

fn bg(c: Rgba) -> BackgroundColor {
    BackgroundColor(to_color(c))
}

fn setup(mut commands: Commands, server: Res<AssetServer>, mut images: ResMut<Assets<Image>>) {
    let font: Handle<Font> = server.load(FONT);
    let n = 192u32;
    let mm = images.add(rgba_image(n, n, vec![0; (n * n * 4) as usize], RenderAssetUsages::all()));
    commands.spawn((Camera2d, Camera { order: 1, clear_color: ClearColorConfig::None, ..default() }));

    // Menu: a centred column of lines.
    commands
        .spawn((
            MenuRoot,
            Node {
                width: percent(100),
                height: percent(100),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                padding: UiRect::top(px(110)),
                ..default()
            },
            bg(rgba(0.05, 0.035, 0.03, 1.0)),
        ))
        .with_children(|p| {
            for i in 0..24 {
                p.spawn((MenuLine(i), text(&font, "", 20.0, rgba(1.0, 1.0, 1.0, 1.0)), Node { margin: UiRect::bottom(px(6)), ..default() }));
            }
        });

    let team = rgba(0.22, 0.52, 0.98, 1.0);
    let label_c = rgba(0.6, 0.62, 0.68, 1.0);
    commands
        .spawn((GameRoot, Node { width: percent(100), height: percent(100), ..default() }, Visibility::Hidden))
        .with_children(|root| {
            root.spawn((
                DragRect,
                Node { border: UiRect::all(px(1.5)), ..abs(0.0, 0.0, 0.0, 0.0) },
                bg(rgba(0.3, 1.0, 0.5, 0.08)),
                BorderColor::all(to_color(rgba(0.7, 1.0, 0.8, 0.9))),
                Visibility::Hidden,
            ));
            // Esc menu.
            root.spawn((
                PausePanel,
                Node { border: UiRect::all(px(2)), ..abs(0.0, 0.0, 340.0, 290.0) },
                bg(rgba(0.07, 0.08, 0.11, 0.96)),
                BorderColor::all(to_color(team)),
                GlobalZIndex(10),
                Visibility::Hidden,
            ))
            .with_children(|m| {
                m.spawn(label(&font, "MENU", 20.0, rgba(1.0, 0.84, 0.32, 1.0), 20.0, 14.0));
                for (i, name) in crate::game::PAUSE_ITEMS.iter().enumerate() {
                    m.spawn((
                        PauseItem(i),
                        Node {
                            border: UiRect::all(px(1.5)),
                            justify_content: JustifyContent::Center,
                            align_items: AlignItems::Center,
                            ..abs(18.0, 54.0 + i as f32 * 50.0, 300.0, 42.0)
                        },
                        bg(rgba(0.17, 0.19, 0.23, 1.0)),
                        BorderColor::all(to_color(rgba(0.36, 0.38, 0.44, 1.0))),
                    ))
                    .with_child(text(&font, name, 16.0, rgba(0.94, 0.95, 0.98, 1.0)));
                }
            });
            root.spawn((HintBox, abs(10.0, 0.0, 10.0, 26.0), bg(rgba(0.03, 0.04, 0.06, 0.72)), Visibility::Hidden))
                .with_children(|h| {
                    h.spawn((abs(0.0, 0.0, 3.0, 26.0), bg(team)));
                    h.spawn((HintText, label(&font, "", 14.0, rgba(0.92, 0.94, 0.97, 1.0), 12.0, 4.0)));
                });
            root.spawn((StatusText, label(&font, "", 20.0, rgba(1.0, 0.3, 0.25, 1.0), 14.0, 10.0)));
            root.spawn((
                Banner,
                Node {
                    position_type: PositionType::Absolute,
                    left: px(0),
                    width: px(100),
                    height: px(150),
                    flex_direction: FlexDirection::Column,
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    ..default()
                },
                bg(rgba(0.0, 0.0, 0.0, 0.55)),
                Visibility::Hidden,
            ))
            .with_children(|b| {
                b.spawn(text(&font, "", 80.0, rgba(1.0, 0.84, 0.3, 1.0)));
                b.spawn(text(&font, "", 18.0, rgba(0.85, 0.87, 0.92, 1.0)));
            });
            // Tech tree (hold Tab).
            root.spawn((
                TechPanel,
                Node {
                    position_type: PositionType::Absolute,
                    flex_direction: FlexDirection::Column,
                    padding: UiRect::all(px(20)),
                    border: UiRect::all(px(2)),
                    ..default()
                },
                bg(rgba(0.07, 0.08, 0.11, 0.95)),
                BorderColor::all(to_color(team)),
                Visibility::Hidden,
            ))
            .with_children(|t| {
                t.spawn((text(&font, "TECH TREE      (green = you have it)", 18.0, rgba(1.0, 0.84, 0.32, 1.0)), Node { margin: UiRect::bottom(px(14)), ..default() }));
                for (k, unlocks) in tech_rows() {
                    let s = format!("{:<14} unlocks  {}", k.def().name, unlocks);
                    t.spawn((TechRow(k), text(&font, &s, 13.0, rgba(0.75, 0.77, 0.82, 1.0)), Node { margin: UiRect::bottom(px(9)), ..default() }));
                }
            });

            // Sidebar.
            root.spawn((
                Panel,
                Node {
                    position_type: PositionType::Absolute,
                    top: px(0),
                    width: px(SIDE),
                    height: percent(100),
                    border: UiRect::left(px(2)),
                    ..default()
                },
                bg(rgba(0.10, 0.11, 0.14, 1.0)),
                BorderColor::all(to_color(fade(team, 0.8))),
            ))
            .with_children(|p| {
                p.spawn((abs(7.0, 7.0, MM + 6.0, MM + 6.0), bg(rgba(0.02, 0.02, 0.03, 1.0))));
                p.spawn((MmImage, abs(10.0, 10.0, MM, MM), ImageNode::new(mm.clone()))).with_child((
                    MmView,
                    Node { border: UiRect::all(px(1.5)), ..abs(0.0, 0.0, 10.0, 10.0) },
                    BorderColor::all(Color::WHITE),
                ));
                p.spawn(label(&font, "CREDITS", 10.0, label_c, 10.0, 222.0));
                p.spawn((CreditsText, label(&font, "0", 20.0, rgba(1.0, 0.84, 0.32, 1.0), 110.0, 214.0)));
                p.spawn(label(&font, "POWER", 10.0, label_c, 10.0, 243.0));
                p.spawn((abs(60.0, 244.0, SIDE - 70.0, 11.0), bg(rgba(0.02, 0.02, 0.03, 1.0)))).with_children(|b| {
                    b.spawn((PowerFill, abs(1.0, 1.0, 0.0, 9.0), bg(rgba(0.3, 0.85, 1.0, 1.0))));
                    b.spawn((PowerMark, abs(0.0, -1.0, 2.0, 13.0), bg(rgba(1.0, 1.0, 1.0, 1.0))));
                    b.spawn((PowerText, label(&font, "", 9.0, rgba(1.0, 1.0, 1.0, 1.0), 50.0, 0.0)));
                });
                for (i, name) in ["REPAIR  [R]", "SELL  [V]"].iter().enumerate() {
                    p.spawn((ToolBtn(i), Node { border: UiRect::all(px(1.5)), ..abs(8.0 + i as f32 * 104.0, 264.0, 100.0, 22.0) }, bg(rgba(0.2, 0.2, 0.2, 1.0)), BorderColor::all(Color::BLACK)))
                        .with_child(label(&font, name, 11.0, rgba(1.0, 1.0, 1.0, 1.0), 18.0, 4.0));
                }
                let slots = StructKind::BUILDABLE
                    .iter()
                    .enumerate()
                    .map(|(i, k)| (Slot::Build(i), k.def().name, struct_file(*k)))
                    .chain(UnitKind::ALL.iter().enumerate().map(|(i, k)| (Slot::Train(i), k.def().name, Some(unit_file(*k)))));
                for (slot, name, file) in slots {
                    let (i, y0) = match slot {
                        Slot::Build(i) => (i, 290.0),
                        Slot::Train(i) => (i, 290.0 + 6.0 * 44.0 + 10.0),
                    };
                    let (x, y) = (8.0 + (i % 2) as f32 * 104.0, y0 + (i / 2) as f32 * 44.0);
                    p.spawn((
                        IconPart(slot, Role::Body),
                        Node { border: UiRect::all(px(1.5)), ..abs(x, y, 100.0, 41.0) },
                        bg(rgba(0.17, 0.19, 0.23, 1.0)),
                        BorderColor::all(to_color(rgba(0.36, 0.38, 0.44, 1.0))),
                    ))
                    .with_children(|b| {
                        b.spawn((IconPart(slot, Role::Progress), abs(0.0, 0.0, 0.0, 38.0), bg(rgba(0.2, 0.65, 0.35, 0.55))));
                        match file {
                            Some(f) => {
                                b.spawn((abs(46.0, 1.0, 50.0, 37.5), ImageNode::new(server.load(format!("embedded://icons/{f}.png")))));
                            }
                            None => {
                                b.spawn((abs(62.0, 8.0, 22.0, 22.0), bg(rgba(0.62, 0.62, 0.65, 1.0))));
                            }
                        }
                        b.spawn((IconPart(slot, Role::Name), label(&font, name, 10.0, rgba(0.94, 0.95, 0.98, 1.0), 4.0, 2.0)));
                        b.spawn((IconPart(slot, Role::Tag), label(&font, "", 9.0, rgba(0.5, 1.0, 0.6, 1.0), 4.0, 14.0)));
                        b.spawn((IconPart(slot, Role::Cost), label(&font, "", 10.0, rgba(1.0, 0.82, 0.3, 1.0), 4.0, 25.0)));
                        b.spawn((IconPart(slot, Role::Shade), abs(0.0, 0.0, 97.0, 38.0), bg(rgba(0.05, 0.05, 0.07, 0.62))));
                    });
                }
            });
        });
    commands.insert_resource(HudRes { minimap: mm, mm_timer: 0.0 });
}

fn tech_rows() -> Vec<(StructKind, String)> {
    let mut rows = Vec::new();
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
    rows
}

fn update_menu(
    menu: Res<MenuContent>,
    view: Res<View>,
    mut root: Query<&mut Visibility, (With<MenuRoot>, Without<GameRoot>)>,
    mut game_root: Query<&mut Visibility, (With<GameRoot>, Without<MenuRoot>)>,
    mut lines: Query<(&MenuLine, &mut Text, &mut TextFont, &mut TextColor, &mut Node)>,
) {
    let playing = view.0.is_some();
    if let Ok(mut v) = root.single_mut() {
        v.set_if_neq(if playing { Visibility::Hidden } else { Visibility::Inherited });
    }
    if let Ok(mut v) = game_root.single_mut() {
        v.set_if_neq(if playing { Visibility::Inherited } else { Visibility::Hidden });
    }
    if playing {
        return;
    }
    for (l, mut t, mut f, mut c, mut node) in &mut lines {
        match menu.lines.get(l.0) {
            Some((s, size, col)) => {
                if t.0 != *s {
                    put(&mut t, |x| x.0 = s.clone());
                }
                put(&mut f, |x| x.font_size = FontSize::Px(*size));
                put(&mut c, |x| x.0 = to_color(*col));
                put(&mut node, |x| x.display = Display::Flex);
            }
            None => node.display = Display::None,
        }
    }
}

/// Write a component only when the value really changes. Bevy re-lays-out the
/// whole UI tree (and re-shapes text) whenever a Node or Text is touched.
fn put<T: PartialEq + Clone>(m: &mut Mut<T>, f: impl FnOnce(&mut T)) {
    let mut v = (**m).clone();
    f(&mut v);
    if **m != v {
        **m = v;
    }
}

fn set_box(node: &mut Node, x: f32, y: f32, w: f32, h: f32) {
    node.left = px(x);
    node.top = px(y);
    node.width = px(w.max(0.0));
    node.height = px(h.max(0.0));
}

#[allow(clippy::type_complexity)]
fn update_hud(
    view: Res<View>,
    time: Res<Time>,
    mut hud: ResMut<HudRes>,
    mut images: ResMut<Assets<Image>>,
    mut nodes: ParamSet<(
        Query<&mut Node, With<Panel>>,
        Query<&mut Node, With<MmView>>,
        Query<(&mut Node, &mut BackgroundColor), With<PowerFill>>,
        Query<&mut Node, With<PowerMark>>,
        Query<(&mut Node, &mut Visibility), With<HintBox>>,
        Query<(&mut Node, &mut Visibility, &Children), With<Banner>>,
        Query<(&mut Node, &mut Visibility), With<TechPanel>>,
        Query<(&mut Node, &mut Visibility), With<DragRect>>,
    )>,
    mut texts: ParamSet<(
        Query<&mut Text, With<CreditsText>>,
        Query<&mut Text, With<PowerText>>,
        Query<&mut Text, With<HintText>>,
        Query<&mut Text, With<StatusText>>,
        Query<(&mut Text, &mut TextColor)>,
        Query<(&TechRow, &mut TextColor)>,
    )>,
    status: Res<crate::Status>,
) {
    let Some(g) = &view.0 else { return };
    let (vw, sh) = (g.view_w(), g.screen.y);
    if let Ok(mut n) = nodes.p0().single_mut() {
        put(&mut n, |x| x.left = px(vw));
    }

    // Minimap image, refreshed a few times a second.
    hud.mm_timer -= time.delta_secs();
    if hud.mm_timer <= 0.0 {
        hud.mm_timer = 0.25;
        if let Some(mut img) = images.get_mut(&hud.minimap) {
            draw_minimap(g, &mut img);
        }
    }
    let r = g.view_ground();
    let k = MM / MAP_W as f32;
    let (x0, y0) = ((r.min.x * k).max(0.0), (r.min.y * k).max(0.0));
    let (x1, y1) = ((r.max.x * k).min(MM), (r.max.y * k).min(MM));
    if let Ok(mut n) = nodes.p1().single_mut() {
        put(&mut n, |x| set_box(x, x0, y0, x1 - x0, y1 - y0));
    }

    // Credits and power.
    let pl = &g.world.players[g.me as usize];
    if let Ok(mut t) = texts.p0().single_mut() {
        put(&mut t, |x| x.0 = format!("{:>8}", g.shown_credits.round() as i32));
    }
    let low = pl.power_used > pl.power_made;
    let cap = pl.power_made.max(pl.power_used).max(1) as f32;
    let bw = SIDE - 72.0;
    let pc = if low { fade(rgba(1.0, 0.25, 0.2, 1.0), 0.6 + 0.4 * (g.time * 8.0).sin()) } else { rgba(0.3, 0.85, 1.0, 1.0) };
    if let Ok((mut n, mut c)) = nodes.p2().single_mut() {
        put(&mut n, |x| x.width = px(bw * pl.power_used as f32 / cap));
        put(&mut c, |x| x.0 = to_color(pc));
    }
    if let Ok(mut n) = nodes.p3().single_mut() {
        put(&mut n, |x| x.left = px(bw * pl.power_made as f32 / cap));
    }
    if let Ok(mut t) = texts.p1().single_mut() {
        put(&mut t, |x| x.0 = format!("{} / {}", pl.power_used, pl.power_made));
    }

    // Hint line.
    let hint = g.hint();
    if let Ok((mut n, mut v)) = nodes.p4().single_mut() {
        put(&mut n, |x| x.top = px(sh - 34.0));
        put(&mut n, |x| x.width = px(hint.chars().count() as f32 * 8.4 + 24.0));
        v.set_if_neq(if hint.is_empty() { Visibility::Hidden } else { Visibility::Inherited });
    }
    if let Ok(mut t) = texts.p2().single_mut() {
        if t.0 != hint {
            put(&mut t, |x| x.0 = hint);
        }
    }
    if let Ok(mut t) = texts.p3().single_mut() {
        if t.0 != status.0 {
            put(&mut t, |x| x.0 = status.0.clone());
        }
    }

    // Victory / defeat banner.
    let over = if g.world.winner == Some(g.me) {
        Some(("VICTORY", rgba(1.0, 0.84, 0.3, 1.0)))
    } else if g.world.players[g.me as usize].defeated {
        Some(("DEFEATED", rgba(1.0, 0.25, 0.2, 1.0)))
    } else {
        None
    };
    let (mut banner_text, mut banner_sub) = (None, None);
    if let Ok((mut n, mut v, children)) = nodes.p5().single_mut() {
        put(&mut n, |x| x.top = px(sh * 0.4 - 105.0));
        put(&mut n, |x| x.width = px(vw));
        v.set_if_neq(if over.is_some() { Visibility::Inherited } else { Visibility::Hidden });
        banner_text = children.first().copied();
        banner_sub = children.get(1).copied();
    }
    if let Some(e) = banner_sub {
        let sub = match g.menu_in {
            Some(t) => format!("Returning to the main menu in {}", t.ceil().max(0.0) as i32),
            None if over.is_some() => "Keep watching, or press Esc for the menu".to_string(),
            None => String::new(),
        };
        if let Ok((mut txt, _)) = texts.p4().get_mut(e) {
            if txt.0 != sub {
                put(&mut txt, |x| x.0 = sub);
            }
        }
    }
    if let (Some((s, c)), Some(e)) = (over, banner_text) {
        if let Ok((mut t, mut tc)) = texts.p4().get_mut(e) {
            put(&mut t, |x| x.0 = s.to_string());
            put(&mut tc, |x| x.0 = to_color(c));
        }
    }

    // Tech tree.
    if let Ok((mut n, mut v)) = nodes.p6().single_mut() {
        put(&mut n, |x| x.left = px(((vw - 720.0) / 2.0).max(10.0)));
        put(&mut n, |x| x.top = px(60.0));
        v.set_if_neq(if g.show_tech { Visibility::Inherited } else { Visibility::Hidden });
    }
    if g.show_tech {
        for (row, mut c) in &mut texts.p5() {
            let have = g.world.has_struct(g.me, row.0);
            put(&mut c, |x| x.0 = to_color(if have { rgba(0.5, 1.0, 0.6, 1.0) } else { rgba(0.72, 0.74, 0.8, 1.0) }));
        }
    }

    // Drag-select box.
    let m = g.mouse;
    if let Ok((mut n, mut v)) = nodes.p7().single_mut() {
        match g.drag.filter(|d0| d0.distance(m) >= 6.0) {
            Some(d0) => {
                put(&mut n, |x| set_box(x, d0.x.min(m.x), d0.y.min(m.y), (d0.x - m.x).abs(), (d0.y - m.y).abs()));
                v.set_if_neq(Visibility::Inherited);
            }
            None => {
                v.set_if_neq(Visibility::Hidden);
            }
        }
    }
}

fn update_tools(view: Res<View>, mut tools: Query<(&ToolBtn, &mut BackgroundColor, &mut BorderColor)>) {
    let Some(g) = &view.0 else { return };
    let m = g.mouse;
    // Repair / sell buttons.
    for (b, mut c, mut border) in &mut tools {
        let (mode, col) = if b.0 == 0 { (Mode::Repair, rgba(0.35, 1.0, 0.45, 1.0)) } else { (Mode::Sell, rgba(1.0, 0.82, 0.3, 1.0)) };
        let on = g.mode == mode;
        let k = if on { 1.0 } else if g.tool_rect(b.0).contains(m) { 0.5 } else { 0.28 };
        put(&mut c, |x| x.0 = to_color(fade(shade(col, 0.5), k)));
        border.set_if_neq(BorderColor::all(to_color(if on { col } else { rgba(0.36, 0.38, 0.44, 1.0) })));
    }

}

#[allow(clippy::type_complexity)]
fn update_icons(
    view: Res<View>,
    mut icons: Query<(&IconPart, &mut Node, Option<&mut BackgroundColor>, Option<&mut BorderColor>, Option<&mut Text>, Option<&mut TextColor>, Option<&mut Visibility>)>,
) {
    let Some(g) = &view.0 else { return };
    let (m, team, pl) = (g.mouse, PCOL[g.me as usize % 4], &g.world.players[g.me as usize]);
    for (part, mut node, bgc, border, text, tcol, vis) in &mut icons {
        let (name_needs, cost, enabled, progress, tag, rect) = match part.0 {
            Slot::Build(i) => {
                let kind = StructKind::BUILDABLE[i];
                let d = kind.def();
                let cur = pl.construction.as_ref().filter(|c| c.kind == kind);
                let enabled = g.world.can_build(g.me, kind) && (pl.construction.is_none() || cur.is_some());
                let progress = cur.map(|c| c.spent as f32 / d.cost as f32);
                let tag = if cur.is_some_and(|c| c.ready) { "READY".to_string() } else { String::new() };
                (g.struct_needs(kind), d.cost, enabled, progress, tag, g.icon_rect(i, false))
            }
            Slot::Train(i) => {
                let kind = UnitKind::ALL[i];
                let d = kind.def();
                let q = &pl.queues[TRAIN_AT.iter().position(|s| *s == d.made_at).unwrap()];
                let n = q.items.iter().filter(|k| **k == kind).count();
                let progress = (q.items.front() == Some(&kind)).then(|| q.spent as f32 / d.cost as f32);
                let tag = if n > 0 { format!("x{n}") } else { String::new() };
                (g.unit_needs(kind), d.cost, g.world.can_train(g.me, kind), progress, tag, g.icon_rect(i, true))
            }
        };
        let hover = rect.contains(m) && enabled;
        match part.1 {
            Role::Body => {
                let ready = tag == "READY";
                let k = if hover { 1.35 } else { 1.0 };
                if let Some(mut c) = bgc {
                    put(&mut c, |x| x.0 = to_color(shade(rgba(0.17, 0.19, 0.23, 1.0), k)));
                }
                let bc = if ready {
                    fade(rgba(0.4, 1.0, 0.55, 1.0), 0.6 + 0.4 * (g.time * 6.0).sin())
                } else if hover {
                    team
                } else {
                    rgba(0.36, 0.38, 0.44, 1.0)
                };
                if let Some(mut b) = border {
                    b.set_if_neq(BorderColor::all(to_color(bc)));
                }
                put(&mut node, |x| x.border = UiRect::all(px(if ready || hover { 3.0 } else { 1.5 })));
            }
            Role::Progress => put(&mut node, |n| n.width = px(97.0 * progress.unwrap_or(0.0).clamp(0.0, 1.0))),
            Role::Shade => {
                if let Some(mut v) = vis {
                    v.set_if_neq(if enabled { Visibility::Hidden } else { Visibility::Inherited });
                }
            }
            Role::Cost => {
                if let (Some(mut t), Some(mut c)) = (text, tcol) {
                    let (s, col) = match name_needs {
                        Some(n) => (format!("needs {n}"), rgba(1.0, 0.45, 0.4, 1.0)),
                        None => (format!("${cost}"), if enabled { rgba(1.0, 0.82, 0.3, 1.0) } else { rgba(0.5, 0.45, 0.3, 1.0) }),
                    };
                    if t.0 != s {
                        put(&mut t, |x| x.0 = s);
                    }
                    put(&mut c, |x| x.0 = to_color(col));
                }
            }
            Role::Tag => {
                if let Some(mut t) = text {
                    if t.0 != tag {
                        put(&mut t, |x| x.0 = tag);
                    }
                }
            }
            Role::Name => {
                if let Some(mut c) = tcol {
                    put(&mut c, |x| x.0 = to_color(if enabled { rgba(0.94, 0.95, 0.98, 1.0) } else { rgba(0.5, 0.5, 0.54, 1.0) }));
                }
            }
        }
    }

}

fn draw_minimap(g: &Game, img: &mut Image) {
    let n = 192usize;
    let s = n / MAP_W as usize;
    let Some(data) = img.data.as_mut() else { return };
    let mut put = |x: usize, y: usize, c: Rgba| {
        if x < n && y < n {
            let i = (y * n + x) * 4;
            data[i] = (c.x.clamp(0.0, 1.0) * 255.0) as u8;
            data[i + 1] = (c.y.clamp(0.0, 1.0) * 255.0) as u8;
            data[i + 2] = (c.z.clamp(0.0, 1.0) * 255.0) as u8;
            data[i + 3] = 255;
        }
    };
    for y in 0..MAP_H {
        for x in 0..MAP_W {
            let i = idx(x, y);
            let k = 0.9 + 0.2 * hash2(x, y, 9);
            let c = match g.world.terrain[i] {
                Terrain::Dust => rgba(0.50 * k, 0.39 * k, 0.28 * k, 1.0),
                Terrain::Rock if g.world.slab[i] => rgba(0.62, 0.62, 0.65, 1.0),
                Terrain::Rock => rgba(0.33 * k, 0.32 * k, 0.34 * k, 1.0),
                Terrain::Ore => rgba(1.0, 0.58, 0.12, 1.0),
                Terrain::Cliff => rgba(0.13, 0.10, 0.10, 1.0),
            };
            let c = if !g.explored[i] {
                rgba(0.02, 0.02, 0.03, 1.0)
            } else if !g.visible[i] {
                shade(c, 0.5)
            } else {
                c
            };
            for dy in 0..s {
                for dx in 0..s {
                    put(x as usize * s + dx, y as usize * s + dy, c);
                }
            }
        }
    }
    for st in g.shown_structs() {
        let d = st.kind.def();
        let c = PCOL[st.owner as usize % 4];
        for y in (st.y as usize * s)..((st.y + d.h) as usize * s) {
            for x in (st.x as usize * s)..((st.x + d.w) as usize * s) {
                put(x, y, c);
            }
        }
    }
    for u in g.world.units.values().filter(|u| !u.carried && g.unit_visible(u)) {
        let p = Game::unit_tile_pos(u) * s as f32;
        let c = shade(PCOL[u.owner as usize % 4], 1.25);
        for dy in 0..3 {
            for dx in 0..3 {
                put((p.x as usize + dx).saturating_sub(1), (p.y as usize + dy).saturating_sub(1), c);
            }
        }
    }
}

#[allow(clippy::type_complexity)]
fn update_pause(
    view: Res<View>,
    mut panel: Query<(&mut Node, &mut Visibility), (With<PausePanel>, Without<PauseItem>)>,
    mut items: Query<(&PauseItem, &mut BackgroundColor, &mut BorderColor, &Children), Without<PausePanel>>,
    mut texts: Query<(&mut Text, &mut TextColor)>,
) {
    let Some(g) = &view.0 else { return };
    let Ok((mut n, mut v)) = panel.single_mut() else { return };
    let Some(pm) = g.pause else {
        v.set_if_neq(Visibility::Hidden);
        return;
    };
    v.set_if_neq(Visibility::Inherited);
    let r = g.pause_rect(0);
    n.left = px(r.min.x - 20.0);
    n.top = px(r.min.y - 56.0);
    for (item, mut c, mut b, children) in &mut items {
        let (on, ok) = (item.0 == pm.sel, pm.enabled(item.0));
        c.0 = to_color(if on && ok { rgba(0.24, 0.30, 0.40, 1.0) } else { rgba(0.17, 0.19, 0.23, 1.0) });
        *b = BorderColor::all(to_color(if on && ok { PCOL[g.me as usize % 4] } else { rgba(0.36, 0.38, 0.44, 1.0) }));
        if let Some((mut t, mut tc)) = children.first().and_then(|e| texts.get_mut(*e).ok()) {
            let label = match item.0 {
                1 if !ok => "Restart map  (solo only)".to_string(),
                0 if pm.paused => "Resume  (paused)".to_string(),
                i => crate::game::PAUSE_ITEMS[i].to_string(),
            };
            if t.0 != label {
                t.0 = label;
            }
            tc.0 = to_color(if ok { rgba(0.94, 0.95, 0.98, 1.0) } else { rgba(0.5, 0.5, 0.54, 1.0) });
        }
    }
}
