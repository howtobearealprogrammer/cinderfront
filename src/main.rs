mod cursor;
mod game;
mod gfx;
mod hud;
mod scene;
mod terrain;

use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::platform::cell::SyncCell;
use bevy::prelude::*;
use bevy::render::view::screenshot::{save_to_disk, Screenshot};
use bevy::window::{CursorGrabMode, CursorOptions, MonitorSelection, PrimaryWindow, WindowMode, WindowResolution};
use cinderfront::net::{self, Conn, Finder, Host, C2H, H2C};
use cinderfront::sim::{Command, StructKind, UnitKind, World, TICKS_PER_SEC};
use game::{Game, InputSnap};
use gfx::{rgba, Rgba};
use hud::MenuContent;
use scene::View;
use std::collections::{BTreeMap, VecDeque};
use std::net::SocketAddr;

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/assets.rs"));
}

const MAP_SEED: u32 = 7;
const TICK_DT: f32 = 1.0 / TICKS_PER_SEC as f32;
const TICKS_PER_TURN: u32 = 2;
const TURN_DT: f32 = TICK_DT * TICKS_PER_TURN as f32;
const HASH_EVERY: u32 = 20;
/// Seconds the victory / defeat banner shows before returning to the menu.
const END_DELAY: f32 = 5.0;

/// Runs the session (net + sim + input) before anything is drawn.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct RunGame;

/// Red status line (desync, disconnect).
#[derive(Resource, Default)]
pub struct Status(pub String);

enum Net {
    Solo,
    Host(Host),
    Client(Conn),
}

/// Lockstep bookkeeping for a running game. The `Game` itself lives in `View`.
struct Play {
    net: Net,
    queue: VecDeque<Vec<(u8, Command)>>,
    pending: Vec<(u8, Command)>,
    tick_acc: f32,
    turn_acc: f32,
    sub: u32,
    turn: u32,
    hashes: BTreeMap<u32, u64>,
    /// Debug aid (CF_AUTOPLAY=1): scripted orders, to exercise netcode without a human.
    auto: Option<u8>,
    /// Seconds since the match was decided.
    over: Option<f32>,
}

impl Play {
    fn new(net: Net) -> Play {
        Play {
            net,
            queue: VecDeque::new(),
            pending: Vec::new(),
            tick_acc: 0.0,
            turn_acc: 0.0,
            sub: 0,
            turn: 0,
            hashes: BTreeMap::new(),
            auto: std::env::var_os("CF_AUTOPLAY").map(|_| 0),
            over: None,
        }
    }

    fn autoplay(&mut self, game: &Game) -> Vec<Command> {
        let (w, me) = (&game.world, game.me);
        let mut out = Vec::new();
        let Some(step) = &mut self.auto else { return out };
        let kind = StructKind::Power;
        if *step == 0 && self.turn >= 5 {
            *step = 1;
            out.push(Command::StartBuild(kind));
        } else if *step == 1 && w.players[me as usize].construction.as_ref().is_some_and(|c| c.ready) {
            *step = 2;
            let (cx, cy) = w.starts[me as usize];
            let spot = (0..16 * 16).map(|i| (cx - 8 + i % 16, cy - 8 + i / 16)).find(|(x, y)| w.can_place(me, kind, *x, *y));
            if let Some((x, y)) = spot {
                out.push(Command::Place { kind, x, y });
            }
        } else if *step == 2 && self.turn >= 100 {
            *step = 3;
            let units = w.units.values().filter(|u| u.owner == me).map(|u| u.id).collect();
            out.push(Command::Move { units, x: 47, y: 47, attack: true });
        }
        out
    }

