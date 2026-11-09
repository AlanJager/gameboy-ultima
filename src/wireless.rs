//! 插在联机口上的无线适配器。
//!
//! 游戏用 32 位普通模式登录。口令是 `NINTENDO` 再加 `0x8001`，对完才发 `0x9966` 命令。
//! 真正的电波由联机房间转发。没进房间时适配器还在，只是搜不到别人。
//! `0x11` 固定回 `0xFF`，一个房间最多 4 台。

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub(crate) enum WirelessOut {
    Announce([u32; 6]),
    Connect(u32),
    Accept(u32),
    Data(Vec<u32>),
}

#[derive(Clone, Debug)]
pub(crate) enum WirelessIn {
    Directory(Vec<(u32, [u32; 6])>),
    Connect(u32),
    Accept,
    Data(Vec<u32>),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Adapter {
    id: u32,
    #[serde(skip)]
    phase_params: u8,
    #[serde(skip)]
    params_cmd: u8,
    #[serde(skip)]
    params: Vec<u32>,
    #[serde(skip)]
    pending: Vec<u32>,
    #[serde(skip)]
    outgoing: Vec<WirelessOut>,
    #[serde(skip)]
    directory: Vec<(u32, [u32; 6])>,
    #[serde(skip)]
    dir_cursor: usize,
    #[serde(skip)]
    connect_inbox: Vec<u32>,
    #[serde(skip)]
    data_inbox: Vec<Vec<u32>>,
    #[serde(skip)]
    external_ready: Option<u32>,
    si_invert: bool,
    holding: bool,
    connect_ok: bool,
    /// 登录还没开始。第一拍回 0，让游戏自己对齐。
    #[serde(skip)]
    login_started: bool,
    /// 适配器正在出示的那半字，`0..4` 对应 `NINTENDO` 和结束符。
    #[serde(skip)]
    login_index: u8,
    #[serde(skip)]
    login_prev_lo: u16,
    #[serde(skip)]
    login_done: bool,
}

/// `NINTENDO` 五个半字，最后是 `0x8001`。低字节在前。
const LOGIN_KEY: [u16; 5] = [0x494E, 0x544E, 0x4E45, 0x4F44, 0x8001];

impl Adapter {
    pub(crate) fn new() -> Self {
        Self {
            id: fresh_id(),
            phase_params: 0,
            params_cmd: 0,
            params: Vec::new(),
            pending: Vec::new(),
            outgoing: Vec::new(),
            directory: Vec::new(),
            dir_cursor: 0,
            connect_inbox: Vec::new(),
            data_inbox: Vec::new(),
            external_ready: None,
            si_invert: false,
            holding: false,
            connect_ok: false,
            login_started: false,
            login_index: 0,
            login_prev_lo: 0,
            login_done: false,
        }
    }

    /// `RCNT` 把适配器拉进复位时，登录从头再来。已经连上的房间不动。
    pub(crate) fn restart_login(&mut self) {
        self.login_started = false;
        self.login_index = 0;
        self.login_prev_lo = 0;
        self.login_done = false;
    }

    pub(crate) fn id(&self) -> u32 {
        self.id
    }

    pub(crate) fn si_inverted(&self) -> bool {
        self.si_invert
    }

    pub(crate) fn holding(&self) -> bool {
        self.holding && self.external_ready.is_none()
    }

    pub(crate) fn take_external(&mut self) -> Option<u32> {
        if !self.si_invert {
            return None;
        }
        let reply = self.external_ready.take()?;
        self.holding = false;
        Some(reply)
    }

    pub(crate) fn take_outgoing(&mut self) -> Vec<WirelessOut> {
        std::mem::take(&mut self.outgoing)
    }

    pub(crate) fn apply(&mut self, event: WirelessIn) {
        match event {
            WirelessIn::Directory(entries) => {
                self.directory = entries;
                self.dir_cursor = 0;
            }
            WirelessIn::Connect(from) => self.connect_inbox.push(from),
            WirelessIn::Accept => self.connect_ok = true,
            WirelessIn::Data(words) => {
                if !words.is_empty() {
                    self.data_inbox.push(words);
                    self.poke_wait();
                }
            }
        }
    }

    /// 一次 32 位传输。`external` 为真时，时钟由适配器提供。
    pub(crate) fn exchange(&mut self, word: u32, external: bool) -> Option<u32> {
        if external {
            if self.si_invert {
                if let Some(reply) = self.external_ready.take() {
                    self.holding = false;
                    return Some(reply);
                }
                self.holding = true;
                return None;
            }
            return Some(0);
        }
        self.holding = false;
        Some(self.feed(word))
    }

    fn poke_wait(&mut self) {
        if self.holding || self.si_invert {
            self.external_ready = Some(0x9966_0028);
            self.holding = false;
        }
    }

