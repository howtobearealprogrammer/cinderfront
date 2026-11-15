mod game;
mod gfx;
mod terrain;

use cinderfront::net::{self, Conn, Finder, Host, C2H, H2C};
use cinderfront::sim::{Command, StructKind, UnitKind, World, TICKS_PER_SEC};
use game::Game;
use macroquad::prelude::*;
use std::collections::{BTreeMap, VecDeque};
use std::net::SocketAddr;

const MAP_SEED: u32 = 7;
const TICK_DT: f32 = 1.0 / TICKS_PER_SEC as f32;
const TICKS_PER_TURN: u32 = 2;
const TURN_DT: f32 = TICK_DT * TICKS_PER_TURN as f32;
const HASH_EVERY: u32 = 20;

fn window_conf() -> Conf {
    Conf { window_title: "Cinderfront".into(), window_width: 1280, window_height: 800, ..Default::default() }
}

enum Net {
    Solo,
    Host(Host),
    Client(Conn),
}

struct Play {
    game: Game,
    net: Net,
    queue: VecDeque<Vec<(u8, Command)>>,
    pending: Vec<(u8, Command)>,
    tick_acc: f32,
    turn_acc: f32,
    sub: u32,
    turn: u32,
    hashes: BTreeMap<u32, u64>,
    status: String,
    /// Debug aid (CF_AUTOPLAY=1): scripted orders, to exercise netcode without a human.
    auto: Option<u8>,
}

impl Play {
    fn new(players: u8, me: u8, net: Net) -> Play {
        Play {
            game: Game::new(World::new(MAP_SEED, players), me),
            net,
            queue: VecDeque::new(),
            pending: Vec::new(),
            tick_acc: 0.0,
            turn_acc: 0.0,
            sub: 0,
            turn: 0,
            hashes: BTreeMap::new(),
            status: String::new(),
            auto: std::env::var_os("CF_AUTOPLAY").map(|_| 0),
        }
    }

