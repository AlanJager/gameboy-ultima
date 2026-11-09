//! 局域网上的联机房间，最多四台。
//!
//! 主机听 `0.0.0.0:5576`，并每半秒往局域网广播自己。客人搜到名字就能加入，也可以手填地址。
//! 联机线一次交换四台的 16 位。同一条连接也转发无线适配器的广播和数据包。
//! 卡带内容不一致就拒绝。

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::bus::Bus;

pub const LINK_PORT: u16 = 5576;
const DISCOVERY_PORT: u16 = 5577;
const DISCOVERY_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 1);
const MAGIC: &[u8] = b"ULTIMA1";
const READY: u8 = 1;
const RESULT: u8 = 2;
const HELLO: u8 = 3;
const WELCOME: u8 = 4;
const REJECT: u8 = 5;
const ROSTER: u8 = 6;
const WHO: u8 = 7;
const ANNOUNCE: u8 = 8;
const CONNECT: u8 = 9;
const ACCEPT: u8 = 10;
const DATA: u8 = 11;
const NAME: u8 = 12;
const SEATS: u8 = 13;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkSeat {
    pub number: u8,
    pub name: String,
    pub mine: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkView {
    pub line: String,
    pub hosts: Vec<(String, String)>,
    pub error: Option<String>,
    pub lan_ip: String,
    pub self_name: String,
    pub in_room: bool,
    pub seats: Vec<LinkSeat>,
}

impl Default for LinkView {
    fn default() -> Self {
        Self {
            line: String::new(),
            hosts: Vec::new(),
            error: None,
            lan_ip: String::new(),
            self_name: String::new(),
            in_room: false,
            seats: Vec::new(),
        }
    }
}

struct FoundHost {
    name: String,
    addr: SocketAddr,
    seen: Instant,
}

enum Role {
    Idle,
    Host(Peer),
    Guest(Peer),
}

struct Seat {
    stream: TcpStream,
    inbox: Vec<u8>,
    outbox: Vec<u8>,
    ready: VecDeque<u16>,
    last_word: Option<u16>,
    results: VecDeque<[u16; 4]>,
    wire: VecDeque<WireFrame>,
    welcomed: bool,
    closed: bool,
    reject: bool,
    player_id: u8,
    sent_who: bool,
    name: String,
    name_dirty: bool,
}

struct Peer {
    listener: Option<TcpListener>,
    seats: Vec<Seat>,
    port: u16,
    rom_id: String,
    assigned: u8,
    roster: u8,
    names_dirty: bool,
    room: [Option<String>; 4],
}

enum WireFrame {
    Who(u32),
    Announce { id: u32, words: [u32; 6] },
    Connect { from: u32, target: u32 },
    Accept { from: u32, target: u32 },
    Data { from: u32, words: Vec<u32> },
    Roster(u8),
}

pub struct LinkHub {
    role: Role,
    discovery: Option<UdpSocket>,
    beacon_out: Option<UdpSocket>,
    instance: u64,
    last_beacon: Option<Instant>,
    found: Vec<FoundHost>,
    name: String,
    lan_ip: String,
    line: String,
    error: Option<String>,
    player: Option<u8>,
    peers: u8,
    remotes: Vec<(u32, [u32; 6])>,
}

impl LinkHub {
    pub fn new() -> Self {
        let discovery = bind_discovery();
        let beacon_out = beacon_sender();
        Self {
            role: Role::Idle,
            discovery,
            beacon_out,
            instance: next_instance(),
            last_beacon: None,
            found: Vec::new(),
            name: machine_name(),
            lan_ip: lan_ip(),
            line: String::new(),
            error: None,
            player: None,
            peers: 1,
            remotes: Vec::new(),
        }
    }

    pub fn host(&mut self, rom_id: &str) -> Result<(), String> {
        self.host_on(LINK_PORT, rom_id).map(|_| ())
    }

    pub fn host_on(&mut self, port: u16, rom_id: &str) -> Result<u16, String> {
        self.stop();
        let listener = match TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)) {
            Ok(listener) => listener,
            Err(error) => return Err(self.fail(format!("打开联机端口 {port} 失败：{error}"))),
        };
        if let Err(error) = listener.set_nonblocking(true) {
            return Err(self.fail(format!("联机端口无法改成非阻塞：{error}")));
        }
        let port = match listener.local_addr() {
            Ok(addr) => addr.port(),
            Err(error) => return Err(self.fail(format!("读取联机端口失败：{error}"))),
        };
        self.role = Role::Host(Peer::new(Some(listener), port, rom_id));
        self.player = None;
        self.error = None;
        self.line = format!("等待加入 · 1/4 · {}:{port}", self.lan_ip);
        self.beacon();
        Ok(port)
    }

    pub fn join(&mut self, address: &str, rom_id: &str) -> Result<(), String> {
        let addr = match parse_address(address) {
            Ok(addr) => addr,
            Err(error) => return Err(self.fail(error)),
        };
        self.stop();
        let stream = match TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
            Ok(stream) => stream,
            Err(error) => return Err(self.fail(format!("连接 {addr} 失败：{error}"))),
        };
        if let Err(error) = prepare_stream(&stream) {
            return Err(self.fail(error));
        }
        let mut peer = Peer::new(None, addr.port(), rom_id);
        peer.attach(stream);
        peer.send_hello();
        peer.send_name(&self.name);
        self.role = Role::Guest(peer);
        self.player = None;
        self.error = None;
        self.line = format!("正在连接 {addr}");
        Ok(())
    }

    pub fn stop(&mut self) {
        self.remove_announcement();
        self.role = Role::Idle;
        self.player = None;
        self.peers = 1;
        self.remotes.clear();
        self.line.clear();
        self.error = None;
    }

    pub fn set_name(&mut self, name: &str) {
        let name = normalize_link_name(name);
        let name = if name.is_empty() {
            machine_name()
        } else {
            name
        };
        if name == self.name {
            return;
        }
        self.name = name;
        let hosting = matches!(self.role, Role::Host(_));
        let port = self.port();
        let current = self.name.clone();
        match &mut self.role {
            Role::Host(peer) => peer.broadcast_seats(&current),
            Role::Guest(peer) => peer.send_name(&current),
            Role::Idle => {}
        }
        if hosting {
            if let Some(port) = port {
                self.publish_announcement(port);
            }
        }
    }

    pub fn note(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.line = message.clone();
        self.error = Some(message);
    }

    fn fail(&mut self, message: String) -> String {
        self.line = message.clone();
        self.error = Some(message.clone());
        message
    }

    pub fn port(&self) -> Option<u16> {
        match &self.role {
            Role::Host(peer) | Role::Guest(peer) => Some(peer.port),
            Role::Idle => None,
        }
    }

    pub fn connected(&self) -> bool {
        self.player.is_some()
    }

    pub fn view(&self) -> LinkView {
        let mut hosts: Vec<(String, String)> = self
            .found
            .iter()
            .map(|host| (host.name.clone(), host.addr.to_string()))
            .collect();
        hosts.sort();
        hosts.dedup();
        let routable_ports: Vec<u16> = hosts
            .iter()
            .filter_map(|(_, addr)| addr.parse::<SocketAddr>().ok())
            .filter(|addr| !addr.ip().is_loopback())
            .map(|addr| addr.port())
            .collect();
        hosts.retain(|(_, addr)| match addr.parse::<SocketAddr>() {
            Ok(addr) => !(addr.ip().is_loopback() && routable_ports.contains(&addr.port())),
            Err(_) => true,
        });
        let (in_room, seats) = self.present_seats();
        LinkView {
            line: self.line.clone(),
            hosts,
            error: self.error.clone(),
            lan_ip: self.lan_ip.clone(),
            self_name: self.name.clone(),
            in_room,
            seats,
        }
    }

    fn present_seats(&self) -> (bool, Vec<LinkSeat>) {
        let (names, mine) = match &self.role {
            Role::Idle => return (false, Vec::new()),
            Role::Host(peer) => (peer.occupied_names(&self.name), Some(0)),
            Role::Guest(peer) => {
                let mut names = peer.room.clone();
                if names.iter().all(|name| name.is_none()) {
                    if let Some(id) = self.player {
                        if (id as usize) < names.len() {
                            names[id as usize] = Some(self.name.clone());
                        }
                    }
                }
                (names, self.player.map(|id| id as usize))
            }
        };
        let seats = names
            .into_iter()
            .enumerate()
            .map(|(index, name)| LinkSeat {
                number: index as u8 + 1,
                name: name.unwrap_or_default(),
                mine: mine == Some(index),
            })
            .collect();
        (true, seats)
    }

    pub fn poll(&mut self) {
        self.sweep_hosts();
        self.beacon();
        self.pump();
    }

    pub fn service(&mut self, bus: &mut Bus) {
        self.poll();
        let peers = if self.player.is_some() { self.peers } else { 1 };
        bus.set_link_roster(self.player, peers);
        match &mut self.role {
            Role::Idle => {}
            Role::Host(peer) => {
                if let Some(word) = bus.poll_link_need() {
                    let got = peer.collect(Duration::from_millis(400));
                    let mut words = [0xFFFF; 4];
                    words[0] = word;
                    let mut heard = false;
                    for (id, value) in got {
                        if (1..4).contains(&id) {
                            words[id as usize] = value;
                            heard = true;
                        }
                    }
                    if heard || peer.welcomed_count() == 0 {
                        peer.send_result(words);
                        bus.finish_link(words);
                    } else {
                        self.error = Some("对方没有及时送出联机数据".into());
                        bus.fail_link();
                    }
                }
            }
            Role::Guest(peer) => {
                if let Some(word) = bus.poll_link_send() {
                    peer.send_ready(word);
                }
                if let Some(words) = peer.take_result() {
                    bus.accept_remote_link(words);
                }
            }
        }
        self.exchange_wireless(bus);
        bus.release_wireless();
    }

    fn pump(&mut self) {
        let guest_rejected = match &mut self.role {
            Role::Host(peer) => {
                let port = peer.port;
                peer.accept_guest();
                peer.pump_io();
                if peer.take_reject() {
                    self.line = "有一台卡带不一致".into();
                    self.error = Some("两边要打开同一张卡带".into());
                }
                if peer.take_name_updates() {
                    let name = self.name.clone();
                    peer.broadcast_seats(&name);
                }
                let welcomed = peer.welcomed_count();
                if welcomed > 0 {
                    self.player = Some(0);
                    self.peers = (1 + welcomed).min(4);
                    peer.note_roster(self.peers);
                    if self.error.is_none() {
                        self.line = format!("联机 · {}/4 · 1号机", self.peers);
                    }
                } else if self.error.is_none() {
                    self.player = None;
                    self.peers = 1;
                    self.line = format!("等待加入 · 1/4 · {}:{port}", self.lan_ip);
                }
                false
            }
            Role::Guest(peer) => {
                peer.pump_io();
                if peer.take_reject() {
                    self.player = None;
                    self.line = "卡带不一致".into();
                    self.error = Some("两边要打开同一张卡带".into());
                    true
                } else if peer.welcomed_count() > 0 {
                    self.player = Some(peer.assigned);
                    self.peers = peer.roster.max(2).min(4);
                    let number = u16::from(peer.assigned) + 1;
                    self.line = format!("联机 · {}/4 · {number}号机", self.peers);
                    self.error = None;
                    false
                } else {
                    false
                }
            }
            Role::Idle => false,
        };
        if guest_rejected {
            self.role = Role::Idle;
        }
        if self.guest_closed() {
            self.stop();
            self.note("联机已断开");
        }
    }

    fn guest_closed(&self) -> bool {
        match &self.role {
            Role::Guest(peer) => peer.seats.first().is_some_and(|seat| seat.closed),
            Role::Host(_) | Role::Idle => false,
        }
    }

    fn exchange_wireless(&mut self, bus: &mut Bus) {
        let local = bus.wireless_id();
        self.introduce(local);
        for event in bus.take_wireless_out() {
            self.relay_out(local, event);
        }
        for event in self.drain_wire(local) {
            bus.apply_wireless(event);
        }
        let directory: Vec<(u32, [u32; 6])> = self
            .remotes
            .iter()
            .filter(|(id, _)| *id != local)
            .copied()
            .collect();
        bus.apply_wireless(crate::wireless::WirelessIn::Directory(directory));
    }

    fn introduce(&mut self, local: u32) {
        let (Role::Host(peer) | Role::Guest(peer)) = &mut self.role else {
            return;
        };
        for seat in &mut peer.seats {
            if seat.welcomed && !seat.sent_who {
                seat.enqueue(&who_frame(local));
                seat.sent_who = true;
            }
        }
    }

    fn relay_out(&mut self, local: u32, event: crate::wireless::WirelessOut) {
        let frame = match event {
            crate::wireless::WirelessOut::Announce(words) => announce_frame(local, words),
            crate::wireless::WirelessOut::Connect(target) => connect_frame(local, target),
            crate::wireless::WirelessOut::Accept(from) => accept_frame(local, from),
            crate::wireless::WirelessOut::Data(words) => data_frame(local, &words),
        };
        self.send_all(&frame);
    }

    fn send_all(&mut self, frame: &[u8]) {
        let (Role::Host(peer) | Role::Guest(peer)) = &mut self.role else {
            return;
        };
        for seat in &mut peer.seats {
            if seat.welcomed && !seat.closed {
                seat.enqueue(frame);
            }
        }
    }

    fn drain_wire(&mut self, local: u32) -> Vec<crate::wireless::WirelessIn> {
        let frames = match &mut self.role {
            Role::Host(peer) | Role::Guest(peer) => peer.take_wire(),
            Role::Idle => Vec::new(),
        };
        let mut incoming = Vec::new();
        let mut forward = Vec::new();
        for (seat_id, frame) in frames {
            match frame {
                WireFrame::Who(id) => {
                    self.remember_remote(id, [0; 6]);
                    forward.push((seat_id, who_frame(id)));
                }
                WireFrame::Announce { id, words } => {
                    self.remember_remote(id, words);
                    forward.push((seat_id, announce_frame(id, words)));
                }
                WireFrame::Connect { from, target } => {
                    if target == local {
                        incoming.push(crate::wireless::WirelessIn::Connect(from));
                    }
                    forward.push((seat_id, connect_frame(from, target)));
                }
                WireFrame::Accept { from, target } => {
                    if target == local {
                        incoming.push(crate::wireless::WirelessIn::Accept);
                    }
                    forward.push((seat_id, accept_frame(from, target)));
                }
                WireFrame::Data { from, words } => {
                    if from != local {
                        incoming.push(crate::wireless::WirelessIn::Data(words.clone()));
                    }
                    forward.push((seat_id, data_frame(from, &words)));
                }
                WireFrame::Roster(count) => {
                    if let Role::Guest(peer) = &mut self.role {
                        peer.roster = count.max(2).min(4);
                    }
                }
            }
        }
        if let Role::Host(peer) = &mut self.role {
            for (from_seat, frame) in forward {
                for seat in &mut peer.seats {
                    if seat.player_id != from_seat && seat.welcomed && !seat.closed {
                        seat.enqueue(&frame);
                    }
                }
            }
        }
        incoming
    }

    fn remember_remote(&mut self, id: u32, words: [u32; 6]) {
        if id == 0 {
            return;
        }
        if let Some(slot) = self.remotes.iter_mut().find(|(known, _)| *known == id) {
            if words != [0; 6] {
                slot.1 = words;
            }
        } else {
            self.remotes.push((id, words));
        }
    }

    fn beacon(&mut self) {
        let Role::Host(peer) = &self.role else {
            return;
        };
        let due = self
            .last_beacon
            .is_none_or(|at| at.elapsed() >= Duration::from_millis(500));
        if !due {
            return;
        }
        let port = peer.port;
        let name = self.name.clone();
        let targets = broadcast_targets(&self.lan_ip);
        self.publish_announcement(port);
        self.last_beacon = Some(Instant::now());
        let Some(socket) = self.beacon_out.as_ref() else {
            return;
        };
        let packet = beacon_packet(port, &name);
        for dest in targets {
            let _ = socket.send_to(&packet, dest);
        }
    }

    fn sweep_hosts(&mut self) {
        self.sweep_local();
        let packets = {
            let mut packets = Vec::new();
            if let Some(socket) = self.discovery.as_ref() {
                let mut buf = [0u8; 128];
                loop {
                    let Ok((len, from)) = socket.recv_from(&mut buf) else {
                        break;
                    };
                    packets.push((buf[..len].to_vec(), from));
                }
            }
            packets
        };
        for (buf, from) in packets {
            if let Some(found) = parse_beacon(&buf, from) {
                if self.own_beacon(&found) {
                    continue;
                }
                self.remember(found);
            }
        }
        self.found
            .retain(|host| host.seen.elapsed() < Duration::from_secs(3));
    }

    fn remember(&mut self, found: FoundHost) {
        if let Some(slot) = self.found.iter_mut().find(|host| host.addr == found.addr) {
            *slot = found;
        } else {
            self.found.push(found);
        }
    }

    fn own_beacon(&self, found: &FoundHost) -> bool {
        let Role::Host(peer) = &self.role else {
            return false;
        };
        found.addr.port() == peer.port
            && (found.addr.ip().is_loopback() || found.addr.ip().to_string() == self.lan_ip)
    }

    fn publish_announcement(&self, port: u16) {
        let Ok(()) = std::fs::create_dir_all(local_dir()) else {
            return;
        };
        let ip = if self.lan_ip.parse::<Ipv4Addr>().is_ok() {
            self.lan_ip.as_str()
        } else {
            "127.0.0.1"
        };
        let name = self.name.replace(['\n', '\r'], " ");
        let body = format!("{name}\n{ip}\n{port}\n");
        let _ = std::fs::write(self.announcement_path(), body);
    }

    fn remove_announcement(&self) {
        let _ = std::fs::remove_file(self.announcement_path());
    }

    fn announcement_path(&self) -> PathBuf {
        local_dir().join(format!("{}-{}", std::process::id(), self.instance))
    }

    fn sweep_local(&mut self) {
        let Ok(entries) = std::fs::read_dir(local_dir()) else {
            return;
        };
        let own = self.announcement_path();
        for entry in entries.flatten() {
            let path = entry.path();
            if path == own {
                continue;
            }
            let stale = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > Duration::from_secs(3));
            if stale {
                let _ = std::fs::remove_file(&path);
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let mut lines = text.lines();
            let Some(name) = lines.next() else { continue };
            let Some(ip) = lines.next().and_then(|ip| ip.parse::<Ipv4Addr>().ok()) else {
                continue;
            };
            let Some(port) = lines.next().and_then(|port| port.parse::<u16>().ok()) else {
                continue;
            };
            let found = FoundHost {
                name: name.to_string(),
                addr: SocketAddr::V4(SocketAddrV4::new(ip, port)),
                seen: Instant::now(),
            };
            if self.own_beacon(&found) {
                continue;
            }
            self.remember(found);
        }
    }
}

