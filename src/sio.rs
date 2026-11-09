//! 联机口。多人模式一次传 16 位，四格结果落在 `SIOMULTI0`–`3`。
//!
//! 没插线时到点自己结束，空位是 `0xFFFF`，忙位会清掉。插着线时，主机等到对方的字再结束。
//! 客人自己不拉开始位，等主机把四个字送回来。

use serde::{Deserialize, Serialize};

const IF_SERIAL: u16 = 1 << 7;
const CPU_HZ: u64 = 16_777_216;

const CNT_BAUD: u16 = 0x0003;
const CNT_SI: u16 = 1 << 2;
const CNT_SD: u16 = 1 << 3;
const CNT_ID: u16 = 0x0030;
const CNT_ERROR: u16 = 1 << 6;
const CNT_START: u16 = 1 << 7;
const CNT_LENGTH: u16 = 1 << 12;
const CNT_MODE: u16 = 1 << 13;
const CNT_IRQ: u16 = 1 << 14;
const RCNT_GPIO: u16 = 1 << 15;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Sio {
    cnt: u16,
    send: u16,
    data: [u16; 4],
    rcnt: u16,
    /// `Some` 是已经插上的机号，0 是主机。
    player: Option<u8>,
    /// 这根线上有几台。0 表示还没告诉过，插上后按两台算。
    #[serde(default)]
    peers: u8,
    #[serde(default)]
    adapter: crate::wireless::Adapter,
    error: bool,
    busy: bool,
    finish_at: Option<u64>,
    incoming: Option<[u16; 4]>,
    stalled: bool,
    send_generation: u32,
    published_generation: u32,
}

impl Sio {
    pub(crate) fn new() -> Self {
        Self {
            cnt: 0,
            send: 0,
            data: [0xFFFF; 4],
            rcnt: 0,
            player: None,
            peers: 0,
            adapter: crate::wireless::Adapter::new(),
            error: false,
            busy: false,
            finish_at: None,
            incoming: None,
            stalled: false,
            send_generation: 0,
            published_generation: 0,
        }
    }

    pub(crate) fn waiting_for_peer(&self) -> bool {
        self.stalled || self.adapter.holding()
    }

    pub(crate) fn set_roster(&mut self, player: Option<u8>, peers: u8) -> u16 {
        self.peers = peers;
        self.set_player(player)
    }

    pub(crate) fn wireless_id(&self) -> u32 {
        self.adapter.id()
    }

    pub(crate) fn take_wireless_out(&mut self) -> Vec<crate::wireless::WirelessOut> {
        self.adapter.take_outgoing()
    }

    pub(crate) fn apply_wireless(&mut self, event: crate::wireless::WirelessIn) {
        self.adapter.apply(event);
    }

    pub(crate) fn release_wireless(&mut self, now: u64) -> u16 {
        if !(self.busy && self.incoming.is_none() && !self.multi()) {
            return 0;
        }
        let Some(word) = self.adapter.take_external() else {
            return 0;
        };
        self.supply([word as u16, (word >> 16) as u16, 0xFFFF, 0xFFFF], now)
    }

    pub(crate) fn set_player(&mut self, player: Option<u8>) -> u16 {
        let next = player.map(|id| id & 3);
        if self.player == next {
            return 0;
        }
        self.player = next;
        if next.is_none() && (self.stalled || self.busy) {
            self.fail()
        } else {
            0
        }
    }

    pub(crate) fn read_byte(&self, offset: usize) -> u8 {
        let word = match offset {
            0x120 | 0x121 => self.data[0],
            0x122 | 0x123 => self.data[1],
            0x124 | 0x125 => self.data[2],
            0x126 | 0x127 => self.data[3],
            0x128 | 0x129 => self.control(),
            0x12A | 0x12B => self.send,
            0x134 | 0x135 => self.rcnt,
            _ => 0,
        };
        if offset % 2 == 0 {
            word as u8
        } else {
            (word >> 8) as u8
        }
    }

