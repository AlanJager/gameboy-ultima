// 模拟器总线模拟，CPU 和 PPU 都从这里读写
use crate::apu::Apu;
use crate::backup::Backup;
use crate::dma::Dma;
use crate::gfxcache::{self, Gfx};
use crate::icache::ICache;
use crate::keypad::Keypad;
use crate::ppu::{
    latch_span, next_paint, scan_at, Affine, DirtyLines, Frame, Sample, CYCLES_PER_FRAME,
    CYCLES_PER_LINE, LINES_PER_FRAME, VISIBLE_CYCLES, VISIBLE_LINES,
};
use crate::sched::{EventKind, Scheduler};
use crate::sound::{sample_hz, sample_period, Sampler};
use crate::timer::Timers;
use crate::wait::Waits;
use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicU8;
use std::sync::Arc;

const EWRAM: usize = 256 * 1024;
const IWRAM: usize = 32 * 1024;
const IO: usize = 0x400;
const PALETTE: usize = 0x400;
const VRAM: usize = 0x18000;
const OAM: usize = 1024;
const DISPSTAT: usize = 0x004;
const VCOUNT: usize = 0x006;
const KEYINPUT: usize = 0x130;
const IE: usize = 0x200;
const IF_REG: usize = 0x202;
const IME: usize = 0x208;
const BIOS_FLAGS: u32 = 0x0300_7FF8;
/// `bx lr`。声音跳转表里的指针都指到这里，调用之后马上回来。
const BIOS_RETURN: u32 = 0x188;
/// 中断允许位。低 3 位是只读状态，写进去留不下。
const DISPSTAT_ENABLE: u8 = 0x38;

/// CPU 停在 `swi` 里等中断。画面、定时器和声音继续走。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum CpuStall {
    Run,
    /// `(IE & IF)` 非 0 就醒，跟 `IME` 和 CPSR 的 I 无关。
    Halt,
    /// 只等按键、串口或卡带中断。
    Stop,
    /// 等 `0x03007FF8` 里出现这些位。
    Intr(u16),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bus {
    bios: Vec<u8>,
    rom: Vec<u8>,
    ewram: Vec<u8>,
    iwram: Vec<u8>,
    io: Vec<u8>,
    pub(crate) palette: Vec<u8>,
    pub(crate) vram: Vec<u8>,
    pub(crate) oam: Vec<u8>,
    cycles: u64,
    frame: Frame,
    apu: Apu,
    sound: Sampler,
    timers: Timers,
    dma: Dma,
    keys: Keypad,
    waits: Waits,
    /// 这条指令已经算过、还没加进 `cycles` 的拍。用来判断访问落在扫描线的哪一段。
    pending: u32,
    /// 正在走周期。这段时间里再开的 DMA 先记账，等这一圈回到循环里再付。
    in_clock: bool,
    stall: CpuStall,
    /// `SoundDriverInit` 记下的工作区、`SoundDriverMode` 的模式字，以及垂直消隐还进不进驱动。
    sound_area: u32,
    sound_mode: u32,
    sound_vsync: bool,
    backup: Backup,
    /// 下一次定时器、消隐、采样。空档里只把拍数加上去。
    scheduler: Scheduler,
    /// 译过的指令。命中不再拆位域，执行还是解释。
    #[serde(skip, default = "ICache::new")]
    icache: ICache,
    /// 哪些扫描线要重新合成。干净的行光束直接跳过。
    dirty: DirtyLines,
    /// 图块索引和精灵行表。只少做解码和 OAM 扫描。
    #[serde(skip, default = "Gfx::new")]
    pub(crate) gfx: Gfx,
    /// 直接处理已实现的 BIOS SWI，避免进入合成异常向量。
    bios_hle: bool,
    /// 这一路补过一次却没有变长。先别每条指令再搬，等弹出样本或重新开工。
    fifo_refill_stuck: [bool; 2],
    #[cfg(test)]
    #[cfg_attr(test, serde(skip))]
    fifo_bursts: u32,
}

impl Bus {
    pub fn new() -> Self {
        let mut bus = Self {
            bios: fresh_bios(),
            rom: Vec::new(),
            ewram: vec![0; EWRAM],
            iwram: vec![0; IWRAM],
            io: vec![0; IO],
            palette: vec![0; PALETTE],
            vram: vec![0; VRAM],
            oam: vec![0; OAM],
            cycles: 0,
            frame: Frame::blank(),
            apu: Apu::new(),
            sound: Sampler::new(),
            timers: Timers::new(),
            dma: Dma::new(),
            keys: Keypad::new(),
            waits: Waits::new(),
            pending: 0,
            in_clock: false,
            stall: CpuStall::Run,
            sound_area: 0,
            sound_mode: 0,
            sound_vsync: true,
            backup: Backup::new(),
            scheduler: Scheduler::new(),
            icache: ICache::new(),
            dirty: DirtyLines::new(),
            gfx: Gfx::new(),
            bios_hle: true,
            fifo_refill_stuck: [false; 2],
            #[cfg(test)]
            fifo_bursts: 0,
        };
        bus.seed_scheduler();
        bus
    }

    pub fn bios_hle(&self) -> bool {
        self.bios_hle
    }

    pub fn set_bios_hle(&mut self, enabled: bool) {
        self.bios_hle = enabled;
    }

    pub fn same_rom(&self, other: &Self) -> bool {
        self.rom == other.rom
    }

    #[cfg(test)]
    pub(crate) fn fifo_bursts(&self) -> u32 {
        self.fifo_bursts
    }

    // 读取存档文件，并根据文件长度决定芯片类型
    pub fn load_backup(&mut self, bytes: &[u8]) {
        self.backup.load(bytes);
    }

    pub fn backup_bytes(&self) -> &[u8] {
        self.backup.bytes()
    }

    pub fn backup_dirty(&self) -> bool {
        self.backup.dirty()
    }

    // 左声道、右声道。主开关关掉时是 `(0, 0)`
    pub fn audio(&self) -> (i16, i16) {
        self.apu.output()
    }

    // 还留在环里的左右 PWM 采样。播放线程取走过的不算。
    pub fn audio_samples(&self) -> (Vec<i16>, Vec<i16>) {
        self.sound.samples()
    }

    // 控制面上的采样率。跟着最近一次写入环的 `SOUNDBIAS`。
    pub fn audio_hz(&self) -> u32 {
        self.sound.hz()
    }

    // 环满之后被丢掉的新样本数。模拟没有停下来等。
    pub fn audio_overruns(&self) -> u64 {
        self.sound.overruns()
    }

    /// 打开系统音频设备播放采样环中的声音。
    pub fn play_audio(&self, volume: Arc<AtomicU8>) -> Result<crate::sound::AudioPlay, String> {
        crate::sound::AudioPlay::spawn(self.sound.link(), volume)
    }

    /// 放进环里的样本数，含已经被播放线程取走的。
    pub fn audio_produced(&self) -> u64 {
        self.sound.link().produced()
    }