    fn autoplay(&mut self) -> Vec<Command> {
        let (w, me) = (&self.game.world, self.game.me);
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

    fn frame(&mut self) {
        let dt = get_frame_time().min(0.25);
        match &mut self.net {
            Net::Solo => {}
            Net::Host(h) => {
                let (cmds, reports) = h.collect();
                self.pending.extend(cmds);
                for (p, turn, hash) in reports {
                    if self.hashes.get(&turn).is_some_and(|mine| *mine != hash) {
                        self.status = format!("DESYNC with player {} at turn {turn}", p + 1);
                        h.broadcast(&H2C::Desync);
                    }
                }
            }
            Net::Client(c) => {
                for m in c.poll::<H2C>() {
                    match m {
                        H2C::Turn { cmds } => self.queue.push_back(cmds),
                        H2C::Desync => self.status = "DESYNC detected by host".into(),
                        _ => {}
                    }
                }
                if !c.open {
                    self.status = "Host disconnected".into();
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
                            self.game.world.apply(p, &c);
                        }
                    }
                    None => {
                        self.tick_acc = TICK_DT * 0.999;
                        break;
                    }
                }
            }
            self.game.world.tick();
            self.game.after_tick();
            self.tick_acc -= TICK_DT;
            self.sub = (self.sub + 1) % TICKS_PER_TURN;
            if self.sub == 0 {
                self.turn += 1;
                if self.turn % HASH_EVERY == 0 {
                    let hash = self.game.world.hash();
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

        let me = self.game.me;
        let mut cmds = self.game.frame(self.tick_acc / TICK_DT, dt, &self.status);
        cmds.extend(self.autoplay());
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

/// Sandbox with one of everything, for eyeballing the renderer.
fn demo() -> Screen {
    let mut p = Play::new(2, 0, Net::Solo);
    let w = &mut p.game.world;
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
    ];
    for (k, dx, dy) in spots {
        w.create_struct(0, k, cx + dx, cy + dy, k.def().hp);
    }
    for (i, k) in UnitKind::ALL.iter().enumerate() {
        w.new_unit(0, *k, (cx - 5 + 2 * i as i32, cy + 5));
        w.new_unit(1, *k, (cx - 5 + 2 * i as i32, cy + 9));
    }
    w.players[0].credits = 20000;
    Screen::Play(Box::new(p))
}

fn text_c(s: &str, y: f32, size: f32, col: Color) {
    let size = size * 0.72; // the bundled font runs larger than macroquad's default
    gfx::text_sh(s, (screen_width() - gfx::text_w(s, size)) / 2.0, y, size, col);
}

fn backdrop(t: f32) {
    let (w, h) = (screen_width(), screen_height());
    gfx::vgrad(0.0, 0.0, w, h, Color::new(0.10, 0.07, 0.06, 1.0), Color::new(0.02, 0.02, 0.03, 1.0));
    for i in 0..5 {
        let x = w * (0.1 + 0.2 * i as f32) + (t * 0.3 + i as f32).sin() * 60.0;
        let c = Color::new(1.0, 0.45, 0.1, 0.07 + 0.03 * (t * 0.7 + i as f32 * 2.0).sin());
        gfx::glow(vec2(x, h * 0.95), 320.0, 260.0, c);
    }
    gfx::glow(vec2(w / 2.0, 120.0), 420.0, 110.0, Color::new(1.0, 0.5, 0.1, 0.16));
}

#[macroquad::main(window_conf)]
async fn main() {
    gfx::init();
    let args: Vec<String> = std::env::args().collect();
    let mut screen = match args.get(1).map(|s| s.as_str()) {
        Some("solo") => Screen::Play(Box::new(Play::new(2, 0, Net::Solo))),
        Some("demo") => demo(),
        Some("host") => host(net::TCP_PORT, args.get(2).and_then(|n| n.parse().ok()).unwrap_or(0)),
        Some("join") if args.len() > 2 => join(&args[2]),
        _ => menu(""),
    };

    loop {
        clear_background(Color::new(0.03, 0.03, 0.04, 1.0));
        if !matches!(screen, Screen::Play(_)) {
            backdrop(get_time() as f32);
        }
        let mut next: Option<Screen> = None;
        match &mut screen {
            Screen::Menu(m) => {
                if let Some(f) = &m.finder {
                    for a in f.poll() {
                        if !m.found.contains(&a) {
                            m.found.push(a);
                        }
                    }
                }
                while let Some(c) = get_char_pressed() {
                    if (c.is_ascii_digit() || c == '.' || c == ':') && m.ip.len() < 21 {
                        m.ip.push(c);
                    }
                }
                if is_key_pressed(KeyCode::Backspace) {
                    m.ip.pop();
                }
                text_c("CINDERFRONT", 150.0, 80.0, ORANGE);
                text_c("[H] Host LAN game     [J] Join     [S] Solo sandbox", 230.0, 28.0, WHITE);
                text_c(&format!("Join address (type, blank = first found): {}_", m.ip), 280.0, 24.0, LIGHTGRAY);
                let found: Vec<String> = m.found.iter().map(|a| a.to_string()).collect();
                let found = if found.is_empty() { "searching...".to_string() } else { found.join(", ") };
                text_c(&format!("Hosts on LAN: {found}"), 312.0, 24.0, LIGHTGRAY);
                text_c(&m.msg, 350.0, 24.0, RED);
                let help = [
                    "Left click / drag: select      Right click: move, attack, harvest",
                    "Sidebar: left click build or train, right click cancel",
                    "When a structure shows READY, click it again, then left click the map to place (Shift = build another)",
                    "Structures need rock. Lay slabs first or they start at half strength.",
                    "Q + click: attack-move     X: stop     Ctrl+1..9 / 1..9: groups     R: repair     V: sell",
                    "MCV: build at Factory, drive (or airlift) to clear rock, press F to unpack a new Command Yard",
                    "TECH TREE: hold Tab in game. Locked buttons say what they need",
                    "GATHER POINT: left click a Barracks / Factory / Airpad, then right click the map",
                    "AIRLIFT: select ground units, press T, click anywhere - free Carriers fetch and deliver",
                    "         or select a Carrier, right click a unit to lift, right click ground to drop",
                    "WASD / arrows / screen edge / middle-drag: scroll     Wheel: zoom     Space: jump to base",
                ];
                for (i, h) in help.iter().enumerate() {
                    text_c(h, 400.0 + i as f32 * 28.0, 22.0, GRAY);
                }
                if is_key_pressed(KeyCode::H) {
                    next = Some(host(net::TCP_PORT, 0));
                } else if is_key_pressed(KeyCode::S) {
                    next = Some(Screen::Play(Box::new(Play::new(2, 0, Net::Solo))));
                } else if is_key_pressed(KeyCode::J) || is_key_pressed(KeyCode::Enter) {
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
                text_c("HOSTING", 170.0, 60.0, ORANGE);
                text_c(&format!("Players: {} / 4", h.players()), 225.0, 30.0, WHITE);
                text_c("[Enter] Start     [Esc] Back", 265.0, 26.0, LIGHTGRAY);
                text_c("Listening on every network below. Give your friend the address on the network you share:", 330.0, 22.0, GRAY);
                for (i, f) in h.ifaces.iter().enumerate() {
                    text_c(&format!("{}:{}    ({})", f.ip, h.port(), f.name), 370.0 + i as f32 * 34.0, 30.0, WHITE);
                }
                if h.ifaces.is_empty() {
                    text_c("No network connection found", 370.0, 30.0, RED);
                }
                if is_key_pressed(KeyCode::Escape) {
                    next = Some(menu(""));
                } else if is_key_pressed(KeyCode::Enter) || (*auto_start > 1 && h.players() >= *auto_start) {
                    let players = h.start(MAP_SEED);
                    if let Screen::HostLobby(h, _) = std::mem::replace(&mut screen, menu("")) {
                        next = Some(Screen::Play(Box::new(Play::new(players, 0, Net::Host(h)))));
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
                text_c("JOINED", 200.0, 60.0, ORANGE);
                text_c(&format!("Players: {players} / 4"), 290.0, 30.0, WHITE);
                text_c("Waiting for host to start...     [Esc] Leave", 350.0, 26.0, LIGHTGRAY);
                if let Some((n, you)) = start {
                    if let Screen::ClientLobby { conn, .. } = std::mem::replace(&mut screen, menu("")) {
                        let mut p = Play::new(n, you, Net::Client(conn));
                        p.queue = early;
                        next = Some(Screen::Play(Box::new(p)));
                    }
                } else if !conn.open {
                    next = Some(menu("Host closed the lobby"));
                } else if is_key_pressed(KeyCode::Escape) {
                    next = Some(menu(""));
                }
            }
            Screen::Play(p) => {
                p.frame();
                if std::env::var_os("CF_FPS").is_some() {
                    gfx::text_sh(&format!("{} fps", get_fps()), 14.0, 54.0, 14.0, WHITE);
                    eprintln!("frame {:.1} ms", get_frame_time() * 1000.0);
                }
            }
        }
        if let Some(n) = next {
            screen = n;
        }
        next_frame().await;
    }
}