    /// 半字的低字节先记下，高字节写完再决定要不要开始。
    /// 单独写一个字节时 `commit` 为真。
    pub(crate) fn write_byte(
        &mut self,
        offset: usize,
        value: u8,
        now: u64,
        commit: bool,
    ) -> Option<u64> {
        match offset {
            0x120..=0x127 => {
                let slot = (offset - 0x120) / 2;
                let low = offset % 2 == 0;
                let word = &mut self.data[slot];
                if low {
                    *word = (*word & 0xFF00) | u16::from(value);
                } else {
                    *word = (*word & 0x00FF) | (u16::from(value) << 8);
                }
                None
            }
            0x128 => {
                self.cnt = (self.cnt & 0xFF00) | u16::from(value);
                if commit {
                    self.maybe_start(now)
                } else {
                    None
                }
            }
            0x129 => {
                self.cnt = (self.cnt & 0x00FF) | (u16::from(value) << 8);
                self.maybe_start(now)
            }
            0x12A => {
                self.send = (self.send & 0xFF00) | u16::from(value);
                if commit {
                    self.bump_send();
                }
                None
            }
            0x12B => {
                self.send = (self.send & 0x00FF) | (u16::from(value) << 8);
                self.bump_send();
                None
            }
            0x134 => {
                self.rcnt = (self.rcnt & 0xFF00) | u16::from(value);
                None
            }
            0x135 => {
                self.rcnt = (self.rcnt & 0x00FF) | (u16::from(value) << 8);
                None
            }
            _ => None,
        }
    }

    pub(crate) fn on_due(&mut self, now: u64) -> u16 {
        let Some(when) = self.finish_at else {
            return 0;
        };
        if now < when {
            return 0;
        }
        if self.incoming.is_none() {
            self.stalled = true;
            return 0;
        }
        self.complete()
    }

    pub(crate) fn take_send(&mut self) -> Option<u16> {
        if self.send_generation == self.published_generation {
            return None;
        }
        self.published_generation = self.send_generation;
        Some(self.send)
    }

    pub(crate) fn take_need(&mut self) -> Option<u16> {
        if !self.stalled {
            return None;
        }
        self.stalled = false;
        Some(self.send)
    }

    /// 主机拿到四个字。到点了就立刻结束，否则先存着等日程。
    pub(crate) fn supply(&mut self, words: [u16; 4], now: u64) -> u16 {
        self.incoming = Some(words);
        self.stalled = false;
        if self.finish_at.is_some_and(|when| now >= when) {
            self.complete()
        } else {
            0
        }
    }

    /// 客人收到主机的结果。忙位先亮着，到点再写进四个寄存器。
    pub(crate) fn begin_remote(&mut self, words: [u16; 4], now: u64) -> Option<u64> {
        self.arm(now, Some(words))
    }

    pub(crate) fn fail(&mut self) -> u16 {
        let word = self.send;
        self.incoming = Some([word, 0xFFFF, 0xFFFF, 0xFFFF]);
        self.error = true;
        self.stalled = false;
        self.complete()
    }

    fn maybe_start(&mut self, now: u64) -> Option<u64> {
        if self.cnt & CNT_START == 0 || self.busy || self.rcnt & RCNT_GPIO != 0 {
            return None;
        }
        if self.multi() && self.player.is_some_and(|id| id != 0) {
            self.cnt &= !CNT_START;
            return None;
        }
        if !self.multi() && self.cnt & CNT_LENGTH != 0 {
            return self.start_adapter(now);
        }
        let waiting = self.multi() && self.player == Some(0) && self.connected_players() >= 2;
        let words = if waiting {
            None
        } else {
            Some(self.local_words())
        };
        self.arm(now, words)
    }