impl Drop for LinkHub {
    fn drop(&mut self) {
        self.remove_announcement();
    }
}

impl Default for LinkHub {
    fn default() -> Self {
        Self::new()
    }
}

impl Peer {
    fn new(listener: Option<TcpListener>, port: u16, rom_id: &str) -> Self {
        Self {
            listener,
            seats: Vec::new(),
            port,
            rom_id: rom_id.to_string(),
            assigned: 1,
            roster: 2,
            names_dirty: false,
            room: [None, None, None, None],
        }
    }

    fn attach(&mut self, stream: TcpStream) {
        self.seats.push(Seat::new(stream, 0));
    }

    fn send_hello(&mut self) {
        let bytes = self.rom_id.as_bytes();
        let len = bytes.len().min(64);
        let mut frame = vec![HELLO, len as u8];
        frame.extend_from_slice(&bytes[..len]);
        if let Some(seat) = self.seats.first_mut() {
            seat.enqueue(&frame);
        }
    }

    fn send_name(&mut self, name: &str) {
        let frame = name_frame(name);
        if let Some(seat) = self.seats.first_mut() {
            seat.enqueue(&frame);
        }
    }

    fn occupied_names(&self, host_name: &str) -> [Option<String>; 4] {
        let mut names = [None, None, None, None];
        names[0] = Some(host_name.to_string());
        for seat in &self.seats {
            if !seat.welcomed || seat.closed {
                continue;
            }
            let index = seat.player_id as usize;
            if (1..4).contains(&index) {
                names[index] = Some(if seat.name.is_empty() {
                    format!("{}号机", index + 1)
                } else {
                    seat.name.clone()
                });
            }
        }
        names
    }