    fn frame(&mut self, game: &mut Game, status: &mut String, dt: f32, inp: &InputSnap) {
        match &mut self.net {
            Net::Solo => {}
            Net::Host(h) => {
                let (cmds, reports) = h.collect();
                self.pending.extend(cmds);
                for (p, turn, hash) in reports {
                    if self.hashes.get(&turn).is_some_and(|mine| *mine != hash) {
                        *status = format!("DESYNC with player {} at turn {turn}", p + 1);
                        h.broadcast(&H2C::Desync);
                    }
                }
            }
            Net::Client(c) => {
                for m in c.poll::<H2C>() {
                    match m {
                        H2C::Turn { cmds } => self.queue.push_back(cmds),
                        H2C::Desync => *status = "DESYNC detected by host".into(),
                        _ => {}
                    }
                }
                if !c.open {
                    *status = "Host disconnected".into();
                }
            }
        }

        // The host (or solo player) cuts a new turn every TURN_DT.
        if !matches!(self.net, Net::Client(_)) {
            self.turn_acc += dt;
            while self.turn_acc >= TURN_DT {
                self.turn_acc -= TURN_DT;
                let cmds = std::mem::take(&mut self.pending);
                if let Net::Host(h) = &mut self.net {
                    h.broadcast(&H2C::Turn { cmds: cmds.clone() });
                }
                self.queue.push_back(cmds);
            }
        }

        // Advance the sim only as far as we hold turns for; catch up if behind.
        self.tick_acc += dt * if self.queue.len() > 2 { 2.0 } else { 1.0 };
        while self.tick_acc >= TICK_DT {
            if self.sub == 0 {
                match self.queue.pop_front() {
                    Some(cmds) => {
                        for (p, c) in cmds {
                            game.world.apply(p, &c);
                        }
                    }
                    None => {
                        self.tick_acc = TICK_DT * 0.999;
                        break;
                    }
                }
            }
            game.world.tick();
            game.after_tick();
            self.tick_acc -= TICK_DT;
            self.sub = (self.sub + 1) % TICKS_PER_TURN;
            if self.sub == 0 {
                self.turn += 1;
                if self.turn % HASH_EVERY == 0 {
                    let hash = game.world.hash();
                    match &mut self.net {
                        Net::Host(_) => {
                            self.hashes.insert(self.turn, hash);
                            while self.hashes.len() > 64 {
                                self.hashes.pop_first();
                            }
                        }
                        Net::Client(c) => c.send(&C2H::Hash { turn: self.turn, hash }),
                        Net::Solo => {}
                    }
                }
            }
        }

        let me = game.me;
        let mut cmds = game.frame(self.tick_acc / TICK_DT, dt, inp);
        cmds.extend(self.autoplay(game));
        for cmd in cmds {
            match &mut self.net {
                Net::Client(c) => c.send(&C2H::Cmd(cmd)),
                _ => self.pending.push((me, cmd)),
            }
        }
    }
}

struct Menu {
    ip: String,
    finder: Option<Finder>,
    found: Vec<SocketAddr>,
    msg: String,
}

enum Screen {
    Menu(Menu),
    HostLobby(Host, u8),
    ClientLobby { conn: Conn, players: u8 },
    Play(Box<Play>),
}

/// The current screen. Sockets and channels are not `Sync`, hence the cells.
#[derive(Resource)]
struct Session {
    screen: SyncCell<Screen>,
    /// A match this machine hosts but its player has left (knocked out): it keeps
    /// running, unseen, so the others can finish it.
    background: SyncCell<Option<(Box<Play>, Game)>>,
}

fn menu(msg: &str) -> Screen {
    Screen::Menu(Menu { ip: String::new(), finder: Finder::new().ok(), found: Vec::new(), msg: msg.into() })
}

/// `auto_start`: begin automatically once that many players are in (0 = wait for Enter).
fn host(port: u16, auto_start: u8) -> Screen {
    match Host::new(port) {
        Ok(h) => Screen::HostLobby(h, auto_start),
        Err(e) => menu(&format!("Cannot host: {e}")),
    }
}