    #[cfg(test)]
    pub(crate) fn collect_audio(&self) -> crate::sound::AudioPlay {
        crate::sound::AudioPlay::collect(self.sound.link())
    }

    /// 写出这一刻 `SOUNDBIAS` 对应的采样率。
    pub fn sample_rate(&self) -> u32 {
        sample_hz(self.apu.bias())
    }

    #[cfg(test)]
    pub(crate) fn square_frequency(&self, index: usize) -> u16 {
        self.apu.square_frequency(index)
    }

    pub fn frame(&self) -> &Frame {
        &self.frame
    }

    /// 这一行的合成位还在。扫完可见段会清掉，扫描中又写入则留到下一帧。
    pub fn line_dirty(&self, y: usize) -> bool {
        self.dirty.line_dirty(y)
    }

    /// 光束真正调用过合成的像素次数。干净行不加。
    pub fn composed_pixels(&self) -> u64 {
        self.dirty.composed()
    }

    /// 从上次贴窗口到现在，哪些行重新合成过。读完就清掉这些位。
    pub fn take_present(&mut self) -> [u8; 20] {
        self.dirty.take_present()
    }

    fn note_beam(&mut self) {
        let pos = scan_at(self.cycles);
        self.dirty.set_beam(pos.line, pos.cycle);
    }

    /// 改显存但不标脏。用来证明干净行不会重读。
    #[cfg(test)]
    pub(crate) fn poke_vram_quietly(&mut self, off: usize, value: u8) {
        self.vram[off] = value;
    }

    #[cfg(test)]
    pub(crate) fn tile_hits(&self) -> u64 {
        self.gfx.tile_hits()
    }

    #[cfg(test)]
    pub(crate) fn tile_misses(&self) -> u64 {
        self.gfx.tile_misses()
    }

    #[cfg(test)]
    pub(crate) fn sprite_hits(&self) -> u64 {
        self.gfx.sprite_hits()
    }

    #[cfg(test)]
    pub(crate) fn sprite_scans(&self) -> u64 {
        self.gfx.sprite_scans()
    }

    #[cfg(test)]
    pub(crate) fn set_gfx_raw(&mut self, raw: bool) {
        self.gfx.set_raw(raw);
    }

    #[allow(dead_code)]
    pub(crate) fn vram(&self) -> &[u8] {
        &self.vram
    }

    #[allow(dead_code)]
    pub(crate) fn oam(&self) -> &[u8] {
        &self.oam
    }

    #[allow(dead_code)]
    pub(crate) fn palette(&self) -> &[u8] {
        &self.palette
    }

    pub fn cycles(&self) -> u64 {
        self.cycles
    }

    pub(crate) fn stall(&self) -> CpuStall {
        self.stall
    }

    pub(crate) fn set_stall(&mut self, stall: CpuStall) {
        self.stall = stall;
    }

    pub(crate) fn ime(&self) -> bool {
        self.io[IME] & 1 != 0
    }

    /// `IE` 和 `IF`。
    pub(crate) fn irq_lines(&self) -> (u16, u16) {
        let ie = u16::from(self.io[IE]) | (u16::from(self.io[IE + 1]) << 8);
        let flags = u16::from(self.io[IF_REG]) | (u16::from(self.io[IF_REG + 1]) << 8);
        (ie, flags)
    }

    /// BIOS 中断标志，在 `0x03007FF8`。处理函数确认 `IF` 时也会往这里或。
    pub(crate) fn bios_flags(&self) -> u16 {
        self.read_u16(BIOS_FLAGS)
    }

    pub(crate) fn or_bios_flags(&mut self, mask: u16) {
        if mask == 0 {
            return;
        }
        self.write_u16(BIOS_FLAGS, self.bios_flags() | mask);
    }

    pub(crate) fn clear_bios_flags(&mut self, mask: u16) {
        self.write_u16(BIOS_FLAGS, self.bios_flags() & !mask);
    }

    /// 走到下一行的边界。停机时用它推进画面，又不会一次跨过整帧。
    pub(crate) fn idle_line(&mut self) -> u32 {
        let pos = (self.cycles % CYCLES_PER_LINE) as u32;
        let step = (CYCLES_PER_LINE as u32 - pos).max(1);
        self.advance(step);
        step
    }

    pub(crate) fn sound_bias(&self) -> u16 {
        self.apu.bias()
    }

    pub(crate) fn set_sound_bias(&mut self, value: u16) {
        self.write_u8(0x0400_0088, value as u8);
        self.write_u8(0x0400_0089, (value >> 8) as u8);
    }

    #[cfg(test)]
    pub(crate) fn sound_area(&self) -> u32 {
        self.sound_area
    }

    pub(crate) fn set_sound_area(&mut self, area: u32) {
        self.sound_area = area;
    }

    #[cfg(test)]
    pub(crate) fn sound_mode(&self) -> u32 {
        self.sound_mode
    }

    pub(crate) fn set_sound_mode(&mut self, mode: u32) {
        self.sound_mode = mode;
    }

    #[cfg(test)]
    pub(crate) fn sound_vsync(&self) -> bool {
        self.sound_vsync
    }

    pub(crate) fn set_sound_vsync(&mut self, on: bool) {
        self.sound_vsync = on;
    }

    /// 声音跳转表写进调用者缓冲区的地址。那里是一条 `bx lr`。
    pub(crate) fn sound_return_stub(&self) -> u32 {
        BIOS_RETURN
    }

    pub(crate) fn clear_ewram(&mut self) {
        self.ewram.fill(0);
    }

    /// 片上内存只留最后 `0x200` 字节，栈和 BIOS 中断标志在那里。
    pub(crate) fn clear_iwram_keep_stack(&mut self) {
        self.iwram[..0x7E00].fill(0);
    }

    pub(crate) fn clear_iwram_tail(&mut self) {
        self.iwram[0x7E00..].fill(0);
    }

    pub(crate) fn clear_iwram(&mut self) {
        self.iwram.fill(0);
    }

    pub(crate) fn clear_palette(&mut self) {
        self.palette.fill(0);
        self.note_beam();
        self.dirty.mark_all();
    }

    pub(crate) fn clear_vram(&mut self) {
        self.vram.fill(0);
        self.note_beam();
        self.dirty.mark_all();
        self.gfx.invalidate_tiles();
    }

    pub(crate) fn clear_oam(&mut self) {
        self.oam.fill(0);
        self.note_beam();
        self.dirty.mark_all();
    }

    pub(crate) fn reset_apu(&mut self) {
        self.apu.reset();
    }