    fn start_adapter(&mut self, now: u64) -> Option<u64> {
        let word = u32::from(self.data[0]) | (u32::from(self.data[1]) << 16);
        let external = self.cnt & 1 == 0;
        match self.adapter.exchange(word, external) {
            Some(reply) => self.arm(
                now,
                Some([reply as u16, (reply >> 16) as u16, 0xFFFF, 0xFFFF]),
            ),
            None => self.arm(now, None),
        }
    }

    fn arm(&mut self, now: u64, words: Option<[u16; 4]>) -> Option<u64> {
        self.busy = true;
        self.error = false;
        self.cnt |= CNT_START;
        self.data = [0xFFFF; 4];
        self.incoming = words;
        self.stalled = false;
        let when = now.saturating_add(self.transfer_cycles());
        self.finish_at = Some(when);
        Some(when)
    }

    fn complete(&mut self) -> u16 {
        if let Some(words) = self.incoming.take() {
            self.data = words;
        }
        self.busy = false;
        self.finish_at = None;
        self.stalled = false;
        self.cnt &= !CNT_START;
        if self.cnt & CNT_IRQ != 0 {
            IF_SERIAL
        } else {
            0
        }
    }

    fn local_words(&self) -> [u16; 4] {
        let mut words = [0xFFFF; 4];
        words[0] = self.send;
        words
    }

    fn control(&self) -> u16 {
        let mut cnt = self.cnt & !(CNT_SI | CNT_SD | CNT_ID | CNT_ERROR | CNT_START);
        if self.multi() {
            let id = self.player.unwrap_or(0) & 3;
            cnt |= u16::from(id) << 4;
            if id != 0 {
                cnt |= CNT_SI;
            }
            if self.player.is_some() && self.connected_players() >= 2 {
                cnt |= CNT_SD;
            }
        } else if self.cnt & CNT_LENGTH != 0 {
            let so = self.cnt & (1 << 3) != 0;
            let si = if self.adapter.si_inverted() { !so } else { so };
            if si {
                cnt |= CNT_SI;
            }
        }
        if self.error {
            cnt |= CNT_ERROR;
        }
        if self.busy {
            cnt |= CNT_START;
        }
        cnt
    }

    fn multi(&self) -> bool {
        self.cnt & CNT_MODE != 0 && self.cnt & CNT_LENGTH == 0 && self.rcnt & RCNT_GPIO == 0
    }

    fn transfer_cycles(&self) -> u64 {
        if !self.multi() {
            return 2048;
        }
        let baud = match self.cnt & CNT_BAUD {
            0 => 9_600,
            1 => 38_400,
            2 => 57_600,
            _ => 115_200,
        };
        let players = u64::from(self.connected_players());
        let bits = 18 * players + if players < 4 { 18 } else { 0 };
        bits * CPU_HZ / baud
    }

    fn connected_players(&self) -> u8 {
        if self.player.is_none() {
            1
        } else if self.peers == 0 {
            2
        } else {
            self.peers.clamp(1, 4)
        }
    }

    fn bump_send(&mut self) {
        self.send_generation = self.send_generation.wrapping_add(1);
    }
}

impl Default for Sio {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::Bus;

    const SIOCNT: u32 = 0x0400_0128;
    const SEND: u32 = 0x0400_012A;
    const MULTI: u32 = 0x0400_0120;
    const IF_REG: u32 = 0x0400_0202;

    fn start_multi(bus: &mut Bus, irq: bool) {
        let mut cnt = (1 << 13) | (3 << 0) | (1 << 7);
        if irq {
            cnt |= 1 << 14;
        }
        bus.write_u16(SIOCNT, cnt);
    }