    fn feed(&mut self, word: u32) -> u32 {
        if self.phase_params == 0 && word & 0xFFFF_0000 == 0x9966_0000 {
            self.pending.clear();
            self.si_invert = false;
            let cmd = (word & 0xFF) as u8;
            let count = ((word >> 8) & 0xFF) as u8;
            if cmd == 0xA8 {
                self.holding = false;
                self.external_ready = None;
                return 0x8000_0000;
            }
            self.params.clear();
            self.params_cmd = cmd;
            if count == 0 {
                self.queue(cmd, &[]);
            } else {
                self.phase_params = count;
                self.params.reserve(usize::from(count));
            }
            return 0x8000_0000;
        }
        if self.phase_params > 0 {
            self.params.push(word);
            self.phase_params -= 1;
            if self.phase_params == 0 {
                let cmd = self.params_cmd;
                let params = std::mem::take(&mut self.params);
                self.queue(cmd, &params);
            }
            return 0x8000_0000;
        }
        if let Some(reply) = self.pending_pop() {
            return reply;
        }
        if !self.login_done {
            return self.login_word(word);
        }
        let low = word & 0xFFFF;
        let high = word >> 16;
        (low << 16) | high
    }

    /// 高半字是适配器自己的口令，低半字是上一拍游戏低半字的反码。
    /// 游戏把这两半对上之后，口令往前走一格。收到 `0x8001` 才算登录完成。
    fn login_word(&mut self, word: u32) -> u32 {
        let lo = (word & 0xFFFF) as u16;
        let hi = (word >> 16) as u16;
        let reply = if !self.login_started {
            0
        } else {
            let own = LOGIN_KEY[usize::from(self.login_index.min(4))];
            (u32::from(own) << 16) | u32::from(!self.login_prev_lo)
        };
        if self.login_started {
            let expect = LOGIN_KEY[usize::from(self.login_index.min(4))];
            if lo == expect && hi == !expect && self.login_index < 4 {
                self.login_index += 1;
            } else if self.login_index == 4 && lo == LOGIN_KEY[4] {
                self.login_done = true;
            } else if lo == LOGIN_KEY[0] && self.login_index > 0 {
                self.login_index = 0;
            }
        }
        self.login_prev_lo = lo;
        self.login_started = true;
        reply
    }

    fn pending_pop(&mut self) -> Option<u32> {
        if self.pending.is_empty() {
            return None;
        }
        let reply = self.pending.remove(0);
        if reply == 0x9966_00A5 || reply == 0x9966_00A7 {
            self.si_invert = true;
            if !self.data_inbox.is_empty() {
                self.external_ready = Some(0x9966_0028);
                self.holding = false;
            } else {
                self.holding = true;
                self.external_ready = None;
            }
        }
        Some(reply)
    }

    fn queue(&mut self, cmd: u8, params: &[u32]) {
        self.params_cmd = cmd;
        let replies = self.handle(cmd, params);
        let count = replies.len().min(255) as u32;
        let header = 0x9966_0000 | (count << 8) | u32::from(cmd.wrapping_add(0x80));
        self.pending.push(header);
        self.pending.extend(replies.into_iter().take(255));
    }