    pub(crate) fn sample(&self) -> Sample {
        let io16 = |off: usize| u16::from(self.io[off]) | (u16::from(self.io[off + 1]) << 8);
        let io32 = |off: usize| {
            u32::from(self.io[off])
                | (u32::from(self.io[off + 1]) << 8)
                | (u32::from(self.io[off + 2]) << 16)
                | (u32::from(self.io[off + 3]) << 24)
        };
        let affine_at = |off: usize| Affine {
            pa: io16(off) as i16,
            pb: io16(off + 2) as i16,
            pc: io16(off + 4) as i16,
            pd: io16(off + 6) as i16,
            x: sign_extend_28(io32(off + 8)),
            y: sign_extend_28(io32(off + 12)),
        };
        let bg_cnt = [io16(0x08), io16(0x0A), io16(0x0C), io16(0x0E)];
        Sample {
            disp: io16(0),
            bg_priority: [
                (bg_cnt[0] & 3) as u8,
                (bg_cnt[1] & 3) as u8,
                (bg_cnt[2] & 3) as u8,
                (bg_cnt[3] & 3) as u8,
            ],
            bg_cnt,
            bg_hofs: [io16(0x10), io16(0x14), io16(0x18), io16(0x1C)],
            bg_vofs: [io16(0x12), io16(0x16), io16(0x1A), io16(0x1E)],
            affine: [affine_at(0x20), affine_at(0x30)],
            mosaic: io16(0x4C),
            win_h: [io16(0x40), io16(0x42)],
            win_v: [io16(0x44), io16(0x46)],
            win_in: io16(0x48),
            win_out: io16(0x4A),
            bldcnt: io16(0x50),
            bldalpha: io16(0x52),
            bldy: io16(0x54),
            backdrop: (u16::from(self.palette[0]) | (u16::from(self.palette[1]) << 8)) & 0x7FFF,
        }
    }

    pub(crate) fn deadline(&self) -> u64 {
        self.scheduler.deadline()
    }

    pub(crate) fn advance(&mut self, cycles: u32) {
        self.pending = 0;
        if cycles == 0 {
            return;
        }
        // 下一件日程还在后面，并且没有欠着的 DMA：只加拍，不翻堆。
        // 先看截止拍，每拍都有事件时就不必再去加 DMA 的账。
        let end = self.cycles.saturating_add(u64::from(cycles));
        if end < self.scheduler.deadline() && self.dma.owing() == 0 {
            self.pass_time(cycles);
            return;
        }
        self.run_clock(cycles);
    }

    /// 这次访问的拍数。`code` 为真时预取缓冲可以命中。
    pub(crate) fn account(&mut self, addr: u32, width: u32, code: bool) -> u32 {
        self.account_with(addr, width, code, None)
    }

    /// DMA 的源和目标各自保持突发，不因为中间夹了另一次访问就断开。
    pub(crate) fn account_seq(&mut self, addr: u32, width: u32, sequential: bool) -> u32 {
        self.account_with(addr, width, false, Some(sequential))
    }

    fn account_with(&mut self, addr: u32, width: u32, code: bool, sequential: Option<bool>) -> u32 {
        let at = self.cycles.saturating_add(u64::from(self.pending));
        let cost = self
            .waits
            .access(addr, width, code, sequential, self.drawing(at));
        self.pending += cost;
        cost
    }

    /// 比 1 拍多出来的部分。指令原来的拍数里已经算过那 1 拍。
    pub(crate) fn data_stall(&mut self, addr: u32, width: u32) -> u32 {
        self.account(addr, width, false).saturating_sub(1)
    }

    pub(crate) fn burst_stall(&mut self, mut addr: u32, count: u32, width: u32) -> u32 {
        let mut stall = 0;
        for _ in 0..count {
            stall += self.data_stall(addr, width);
            addr = addr.wrapping_add(width);
        }
        stall
    }

    /// 跳转后再取两条。返回比「每条 1 拍」多出来的拍。
    pub(crate) fn refill_stall(&mut self, target: u32, width: u32) -> u32 {
        let first = self.account(target, width, true).saturating_sub(1);
        let second = self
            .account(target.wrapping_add(width), width, true)
            .saturating_sub(1);
        first + second
    }

    pub(crate) fn note_internal(&mut self, clocks: u32) {
        self.waits.idle(clocks);
    }

    pub(crate) fn snap_timing(&self) -> (Waits, u32) {
        (self.waits.clone(), self.pending)
    }

    pub(crate) fn restore_timing(&mut self, snap: (Waits, u32)) {
        self.waits = snap.0;
        self.pending = snap.1;
    }

    fn drawing(&self, at: u64) -> bool {
        if self.io[0] & 0x80 != 0 {
            return false;
        }
        let line = (at % CYCLES_PER_FRAME) / CYCLES_PER_LINE;
        let pos = at % CYCLES_PER_LINE;
        line < VISIBLE_LINES && pos < VISIBLE_CYCLES
    }

    /// CPU 要的拍数，加上已经开工的 DMA 还欠的拍数。
    ///
    /// 空档直接跳到下一件日程和这段工作里更近的那个。同一拍的顺序见 `sched`：
    /// 定时器，行起点，水平消隐（这时才开消隐 DMA），声音采样。
    fn run_clock(&mut self, mut cpu: u32) {
        self.in_clock = true;
        loop {
            let owing = self.dma.owing();
            if cpu == 0 && owing == 0 {
                break;
            }
            let next = self.scheduler.deadline();
            if next <= self.cycles {
                let was_busy = self.dma.busy();
                self.dispatch_due(&was_busy);
                continue;
            }
            let room = u32::try_from(next - self.cycles).unwrap_or(u32::MAX);
            let step = room.min(if owing > 0 { owing } else { cpu });
            if step == 0 {
                break;
            }
            let was_busy = self.dma.busy();
            self.pass_time(step);
            if owing > 0 {
                self.dma.pay(step);
            } else {
                cpu -= step;
            }
            if self.scheduler.deadline() <= self.cycles {
                self.dispatch_due(&was_busy);
            }
        }
        self.in_clock = false;
    }

    /// 这段里面没有定时器溢出，也没有消隐沿。只加拍、走音源、锁像素。
    fn pass_time(&mut self, cycles: u32) {
        let from = self.cycles;
        self.apu.tick(cycles);
        self.sound.advance_accum(cycles);
        self.cycles += u64::from(cycles);
        self.raise_if(self.keys.irq_mask());
        if next_paint(from) < self.cycles {
            let sample = self.sample();
            let cycles = self.cycles;
            let Bus {
                frame,
                vram,
                oam,
                palette,
                dirty,
                gfx,
                ..
            } = self;
            latch_span(frame, &sample, vram, oam, palette, dirty, gfx, from, cycles);
        }
    }

    fn dispatch_due(&mut self, was_busy: &[bool; 4]) {
        let now = self.cycles;
        let mut timers = false;
        let mut line = false;
        let mut hblank = false;
        let mut sound = false;
        while let Some(kind) = self.scheduler.pop_due(now) {
            match kind {
                EventKind::Timer(_) => timers = true,
                EventKind::Line => line = true,
                EventKind::HBlank => hblank = true,
                EventKind::Sound => sound = true,
            }
        }
        if timers {
            self.catch_up_timers();
            self.reschedule_timers();
        }
        if line {
            self.on_line(was_busy);
        }
        if hblank {
            self.on_hblank(was_busy);
        }
        if sound {
            self.on_sound();
        }
    }