    #[test]
    fn an_unplugged_link_finishes_and_raises_the_serial_irq() {
        let mut bus = Bus::new();
        bus.write_u16(SEND, 0x1234);
        start_multi(&mut bus, true);
        assert_ne!(bus.read_u16(SIOCNT) & 0x80, 0);
        assert_eq!(bus.read_u16(MULTI), 0xFFFF);
        bus.advance(8_000);
        assert_eq!(bus.read_u16(MULTI), 0x1234);
        assert_eq!(bus.read_u16(MULTI + 2), 0xFFFF);
        assert_eq!(bus.read_u16(MULTI + 4), 0xFFFF);
        assert_eq!(bus.read_u16(MULTI + 6), 0xFFFF);
        assert_eq!(bus.read_u16(SIOCNT) & 0x80, 0);
        assert_ne!(bus.read_u16(IF_REG) & (1 << 7), 0);
    }

    #[test]
    fn a_child_does_not_start_the_transfer() {
        let mut bus = Bus::new();
        bus.set_link_player(Some(1));
        bus.write_u16(SEND, 0x00AA);
        start_multi(&mut bus, true);
        assert_eq!(bus.read_u16(SIOCNT) & 0x80, 0);
        assert_eq!(bus.read_u16(SIOCNT) & 0x30, 1 << 4);
        assert_ne!(bus.read_u16(SIOCNT) & (1 << 3), 0);
        bus.advance(8_000);
        assert_eq!(bus.read_u16(IF_REG) & (1 << 7), 0);
    }

    #[test]
    fn the_master_waits_for_the_peer_then_both_see_the_same_words() {
        let mut master = Bus::new();
        let mut child = Bus::new();
        master.set_link_player(Some(0));
        child.set_link_player(Some(1));
        master.write_u16(SEND, 0x1111);
        child.write_u16(SEND, 0x2222);
        child.write_u16(SIOCNT, (1 << 13) | (1 << 14) | 3);
        start_multi(&mut master, true);
        master.advance(8_000);
        assert_ne!(master.read_u16(SIOCNT) & 0x80, 0);
        assert_eq!(master.poll_link_need(), Some(0x1111));
        let words = [0x1111, 0x2222, 0xFFFF, 0xFFFF];
        master.finish_link(words);
        child.accept_remote_link(words);
        child.advance(8_000);
        assert_eq!(master.read_u16(MULTI), 0x1111);
        assert_eq!(master.read_u16(MULTI + 2), 0x2222);
        assert_eq!(child.read_u16(MULTI), 0x1111);
        assert_eq!(child.read_u16(MULTI + 2), 0x2222);
        assert_eq!(master.read_u16(SIOCNT) & 0x80, 0);
        assert_eq!(child.read_u16(SIOCNT) & 0x80, 0);
        assert_ne!(master.read_u16(IF_REG) & (1 << 7), 0);
        assert_ne!(child.read_u16(IF_REG) & (1 << 7), 0);
    }

    #[test]
    fn four_players_take_longer_than_two_and_keep_every_slot() {
        let mut master = Bus::new();
        master.set_link_roster(Some(0), 4);
        master.write_u16(SEND, 0x0001);
        start_multi(&mut master, false);
        master.advance(8_000);
        assert_ne!(master.read_u16(SIOCNT) & 0x80, 0, "four players should still be shifting");
        master.advance(4_000);
        assert_eq!(master.poll_link_need(), Some(0x0001));
        let words = [0x0001, 0x1111, 0x2222, 0x3333];
        master.finish_link(words);
        assert_eq!(master.read_u16(MULTI), 0x0001);
        assert_eq!(master.read_u16(MULTI + 2), 0x1111);
        assert_eq!(master.read_u16(MULTI + 4), 0x2222);
        assert_eq!(master.read_u16(MULTI + 6), 0x3333);
    }

    #[test]
    fn the_wireless_adapter_answers_the_login_key() {
        let mut bus = Bus::new();
        bus.write_u32(0x0400_0120, 0xFFFF_494E);
        bus.write_u16(0x0400_0128, 0x5083);
        bus.advance(3_000);
        assert_eq!(bus.read_u32(0x0400_0120), 0x494E_FFFF);
        assert_eq!(bus.read_u16(0x0400_0128) & 0x80, 0);
    }
}