fn join(addr: &str) -> Screen {
    let full = if addr.contains(':') { addr.to_string() } else { format!("{addr}:{}", net::TCP_PORT) };
    match full.parse::<SocketAddr>().map_err(|e| e.to_string()).and_then(|a| Conn::connect(a).map_err(|e| e.to_string())) {
        Ok(conn) => Screen::ClientLobby { conn, players: 0 },
        Err(e) => menu(&format!("Cannot join {full}: {e}")),
    }
}

fn solo(view: &mut View) -> Screen {
    view.0 = Some(Game::new(World::new(MAP_SEED, 2), 0));
    Screen::Play(Box::new(Play::new(Net::Solo)))
}

/// Sandbox with one of everything, for eyeballing the renderer.
fn demo(view: &mut View) -> Screen {
    let mut w = World::new(MAP_SEED, 2);
    let (cx, cy) = w.starts[0];
    let spots = [
        (StructKind::Power, -6, -1),
        (StructKind::Refinery, 3, -2),
        (StructKind::Barracks, -6, 2),
        (StructKind::Factory, 3, 1),
        (StructKind::Airpad, -3, -5),
        (StructKind::Turret, 5, 4),
        (StructKind::Wall, 6, 4),
        (StructKind::Wall, 7, 4),
        (StructKind::Armory, -9, 2),
        (StructKind::TechLab, -9, -1),
        (StructKind::RocketTurret, -1, 4),
    ];
    for (k, dx, dy) in spots {
        w.create_struct(0, k, cx + dx, cy + dy, k.def().hp);
    }
    for (i, k) in UnitKind::ALL.iter().enumerate() {
        w.new_unit(0, *k, (cx - 5 + 2 * i as i32, cy + 5));
        w.new_unit(1, *k, (cx - 5 + 2 * i as i32, cy + 9));
    }
    w.players[0].credits = 20000;
    let mut g = Game::new(w, 0);
    if let Some(z) = std::env::var("CF_ZOOM").ok().and_then(|z| z.parse().ok()) {
        g.zoom = z;
    }
    view.0 = Some(g);
    Screen::Play(Box::new(Play::new(Net::Solo)))
}

fn line(s: impl Into<String>, size: f32, c: Rgba) -> (String, f32, Rgba) {
    (s.into(), size * 0.72, c)
}