    fn seed_scheduler(&mut self) {
        self.scheduler.schedule_hblank(next_hblank(self.cycles));
        self.scheduler.schedule_line(next_line(self.cycles));
        self.schedule_sound();
    }

    fn reschedule_timers(&mut self) {
        for index in 0..4 {
            self.reschedule_timer(index);
        }
    }

    fn reschedule_timer(&mut self, index: usize) {
        match self.timers.next_overflow(index, self.cycles) {
            Some(when) if when > self.cycles => self.scheduler.schedule_timer(index as u8, when),
            Some(_) => self
                .scheduler
                .schedule_timer(index as u8, self.cycles.saturating_add(1)),
            None => self.scheduler.silence_timer(index as u8),
        }
    }

    fn schedule_sound(&mut self) {
        let period = u64::from(sample_period(self.apu.bias()));
        let accum = u64::from(self.sound.accum());
        let delta = if accum >= period {
            period
        } else {
            period - accum
        };
        self.scheduler
            .schedule_sound(self.cycles.saturating_add(delta.max(1)));
    }

    fn on_line(&mut self, was_busy: &[bool; 4]) {
        let line = (self.cycles / CYCLES_PER_LINE) % LINES_PER_FRAME;
        let enable = self.io[DISPSTAT];
        if line == VISIBLE_LINES && enable & 0x08 != 0 {
            self.io[IF_REG] |= 1;
        }
        let lyc = u64::from(self.io[DISPSTAT + 1]);
        if enable & 0x20 != 0 && line == lyc {
            self.io[IF_REG] |= 1 << 2;
        }
        if line == VISIBLE_LINES {
            self.start_crossed_now(false, true, false, was_busy);
        }
        self.scheduler.schedule_line(next_line(self.cycles));
    }

    fn on_hblank(&mut self, was_busy: &[bool; 4]) {
        let enable = self.io[DISPSTAT];
        if enable & 0x10 != 0 {
            self.io[IF_REG] |= 1 << 1;
        }
        let line = (self.cycles / CYCLES_PER_LINE) % LINES_PER_FRAME;
        self.start_crossed_now(true, false, line < VISIBLE_LINES, was_busy);
        self.scheduler.schedule_hblank(next_hblank(self.cycles));
    }

    fn on_sound(&mut self) {
        let bias = self.apu.bias();
        self.sound.latch(self.apu.output(), bias);
        self.schedule_sound();
    }

    fn start_crossed_now(
        &mut self,
        hblank: bool,
        vblank: bool,
        visible: bool,
        was_busy: &[bool; 4],
    ) {
        let pending = self.dma.pending(hblank, vblank, visible);
        for (index, due) in pending.iter().enumerate() {
            if *due && !was_busy[index] {
                self.run_dma(index, false);
            }
        }
    }

    /// IME 开着，并且 IE 和 IF 有重合的位。CPU 还要看自己的 I 位。
    pub(crate) fn irq_requested(&self) -> bool {
        if self.io[IME] & 1 == 0 {
            return false;
        }
        let ie = u16::from(self.io[IE]) | (u16::from(self.io[IE + 1]) << 8);
        let pending = u16::from(self.io[IF_REG]) | (u16::from(self.io[IF_REG + 1]) << 8);
        ie & pending != 0
    }

    fn catch_up_timers(&mut self) {
        let (mask, overflows) = self.timers.materialize(self.cycles);
        self.raise_if(mask);
        if self.dma.charging() {
            if overflows != [0, 0] {
                self.apu.clock_direct(overflows);
                self.fifo_refill_stuck = [false; 2];
            }
            return;
        }
        // 没有新的溢出，队列也还高于 16 字节：不必进补数，更不必搬。
        if overflows == [0, 0] && !self.fifo_needs_refill() {
            return;
        }
        self.pump_direct(overflows);
    }

    fn fifo_needs_refill(&self) -> bool {
        (0..2).any(|fifo| {
            self.dma.fifo_armed(fifo + 1)
                && !self.fifo_refill_stuck[fifo]
                && self.apu.fifo_len(fifo) <= 16
        })
    }

    /// 队列降到 16 字节及以下时先补，再弹出。两路可以盯同一个定时器，溢出次数不拆开。
    fn pump_direct(&mut self, total: [u64; 2]) {
        let mut done = [0u64; 2];
        loop {
            if self.fifo_needs_refill() {
                self.refill_low_fifos();
            }
            let step = self.next_pop_step(&done, &total);
            if step == 0 {
                break;
            }
            let mut used = [false; 2];
            for fifo in 0..2 {
                let timer = self.apu.direct_timer(fifo);
                if done[timer] >= total[timer] {
                    continue;
                }
                self.fifo_refill_stuck[fifo] = false;
                self.apu.pop_direct(fifo, step);
                used[timer] = true;
            }
            if !used.contains(&true) {
                break;
            }
            for timer in 0..2 {
                if used[timer] {
                    done[timer] += step;
                }
            }
        }
    }

    fn next_pop_step(&self, done: &[u64; 2], total: &[u64; 2]) -> u64 {
        let mut step = None;
        for fifo in 0..2 {
            let timer = self.apu.direct_timer(fifo);
            let remain = total[timer].saturating_sub(done[timer]);
            if remain == 0 {
                continue;
            }
            let chunk = if self.dma.fifo_armed(fifo + 1) {
                let until = self.apu.fifo_len(fifo).saturating_sub(16) as u64;
                if until == 0 {
                    remain
                } else {
                    remain.min(until)
                }
            } else {
                remain
            };
            step = Some(step.map_or(chunk, |so_far: u64| so_far.min(chunk)));
        }
        step.unwrap_or(0)
    }

    fn refill_low_fifos(&mut self) {
        for fifo in 0..2 {
            if self.fifo_refill_stuck[fifo] {
                continue;
            }
            let mut spins = 0;
            while self.dma.fifo_armed(fifo + 1) && self.apu.fifo_len(fifo) <= 16 {
                spins += 1;
                if spins > 3 {
                    break;
                }
                let before = self.apu.fifo_len(fifo);
                self.run_dma(fifo + 1, true);
                if self.apu.fifo_len(fifo) <= before {
                    self.fifo_refill_stuck[fifo] = true;
                    break;
                }
            }
        }
    }

    fn run_dma(&mut self, index: usize, fifo_burst: bool) {
        if !fifo_burst && self.dma.is_busy(index) {
            return;
        }
        let (units, width) = if fifo_burst {
            (4, 4)
        } else {
            (self.dma.units(index), self.dma.width(index))
        };
        self.dma.set_charging(true);
        // 正在付周期时，补队列只搬字节。等待数会被丢掉，不必每条指令再算一遍。
        let cycles = self.transfer(index, units, width, !(fifo_burst && self.in_clock));
        #[cfg(test)]
        if fifo_burst {
            self.fifo_bursts += 1;
        }
        let irq = self.dma.finish(index);
        self.raise_if(irq);
        self.dma.set_charging(false);
        self.pending = 0;
        // 正在付周期时，声音队列的补数不再记成新债。
        // 定时器一次溢出弹一个字节，补 4 个字要几十拍。这些拍再溢出，又会补，
        // 欠的拍数越滚越大，这一步就回不到 CPU。数据仍然补进队列。
        if !(fifo_burst && self.in_clock) {
            self.dma.charge(index, cycles, !fifo_burst);
        }
    }