    fn broadcast_seats(&mut self, host_name: &str) {
        if self.welcomed_count() == 0 {
            return;
        }
        let frame = seats_frame(&self.occupied_names(host_name));
        for seat in &mut self.seats {
            if seat.welcomed && !seat.closed {
                seat.enqueue(&frame);
            }
        }
    }

    fn send_ready(&mut self, word: u16) {
        if let Some(seat) = self.seats.first_mut() {
            seat.enqueue(&[READY, word as u8, (word >> 8) as u8]);
        }
    }

    fn send_result(&mut self, words: [u16; 4]) {
        let mut frame = vec![RESULT];
        for word in words {
            frame.push(word as u8);
            frame.push((word >> 8) as u8);
        }
        for seat in &mut self.seats {
            if seat.welcomed && !seat.closed {
                seat.enqueue(&frame);
            }
        }
    }

    fn take_result(&mut self) -> Option<[u16; 4]> {
        self.seats.first_mut()?.results.pop_front()
    }

    fn welcomed_count(&self) -> u8 {
        self.seats
            .iter()
            .filter(|seat| seat.welcomed && !seat.closed)
            .count() as u8
    }

    fn note_roster(&mut self, count: u8) {
        if self.roster == count {
            return;
        }
        self.roster = count;
        let frame = [ROSTER, count];
        for seat in &mut self.seats {
            if seat.welcomed && !seat.closed {
                seat.enqueue(&frame);
            }
        }
    }

