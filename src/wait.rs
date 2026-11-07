//! 内存访问要占的拍数。
//!
//! 指令原来的拍数按每次访问 1 拍来算。这里换成这块地址真正要等的拍数。
//! `0x04000204` 只影响卡带：`0x08000000`、`0x0A000000`、`0x0C000000` 三档，以及 `0x0E000000` 的 SRAM。
//! 第 14 位打开时，总线空闲会提前把顺序指令读进 8 个半字的缓冲。命中时每个半字 1 拍。
//! 第 11、12 位是 PHI 输出，不进 CPU 的拍数。第 15 位读出来是 0。

use serde::{Deserialize, Serialize};

const PREFETCH: u16 = 1 << 14;
const PREFETCH_LIMIT: u8 = 8;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Waits {
    control: u16,
    /// 下一次紧挨着的地址。对不上就是第一次访问。
    seq: Option<u32>,
    /// 预取缓冲里下一个还没填的半字。`len` 是已经填好、从这里往前数的个数。
    pre_next: Option<u32>,
    pre_len: u8,
}

impl Waits {
    pub(crate) fn new() -> Self {
        Self {
            control: 0,
            seq: None,
            pre_next: None,
            pre_len: 0,
        }
    }

    pub(crate) fn read_byte(&self, offset: usize) -> u8 {
        if offset == 0 {
            self.control as u8
        } else {
            (self.control >> 8) as u8
        }
    }

    pub(crate) fn write_byte(&mut self, offset: usize, value: u8) {
        if offset == 0 {
            self.control = (self.control & 0xFF00) | u16::from(value);
        } else {
            self.control = (self.control & 0x00FF) | ((u16::from(value) << 8) & 0x7F00);
        }
    }

    /// `sequential` 有值时，DMA 用它自己的突发，不看上一次 CPU 的地址。
    pub(crate) fn access(
        &mut self,
        addr: u32,
        width: u32,
        code: bool,
        sequential: Option<bool>,
        drawing: bool,
    ) -> u32 {
        let width = match width {
            4 => 4,
            2 => 2,
            _ => 1,
        };
        let aligned = align(addr, width);
        if code && sequential.is_none() {
            if let Some(cost) = self.take_prefetch(aligned, width, drawing) {
                self.seq = Some(next_addr(aligned, width));
                return cost;
            }
        }
        let first = sequential.unwrap_or(self.seq == Some(aligned));
        let cost = self.raw(aligned, width, first, drawing);
        self.seq = Some(next_addr(aligned, width));
        if code && is_rom(aligned) && self.control & PREFETCH != 0 {
            self.pre_next = self.seq;
            self.pre_len = 0;
        } else {
            self.pre_next = None;
            self.pre_len = 0;
            if !is_rom(aligned) {
                self.idle(cost);
            }
        }
        cost
    }

    /// 不占卡带总线的拍。预取可以在这段时间里填半字。
    pub(crate) fn idle(&mut self, mut clocks: u32) {
        if self.control & PREFETCH == 0 {
            return;
        }
        while self.pre_len < PREFETCH_LIMIT {
            let Some(cursor) = self.pre_next else {
                return;
            };
            if !is_rom(cursor) {
                return;
            }
            let sequential =
                is_rom(cursor.wrapping_sub(2)) && rom_ws(cursor) == rom_ws(cursor.wrapping_sub(2));
            let cost = self.cart_half(rom_ws(cursor).unwrap_or(0), sequential);
            if clocks < cost {
                return;
            }
            clocks -= cost;
            self.pre_len += 1;
            self.pre_next = Some(cursor.wrapping_add(2));
        }
    }

    fn take_prefetch(&mut self, addr: u32, width: u32, drawing: bool) -> Option<u32> {
        if self.control & PREFETCH == 0 || !is_rom(addr) || self.pre_len == 0 {
            return None;
        }
        let end = self.pre_next?;
        let start = end.wrapping_sub(u32::from(self.pre_len) * 2);
        if addr != start {
            return None;
        }
        let need: u8 = if width >= 4 { 2 } else { 1 };
        let take = self.pre_len.min(need);
        self.pre_len -= take;
        let mut cost = u32::from(take);
        if take < need {
            let rest = addr.wrapping_add(u32::from(take) * 2);
            cost += self.raw(rest, 2, true, drawing);
            self.pre_next = Some(rest.wrapping_add(2));
            self.pre_len = 0;
        } else {
            self.idle(cost);
        }
        Some(cost)
    }

    fn raw(&self, aligned: u32, width: u32, sequential: bool, drawing: bool) -> u32 {
        let beats = if width >= 4 { 2 } else { 1 };
        match region(aligned) {
            Kind::Ewram => 3 * beats,
            Kind::Video => {
                let penalty = u32::from(drawing);
                beats * (1 + penalty)
            }
            Kind::Oam => 1 + u32::from(drawing),
            Kind::Sram => self.sram() * width,
            Kind::Rom(ws) => {
                let mut cost = 0;
                let mut seq = sequential;
                for _ in 0..beats {
                    cost += self.cart_half(ws, seq);
                    seq = true;
                }
                cost
            }
            Kind::Fast => 1,
        }
    }

    fn cart_half(&self, ws: u8, sequential: bool) -> u32 {
        let (first, second) = match ws {
            0 => (
                n_cycles((self.control >> 2) & 3),
                s_cycles(self.control & 0x10 != 0, 2),
            ),
            1 => (
                n_cycles((self.control >> 5) & 3),
                s_cycles(self.control & 0x80 != 0, 4),
            ),
            _ => (
                n_cycles((self.control >> 8) & 3),
                s_cycles(self.control & 0x400 != 0, 8),
            ),
        };
        if sequential {
            second
        } else {
            first
        }
    }