#[allow(clippy::too_many_arguments)]
fn run_session(
    mut session: ResMut<Session>,
    mut view: ResMut<View>,
    mut status: ResMut<Status>,
    mut menu_out: ResMut<MenuContent>,
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    scroll: Res<AccumulatedMouseScroll>,
    mut typed: MessageReader<KeyboardInput>,
    windows: Query<&Window>,
) {
    let dt = time.delta_secs().min(0.25);
    let Session { screen, background } = &mut *session;
    let (screen, background) = (screen.get(), background.get());
    if let Some((p, g)) = background.as_mut() {
        let mut quiet = String::new();
        p.frame(g, &mut quiet, dt, &InputSnap::default());
        let everyone_gone = matches!(&p.net, Net::Host(h) if h.clients.iter().all(|c| !c.open));
        if g.world.winner.is_some() || everyone_gone {
            *background = None;
        }
    }
    let mut leave = false;
    let (white, light, gray, red, orange) = (
        rgba(1.0, 1.0, 1.0, 1.0),
        rgba(0.78, 0.78, 0.78, 1.0),
        rgba(0.51, 0.51, 0.51, 1.0),
        rgba(0.9, 0.16, 0.22, 1.0),
        rgba(1.0, 0.63, 0.0, 1.0),
    );
    let chars: Vec<char> = typed
        .read()
        .filter(|k| k.state.is_pressed())
        .filter_map(|k| match &k.logical_key {
            Key::Character(s) => s.chars().next(),
            _ => None,
        })
        .collect();
    menu_out.time += dt;
    let mut next: Option<Screen> = None;
    match screen {
        Screen::Menu(m) => {
            if let Some(f) = &m.finder {
                for a in f.poll() {
                    if !m.found.contains(&a) {
                        m.found.push(a);
                    }
                }
            }
            for c in chars {
                if (c.is_ascii_digit() || c == '.' || c == ':') && m.ip.len() < 21 {
                    m.ip.push(c);
                }
            }
            if keys.just_pressed(KeyCode::Backspace) {
                m.ip.pop();
            }
            let found: Vec<String> = m.found.iter().map(|a| a.to_string()).collect();
            let found = if found.is_empty() { "searching...".to_string() } else { found.join(", ") };
            let mut lines = vec![
                line("CINDERFRONT", 80.0, orange),
                line("", 10.0, white),
                line("[H] Host LAN game     [J] Join     [S] Solo sandbox", 28.0, white),
                line(format!("Join address (type, blank = first found): {}_", m.ip), 24.0, light),
                line(format!("Hosts on LAN: {found}"), 24.0, light),
                line(m.msg.clone(), 24.0, red),
                line(
                    if background.is_some() { "Still hosting your last match for the other players - it closes when they finish" } else { "" },
                    20.0,
                    gray,
                ),
                line("", 10.0, white),
            ];
            let help = [
                "Left click / drag: select units (or buildings)     Double click: all of that kind on screen",
                "Right click: move, attack, harvest - the cursor shows which     Shift: queue     Ctrl: formation     Alt: attack-move",
                "A: attack-move     Q: patrol     S: stop     R: repair     V: sell     F11: fullscreen",
                "1..9: select group (twice: jump to it)     Ctrl+1..9: set group     Shift+1..9: add to group",
                "Sidebar: left click build or train, right click cancel",
                "When a structure shows READY, click it again, then left click the map to place (Shift = build another)",
                "Structures need rock. Lay slabs first or they start at half strength.",
                "MCV: build at Factory, drive (or airlift) to clear rock, press F to unpack a new Command Yard",
                "TECH TREE: hold Tab in game. Locked buttons say what they need",
                "GATHER POINT: select a Barracks / Factory / Airpad, then right click the map",
                "AIRLIFT: select ground units, press T, click anywhere - free Carriers fetch and deliver",
                "         or select a Carrier, right click a unit to lift, right click ground to drop",
                "Arrows / screen edge / middle-drag: scroll     Wheel: zoom     Space: jump to base",
            ];
            lines.extend(help.iter().map(|h| line(*h, 22.0, gray)));
            menu_out.lines = lines;
            if keys.just_pressed(KeyCode::KeyH) && background.is_some() {
                m.msg = "Can't host a new game until your last match finishes".into();
            } else if keys.just_pressed(KeyCode::KeyH) {
                next = Some(host(net::TCP_PORT, 0));
            } else if keys.just_pressed(KeyCode::KeyS) {
                next = Some(solo(&mut view));
            } else if keys.just_pressed(KeyCode::KeyJ) || keys.just_pressed(KeyCode::Enter) {
                if !m.ip.is_empty() {
                    next = Some(join(&m.ip));
                } else if let Some(a) = m.found.first() {
                    next = Some(join(&a.to_string()));
                } else {
                    m.msg = "No host found yet - type the host's address".into();
                }
            }
        }
        Screen::HostLobby(h, auto_start) => {
            h.lobby_poll();
            let mut lines = vec![
                line("HOSTING", 60.0, orange),
                line(format!("Players: {} / 4", h.players()), 30.0, white),
                line("[Enter] Start     [Esc] Back", 26.0, light),
                line("Listening on every network below. Give your friend the address on the network you share:", 22.0, gray),
            ];
            for f in &h.ifaces {
                lines.push(line(format!("{}:{}    ({})", f.ip, h.port(), f.name), 30.0, white));
            }
            if h.ifaces.is_empty() {
                lines.push(line("No network connection found", 30.0, red));
            }
            menu_out.lines = lines;
            if keys.just_pressed(KeyCode::Escape) {
                next = Some(menu(""));
            } else if keys.just_pressed(KeyCode::Enter) || (*auto_start > 1 && h.players() >= *auto_start) {
                let players = h.start(MAP_SEED);
                if let Screen::HostLobby(h, _) = std::mem::replace(screen, menu("")) {
                    view.0 = Some(Game::new(World::new(MAP_SEED, players), 0));
                    next = Some(Screen::Play(Box::new(Play::new(Net::Host(h)))));
                }
            }
        }
        Screen::ClientLobby { conn, players } => {
            let mut start = None;
            let mut early = VecDeque::new();
            for m in conn.poll::<H2C>() {
                match m {
                    H2C::Lobby { players: n } => *players = n,
                    H2C::Start { players, you, .. } => start = Some((players, you)),
                    H2C::Turn { cmds } => early.push_back(cmds),
                    H2C::Desync => {}
                }
            }
            menu_out.lines = vec![
                line("JOINED", 60.0, orange),
                line(format!("Players: {players} / 4"), 30.0, white),
                line("Waiting for host to start...     [Esc] Leave", 26.0, light),
            ];
            if let Some((n, you)) = start {
                if let Screen::ClientLobby { conn, .. } = std::mem::replace(screen, menu("")) {
                    let mut p = Play::new(Net::Client(conn));
                    p.queue = early;
                    view.0 = Some(Game::new(World::new(MAP_SEED, n), you));
                    next = Some(Screen::Play(Box::new(p)));
                }
            } else if !conn.open {
                next = Some(menu("Host closed the lobby"));
            } else if keys.just_pressed(KeyCode::Escape) {
                next = Some(menu(""));
            }
        }
        Screen::Play(p) => {
            menu_out.lines.clear();
            let Some(g) = view.0.as_mut() else { return };
            let win = windows.single().ok();
            let to_btn = |b: MouseButton| match b {
                MouseButton::Left => 0,
                MouseButton::Right => 1,
                _ => 2,
            };
            let mut inp = InputSnap {
                mouse: win.and_then(|w| w.cursor_position()).unwrap_or(g.mouse),
                hover: win.is_some_and(|w| w.focused && w.cursor_position().is_some()),
                screen: win.map_or(Vec2::new(1280.0, 800.0), |w| Vec2::new(w.width(), w.height())),
                wheel: scroll.delta.y,
                keys_down: keys.get_pressed().copied().collect(),
                keys_pressed: keys.get_just_pressed().copied().collect(),
                ..default()
            };
            for b in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
                let i = to_btn(b);
                inp.mouse_down[i] = mouse.pressed(b);
                inp.mouse_pressed[i] = mouse.just_pressed(b);
                inp.mouse_released[i] = mouse.just_released(b);
            }
            p.frame(g, &mut status.0, dt, &inp);
            // Knocked out while others play on: Esc leaves.
            let out = g.world.players[g.me as usize].defeated && g.world.winner.is_none();
            leave = out && keys.just_pressed(KeyCode::Escape);
            // Once someone has won, show the result for a few seconds, then back to the menu.
            if g.world.winner.is_some() {
                let t = p.over.get_or_insert(0.0);
                *t += dt;
                g.menu_in = Some(END_DELAY - *t);
                if *t >= END_DELAY {
                    status.0.clear();
                    next = Some(menu(""));
                }
            }
        }
    }
    if leave {
        if let Screen::Play(p) = std::mem::replace(screen, menu("")) {
            let game = view.0.take();
            // A host has to keep relaying turns, so its match carries on in the background.
            if let (Net::Host(_), Some(g)) = (&p.net, game) {
                *background = Some((p, g));
            }
        }
        status.0.clear();
    }
    if let Some(n) = next {
        if !matches!(n, Screen::Play(_)) {
            view.0 = None;
        }
        *screen = n;
    }
}