    fn accept_guest(&mut self) {
        let Some(listener) = self.listener.as_ref() else {
            return;
        };
        if self.seats.len() >= 3 {
            if let Ok((stream, _)) = listener.accept() {
                drop(stream);
            }
            return;
        }
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        if prepare_stream(&stream).is_err() {
            return;
        }
        let id = self.seats.len() as u8 + 1;
        self.seats.push(Seat::new(stream, id));
    }

    fn take_name_updates(&mut self) -> bool {
        let dirty = self.names_dirty || self.seats.iter().any(|seat| seat.name_dirty);
        self.names_dirty = false;
        for seat in &mut self.seats {
            seat.name_dirty = false;
        }
        dirty
    }

    fn pump_io(&mut self) {
        let rom_id = self.rom_id.clone();
        let mut welcome = Vec::new();
        let mut assigned = None;
        let mut roster = None;
        let mut names_dirty = false;
        let mut room = None;
        for seat in &mut self.seats {
            seat.flush();
            seat.read_bytes();
            while let Some(frame) = seat.next_frame() {
                match frame {
                    SeatFrame::Hello(rom) => {
                        if rom == rom_id {
                            seat.welcomed = true;
                            welcome.push(seat.player_id);
                            names_dirty = true;
                        } else {
                            seat.enqueue(&[REJECT]);
                            seat.reject = true;
                            seat.closed = true;
                        }
                    }
                    SeatFrame::Welcome { id, peers } => {
                        seat.welcomed = true;
                        assigned = Some(id);
                        roster = Some(peers);
                    }
                    SeatFrame::Reject => {
                        seat.reject = true;
                        seat.closed = true;
                    }
                    SeatFrame::Ready(word) => {
                        seat.ready.push_back(word);
                        seat.last_word = Some(word);
                    }
                    SeatFrame::Result(words) => seat.results.push_back(words),
                    SeatFrame::Name(name) => {
                        if seat.name != name {
                            seat.name = name;
                            names_dirty = true;
                        }
                    }
                    SeatFrame::Seats(list) => room = Some(room_from(&list)),
                    SeatFrame::Wire(frame) => {
                        if let WireFrame::Roster(count) = frame {
                            roster = Some(count);
                        }
                        seat.wire.push_back(frame);
                    }
                }
            }
        }
        if let Some(id) = assigned {
            self.assigned = id.clamp(1, 3);
        }
        if let Some(count) = roster {
            self.roster = count.clamp(2, 4);
        }
        if let Some(room) = room {
            self.room = room;
        }
        if names_dirty || self.seats.iter().any(|seat| seat.name_dirty) {
            self.names_dirty = true;
            for seat in &mut self.seats {
                seat.name_dirty = false;
            }
        }
        let peers = (1 + self.seats.iter().filter(|seat| seat.welcomed).count() as u8).clamp(2, 4);
        for id in welcome {
            let Some(seat) = self.seats.iter_mut().find(|seat| seat.player_id == id) else {
                continue;
            };
            seat.enqueue(&[WELCOME, id, peers]);
        }
    }

    fn take_reject(&mut self) -> bool {
        let reject = self.seats.iter().any(|seat| seat.reject);
        let before = self.seats.len();
        // 断开的那一拍可能刚把广播读进队列。先留着，等这一轮送进适配器。
        self.seats
            .retain(|seat| !seat.closed || !seat.wire.is_empty());
        if self.seats.len() != before {
            self.names_dirty = true;
        }
        reject
    }