    fn sram(&self) -> u32 {
        n_cycles(self.control & 3)
    }
}

fn n_cycles(select: u16) -> u32 {
    [4, 3, 2, 8][(select & 3) as usize]
}

fn s_cycles(fast: bool, slow: u32) -> u32 {
    if fast {
        1
    } else {
        slow
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Fast,
    Ewram,
    Video,
    Oam,
    Sram,
    Rom(u8),
}

fn region(addr: u32) -> Kind {
    match addr >> 24 {
        0x02 => Kind::Ewram,
        0x05 | 0x06 => Kind::Video,
        0x07 => Kind::Oam,
        0x0E => Kind::Sram,
        0x08 | 0x09 => Kind::Rom(0),
        0x0A | 0x0B => Kind::Rom(1),
        0x0C | 0x0D => Kind::Rom(2),
        _ => Kind::Fast,
    }
}

fn is_rom(addr: u32) -> bool {
    matches!(region(addr), Kind::Rom(_))
}

fn rom_ws(addr: u32) -> Option<u8> {
    match region(addr) {
        Kind::Rom(ws) => Some(ws),
        _ => None,
    }
}

fn align(addr: u32, width: u32) -> u32 {
    match width {
        4 => addr & !3,
        2 => addr & !1,
        _ => addr,
    }
}

fn next_addr(aligned: u32, width: u32) -> u32 {
    match region(aligned) {
        Kind::Ewram | Kind::Video | Kind::Rom(_) => {
            let step = if width >= 4 { 4 } else { 2 };
            (aligned & !1).wrapping_add(step)
        }
        Kind::Sram => aligned.wrapping_add(width),
        Kind::Fast | Kind::Oam => aligned.wrapping_add(width),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_memory_keeps_its_width() {
        let mut waits = Waits::new();
        assert_eq!(waits.access(0x0300_0000, 4, true, None, false), 1);
        assert_eq!(waits.access(0x0200_0000, 2, false, None, false), 3);
        assert_eq!(waits.access(0x0200_0002, 4, false, None, false), 6);
        assert_eq!(waits.access(0x0000_0000, 4, true, None, false), 1);
        assert_eq!(waits.access(0x0400_0000, 2, false, None, false), 1);
    }

    #[test]
    fn rom_words_use_the_three_wait_states() {
        let mut waits = Waits::new();
        assert_eq!(waits.access(0x0800_0000, 4, true, None, false), 6);
        assert_eq!(waits.access(0x0800_0004, 4, true, None, false), 4);
        assert_eq!(waits.access(0x0800_0008, 2, true, None, false), 2);

        let mut waits = Waits::new();
        assert_eq!(waits.access(0x0A00_0000, 4, true, None, false), 8);
        assert_eq!(waits.access(0x0A00_0004, 2, true, None, false), 4);
        let mut waits = Waits::new();
        assert_eq!(waits.access(0x0C00_0000, 4, false, None, false), 12);
        assert_eq!(waits.access(0x0C00_0004, 2, false, None, false), 8);
    }

    #[test]
    fn waitcnt_changes_the_first_and_next_access() {
        let mut waits = Waits::new();
        waits.write_byte(0, 0x18);
        assert_eq!(waits.read_byte(0), 0x18);
        assert_eq!(waits.access(0x0800_0000, 4, true, None, false), 3);
        assert_eq!(waits.access(0x0800_0004, 4, true, None, false), 2);
        waits.write_byte(1, 0xFF);
        assert_eq!(waits.read_byte(1), 0x7F);
        assert_eq!(waits.access(0x0E00_0000, 1, false, None, false), 4);
        let mut waits = Waits::new();
        assert_eq!(waits.access(0x0E00_0000, 1, false, None, false), 4);
        assert_eq!(waits.access(0x0E00_0001, 2, false, None, false), 8);
    }

    #[test]
    fn display_adds_one_beat_while_it_is_drawing() {
        let mut waits = Waits::new();
        assert_eq!(waits.access(0x0500_0000, 2, false, None, true), 2);
        assert_eq!(waits.access(0x0600_0000, 4, false, None, true), 4);
        assert_eq!(waits.access(0x0700_0000, 4, false, None, true), 2);
        assert_eq!(waits.access(0x0500_0000, 2, false, None, false), 1);
        assert_eq!(waits.access(0x0600_0002, 4, false, None, false), 2);
        assert_eq!(waits.access(0x0700_0000, 2, false, None, false), 1);
    }

    #[test]
    fn prefetch_fills_only_while_the_cart_is_idle() {
        let mut waits = Waits::new();
        waits.write_byte(1, 0x40);
        assert_eq!(waits.access(0x0800_0000, 4, true, None, false), 6);
        assert_eq!(waits.access(0x0800_0004, 4, true, None, false), 4);

        let mut waits = Waits::new();
        waits.write_byte(1, 0x40);
        assert_eq!(waits.access(0x0800_0000, 2, true, None, false), 4);
        waits.idle(2);
        assert_eq!(waits.access(0x0800_0002, 2, true, None, false), 1);

        let mut waits = Waits::new();
        waits.write_byte(1, 0x40);
        assert_eq!(waits.access(0x0800_0000, 4, true, None, false), 6);
        waits.idle(4);
        assert_eq!(waits.access(0x0800_0004, 4, true, None, false), 2);
        waits.access(0x0300_0000, 4, false, None, false);
        assert_eq!(waits.access(0x0800_0008, 4, true, None, false), 6);
    }
}