    fn transfer(&mut self, index: usize, units: u32, width: u32, time_it: bool) -> u32 {
        let src_step = self.dma.address_step(index, true, width);
        let dst_step = self.dma.address_step(index, false, width);
        let mut src = self.dma.cursor_source(index);
        let mut dst = self.dma.cursor_dest(index);
        if index == 3 && width == 2 && self.backup.claims_eeprom(src, dst, self.rom.len()) {
            return self.transfer_eeprom(src, dst, src_step, dst_step, units);
        }
        let mut src_seq = false;
        let mut dst_seq = false;
        let mut cycles = 0u32;
        for _ in 0..units {
            let aligned_src = src & 0x0FFF_FFFF & !(width - 1);
            let aligned_dst = dst & 0x0FFF_FFFF & !(width - 1);
            if time_it {
                cycles += self.account_seq(aligned_src, width, src_seq);
            }
            let readable = self.dma.internal_source(index, aligned_src);
            if width == 4 {
                let value = if readable {
                    self.read_u32(aligned_src)
                } else {
                    0
                };
                if time_it {
                    cycles += self.account_seq(aligned_dst, width, dst_seq);
                }
                self.write_u32(aligned_dst, value);
            } else {
                let value = if readable {
                    self.read_u16(aligned_src)
                } else {
                    0
                };
                if time_it {
                    cycles += self.account_seq(aligned_dst, width, dst_seq);
                }
                self.write_u16(aligned_dst, value);
            }
            src_seq = src_step == width;
            dst_seq = dst_step == width;
            src = aligned_src.wrapping_add(src_step);
            dst = aligned_dst.wrapping_add(dst_step);
        }
        self.dma.set_cursors(index, src, dst);
        cycles
    }

    /// EEPROM 只走 DMA3 的半字。写入是一串最低位，读出时队列空了就回 1。
    fn transfer_eeprom(
        &mut self,
        mut src: u32,
        mut dst: u32,
        src_step: u32,
        dst_step: u32,
        units: u32,
    ) -> u32 {
        let rom_len = self.rom.len();
        let writing = Backup::in_window(dst, rom_len);
        let mut src_seq = false;
        let mut dst_seq = false;
        let mut cycles = 0u32;
        let mut bits = Vec::with_capacity(units as usize);
        for _ in 0..units {
            let aligned_src = src & 0x0FFF_FFFF & !1;
            let aligned_dst = dst & 0x0FFF_FFFF & !1;
            cycles += self.account_seq(aligned_src, 2, src_seq);
            cycles += self.account_seq(aligned_dst, 2, dst_seq);
            if writing {
                bits.push((self.read_u16(aligned_src) & 1) as u8);
            } else {
                let bit = u16::from(self.backup.eeprom_out());
                self.write_u16(aligned_dst, bit);
            }
            src_seq = src_step == 2;
            dst_seq = dst_step == 2;
            src = aligned_src.wrapping_add(src_step);
            dst = aligned_dst.wrapping_add(dst_step);
        }
        if writing {
            self.backup.eeprom_submit(&bits);
        }
        self.dma.set_cursors(3, src, dst);
        cycles
    }

    /// 加速时关掉采样并清掉已经进环的声音，避免满环之后一直播旧样本。
    pub fn set_fast_forward(&mut self, fast: bool) {
        self.sound.set_muted(fast);
        if fast {
            self.sound.discard();
        }
    }

    /// `pressed` 里 1 表示按下。低 10 位从低到高是 A、B、Select、Start、右、左、上、下、R、L。
    pub fn set_keys(&mut self, pressed: u16) {
        self.keys.set_pressed(pressed);
        self.raise_if(self.keys.irq_mask());
    }

    fn raise_if(&mut self, mask: u16) {
        if mask == 0 {
            return;
        }
        self.io[IF_REG] |= mask as u8;
        self.io[IF_REG + 1] |= (mask >> 8) as u8;
    }

    fn arm_dma(&mut self, index: usize) {
        if index == 1 || index == 2 {
            self.fifo_refill_stuck[index - 1] = false;
        }
        self.dma.latch(index);
        if self.dma.is_immediate(index) {
            self.run_dma(index, false);
        } else if self.dma.fifo_armed(index) {
            self.refill_low_fifos();
        }
        if !self.in_clock {
            self.run_clock(0);
        }
    }

    pub fn load_rom(&mut self, rom: &[u8]) {
        self.rom = rom.to_vec();
        self.icache.clear();
    }

    pub(crate) fn decode_arm(&mut self, pc: u32, word: u32) -> Option<crate::disasm::Op> {
        self.icache.arm(pc, word)
    }

    pub(crate) fn decode_thumb(&mut self, pc: u32, half: u16) -> crate::thumb::Op {
        self.icache.thumb(pc, half)
    }

    #[cfg(test)]
    pub(crate) fn decode_hits(&self) -> u64 {
        self.icache.hits()
    }

    #[cfg(test)]
    pub(crate) fn decode_misses(&self) -> u64 {
        self.icache.misses()
    }

    /// 测试里改 BIOS 向量。正式运行写不进这块。
    #[cfg(test)]
    pub(crate) fn poke_bios_word(&mut self, addr: u32, value: u32) {
        let off = addr as usize;
        assert!(addr % 4 == 0 && off + 4 <= self.bios.len(), "BIOS 地址不对");
        self.bios[off..off + 4].copy_from_slice(&value.to_le_bytes());
        self.icache.invalidate(addr, 4);
    }

    pub fn read_u8(&self, addr: u32) -> u8 {
        match addr >> 24 {
            0x00 if addr < 0x4000 => self.bios[addr as usize],
            0x02 => self.ewram[(addr as usize) & (EWRAM - 1)],
            0x03 => self.iwram[(addr as usize) & (IWRAM - 1)],
            0x04 => self.read_io((addr as usize) & (IO - 1)),
            0x05 => self.palette[(addr as usize) & (PALETTE - 1)],
            0x06 => self.vram[vram_index(addr)],
            0x07 => self.oam[(addr as usize) & (OAM - 1)],
            0x08..=0x0D => self
                .rom
                .get((addr as usize) & 0x01FF_FFFF)
                .copied()
                .unwrap_or(0),
            0x0E => self.backup.read(addr),
            _ => 0,
        }
    }

    /// CPU 的 8 位写入。调色板和显存的低区把这个字节填进对齐半字的两边。
    /// 精灵表，以及显存高区，这次写入丢掉。
    pub fn write_u8(&mut self, addr: u32, value: u8) {
        match self.byte_store(addr) {
            ByteStore::Drop => {}
            ByteStore::One => self.store_u8(addr, value),
            ByteStore::Both => {
                let aligned = addr & !1;
                self.store_u8(aligned, value);
                self.store_u8(aligned | 1, value);
            }
        }
    }