    fn collect(&mut self, limit: Duration) -> Vec<(u8, u16)> {
        let start = Instant::now();
        let mut pending: Vec<u8> = self
            .seats
            .iter()
            .filter(|seat| seat.welcomed)
            .map(|seat| seat.player_id)
            .collect();
        let mut got = Vec::new();
        while !pending.is_empty() {
            for seat in &mut self.seats {
                if !pending.contains(&seat.player_id) {
                    continue;
                }
                seat.read_bytes();
                while let Some(frame) = seat.next_frame() {
                    match frame {
                        SeatFrame::Ready(word) => {
                            seat.last_word = Some(word);
                            seat.ready.push_back(word);
                        }
                        SeatFrame::Wire(frame) => seat.wire.push_back(frame),
                        SeatFrame::Name(name) => {
                            if seat.name != name {
                                seat.name = name;
                                seat.name_dirty = true;
                            }
                        }
                        SeatFrame::Hello(_)
                        | SeatFrame::Welcome { .. }
                        | SeatFrame::Reject
                        | SeatFrame::Result(_)
                        | SeatFrame::Seats(_) => {}
                    }
                }
                if let Some(word) = seat.ready.pop_front() {
                    got.push((seat.player_id, word));
                    pending.retain(|id| *id != seat.player_id);
                } else if start.elapsed() >= limit {
                    if let Some(word) = seat.last_word {
                        got.push((seat.player_id, word));
                    }
                    pending.retain(|id| *id != seat.player_id);
                }
            }
            if start.elapsed() >= limit {
                break;
            }
            if !pending.is_empty() {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        got
    }

    fn take_wire(&mut self) -> Vec<(u8, WireFrame)> {
        let mut frames = Vec::new();
        for seat in &mut self.seats {
            while let Some(frame) = seat.wire.pop_front() {
                frames.push((seat.player_id, frame));
            }
        }
        frames
    }
}

impl Seat {
    fn new(stream: TcpStream, player_id: u8) -> Self {
        Self {
            stream,
            inbox: Vec::new(),
            outbox: Vec::new(),
            ready: VecDeque::new(),
            last_word: None,
            results: VecDeque::new(),
            wire: VecDeque::new(),
            welcomed: false,
            closed: false,
            reject: false,
            player_id,
            sent_who: false,
            name: String::new(),
            name_dirty: false,
        }
    }

    fn enqueue(&mut self, bytes: &[u8]) {
        self.outbox.extend_from_slice(bytes);
        self.flush();
    }

    fn flush(&mut self) {
        while !self.outbox.is_empty() {
            match self.stream.write(&self.outbox) {
                Ok(0) => {
                    self.closed = true;
                    return;
                }
                Ok(len) => {
                    self.outbox.drain(..len);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(_) => {
                    self.closed = true;
                    return;
                }
            }
        }
    }

    fn read_bytes(&mut self) {
        let mut buf = [0u8; 512];
        loop {
            match self.stream.read(&mut buf) {
                Ok(0) => {
                    self.closed = true;
                    return;
                }
                Ok(len) => self.inbox.extend_from_slice(&buf[..len]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(_) => {
                    self.closed = true;
                    return;
                }
            }
        }
    }

    fn next_frame(&mut self) -> Option<SeatFrame> {
        let kind = *self.inbox.first()?;
        let size = match kind {
            READY if self.inbox.len() >= 3 => 3,
            RESULT if self.inbox.len() >= 9 => 9,
            HELLO if self.inbox.len() >= 2 => {
                let len = self.inbox[1] as usize;
                if self.inbox.len() < 2 + len {
                    return None;
                }
                2 + len
            }
            WELCOME if self.inbox.len() >= 3 => 3,
            REJECT => 1,
            ROSTER if self.inbox.len() >= 2 => 2,
            NAME => match name_len(&self.inbox) {
                FrameLen::NeedMore => return None,
                FrameLen::Skip => 1,
                FrameLen::Ready(size) => size,
            },
            SEATS => match seats_len(&self.inbox) {
                FrameLen::NeedMore => return None,
                FrameLen::Skip => 1,
                FrameLen::Ready(size) => size,
            },
            WHO if self.inbox.len() >= 5 => 5,
            ANNOUNCE if self.inbox.len() >= 29 => 29,
            CONNECT | ACCEPT if self.inbox.len() >= 9 => 9,
            DATA if self.inbox.len() >= 7 => {
                let count = u16::from_le_bytes([self.inbox[5], self.inbox[6]]) as usize;
                if self.inbox.len() < 7 + count * 4 {
                    return None;
                }
                7 + count * 4
            }
            READY | RESULT | HELLO | WELCOME | ROSTER | WHO | ANNOUNCE | CONNECT | ACCEPT | DATA => {
                return None;
            }
            _ => 1,
        };
        let bytes: Vec<u8> = self.inbox.drain(..size).collect();
        if size == 1 && kind != REJECT {
            return self.next_frame();
        }
        Some(decode_frame(&bytes))
    }
}

enum SeatFrame {
    Hello(String),
    Welcome { id: u8, peers: u8 },
    Reject,
    Ready(u16),
    Result([u16; 4]),
    Name(String),
    Seats(Vec<(u8, String)>),
    Wire(WireFrame),
}

fn decode_frame(bytes: &[u8]) -> SeatFrame {
    match bytes.first().copied().unwrap_or(0) {
        READY => SeatFrame::Ready(u16::from_le_bytes([bytes[1], bytes[2]])),
        RESULT => {
            let mut words = [0u16; 4];
            for (index, word) in words.iter_mut().enumerate() {
                let at = 1 + index * 2;
                *word = u16::from_le_bytes([bytes[at], bytes[at + 1]]);
            }
            SeatFrame::Result(words)
        }
        HELLO => {
            let len = bytes[1] as usize;
            SeatFrame::Hello(String::from_utf8_lossy(&bytes[2..2 + len]).into_owned())
        }
        WELCOME => SeatFrame::Welcome {
            id: bytes[1],
            peers: bytes[2],
        },
        ROSTER => SeatFrame::Wire(WireFrame::Roster(bytes[1])),
        NAME => {
            let len = bytes[1] as usize;
            SeatFrame::Name(normalize_link_name(&String::from_utf8_lossy(
                &bytes[2..2 + len],
            )))
        }
        SEATS => SeatFrame::Seats(decode_seats(bytes)),
        WHO => SeatFrame::Wire(WireFrame::Who(u32::from_le_bytes(bytes[1..5].try_into().unwrap()))),
        ANNOUNCE => SeatFrame::Wire(WireFrame::Announce {
            id: read_u32(bytes, 1),
            words: read_words(bytes, 5, 6),
        }),
        CONNECT => SeatFrame::Wire(WireFrame::Connect {
            from: read_u32(bytes, 1),
            target: read_u32(bytes, 5),
        }),
        ACCEPT => SeatFrame::Wire(WireFrame::Accept {
            from: read_u32(bytes, 1),
            target: read_u32(bytes, 5),
        }),
        DATA => {
            let count = u16::from_le_bytes([bytes[5], bytes[6]]) as usize;
            SeatFrame::Wire(WireFrame::Data {
                from: read_u32(bytes, 1),
                words: (0..count)
                    .map(|index| read_u32(bytes, 7 + index * 4))
                    .collect(),
            })
        }
        _ => SeatFrame::Reject,
    }
}

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn read_words(bytes: &[u8], at: usize, count: usize) -> [u32; 6] {
    let mut words = [0u32; 6];
    for (index, word) in words.iter_mut().enumerate().take(count.min(6)) {
        *word = read_u32(bytes, at + index * 4);
    }
    words
}

enum FrameLen {
    NeedMore,
    Skip,
    Ready(usize),
}

fn name_len(inbox: &[u8]) -> FrameLen {
    if inbox.len() < 2 {
        return FrameLen::NeedMore;
    }
    let len = inbox[1] as usize;
    if len > 32 {
        return FrameLen::Skip;
    }
    if inbox.len() < 2 + len {
        FrameLen::NeedMore
    } else {
        FrameLen::Ready(2 + len)
    }
}

fn seats_len(inbox: &[u8]) -> FrameLen {
    if inbox.len() < 2 {
        return FrameLen::NeedMore;
    }
    let count = inbox[1] as usize;
    if count > 4 {
        return FrameLen::Skip;
    }
    let mut at = 2;
    for _ in 0..count {
        if inbox.len() < at + 2 {
            return FrameLen::NeedMore;
        }
        let len = inbox[at + 1] as usize;
        if len > 32 {
            return FrameLen::Skip;
        }
        if inbox.len() < at + 2 + len {
            return FrameLen::NeedMore;
        }
        at += 2 + len;
    }
    FrameLen::Ready(at)
}

fn decode_seats(bytes: &[u8]) -> Vec<(u8, String)> {
    let count = bytes.get(1).copied().unwrap_or(0) as usize;
    let mut list = Vec::new();
    let mut at = 2;
    for _ in 0..count.min(4) {
        if bytes.len() < at + 2 {
            break;
        }
        let id = bytes[at];
        let len = bytes[at + 1] as usize;
        let end = at + 2 + len;
        if bytes.len() < end {
            break;
        }
        let name = normalize_link_name(&String::from_utf8_lossy(&bytes[at + 2..end]));
        list.push((id, name));
        at = end;
    }
    list
}

fn room_from(list: &[(u8, String)]) -> [Option<String>; 4] {
    let mut room = [None, None, None, None];
    for (id, name) in list {
        let index = *id as usize;
        if index < room.len() && !name.is_empty() {
            room[index] = Some(name.clone());
        }
    }
    room
}

fn name_frame(name: &str) -> Vec<u8> {
    let name = normalize_link_name(name);
    let bytes = name.as_bytes();
    let mut frame = vec![NAME, bytes.len() as u8];
    frame.extend_from_slice(bytes);
    frame
}

fn seats_frame(names: &[Option<String>; 4]) -> Vec<u8> {
    let present: Vec<(u8, String)> = names
        .iter()
        .enumerate()
        .filter_map(|(index, name)| name.clone().map(|name| (index as u8, name)))
        .collect();
    let mut frame = vec![SEATS, present.len() as u8];
    for (id, name) in present {
        let bytes = name.as_bytes();
        let len = bytes.len().min(32);
        frame.push(id);
        frame.push(len as u8);
        frame.extend_from_slice(&bytes[..len]);
    }
    frame
}

pub fn normalize_link_name(raw: &str) -> String {
    let trimmed: String = raw
        .trim()
        .chars()
        .filter(|ch| !ch.is_control())
        .collect();
    let mut end = trimmed.len().min(32);
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    trimmed[..end].trim().to_string()
}

fn who_frame(id: u32) -> Vec<u8> {
    let mut frame = vec![WHO];
    frame.extend(id.to_le_bytes());
    frame
}

fn announce_frame(id: u32, words: [u32; 6]) -> Vec<u8> {
    let mut frame = vec![ANNOUNCE];
    frame.extend(id.to_le_bytes());
    for word in words {
        frame.extend(word.to_le_bytes());
    }
    frame
}

fn connect_frame(from: u32, target: u32) -> Vec<u8> {
    let mut frame = vec![CONNECT];
    frame.extend(from.to_le_bytes());
    frame.extend(target.to_le_bytes());
    frame
}

fn accept_frame(from: u32, target: u32) -> Vec<u8> {
    let mut frame = vec![ACCEPT];
    frame.extend(from.to_le_bytes());
    frame.extend(target.to_le_bytes());
    frame
}

fn data_frame(from: u32, words: &[u32]) -> Vec<u8> {
    let mut frame = vec![DATA];
    frame.extend(from.to_le_bytes());
    let count = words.len().min(255) as u16;
    frame.extend(count.to_le_bytes());
    for word in words.iter().take(count as usize) {
        frame.extend(word.to_le_bytes());
    }
    frame
}
fn prepare_stream(stream: &TcpStream) -> Result<(), String> {
    stream
        .set_nodelay(true)
        .map_err(|error| format!("联机套接字设置失败：{error}"))?;
    stream
        .set_nonblocking(true)
        .map_err(|error| format!("联机套接字设置失败：{error}"))?;
    Ok(())
}

fn parse_address(address: &str) -> Result<SocketAddr, String> {
    let address = address.trim();
    if address.is_empty() {
        return Err("先填写对方的地址".into());
    }
    if address.parse::<SocketAddr>().is_ok() {
        return address.parse::<SocketAddr>().map_err(|error| error.to_string());
    }
    let ip: Ipv4Addr = address
        .parse()
        .map_err(|_| format!("地址无效：{address}"))?;
    Ok(SocketAddr::V4(SocketAddrV4::new(ip, LINK_PORT)))
}

fn broadcast_targets(lan_ip: &str) -> Vec<SocketAddrV4> {
    let mut targets = vec![
        SocketAddrV4::new(DISCOVERY_GROUP, DISCOVERY_PORT),
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, DISCOVERY_PORT),
        SocketAddrV4::new(Ipv4Addr::BROADCAST, DISCOVERY_PORT),
    ];
    if let Ok(ip) = lan_ip.parse::<Ipv4Addr>() {
        let octets = ip.octets();
        if octets[0] != 127 && octets[3] != 255 {
            targets.push(SocketAddrV4::new(
                Ipv4Addr::new(octets[0], octets[1], octets[2], 255),
                DISCOVERY_PORT,
            ));
        }
    }
    targets
}

fn beacon_packet(port: u16, name: &str) -> Vec<u8> {
    let name = name.as_bytes();
    let name = &name[..name.len().min(32)];
    let mut packet = Vec::with_capacity(MAGIC.len() + 3 + name.len());
    packet.extend_from_slice(MAGIC);
    packet.extend_from_slice(&port.to_le_bytes());
    packet.push(name.len() as u8);
    packet.extend_from_slice(name);
    packet
}

fn parse_beacon(buf: &[u8], from: SocketAddr) -> Option<FoundHost> {
    if buf.len() < MAGIC.len() + 3 || &buf[..MAGIC.len()] != MAGIC {
        return None;
    }
    let port = u16::from_le_bytes([buf[MAGIC.len()], buf[MAGIC.len() + 1]]);
    let len = buf[MAGIC.len() + 2] as usize;
    let name_at = MAGIC.len() + 3;
    if buf.len() < name_at + len {
        return None;
    }
    let name = String::from_utf8_lossy(&buf[name_at..name_at + len]).into_owned();
    let ip = match from.ip() {
        std::net::IpAddr::V4(ip) => ip,
        std::net::IpAddr::V6(_) => return None,
    };
    Some(FoundHost {
        name: if name.is_empty() {
            ip.to_string()
        } else {
            name
        },
        addr: SocketAddr::V4(SocketAddrV4::new(ip, port)),
        seen: Instant::now(),
    })
}

fn next_instance() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn local_dir() -> PathBuf {
    std::env::temp_dir().join("ultima-link")
}

fn beacon_sender() -> Option<UdpSocket> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.set_broadcast(true).ok()?;
    socket.set_nonblocking(true).ok()?;
    Some(socket)
}

fn bind_discovery() -> Option<UdpSocket> {
    #[cfg(unix)]
    {
        bind_discovery_reuse()
    }
    #[cfg(not(unix))]
    {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, DISCOVERY_PORT)).ok()?;
        let _ = socket.join_multicast_v4(&DISCOVERY_GROUP, &Ipv4Addr::UNSPECIFIED);
        socket.set_broadcast(true).ok()?;
        socket.set_nonblocking(true).ok()?;
        Some(socket)
    }
}

