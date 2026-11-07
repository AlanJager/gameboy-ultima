//! 四路 DMA。通道本身是同一套：源地址、目标地址、次数、步长、宽度、重复和完成中断。
//!
//! DMA0、DMA1、DMA2 的次数是 14 位，写入 0 表示 `0x4000`。
//! DMA1、DMA2 的特殊开工听声音队列：一次固定搬 4 个 32 位字。DMA0 的特殊开工是禁止的，使能位留着也不搬。
//! DMA3 的次数是 16 位，写入 0 表示 `0x10000`。特殊开工是可见行的水平消隐。
//! DMA0 只从内部内存读。源地址到了 `0x08000000` 及以上，读出来是 0。
//! 卡带 DRQ（控制位第 11 位）会留下，但没有设备来拉它。
//!
//! 寄存器写完使能位从 0 变 1 时，把源、目标和次数抄进内部指针。源地址控制写 3 时按加处理。
//! 立刻开工的跑完就清掉使能，重复位不算数。垂直消隐、水平消隐和特殊开工可以重复：次数重装，目标只有在「加并且重装」时回到起点，源地址接着往下走。
//! 每搬一个单位，源和目标各自按那块内存的等待来计拍，先付清再继续 CPU 的周期。这几拍跨过的下一次触发，正在付账的那一路不再重入。

use serde::{Deserialize, Serialize};

const ENABLE: u16 = 1 << 15;
const IRQ: u16 = 1 << 14;
const REPEAT: u16 = 1 << 9;
const ADDR_MASK: u32 = 0x0FFF_FFFF;

struct Spec {
    count_mask: u16,
    if_bit: u8,
    /// 特殊开工用来补直接声。
    fifo: bool,
    /// DMA3 的特殊开工是可见行水平消隐。DMA0 的特殊开工两样都不是。
    visible_hblank: bool,
}

const CHANNELS: usize = 4;