/// In fullscreen the pointer stays on this window (so edge scrolling works with
/// a second monitor); windowed or unfocused, it is free.
fn confine_cursor(mut windows: Query<(&Window, &mut CursorOptions), With<PrimaryWindow>>) {
    let Ok((w, mut c)) = windows.single_mut() else { return };
    let want = if w.focused && !matches!(w.mode, WindowMode::Windowed) { CursorGrabMode::Confined } else { CursorGrabMode::None };
    if c.grab_mode != want {
        c.grab_mode = want;
    }
}

/// F11 switches between borderless fullscreen and a window.
fn toggle_fullscreen(keys: Res<ButtonInput<KeyCode>>, mut windows: Query<&mut Window, With<PrimaryWindow>>) {
    if !keys.just_pressed(KeyCode::F11) {
        return;
    }
    if let Ok(mut w) = windows.single_mut() {
        w.mode = match w.mode {
            WindowMode::Windowed => WindowMode::BorderlessFullscreen(MonitorSelection::Current),
            _ => WindowMode::Windowed,
        };
    }
}

/// CF_SHOT=path.png: screenshot after CF_SHOT_AT seconds (default 8), then quit. For checking visuals.
/// CF_ZOOM (demo only) sets the starting zoom.
fn auto_shot(
    mut commands: Commands,
    time: Res<Time>,
    mut done: Local<u8>,
    mut exit: MessageWriter<AppExit>,
) {
    let Some(path) = std::env::var_os("CF_SHOT") else { return };
    let at: f32 = std::env::var("CF_SHOT_AT").ok().and_then(|v| v.parse().ok()).unwrap_or(8.0);
    let t = time.elapsed_secs();
    if *done == 0 && t > at {
        *done = 1;
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(std::path::PathBuf::from(path)));
    } else if *done == 1 && t > at + 2.0 {
        *done = 2;
        exit.write(AppExit::Success);
    }
}