    /// 按地址写下这一个字节，不套用显存和调色板的 8 位规则。
    pub(crate) fn store_u8(&mut self, addr: u32, value: u8) {
        match addr >> 24 {
            0x02 => {
                self.ewram[(addr as usize) & (EWRAM - 1)] = value;
                self.icache.invalidate(addr, 1);
            }
            0x03 => {
                self.iwram[(addr as usize) & (IWRAM - 1)] = value;
                self.icache.invalidate(addr, 1);
            }
            0x04 => self.write_io((addr as usize) & (IO - 1), value),
            0x05 => {
                self.palette[(addr as usize) & (PALETTE - 1)] = value;
                self.note_beam();
                self.dirty.mark_all();
            }
            0x06 => {
                let off = vram_index(addr);
                self.vram[off] = value;
                self.note_beam();
                self.dirty.mark_vram(off, self.io[0] & 7);
                self.gfx.invalidate_tile_byte(off);
                self.icache.invalidate(addr, 1);
            }
            0x07 => {
                let off = (addr as usize) & (OAM - 1);
                let before = gfxcache::sprite_coverage(&self.oam, off);
                self.note_beam();
                self.dirty.mark_oam_byte(&self.oam, off);
                self.oam[off] = value;
                self.dirty.mark_oam_byte(&self.oam, off);
                if gfxcache::sprite_coverage(&self.oam, off) != before {
                    self.gfx.invalidate_sprite_lines();
                }
            }
            0x0E => self.backup.write(addr, value),
            _ => {}
        }
    }

    fn byte_store(&self, addr: u32) -> ByteStore {
        match addr >> 24 {
            0x05 => ByteStore::Both,
            0x07 => ByteStore::Drop,
            0x06 => {
                let off = vram_index(addr);
                let bitmap = self.io[0] & 7 >= 3;
                let repeats = if bitmap {
                    off < 0x1_4000
                } else {
                    off < 0x1_0000
                };
                if repeats {
                    ByteStore::Both
                } else {
                    ByteStore::Drop
                }
            }
            _ => ByteStore::One,
        }
    }

    pub fn read_u16(&self, addr: u32) -> u16 {
        u16::from(self.read_u8(addr)) | (u16::from(self.read_u8(addr.wrapping_add(1))) << 8)
    }

    /// 半字加载。地址最低位是 1 时，`ldrh` 改走没对齐的字加载，`ldrsh` 改成符号扩展的字节。
    pub(crate) fn read_half(&self, addr: u32, signed: bool) -> u32 {
        if addr & 1 != 0 {
            if signed {
                self.read_u8(addr) as i8 as i32 as u32
            } else {
                self.read_u32(addr & !3).rotate_right((addr & 3) * 8)
            }
        } else {
            let raw = self.read_u16(addr);
            if signed {
                raw as i16 as i32 as u32
            } else {
                u32::from(raw)
            }
        }
    }

    pub fn write_u16(&mut self, addr: u32, value: u16) {
        self.store_u8(addr, value as u8);
        self.store_u8(addr.wrapping_add(1), (value >> 8) as u8);
    }

    pub fn read_u32(&self, addr: u32) -> u32 {
        u32::from(self.read_u16(addr)) | (u32::from(self.read_u16(addr.wrapping_add(2))) << 16)
    }

    pub fn write_u32(&mut self, addr: u32, value: u32) {
        self.write_u16(addr, value as u16);
        self.write_u16(addr.wrapping_add(2), (value >> 16) as u16);
    }

    fn read_io(&self, off: usize) -> u8 {
        if off == DISPSTAT {
            let lyc = self.io[DISPSTAT + 1];
            let flags = scan_at(self.cycles).dispstat_flags(lyc);
            (self.io[DISPSTAT] & DISPSTAT_ENABLE) | flags
        } else if off == VCOUNT {
            scan_at(self.cycles).line as u8
        } else if off == VCOUNT + 1 {
            0
        } else if (KEYINPUT..KEYINPUT + 4).contains(&off) {
            self.keys.read_byte(off - KEYINPUT)
        } else if (0x100..0x110).contains(&off) {
            self.timers.read_byte(off - 0x100, self.cycles)
        } else if (0x60..0xA0).contains(&off) {
            self.apu.read(off)
        } else if (0x204..0x206).contains(&off) {
            self.waits.read_byte(off - 0x204)
        } else if (0xB0..0xE0).contains(&off) {
            self.dma.read_byte(off - 0xB0)
        } else {
            self.io.get(off).copied().unwrap_or(0)
        }
    }

    fn write_io(&mut self, off: usize, value: u8) {
        if off == 1 {
            let old_obj = self.io[1] & (1 << 4) != 0;
            let new_obj = value & (1 << 4) != 0;
            if old_obj != new_obj {
                self.gfx.invalidate_sprite_lines();
            }
        }
        if screen_register(off) {
            self.note_beam();
            self.dirty.mark_all();
        }
        if off == DISPSTAT {
            self.io[DISPSTAT] = value & DISPSTAT_ENABLE;
            return;
        }
        if off == VCOUNT || off == VCOUNT + 1 {
            return;
        }
        if (KEYINPUT..KEYINPUT + 4).contains(&off) {
            self.keys.write_byte(off - KEYINPUT, value);
            self.raise_if(self.keys.irq_mask());
            return;
        }
        if (0x100..0x110).contains(&off) {
            self.catch_up_timers();
            let index = (off - 0x100) / 4;
            self.timers.write_byte(off - 0x100, value, self.cycles);
            if (off - 0x100) % 4 == 2 {
                self.reschedule_timer(index);
            }
            return;
        }
        if (0xA0..0xA8).contains(&off) {
            self.apu.push_sample(off, value);
            return;
        }
        if (0x204..0x206).contains(&off) {
            self.waits.write_byte(off - 0x204, value);
            return;
        }
        if (0xB0..0xE0).contains(&off) {
            if let Some(index) = self.dma.write_byte(off - 0xB0, value) {
                self.arm_dma(index);
            }
            return;
        }
        if off == IF_REG || off == IF_REG + 1 {
            self.io[off] &= !value;
            return;
        }
        if off == IME {
            self.io[IME] = value & 1;
            return;
        }
        if (IME + 1..=IME + 3).contains(&off) {
            return;
        }
        if (0x60..0xA0).contains(&off) {
            self.apu.write(off, value);
            if off == 0x88 || off == 0x89 {
                self.schedule_sound();
            }
            return;
        }
        if let Some(cell) = self.io.get_mut(off) {
            *cell = value;
        }
    }
}

fn next_hblank(now: u64) -> u64 {
    let pos = now % CYCLES_PER_LINE;
    let base = now - pos;
    if pos < VISIBLE_CYCLES {
        base + VISIBLE_CYCLES
    } else {
        base + CYCLES_PER_LINE + VISIBLE_CYCLES
    }
}

fn next_line(now: u64) -> u64 {
    let pos = now % CYCLES_PER_LINE;
    now - pos + CYCLES_PER_LINE
}