const SPEC: [Spec; CHANNELS] = [
    Spec {
        count_mask: 0x3FFF,
        if_bit: 8,
        fifo: false,
        visible_hblank: false,
    },
    Spec {
        count_mask: 0x3FFF,
        if_bit: 9,
        fifo: true,
        visible_hblank: false,
    },
    Spec {
        count_mask: 0x3FFF,
        if_bit: 10,
        fifo: true,
        visible_hblank: false,
    },
    Spec {
        count_mask: 0xFFFF,
        if_bit: 11,
        fifo: false,
        visible_hblank: true,
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Start {
    Immediate,
    VBlank,
    HBlank,
    Special,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Channel {
    source: u32,
    dest: u32,
    count: u16,
    control: u16,
    cursor_source: u32,
    cursor_dest: u32,
    saved_dest: u32,
    saved_units: u32,
}

impl Channel {
    fn new() -> Self {
        Self {
            source: 0,
            dest: 0,
            count: 0,
            control: 0,
            cursor_source: 0,
            cursor_dest: 0,
            saved_dest: 0,
            saved_units: 0,
        }
    }

    fn enabled(&self) -> bool {
        self.control & ENABLE != 0
    }

    fn start(&self) -> Start {
        match (self.control >> 12) & 3 {
            0 => Start::Immediate,
            1 => Start::VBlank,
            2 => Start::HBlank,
            _ => Start::Special,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Dma {
    channel: [Channel; CHANNELS],
    debt: [u32; CHANNELS],
    busy: [bool; CHANNELS],
    charge_depth: u8,
}

impl Dma {
    pub(crate) fn new() -> Self {
        Self {
            channel: std::array::from_fn(|_| Channel::new()),
            debt: [0; CHANNELS],
            busy: [false; CHANNELS],
            charge_depth: 0,
        }
    }

    pub(crate) fn read_byte(&self, offset: usize) -> u8 {
        let channel = &self.channel[offset / 12];
        match offset % 12 {
            10 => channel.control as u8,
            11 => (channel.control >> 8) as u8,
            _ => 0,
        }
    }

    /// 使能位从 0 变 1 时返回通道号，调用方再决定开工。
    pub(crate) fn write_byte(&mut self, offset: usize, value: u8) -> Option<usize> {
        let index = offset / 12;
        let channel = &mut self.channel[index];
        let mut armed = false;
        match offset % 12 {
            0 => channel.source = (channel.source & 0xFFFF_FF00) | u32::from(value),
            1 => channel.source = (channel.source & 0xFFFF_00FF) | (u32::from(value) << 8),
            2 => channel.source = (channel.source & 0xFF00_FFFF) | (u32::from(value) << 16),
            3 => {
                channel.source = (channel.source & 0x00FF_FFFF) | (u32::from(value & 0x0F) << 24);
            }
            4 => channel.dest = (channel.dest & 0xFFFF_FF00) | u32::from(value),
            5 => channel.dest = (channel.dest & 0xFFFF_00FF) | (u32::from(value) << 8),
            6 => channel.dest = (channel.dest & 0xFF00_FFFF) | (u32::from(value) << 16),
            7 => channel.dest = (channel.dest & 0x00FF_FFFF) | (u32::from(value & 0x0F) << 24),
            8 => channel.count = (channel.count & 0xFF00) | u16::from(value),
            9 => channel.count = (channel.count & 0x00FF) | (u16::from(value) << 8),
            10 => channel.control = (channel.control & 0xFF00) | u16::from(value & 0xE0),
            11 => {
                let was = channel.enabled();
                channel.control = (channel.control & 0x00FF) | (u16::from(value) << 8);
                armed = channel.enabled() && !was;
            }
            _ => {}
        }
        armed.then_some(index)
    }

    pub(crate) fn latch(&mut self, index: usize) {
        let mask = u32::from(SPEC[index].count_mask);
        let channel = &mut self.channel[index];
        channel.cursor_source = channel.source & ADDR_MASK;
        channel.cursor_dest = channel.dest & ADDR_MASK;
        channel.saved_dest = channel.cursor_dest;
        let raw = u32::from(channel.count) & mask;
        channel.saved_units = if raw == 0 { mask + 1 } else { raw };
    }

    pub(crate) fn is_immediate(&self, index: usize) -> bool {
        self.channel[index].start() == Start::Immediate
    }

    /// 这一路开着，并且特殊开工是给声音队列的。1 是 DMA1，2 是 DMA2。
    pub(crate) fn fifo_armed(&self, index: usize) -> bool {
        let channel = &self.channel[index];
        SPEC[index].fifo && channel.enabled() && channel.start() == Start::Special
    }

    /// DMA0 的源到了卡带窗口时读出来是 0。
    pub(crate) fn internal_source(&self, index: usize, addr: u32) -> bool {
        index != 0 || addr < 0x0800_0000
    }

    pub(crate) fn units(&self, index: usize) -> u32 {
        self.channel[index].saved_units
    }

    pub(crate) fn width(&self, index: usize) -> u32 {
        if self.channel[index].control & (1 << 10) != 0 {
            4
        } else {
            2
        }
    }

    pub(crate) fn address_step(&self, index: usize, source: bool, width: u32) -> u32 {
        let mode = if source {
            (self.channel[index].control >> 7) & 3
        } else {
            (self.channel[index].control >> 5) & 3
        };
        match mode {
            1 => 0u32.wrapping_sub(width),
            2 => 0,
            _ => width,
        }
    }

    pub(crate) fn cursor_source(&self, index: usize) -> u32 {
        self.channel[index].cursor_source
    }

    pub(crate) fn cursor_dest(&self, index: usize) -> u32 {
        self.channel[index].cursor_dest
    }

    pub(crate) fn set_cursors(&mut self, index: usize, source: u32, dest: u32) {
        self.channel[index].cursor_source = source;
        self.channel[index].cursor_dest = dest;
    }

    /// 搬完一块。重复的消隐和特殊开工留下使能位，并把要重装的目标拨回去。
    pub(crate) fn finish(&mut self, index: usize) -> u16 {
        let channel = &mut self.channel[index];
        let irq = if channel.control & IRQ != 0 {
            1u16 << SPEC[index].if_bit
        } else {
            0
        };
        let repeat = channel.control & REPEAT != 0 && channel.start() != Start::Immediate;
        if repeat {
            if (channel.control >> 5) & 3 == 3 {
                channel.cursor_dest = channel.saved_dest;
            }
        } else {
            channel.control &= !ENABLE;
        }
        irq
    }

    pub(crate) fn charge(&mut self, index: usize, cycles: u32, hold: bool) {
        if hold {
            self.busy[index] = true;
        }
        self.debt[index] = self.debt[index].saturating_add(cycles);
    }

    pub(crate) fn owing(&self) -> u32 {
        self.debt.iter().sum()
    }

    pub(crate) fn pay(&mut self, mut step: u32) {
        for (debt, busy) in self.debt.iter_mut().zip(self.busy.iter_mut()) {
            let take = step.min(*debt);
            *debt -= take;
            step -= take;
            if *debt == 0 {
                *busy = false;
            }
        }
    }

    pub(crate) fn is_busy(&self, index: usize) -> bool {
        self.busy[index]
    }

    pub(crate) fn busy(&self) -> [bool; CHANNELS] {
        self.busy
    }

    pub(crate) fn set_charging(&mut self, on: bool) {
        if on {
            self.charge_depth += 1;
        } else {
            self.charge_depth = self.charge_depth.saturating_sub(1);
        }
    }

    pub(crate) fn charging(&self) -> bool {
        self.charge_depth > 0
    }

    pub(crate) fn pending(
        &self,
        hblank: bool,
        vblank: bool,
        visible_hblank: bool,
    ) -> [bool; CHANNELS] {
        let mut out = [false; CHANNELS];
        for (index, channel) in self.channel.iter().enumerate() {
            if !channel.enabled() || self.busy[index] {
                continue;
            }
            out[index] = match channel.start() {
                Start::VBlank => vblank,
                Start::HBlank => hblank,
                Start::Special => SPEC[index].visible_hblank && visible_hblank,
                Start::Immediate => false,
            };
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::Bus;
    use crate::ppu::{CYCLES_PER_FRAME, CYCLES_PER_LINE, VISIBLE_CYCLES, VISIBLE_LINES};

    const IF_REG: u32 = 0x0400_0202;

    fn arm(bus: &mut Bus, index: u32, source: u32, dest: u32, count: u16, control: u16) {
        let base = 0x0400_00BC + index * 12;
        bus.write_u32(base, source);
        bus.write_u32(base + 4, dest);
        bus.write_u32(base + 8, u32::from(count) | (u32::from(control) << 16));
    }

    fn control_at(index: u32) -> u32 {
        0x0400_00BC + index * 12 + 10
    }

    #[test]
    fn immediate_copy_steps_source_and_dest() {
        let mut bus = Bus::new();
        bus.write_u32(0x0200_0000, 0x1122_3344);
        bus.write_u32(0x0200_0004, 0x5566_7788);
        arm(&mut bus, 2, 0x0200_0000, 0x0300_0100, 2, 0x8400);
        assert_eq!(bus.read_u32(0x0300_0100), 0x1122_3344);
        assert_eq!(bus.read_u32(0x0300_0104), 0x5566_7788);
        assert_eq!(bus.read_u16(control_at(2)) & 0x8000, 0);
        assert_eq!(bus.cycles(), 14);
        assert_eq!(bus.read_u32(0x0400_00D4), 0);
    }

    #[test]
    fn dest_can_stay_fixed_and_source_can_step_backward() {
        let mut bus = Bus::new();
        bus.write_u32(0x0200_0000, 0x0000_1111);
        bus.write_u32(0x0200_0004, 0x0000_2222);
        arm(&mut bus, 2, 0x0200_0000, 0x0300_0000, 2, 0x8440);
        assert_eq!(bus.read_u32(0x0300_0000), 0x0000_2222);

        let mut bus = Bus::new();
        bus.write_u16(0x0200_0002, 0x2222);
        bus.write_u16(0x0200_0004, 0x1111);
        arm(&mut bus, 1, 0x0200_0004, 0x0300_0000, 2, 0x8080);
        assert_eq!(bus.read_u16(0x0300_0000), 0x1111);
        assert_eq!(bus.read_u16(0x0300_0002), 0x2222);
    }

    #[test]
    fn prohibited_source_mode_still_increments() {
        let mut bus = Bus::new();
        bus.write_u16(0x0200_0000, 0x1111);
        bus.write_u16(0x0200_0002, 0x2222);
        arm(&mut bus, 2, 0x0200_0000, 0x0300_0000, 2, 0x8180);
        assert_eq!(bus.read_u16(0x0300_0000), 0x1111);
        assert_eq!(bus.read_u16(0x0300_0002), 0x2222);
    }

    #[test]
    fn halfword_dma_into_palette_keeps_both_bytes() {
        let mut bus = Bus::new();
        bus.write_u16(0x0200_0000, 0x1234);
        arm(&mut bus, 2, 0x0200_0000, 0x0500_0000, 1, 0x8000);
        assert_eq!(bus.read_u16(0x0500_0000), 0x1234);
    }

    #[test]
    fn zero_count_means_the_full_range() {
        let mut bus = Bus::new();
        bus.write_u16(0x0200_0000, 0xABCD);
        arm(&mut bus, 0, 0x0200_0000, 0x0300_0000, 0, 0x8000);
        assert_eq!(bus.read_u16(0x0300_0000), 0xABCD);
        assert_eq!(bus.cycles(), 0x4000 * 4);

        let mut bus = Bus::new();
        bus.write_u16(0x0200_0000, 0xABCD);
        arm(&mut bus, 2, 0x0200_0000, 0x0300_0000, 0, 0x8000);
        assert_eq!(bus.cycles(), 0x1_0000 * 4);
    }

    #[test]
    fn completion_sets_the_channel_if_bit_and_registers_read_back() {
        let mut bus = Bus::new();
        bus.write_u16(0x0400_00C6, 0x101F);
        assert_eq!(bus.read_u16(0x0400_00C6), 0x1000);
        assert_eq!(bus.read_u32(0x0400_00BC), 0);
        assert_eq!(bus.read_u16(0x0400_00C4), 0);
        arm(&mut bus, 0, 0x0200_0000, 0x0300_0000, 1, 0x9000);
        assert_eq!(bus.read_u16(control_at(0)), 0x9000);
        assert_eq!(bus.read_u16(0x0300_0000), 0);

        for (index, bit) in [(0u32, 0x0200u16), (1, 0x0400), (2, 0x0800)] {
            let mut bus = Bus::new();
            arm(&mut bus, index, 0x0200_0000, 0x0300_0000, 1, 0xC000);
            assert_eq!(bus.read_u16(IF_REG), bit);
            assert_eq!(bus.read_u16(control_at(index)) & 0x8000, 0);
        }
    }

    #[test]
    fn vblank_repeat_reloads_dest_and_keeps_walking_source() {
        let mut bus = Bus::new();
        for value in 1u16..=8 {
            bus.write_u16(0x0200_0000 + u32::from(value - 1) * 2, value);
        }
        arm(&mut bus, 2, 0x0200_0000, 0x0300_0000, 4, 0x9260);
        assert_eq!(bus.read_u16(0x0300_0000), 0);
        bus.advance((VISIBLE_LINES * CYCLES_PER_LINE) as u32);
        assert_eq!(
            (0..4)
                .map(|index| bus.read_u16(0x0300_0000 + index * 2))
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        assert_eq!(bus.read_u16(control_at(2)) & 0x8000, 0x8000);
        bus.advance((CYCLES_PER_FRAME - 8) as u32);
        assert_eq!(
            (0..4)
                .map(|index| bus.read_u16(0x0300_0000 + index * 2))
                .collect::<Vec<_>>(),
            vec![5, 6, 7, 8]
        );
    }

    #[test]
    fn hblank_runs_dma1_before_dma3_and_dma3_special_skips_vblank() {
        let mut bus = Bus::new();
        bus.write_u16(0x0200_0000, 0x1111);
        arm(&mut bus, 0, 0x0200_0000, 0x0200_0100, 1, 0xA200);
        arm(&mut bus, 2, 0x0200_0100, 0x0200_0200, 1, 0xA200);
        bus.advance(VISIBLE_CYCLES as u32 - 1);
        assert_eq!(bus.read_u16(0x0200_0200), 0);
        bus.advance(1);
        assert_eq!(bus.read_u16(0x0200_0100), 0x1111);
        assert_eq!(bus.read_u16(0x0200_0200), 0x1111);

        let mut bus = Bus::new();
        bus.advance((200 * CYCLES_PER_LINE) as u32);
        bus.write_u16(0x0200_0000, 0x3333);
        arm(&mut bus, 2, 0x0200_0000, 0x0300_0000, 1, 0xB200);
        bus.advance(VISIBLE_CYCLES as u32);
        assert_eq!(bus.read_u16(0x0300_0000), 0);
        let in_frame = bus.cycles() % CYCLES_PER_FRAME;
        bus.advance((CYCLES_PER_FRAME - in_frame + VISIBLE_CYCLES) as u32);
        assert_eq!(bus.read_u16(0x0300_0000), 0x3333);
    }

    #[test]
    fn dma0_copies_internal_memory_and_reads_zero_from_the_cart() {
        let mut bus = Bus::new();
        bus.write_u16(0x0200_0000, 0x1111);
        bus.write_u32(0x0400_00B0, 0x0200_0000);
        bus.write_u32(0x0400_00B4, 0x0300_0000);
        bus.write_u32(0x0400_00B8, 0x8000_0001);
        assert_eq!(bus.read_u16(0x0300_0000), 0x1111);
        assert_eq!(bus.read_u32(0x0400_00B0), 0);
        assert_eq!(bus.read_u16(0x0400_00BA) & 0x8000, 0);
        assert_eq!(bus.cycles(), 4);

        let mut bus = Bus::new();
        bus.write_u16(0x0200_0000, 0x2222);
        bus.write_u32(0x0400_00B0, 0x0200_0000);
        bus.write_u32(0x0400_00B4, 0x0300_0000);
        bus.write_u32(0x0400_00B8, 0xC000_0001);
        assert_eq!(bus.read_u16(IF_REG), 0x0100);

        let mut bus = Bus::new();
        bus.load_rom(&[0x34, 0x12]);
        bus.write_u32(0x0400_00B0, 0x0800_0000);
        bus.write_u32(0x0400_00B4, 0x0300_0000);
        bus.write_u32(0x0400_00B8, 0x8000_0001);
        assert_eq!(bus.read_u16(0x0300_0000), 0);
        arm(&mut bus, 0, 0x0800_0000, 0x0300_0000, 1, 0x8000);
        assert_eq!(bus.read_u16(0x0300_0000), 0x1234);

        let mut bus = Bus::new();
        bus.write_u16(0x0200_0000, 0xABCD);
        bus.write_u32(0x0400_00B0, 0x0200_0000);
        bus.write_u32(0x0400_00B4, 0x0300_0000);
        bus.write_u32(0x0400_00B8, 0xB000_0000);
        bus.advance(CYCLES_PER_FRAME as u32);
        assert_eq!(bus.read_u16(0x0300_0000), 0);
        assert_eq!(bus.read_u16(0x0400_00BA) & 0x8000, 0x8000);

        let mut bus = Bus::new();
        bus.write_u16(0x0200_0000, 0x1111);
        bus.write_u32(0x0400_00B0, 0x0200_0000);
        bus.write_u32(0x0400_00B4, 0x0200_0100);
        bus.write_u32(0x0400_00B8, 1 | (0xA200 << 16));
        arm(&mut bus, 0, 0x0200_0100, 0x0200_0200, 1, 0xA200);
        bus.advance(VISIBLE_CYCLES as u32);
        assert_eq!(bus.read_u16(0x0200_0100), 0x1111);
        assert_eq!(bus.read_u16(0x0200_0200), 0x1111);
    }

    #[test]
    fn sound_fifo_dma_refills_when_sixteen_samples_remain() {
        let mut bus = Bus::new();
        for value in 1u8..=64 {
            bus.write_u8(0x0200_0000 + u32::from(value) - 1, value);
        }
        arm(&mut bus, 0, 0x0200_0000, 0x0400_00A0, 1, 0xB640);
        bus.write_u16(0x0400_0082, 0x0304);
        bus.write_u16(0x0400_0100, 0xFFFF);
        bus.write_u16(0x0400_0102, 0x0083);
        assert_eq!(bus.cycles(), 56);
        bus.advance(1008);
        assert_eq!(bus.audio().0, 1);
        for value in 2..=32 {
            bus.advance(1024);
            assert_eq!(bus.audio().0, value);
        }
        bus.advance(1024);
        assert_eq!(bus.audio().0, 33);

        let mut bus = Bus::new();
        bus.write_u8(0x0200_0000, 40);
        arm(&mut bus, 1, 0x0200_0000, 0x0400_00A4, 1, 0xB640);
        bus.write_u16(0x0400_0082, 0x1008);
        bus.write_u16(0x0400_0100, 0xFFFF);
        bus.write_u16(0x0400_0102, 0x0083);
        bus.advance(1008);
        assert_eq!(bus.audio(), (0, 40));
    }

    #[test]
    fn idle_fifo_does_not_transfer_on_later_elapses() {
        let mut bus = Bus::new();
        for value in 1u8..=64 {
            bus.write_u8(0x0200_0000 + u32::from(value) - 1, value);
        }
        arm(&mut bus, 0, 0x0200_0000, 0x0400_00A0, 1, 0xB640);
        bus.write_u16(0x0400_0082, 0x0304);
        let filled = bus.fifo_bursts();
        assert!(filled >= 1);
        bus.advance(300);
        assert_eq!(bus.fifo_bursts(), filled);
    }

    #[test]
    fn fast_fifo_timer_returns_from_advance() {
        let mut bus = Bus::new();
        for value in 1u8..=64 {
            bus.write_u8(0x0200_0000 + u32::from(value) - 1, value);
        }
        arm(&mut bus, 0, 0x0200_0000, 0x0400_00A0, 1, 0xB640);
        bus.write_u16(0x0400_0082, 0x0304);
        bus.write_u16(0x0400_0100, 0xFFFF);
        bus.write_u16(0x0400_0102, 0x0080);
        let before = bus.cycles();
        bus.advance(4096);
        let spent = bus.cycles() - before;
        assert_eq!(spent, 4096, "声音队列把 advance 滚成了 {spent} 拍");
    }
}