#[cfg(unix)]
fn bind_discovery_reuse() -> Option<UdpSocket> {
    use std::os::fd::FromRawFd;
    unsafe {
        let fd = socket(2, 2, 17);
        if fd < 0 {
            return None;
        }
        let yes = 1i32;
        let _ = setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &yes, 4);
        let _ = setsockopt(fd, SOL_SOCKET, SO_REUSEPORT, &yes, 4);
        let addr = discovery_addr();
        if bind(fd, &addr, std::mem::size_of::<SockAddrIn>() as u32) != 0 {
            let _ = close(fd);
            return None;
        }
        let socket = UdpSocket::from_raw_fd(fd);
        let _ = socket.join_multicast_v4(&DISCOVERY_GROUP, &Ipv4Addr::UNSPECIFIED);
        socket.set_broadcast(true).ok()?;
        socket.set_nonblocking(true).ok()?;
        Some(socket)
    }
}

#[cfg(unix)]
extern "C" {
    fn socket(domain: i32, ty: i32, protocol: i32) -> i32;
    fn bind(fd: i32, addr: *const SockAddrIn, len: u32) -> i32;
    fn setsockopt(fd: i32, level: i32, name: i32, value: *const i32, len: u32) -> i32;
    fn close(fd: i32) -> i32;
}