/// 这些寄存器会改变已经画出来的扫描线。
fn screen_register(off: usize) -> bool {
    matches!(off, 0x00 | 0x01 | 0x08..=0x3F | 0x40..=0x55)
}

/// 显存 96 KiB 放在 128 KiB 的窗口里，高 32 KiB 折回 `0x06010000`。
fn vram_index(addr: u32) -> usize {
    let off = (addr as usize) & 0x1_FFFF;
    if off >= 0x1_8000 {
        off - 0x8000
    } else {
        off
    }
}

enum ByteStore {
    One,
    Both,
    Drop,
}

fn fresh_bios() -> Vec<u8> {
    let mut bios = vec![0u8; 0x4000];
    // 没有未定义指令处理函数。`movs pc, lr` 跳过这条指令，回到原来的模式。
    bios[0x04..0x08].copy_from_slice(&0xE1B0_F00Eu32.to_le_bytes());
    // 没认出来的 `swi` 仍跳到这里。执行到的话，用 `movs pc, lr` 回去。
    bios[0x08..0x0C].copy_from_slice(&0xE1B0_F00Eu32.to_le_bytes());
    // IRQ。`0x18` 跳到 `0x128`，压栈后 `ldr pc, [0x03FFFFFC]`。
    // 处理函数用 `bx lr` 回到 `0x138`，再 `subs pc, lr, #4`。
    // 加载 PC 不看最低位，调用时保持 ARM。T 要等这条 `subs` 才从 SPSR 回来。
    bios[0x18..0x1C].copy_from_slice(&0xEA00_0042u32.to_le_bytes());
    bios[0x128..0x12C].copy_from_slice(&0xE92D_500Fu32.to_le_bytes());
    bios[0x12C..0x130].copy_from_slice(&0xE3A0_0301u32.to_le_bytes());
    bios[0x130..0x134].copy_from_slice(&0xE28F_E000u32.to_le_bytes());
    bios[0x134..0x138].copy_from_slice(&0xE510_F004u32.to_le_bytes());
    bios[0x138..0x13C].copy_from_slice(&0xE8BD_500Fu32.to_le_bytes());
    bios[0x13C..0x140].copy_from_slice(&0xE25E_F004u32.to_le_bytes());
    bios[BIOS_RETURN as usize..BIOS_RETURN as usize + 4]
        .copy_from_slice(&0xE12F_FF1Eu32.to_le_bytes());
    bios
}