    fn handle(&mut self, cmd: u8, params: &[u32]) -> Vec<u32> {
        match cmd {
            0x10 | 0x3D | 0x21 => Vec::new(),
            0x11 => vec![0xFF],
            0x13 => vec![0x0200_1234],
            0x16 => {
                let mut words = [0u32; 6];
                for (slot, value) in words.iter_mut().zip(params.iter().copied()) {
                    *slot = value;
                }
                self.outgoing.push(WirelessOut::Announce(words));
                self.dir_cursor = 0;
                Vec::new()
            }
            0x17 if params.is_empty() => self.next_directory_entry(),
            0x17 => Vec::new(),
            0x1A => match self.connect_inbox.pop() {
                Some(from) => {
                    self.outgoing.push(WirelessOut::Accept(from));
                    vec![from]
                }
                None => Vec::new(),
            },
            0x1D | 0x1E => self.directory_blob(),
            0x1F => {
                self.connect_ok = false;
                if let Some(target) = params.first().copied() {
                    self.outgoing.push(WirelessOut::Connect(target));
                }
                self.dir_cursor = 0;
                Vec::new()
            }
            0x20 => vec![if self.connect_ok { 0 } else { 0x0100_0000 }],
            0x24 => {
                if !params.is_empty() {
                    self.outgoing.push(WirelessOut::Data(params.to_vec()));
                }
                Vec::new()
            }
            0x25 => {
                if !params.is_empty() {
                    self.outgoing.push(WirelessOut::Data(params.to_vec()));
                }
                Vec::new()
            }
            0x26 => self.data_inbox.pop().unwrap_or_default(),
            0x27 => Vec::new(),
            0x30 => {
                self.connect_ok = false;
                self.connect_inbox.clear();
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn next_directory_entry(&mut self) -> Vec<u32> {
        let Some((id, words)) = self.directory.get(self.dir_cursor).copied() else {
            return Vec::new();
        };
        self.dir_cursor += 1;
        let mut reply = Vec::with_capacity(7);
        reply.push(id);
        reply.extend(words);
        reply
    }

    fn directory_blob(&self) -> Vec<u32> {
        let mut reply = Vec::with_capacity(self.directory.len() * 7);
        for (id, words) in &self.directory {
            reply.push(*id);
            reply.extend(words);
        }
        reply
    }
}

impl Default for Adapter {
    fn default() -> Self {
        Self::new()
    }
}

fn fresh_id() -> u32 {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    let mixed = NEXT
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        .wrapping_mul(0x9E37_79B9)
        ^ std::process::id();
    if mixed == 0 {
        1
    } else {
        mixed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pull(adapter: &mut Adapter, word: u32) -> u32 {
        adapter.exchange(word, false).expect("internal clock replies at once")
    }

    #[test]
    fn login_walks_the_nintendo_key_and_then_takes_commands() {
        let mut adapter = Adapter::new();
        let sent = [
            0x7FFF_494E,
            0xFFFF_494E,
            0xB6B1_494E,
            0xB6B1_544E,
            0xABB1_544E,
            0xABB1_4E45,
            0xB1BA_4E45,
            0xB1BA_4F44,
            0xB0BB_4F44,
            0xB0BB_8001,
        ];
        let got = [
            0x0000_0000,
            0x494E_B6B1,
            0x494E_B6B1,
            0x544E_B6B1,
            0x544E_ABB1,
            0x4E45_ABB1,
            0x4E45_B1BA,
            0x4F44_B1BA,
            0x4F44_B0BB,
            0x8001_B0BB,
        ];
        for (word, reply) in sent.into_iter().zip(got) {
            assert_eq!(pull(&mut adapter, word), reply, "{word:08X}");
        }
        assert_eq!(pull(&mut adapter, 0x9966_0011), 0x8000_0000);
        assert_eq!(pull(&mut adapter, 0x8000_0000), 0x9966_0191);
        assert_eq!(pull(&mut adapter, 0x8000_0000), 0xFF);
    }

    #[test]
    fn a_broken_login_starts_the_key_over() {
        let mut adapter = Adapter::new();
        pull(&mut adapter, 0xFFFF_494E);
        pull(&mut adapter, 0xB6B1_494E);
        pull(&mut adapter, 0xB6B1_544E);
        assert_eq!(pull(&mut adapter, 0xFFFF_494E), 0x544E_ABB1);
        assert_eq!(pull(&mut adapter, 0xB6B1_494E), 0x494E_B6B1);
        adapter.restart_login();
        assert_eq!(pull(&mut adapter, 0xFFFF_494E), 0);
    }

    #[test]
    fn hello_then_a_broadcast_shows_up_in_the_directory_command() {
        let mut adapter = Adapter::new();
        assert_eq!(pull(&mut adapter, 0x9966_0010), 0x8000_0000);
        assert_eq!(pull(&mut adapter, 0x8000_0000), 0x9966_0090);
        assert_eq!(pull(&mut adapter, 0x9966_0616), 0x8000_0000);
        for word in [1, 2, 3, 4, 5, 6] {
            assert_eq!(pull(&mut adapter, word), 0x8000_0000);
        }
        assert_eq!(pull(&mut adapter, 0x8000_0000), 0x9966_0096);
        let announced = adapter.take_outgoing();
        assert!(matches!(announced.as_slice(), [WirelessOut::Announce(words)] if words[0] == 1 && words[5] == 6));

        adapter.apply(WirelessIn::Directory(vec![(0x2154, [9, 8, 7, 6, 5, 4])]));
        assert_eq!(pull(&mut adapter, 0x9966_0017), 0x8000_0000);
        assert_eq!(pull(&mut adapter, 0x8000_0000), 0x9966_0797);
        assert_eq!(pull(&mut adapter, 0x8000_0000), 0x2154);
        assert_eq!(pull(&mut adapter, 0x8000_0000), 9);
    }

    #[test]
    fn a_waiting_receive_stays_quiet_until_data_arrives() {
        let mut adapter = Adapter::new();
        assert_eq!(pull(&mut adapter, 0x9966_0027), 0x8000_0000);
        assert_eq!(pull(&mut adapter, 0x8000_0000), 0x9966_00A7);
        assert!(adapter.si_inverted());
        assert!(adapter.exchange(0x8000_0000, true).is_none());
        adapter.apply(WirelessIn::Data(vec![0x1234_5678]));
        assert_eq!(adapter.exchange(0x8000_0000, true), Some(0x9966_0028));
        assert_eq!(pull(&mut adapter, 0x9966_0026), 0x8000_0000);
        assert_eq!(pull(&mut adapter, 0x8000_0000), 0x9966_01A6);
        assert_eq!(pull(&mut adapter, 0x8000_0000), 0x1234_5678);
    }
}