#[cfg(all(unix, target_os = "linux"))]
const SOL_SOCKET: i32 = 1;
#[cfg(all(unix, target_os = "linux"))]
const SO_REUSEADDR: i32 = 2;
#[cfg(all(unix, target_os = "linux"))]
const SO_REUSEPORT: i32 = 15;

#[cfg(all(unix, not(target_os = "linux")))]
const SOL_SOCKET: i32 = 0xffff;
#[cfg(all(unix, not(target_os = "linux")))]
const SO_REUSEADDR: i32 = 0x0004;
#[cfg(all(unix, not(target_os = "linux")))]
const SO_REUSEPORT: i32 = 0x0200;

#[cfg(all(unix, target_os = "linux"))]
#[repr(C)]
struct SockAddrIn {
    sin_family: u16,
    sin_port: u16,
    sin_addr: u32,
    sin_zero: [u8; 8],
}

#[cfg(all(unix, not(target_os = "linux")))]
#[repr(C)]
struct SockAddrIn {
    sin_len: u8,
    sin_family: u8,
    sin_port: u16,
    sin_addr: u32,
    sin_zero: [u8; 8],
}

#[cfg(unix)]
fn discovery_addr() -> SockAddrIn {
    #[cfg(target_os = "linux")]
    {
        SockAddrIn {
            sin_family: 2,
            sin_port: DISCOVERY_PORT.to_be(),
            sin_addr: 0,
            sin_zero: [0; 8],
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        SockAddrIn {
            sin_len: std::mem::size_of::<SockAddrIn>() as u8,
            sin_family: 2,
            sin_port: DISCOVERY_PORT.to_be(),
            sin_addr: 0,
            sin_zero: [0; 8],
        }
    }
}

fn machine_name() -> String {
    std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "ULTIMA".into())
}