fn sign_extend_28(value: u32) -> i32 {
    let value = value & 0x0FFF_FFFF;
    if value & 0x0800_0000 != 0 {
        (value | 0xF000_0000) as i32
    } else {
        value as i32
    }
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vram_and_rom_stay_apart() {
        let mut bus = Bus::new();
        bus.load_rom(&[0x04, 0x00, 0xA0, 0xE3]);
        bus.write_u32(0x0600_0000, 0x001F_001F);
        bus.write_u32(0x0800_0000, 0xFFFF_FFFF);
        assert_eq!(bus.read_u32(0x0600_0000), 0x001F_001F);
        assert_eq!(bus.read_u32(0x0800_0000), 0xE3A0_0004);
        assert_eq!(bus.read_u32(0x0A00_0000), 0xE3A0_0004);
        assert_eq!(bus.read_u32(0x0C00_0000), 0xE3A0_0004);
    }

    #[test]
    fn cartridge_file_starts_at_the_rom_window() {
        let mut bus = Bus::new();
        bus.load_rom(&[0xFE, 0xFF, 0xFF, 0xEA, 0x7F]);
        assert_eq!(bus.read_u32(0x0800_0000), 0xEAFF_FFFE);
        assert_eq!(bus.read_u8(0x0800_0004), 0x7F);
        assert_eq!(bus.read_u8(0x0800_0005), 0);
        assert_eq!(bus.read_u32(0x0A00_0004), 0x7F);
    }

    #[test]
    fn vcount_comes_from_the_cycle_counter() {
        let mut bus = Bus::new();
        bus.advance(160 * 1232);
        assert_eq!(bus.read_u16(0x0400_0006), 160);
        bus.write_u16(0x0400_0006, 0x00FF);
        assert_eq!(bus.read_u8(0x0400_0006), 160);
        assert_eq!(bus.read_u8(0x0400_0007), 0);
    }

    #[test]
    fn keys_read_released_and_the_control_can_raise_if() {
        let mut bus = Bus::new();
        assert_eq!(bus.read_u16(0x0400_0130), 0x03FF);
        bus.write_u16(0x0400_0130, 0);
        assert_eq!(bus.read_u16(0x0400_0130), 0x03FF);
        bus.write_u16(0x0400_0132, 0xFFFF);
        assert_eq!(bus.read_u16(0x0400_0132), 0xC3FF);
        bus.advance(8);
        assert_eq!(bus.read_u16(0x0400_0202) & 0x1000, 0);

        bus.write_u16(0x0400_0132, 0x4001);
        bus.set_keys(0x0001);
        assert_eq!(bus.read_u16(0x0400_0130), 0x03FE);
        assert_eq!(bus.read_u16(0x0400_0202) & 0x1000, 0x1000);
        bus.write_u16(0x0400_0202, 0x1000);
        assert_eq!(bus.read_u16(0x0400_0202) & 0x1000, 0);
        bus.advance(8);
        assert_eq!(bus.read_u16(0x0400_0202) & 0x1000, 0x1000);

        bus.write_u16(0x0400_0202, 0x1000);
        bus.set_keys(0);
        bus.advance(8);
        assert_eq!(bus.read_u16(0x0400_0130), 0x03FF);
        assert_eq!(bus.read_u16(0x0400_0202) & 0x1000, 0);

        bus.write_u16(0x0400_0132, 0xC003);
        bus.set_keys(0x0001);
        bus.advance(8);
        assert_eq!(bus.read_u16(0x0400_0202) & 0x1000, 0);
        bus.set_keys(0x0003);
        assert_eq!(bus.read_u16(0x0400_0202) & 0x1000, 0x1000);
    }

    #[test]
    fn dispstat_flags_follow_the_scan() {
        let mut bus = Bus::new();
        assert_eq!(bus.read_u16(0x0400_0004), 0x0004);

        bus.advance(960);
        assert_eq!(bus.read_u8(0x0400_0004) & 7, 0x6);

        bus.advance(1232 - 960);
        assert_eq!(bus.read_u8(0x0400_0004) & 7, 0);

        bus.advance(159 * 1232);
        assert_eq!(bus.read_u8(0x0400_0004) & 7, 1);
        bus.advance(960);
        assert_eq!(bus.read_u8(0x0400_0004) & 7, 3);

        bus.advance(67 * 1232 - 960);
        assert_eq!(bus.read_u16(0x0400_0006), 227);
        assert_eq!(bus.read_u8(0x0400_0004) & 7, 0);
        bus.advance(960);
        assert_eq!(bus.read_u8(0x0400_0004) & 7, 2);
    }

    #[test]
    fn dispstat_keeps_the_enable_bits_and_the_compare_line() {
        let mut bus = Bus::new();
        bus.write_u16(0x0400_0004, 0xA028);
        assert_eq!(bus.read_u16(0x0400_0004), 0xA028);

        bus.advance(160 * 1232);
        assert_eq!(bus.read_u16(0x0400_0004), 0xA02D);
    }

    #[test]
    fn video_byte_stores_fill_a_halfword_or_drop() {
        let mut bus = Bus::new();
        bus.write_u8(0x0300_0000, 0x02);
        assert_eq!(bus.read_u16(0x0300_0000), 0x0002);

        bus.write_u8(0x0500_0020, 0x01);
        assert_eq!(bus.read_u16(0x0500_0020), 0x0101);
        bus.write_u16(0x0500_0022, 0x0201);
        assert_eq!(bus.read_u16(0x0500_0022), 0x0201);

        bus.write_u8(0x0700_0010, 0x01);
        assert_eq!(bus.read_u32(0x0700_0010), 0);
        bus.write_u32(0x0700_0010, 1);
        assert_eq!(bus.read_u32(0x0700_0410), 1);

        bus.write_u32(0x0601_0000, 2);
        assert_eq!(bus.read_u32(0x0601_8000), 2);
        bus.write_u8(0x0601_0010, 2);
        assert_eq!(bus.read_u16(0x0601_0010), 0);
        bus.write_u8(0x0600_0010, 0x02);
        assert_eq!(bus.read_u16(0x0600_0010), 0x0202);

        bus.write_u16(0x0400_0000, 3);
        bus.write_u8(0x0601_4010, 2);
        assert_eq!(bus.read_u16(0x0601_4010), 0);
        bus.write_u8(0x0600_0012, 0x04);
        assert_eq!(bus.read_u16(0x0600_0012), 0x0404);
    }

    #[test]
    fn display_edges_latch_if_until_cleared() {
        let mut bus = Bus::new();
        bus.write_u16(0x0400_0004, 0x0008);
        bus.advance(160 * 1232);
        assert_eq!(bus.read_u16(0x0400_0202), 1);
        bus.write_u16(0x0400_0202, 1);
        assert_eq!(bus.read_u16(0x0400_0202), 0);
        bus.advance(10);
        assert_eq!(bus.read_u16(0x0400_0202), 0);

        let mut bus = Bus::new();
        bus.write_u16(0x0400_0004, 0x0010);
        bus.advance(960);
        assert_eq!(bus.read_u16(0x0400_0202), 2);
        bus.write_u16(0x0400_0202, 2);
        bus.advance(10);
        assert_eq!(bus.read_u16(0x0400_0202), 0);

        let mut bus = Bus::new();
        bus.write_u16(0x0400_0004, (50 << 8) | 0x0020);
        bus.advance(50 * 1232);
        assert_eq!(bus.read_u16(0x0400_0202), 4);
        bus.write_u16(0x0400_0200, 4);
        assert!(!bus.irq_requested());
        bus.write_u16(0x0400_0208, 1);
        assert!(bus.irq_requested());
        bus.write_u16(0x0400_0202, 4);
        bus.advance(10);
        assert!(!bus.irq_requested());
    }

    #[test]
    fn timers_count_on_the_shared_cycle_clock() {
        let mut bus = Bus::new();
        bus.write_u16(0x0400_0100, 0x1234);
        assert_eq!(bus.read_u16(0x0400_0100), 0);
        bus.write_u16(0x0400_0102, 0x0080);
        assert_eq!(bus.read_u16(0x0400_0100), 0x1234);
        bus.advance(4);
        assert_eq!(bus.read_u16(0x0400_0100), 0x1238);

        bus.write_u16(0x0400_0100, 0xFFF0);
        assert_eq!(bus.read_u16(0x0400_0100), 0x1238);
        bus.advance(0x10000 - 0x1238);
        assert_eq!(bus.read_u16(0x0400_0100), 0xFFF0);
        assert_eq!(bus.read_u16(0x0400_0202), 0);

        bus.write_u16(0x0400_0102, 0);
        bus.advance(50);
        assert_eq!(bus.read_u16(0x0400_0100), 0xFFF0);

        let mut bus = Bus::new();
        bus.advance(60);
        bus.write_u16(0x0400_0100, 0);
        bus.write_u16(0x0400_0102, 0x0081);
        bus.advance(3);
        assert_eq!(bus.read_u16(0x0400_0100), 0);
        bus.advance(1);
        assert_eq!(bus.read_u16(0x0400_0100), 1);

        let mut bus = Bus::new();
        bus.write_u16(0x0400_0102, 0x0084);
        bus.advance(5);
        assert_eq!(bus.read_u16(0x0400_0100), 5);

        let mut bus = Bus::new();
        bus.write_u16(0x0400_0100, 0xFFFE);
        bus.write_u16(0x0400_0102, 0x0080);
        bus.write_u16(0x0400_0106, 0x0084);
        bus.advance(1);
        assert_eq!(bus.read_u16(0x0400_0100), 0xFFFF);
        assert_eq!(bus.read_u16(0x0400_0104), 0);
        bus.advance(1);
        assert_eq!(bus.read_u16(0x0400_0100), 0xFFFE);
        assert_eq!(bus.read_u16(0x0400_0104), 1);

        let mut bus = Bus::new();
        bus.write_u16(0x0400_010E, 0x0083);
        bus.advance(1023);
        assert_eq!(bus.read_u16(0x0400_010C), 0);
        bus.advance(1);
        assert_eq!(bus.read_u16(0x0400_010C), 1);
    }

    #[test]
    fn timer_overflow_sets_if() {
        let mut bus = Bus::new();
        bus.write_u16(0x0400_0100, 0);
        bus.write_u16(0x0400_0102, 0x00C0);
        bus.advance(10);
        assert_eq!(bus.read_u16(0x0400_0100), 10);
        assert_eq!(bus.read_u16(0x0400_0202), 0);

        bus.write_u16(0x0400_0102, 0);
        bus.write_u16(0x0400_0100, 0xFFFE);
        bus.write_u16(0x0400_0102, 0x00C0);
        bus.advance(4);
        assert_eq!(bus.read_u16(0x0400_0100), 0xFFFE);
        assert_eq!(bus.read_u16(0x0400_0202), 0x0008);
        assert!(!bus.irq_requested());
        bus.write_u16(0x0400_0200, 0x0008);
        bus.write_u16(0x0400_0208, 1);
        assert!(bus.irq_requested());

        let mut bus = Bus::new();
        bus.write_u16(0x0400_0100, 0xFFFF);
        bus.write_u16(0x0400_0102, 0x0080);
        bus.write_u16(0x0400_0104, 0xFFFF);
        bus.write_u16(0x0400_0106, 0x00C4);
        bus.advance(1);
        assert_eq!(bus.read_u16(0x0400_0104), 0xFFFF);
        assert_eq!(bus.read_u16(0x0400_0202), 0x0010);
    }
}