fn main() {
    if let Some(path) = std::env::var_os("CF_CURSORS") {
        let (w, h, px) = cursor::sheet();
        cursor::write_png(std::path::Path::new(&path), w, h, &px).expect("write cursor sheet");
        return;
    }
    let args: Vec<String> = std::env::args().collect();
    // Fullscreen by default; `--windowed` (or a screenshot run) opens a 1280x800 window.
    let windowed = args.iter().any(|a| a == "--windowed") || std::env::var_os("CF_SHOT").is_some();
    let args: Vec<String> = args.into_iter().filter(|a| a != "--windowed").collect();
    let mut view = View::default();
    let screen = match args.get(1).map(|s| s.as_str()) {
        Some("solo") => solo(&mut view),
        Some("demo") => demo(&mut view),
        Some("host") => host(net::TCP_PORT, args.get(2).and_then(|n| n.parse().ok()).unwrap_or(0)),
        Some("join") if args.len() > 2 => join(&args[2]),
        _ => menu(""),
    };
    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Cinderfront".into(),
                resolution: WindowResolution::new(1280, 800),
                mode: if windowed { WindowMode::Windowed } else { WindowMode::BorderlessFullscreen(MonitorSelection::Primary) },
                ..default()
            }),
            ..default()
        }));
    // Models, icons and the font are compiled in (see build.rs) and served as embedded://.
    {
        let reg = app.world().resource::<bevy::asset::io::embedded::EmbeddedAssetRegistry>();
        for (path, bytes) in embedded::ASSETS {
            reg.insert_asset(std::path::PathBuf::from(path), std::path::Path::new(path), *bytes);
        }
    }
    app.insert_resource(ClearColor(Color::srgb(0.03, 0.03, 0.04)))
        .insert_resource(view)
        .insert_resource(Session { screen: SyncCell::new(screen), background: SyncCell::new(None) })
        .init_resource::<Status>()
        .add_plugins((scene::ScenePlugin, hud::HudPlugin, cursor::CursorPlugin))
        .add_systems(Update, run_session.in_set(RunGame))
        .add_systems(Update, (auto_shot, toggle_fullscreen, confine_cursor))
        .run();
}