fn lan_ip() -> String {
    let Ok(socket) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else {
        return "本机".into();
    };
    if socket.connect("8.8.8.8:80").is_err() {
        return "本机".into();
    }
    socket
        .local_addr()
        .map(|addr| addr.ip().to_string())
        .unwrap_or_else(|_| "本机".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_beacon_roundtrips_the_name_and_port() {
        let packet = beacon_packet(5576, "客厅");
        let found = parse_beacon(
            &packet,
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 20), 5577)),
        )
        .expect("beacon");
        assert_eq!(found.name, "客厅");
        assert_eq!(found.addr.port(), 5576);
        assert_eq!(found.addr.ip().to_string(), "192.168.1.20");
    }

    #[test]
    fn localhost_exchanges_one_multiplayer_word() {
        let mut host = LinkHub::new();
        let port = host.host_on(0, "same-rom").expect("host");
        let guest = std::thread::spawn(move || {
            let mut guest = LinkHub::new();
            guest
                .join(&format!("127.0.0.1:{port}"), "same-rom")
                .expect("join");
            let mut bus = Bus::new();
            for _ in 0..50 {
                guest.service(&mut bus);
                if guest.connected() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(guest.connected(), "guest did not finish hello");
            bus.write_u16(0x0400_012A, 0x2222);
            bus.write_u16(0x0400_0128, (1 << 13) | (1 << 14) | 3);
            for _ in 0..80 {
                guest.service(&mut bus);
                bus.advance(1_000);
                if bus.read_u16(0x0400_0120) == 0x1111 && bus.read_u16(0x0400_0128) & 0x80 == 0 {
                    return bus.read_u16(0x0400_0122);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("guest never saw the transfer");
        });
        let mut bus = Bus::new();
        for _ in 0..50 {
            host.service(&mut bus);
            if host.connected() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(host.connected(), "host did not see the guest");
        bus.write_u16(0x0400_012A, 0x1111);
        bus.write_u16(0x0400_0128, (1 << 13) | (1 << 14) | 3 | (1 << 7));
        for _ in 0..80 {
            bus.advance(1_000);
            host.service(&mut bus);
            if bus.read_u16(0x0400_0128) & 0x80 == 0 && bus.read_u16(0x0400_0122) == 0x2222 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(bus.read_u16(0x0400_0120), 0x1111);
        assert_eq!(bus.read_u16(0x0400_0122), 0x2222);
        assert_eq!(guest.join().expect("guest thread"), 0x2222);
    }

    #[test]
    fn a_second_machine_sees_the_host_beacon() {
        let mut host = LinkHub::new();
        let port = host.host_on(0, "same-rom").expect("host");
        let mut guest = LinkHub::new();
        let mut seen = None;
        for _ in 0..30 {
            host.poll();
            guest.poll();
            seen = guest
                .view()
                .hosts
                .into_iter()
                .find(|(_, addr)| addr.ends_with(&format!(":{port}")));
            if seen.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let (name, addr) = seen.expect("guest did not see the host");
        assert!(!name.is_empty());
        assert!(addr.ends_with(&format!(":{port}")));
    }

    #[test]
    fn a_different_rom_is_refused() {
        let mut host = LinkHub::new();
        let port = host.host_on(0, "rom-a").expect("host");
        let guest = std::thread::spawn(move || {
            let mut guest = LinkHub::new();
            guest
                .join(&format!("127.0.0.1:{port}"), "rom-b")
                .expect("join");
            let mut bus = Bus::new();
            for _ in 0..40 {
                guest.service(&mut bus);
                if let Some(error) = guest.view().error {
                    return error;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("guest was not told the carts differ");
        });
        let mut bus = Bus::new();
        for _ in 0..40 {
            host.service(&mut bus);
            if host.view().line.contains("卡带") {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let error = guest.join().expect("guest thread");
        assert!(error.contains("同一张卡带"), "{error}");
        assert!(host.view().line.contains("卡带"), "{}", host.view().line);
    }

    #[test]
    fn three_guests_fill_the_four_player_transfer() {
        let mut host = LinkHub::new();
        let port = host.host_on(0, "same-rom").expect("host");
        let guests: Vec<_> = [0x1111u16, 0x2222, 0x3333]
            .into_iter()
            .map(|word| {
                std::thread::spawn(move || {
                    let mut guest = LinkHub::new();
                    guest
                        .join(&format!("127.0.0.1:{port}"), "same-rom")
                        .expect("join");
                    let mut bus = Bus::new();
                    for _ in 0..80 {
                        guest.service(&mut bus);
                        if guest.connected() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    assert!(guest.connected(), "guest {word:04x} did not join");
                    bus.write_u16(0x0400_012A, word);
                    bus.write_u16(0x0400_0128, (1 << 13) | 3);
                    for _ in 0..80 {
                        guest.service(&mut bus);
                        bus.advance(1_000);
                        if bus.read_u16(0x0400_0120) == 0x0001 {
                            return [
                                bus.read_u16(0x0400_0120),
                                bus.read_u16(0x0400_0122),
                                bus.read_u16(0x0400_0124),
                                bus.read_u16(0x0400_0126),
                            ];
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    panic!("guest {word:04x} never finished");
                })
            })
            .collect();
        let mut bus = Bus::new();
        for _ in 0..80 {
            host.service(&mut bus);
            if host.view().line.contains("4/4") {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(host.view().line.contains("4/4"), "{}", host.view().line);
        bus.write_u16(0x0400_012A, 0x0001);
        bus.write_u16(0x0400_0128, (1 << 13) | 3 | (1 << 7));
        for _ in 0..80 {
            bus.advance(1_000);
            host.service(&mut bus);
            if bus.read_u16(0x0400_0128) & 0x80 == 0 && bus.read_u16(0x0400_0126) == 0x3333 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut words = [
            bus.read_u16(0x0400_0120),
            bus.read_u16(0x0400_0122),
            bus.read_u16(0x0400_0124),
            bus.read_u16(0x0400_0126),
        ];
        words.sort();
        assert_eq!(words, [0x0001, 0x1111, 0x2222, 0x3333]);
        for guest in guests {
            let seen = guest.join().expect("guest");
            assert_eq!(seen[0], 0x0001);
            assert!(seen.contains(&0x3333));
        }
    }

    #[test]
    fn a_wireless_broadcast_reaches_the_host() {
        fn transfer(bus: &mut Bus, word: u32) -> u32 {
            bus.write_u32(0x0400_0120, word);
            bus.write_u16(0x0400_0128, 0x5083);
            bus.advance(3_000);
            bus.read_u32(0x0400_0120)
        }

        let mut host = LinkHub::new();
        let port = host.host_on(0, "same-rom").expect("host");
        let guest = std::thread::spawn(move || {
            let mut guest = LinkHub::new();
            guest
                .join(&format!("127.0.0.1:{port}"), "same-rom")
                .expect("join");
            let mut bus = Bus::new();
            for _ in 0..40 {
                guest.service(&mut bus);
                if guest.connected() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(guest.connected());
            assert_eq!(transfer(&mut bus, 0x9966_0616), 0x8000_0000);
            for word in 1..=6 {
                assert_eq!(transfer(&mut bus, word), 0x8000_0000);
            }
            for _ in 0..40 {
                guest.service(&mut bus);
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let mut bus = Bus::new();
        for _ in 0..40 {
            host.service(&mut bus);
            if host.connected() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(host.connected(), "host did not see the guest");
        guest.join().expect("guest");
        host.service(&mut bus);
        assert_eq!(transfer(&mut bus, 0x9966_0017), 0x8000_0000);
        let header = transfer(&mut bus, 0x8000_0000);
        assert_eq!(header, 0x9966_0797, "directory header, remotes {:?}", host.remotes);
        let id = transfer(&mut bus, 0x8000_0000);
        assert_ne!(id, 0);
        assert_eq!(transfer(&mut bus, 0x8000_0000), 1);
    }

    #[test]
    fn names_show_up_for_both_sides_of_the_room() {
        let mut host = LinkHub::new();
        host.set_name("客厅");
        let port = host.host_on(0, "same-rom").expect("host");
        let guest = std::thread::spawn(move || {
            let mut guest = LinkHub::new();
            guest.set_name("卧室");
            guest
                .join(&format!("127.0.0.1:{port}"), "same-rom")
                .expect("join");
            let mut bus = Bus::new();
            for _ in 0..50 {
                guest.service(&mut bus);
                let seats = guest.view().seats;
                if seats.iter().any(|seat| seat.name == "客厅")
                    && seats.iter().any(|seat| seat.name == "卧室")
                {
                    return seats;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("guest never saw both names: {:?}", guest.view().seats);
        });
        let mut bus = Bus::new();
        let host_seats = (0..50)
            .find_map(|_| {
                host.service(&mut bus);
                let seats = host.view().seats;
                let found = seats.iter().any(|seat| seat.name == "卧室");
                std::thread::sleep(Duration::from_millis(10));
                found.then_some(seats)
            })
            .expect("host never saw the guest name");
        assert_eq!(host_seats[0].name, "客厅");
        assert!(host_seats[0].mine);
        assert_eq!(host_seats[1].name, "卧室");
        assert!(!host_seats[1].mine);
        assert!(host_seats[2].name.is_empty());
        assert!(host_seats[3].name.is_empty());
        let guest_seats = guest.join().expect("guest");
        assert_eq!(guest_seats[0].name, "客厅");
        assert!(!guest_seats[0].mine);
        assert_eq!(guest_seats[1].name, "卧室");
        assert!(guest_seats[1].mine);
        assert!(guest_seats[2].name.is_empty() && guest_seats[3].name.is_empty());
    }
}
