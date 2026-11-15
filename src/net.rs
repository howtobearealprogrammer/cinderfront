//! LAN lockstep transport. The host batches everyone's commands into numbered
//! turns and broadcasts them; every peer applies the same turns to its own sim.
//! Messages are newline-delimited JSON over TCP. Hosts announce over UDP broadcast.

use crate::sim::Command;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::io::{self, BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

pub const TCP_PORT: u16 = 47810;
pub const UDP_PORT: u16 = 47811;
const MAGIC: &str = "CINDERFRONT1";

#[derive(Serialize, Deserialize, Debug)]
pub enum C2H {
    Cmd(Command),
    Hash { turn: u32, hash: u64 },
}

#[derive(Serialize, Deserialize, Debug)]
pub enum H2C {
    Lobby { players: u8 },
    Start { seed: u32, players: u8, you: u8 },
    Turn { cmds: Vec<(u8, Command)> },
    Desync,
}

pub struct Conn {
    stream: TcpStream,
    rx: Receiver<Option<String>>,
    pub open: bool,
}

impl Conn {
    pub fn new(stream: TcpStream) -> io::Result<Conn> {
        stream.set_nonblocking(false)?;
        stream.set_nodelay(true)?;
        let read_half = stream.try_clone()?;
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut r = BufReader::new(read_half);
            loop {
                let mut line = String::new();
                match r.read_line(&mut line) {
                    Ok(0) | Err(_) => {
                        let _ = tx.send(None);
                        break;
                    }
                    Ok(_) => {
                        if tx.send(Some(line)).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Ok(Conn { stream, rx, open: true })
    }

    pub fn connect(addr: SocketAddr) -> io::Result<Conn> {
        Conn::new(TcpStream::connect_timeout(&addr, Duration::from_secs(2))?)
    }

    pub fn send<T: Serialize>(&mut self, msg: &T) {
        if !self.open {
            return;
        }
        let mut s = serde_json::to_string(msg).expect("serialize");
        s.push('\n');
        if self.stream.write_all(s.as_bytes()).is_err() {
            self.open = false;
        }
    }

    pub fn poll<T: DeserializeOwned>(&mut self) -> Vec<T> {
        let mut out = Vec::new();
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Some(line) => {
                    if let Ok(v) = serde_json::from_str(&line) {
                        out.push(v);
                    }
                }
                None => self.open = false,
            }
        }
        out
    }
}

pub struct Host {
    listener: TcpListener,
    /// Client i is player i + 1; the host is player 0.
    pub clients: Vec<Conn>,
    /// Every LAN address this machine can be reached on; refreshed each second.
    pub ifaces: Vec<Iface>,
    beacon: Instant,
    port: u16,
}

impl Host {
    pub fn new(port: u16) -> io::Result<Host> {
        let listener = TcpListener::bind(("0.0.0.0", port))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        Ok(Host { listener, clients: Vec::new(), ifaces: interfaces(), beacon: Instant::now() - Duration::from_secs(5), port })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn players(&self) -> u8 {
        self.clients.len() as u8 + 1
    }

    /// Accept joiners and announce on the LAN. Call every frame while in the lobby.
    pub fn lobby_poll(&mut self) {
        let before = self.clients.len();
        self.clients.retain_mut(|c| {
            let _ = c.poll::<C2H>();
            c.open
        });
        while let Ok((stream, _)) = self.listener.accept() {
            if self.clients.len() < 3 {
                if let Ok(c) = Conn::new(stream) {
                    self.clients.push(c);
                }
            }
        }
        if self.clients.len() != before {
            let players = self.players();
            self.broadcast(&H2C::Lobby { players });
        }
        if self.beacon.elapsed() > Duration::from_secs(1) {
            self.beacon = Instant::now();
            // Announce on every interface: a plain 255.255.255.255 broadcast only
            // leaves by the default route, which misses e.g. wifi when ethernet is up.
            self.ifaces = interfaces();
            let msg = format!("{MAGIC} {}", self.port);
            for i in &self.ifaces {
                if let Ok(u) = UdpSocket::bind((i.ip, 0)) {
                    if u.set_broadcast(true).is_ok() {
                        let _ = u.send_to(msg.as_bytes(), (i.broadcast, UDP_PORT));
                        let _ = u.send_to(msg.as_bytes(), (Ipv4Addr::BROADCAST, UDP_PORT));
                    }
                }
            }
        }
    }

    pub fn start(&mut self, seed: u32) -> u8 {
        let players = self.players();
        for (i, c) in self.clients.iter_mut().enumerate() {
            c.send(&H2C::Start { seed, players, you: i as u8 + 1 });
        }
        players
    }

    pub fn broadcast(&mut self, msg: &H2C) {
        for c in &mut self.clients {
            c.send(msg);
        }
    }

    /// Drain client messages: (commands tagged by player, (player, turn, hash) reports).
    pub fn collect(&mut self) -> (Vec<(u8, Command)>, Vec<(u8, u32, u64)>) {
        let (mut cmds, mut hashes) = (Vec::new(), Vec::new());
        for (i, c) in self.clients.iter_mut().enumerate() {
            let p = i as u8 + 1;
            for m in c.poll::<C2H>() {
                match m {
                    C2H::Cmd(cmd) => cmds.push((p, cmd)),
                    C2H::Hash { turn, hash } => hashes.push((p, turn, hash)),
                }
            }
        }
        (cmds, hashes)
    }
}

/// Listens for host beacons on the LAN.
pub struct Finder {
    udp: UdpSocket,
}

impl Finder {
    pub fn new() -> io::Result<Finder> {
        let udp = UdpSocket::bind(("0.0.0.0", UDP_PORT))?;
        udp.set_nonblocking(true)?;
        Ok(Finder { udp })
    }

    pub fn poll(&self) -> Vec<SocketAddr> {
        let mut out = Vec::new();
        let mut buf = [0u8; 64];
        while let Ok((n, from)) = self.udp.recv_from(&mut buf) {
            let msg = String::from_utf8_lossy(&buf[..n]);
            let mut parts = msg.split_whitespace();
            if parts.next() == Some(MAGIC) {
                if let Some(port) = parts.next().and_then(|p| p.parse().ok()) {
                    out.push(SocketAddr::new(from.ip(), port));
                }
            }
        }
        out
    }
}

pub struct Iface {
    pub name: String,
    pub ip: Ipv4Addr,
    pub broadcast: Ipv4Addr,
}

/// IPv4 addresses a friend on the LAN could use to reach us. The host listens
/// on all of them. Container/VM bridges are left out as noise.
pub fn interfaces() -> Vec<Iface> {
    let virt = ["docker", "br-", "veth", "virbr", "vmnet", "vboxnet", "tun", "tap"];
    let mut out = Vec::new();
    for i in if_addrs::get_if_addrs().unwrap_or_default() {
        if i.is_loopback() || virt.iter().any(|v| i.name.starts_with(v)) {
            continue;
        }
        if let if_addrs::IfAddr::V4(a) = &i.addr {
            let broadcast = a.broadcast.unwrap_or(Ipv4Addr::from(u32::from(a.ip) | !u32::from(a.netmask)));
            out.push(Iface { name: i.name.clone(), ip: a.ip, broadcast });
        }
    }
    out
}
