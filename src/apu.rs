// audio processing unit

use serde::{Deserialize, Serialize};

// CPU 每 4 个周期驱动一次 APU
// 取样计算方法 https://www.copetti.org/writings/consoles/game-boy-advance/#functionality
// Nevertheless, games can choose the sampling rate: the technical limit is 65 kHz
// (for reference, CD audio is 44.1 kHz), although higher rates are possible at the expense of sample resolution

// GBA 的 CPU 每秒跳 2²⁴ 下，也就是 16,777,216 下
// 这里的音源模拟的是 gameboy 的音源，每 4 个周期驱动一次 APU，也就是 4,194,304 次 / 秒
// 这个标识 方波、波形、噪声各自倒数，数到 0 就走一步。 相当于 CPU 的 4 个周期驱动一次 这些参数更新
const CPU_PER_APU: u32 = 4;

// 这个标识 音量变化，音符结束等事件的发生
const FRAME_CLOCKS: u32 = 8192;

// 方波的 duty 有 4 种，分别对应 12.5%、25%、50%、75% 的占空比
const DUTY: [[bool; 8]; 4] = [
    [false, false, false, false, false, false, false, true],
    [true, false, false, false, false, false, false, true],
    [true, false, false, false, false, true, true, true],
    [false, true, true, true, true, true, true, false],
];

// 噪声的 divisor 有 8 种，分别对应 1/8、1/16、1/32、1/48、1/64、1/80、1/96、1/112
const NOISE_DIVISOR: [u16; 8] = [8, 16, 32, 48, 64, 80, 96, 112];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Apu {
    enabled: bool,
    #[serde(with = "crate::serde_array")]
    regs: [u8; 0x40],
    cpu_accum: u32,
    frame_timer: u32,
    frame_step: u8,
    squares: [Square; 2],
    wave: Wave,
    noise: Noise,
    direct: Direct,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Square {
    enabled: bool,
    dac: bool,
    duty: u8,
    step: u8,
    timer: u32,
    length: u16,
    length_enable: bool,
    volume: u8,
    envelope_init: u8,
    envelope_period: u8,
    envelope_timer: u8,
    envelope_add: bool,
    freq: u16,
    sweep_period: u8,
    sweep_negate: bool,
    sweep_shift: u8,
    sweep_timer: u8,
    sweep_enable: bool,
    sweep_shadow: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Wave {
    enabled: bool,
    length: u16,
    length_enable: bool,
    timer: u32,
    position: usize,
    ram: [u8; 16],
    freq: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Noise {
    enabled: bool,
    dac: bool,
    timer: u32,
    length: u16,
    length_enable: bool,
    volume: u8,
    envelope_init: u8,
    envelope_period: u8,
    envelope_timer: u8,
    envelope_add: bool,
    divisor: u8,
    narrow: bool,
    shift: u8,
    lfsr: u16,
}

/// 两路直接声。队列满 32 字节后多出来的写入丢掉。空了就保持最后弹出的样本。
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Direct {
    fifo: [Fifo; 2],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Fifo {
    data: [i8; 32],
    read: usize,
    count: usize,
    sample: i8,
}

impl Direct {
    fn new() -> Self {
        Self {
            fifo: [Fifo::new(), Fifo::new()],
        }
    }

    fn mix(&self, low: u8, high: u8) -> (i16, i16) {
        let a = scale_sample(self.fifo[0].sample, low & 0x04 != 0);
        let b = scale_sample(self.fifo[1].sample, low & 0x08 != 0);
        let mut left = 0i16;
        let mut right = 0i16;
        if high & 0x02 != 0 {
            left = left.saturating_add(a);
        }
        if high & 0x01 != 0 {
            right = right.saturating_add(a);
        }
        if high & 0x20 != 0 {
            left = left.saturating_add(b);
        }
        if high & 0x10 != 0 {
            right = right.saturating_add(b);
        }
        (left, right)
    }
}

fn scale_sample(sample: i8, full: bool) -> i16 {
    let value = i16::from(sample);
    if full {
        value
    } else {
        value / 2
    }
}

impl Fifo {
    fn new() -> Self {
        Self {
            data: [0; 32],
            read: 0,
            count: 0,
            sample: 0,
        }
    }

    fn push(&mut self, value: u8) {
        if self.count == 32 {
            return;
        }
        let index = (self.read + self.count) % 32;
        self.data[index] = i8::from_le_bytes([value]);
        self.count += 1;
    }

    fn clock(&mut self, overflows: u64) {
        if overflows == 0 || self.count == 0 {
            return;
        }
        let steps = usize::try_from(overflows.min(self.count as u64)).unwrap_or(self.count);
        self.read = (self.read + steps) % 32;
        self.count -= steps;
        self.sample = self.data[(self.read + 31) % 32];
    }

    fn reset(&mut self) {
        self.read = 0;
        self.count = 0;
        self.sample = 0;
    }
}

impl Apu {
    /// 方波 1、方波 2、波形、噪声。方波和噪声是当前包络 0–15，关掉就是 0。波形是音量档 0–4。
    pub(crate) fn channel_levels(&self) -> [u8; 4] {
        let square = |index: usize| {
            let channel = &self.squares[index];
            if channel.enabled && channel.dac {
                channel.volume
            } else {
                0
            }
        };
        let wave = if self.wave.enabled {
            let reg = self.regs[0x13];
            if reg & 0x80 != 0 {
                4
            } else {
                (reg >> 5) & 3
            }
        } else {
            0
        };
        let noise = if self.noise.enabled && self.noise.dac {
            self.noise.volume
        } else {
            0
        };
        [square(0), square(1), wave, noise]
    }

    pub(crate) fn new() -> Self {
        Self {
            enabled: false,
            regs: [0; 0x40],
            cpu_accum: 0,
            frame_timer: FRAME_CLOCKS,
            frame_step: 0,
            squares: [Square::new(), Square::new()],
            wave: Wave::new(),
            noise: Noise::new(),
            direct: Direct::new(),
        }
    }

    pub(crate) fn tick(&mut self, cpu_cycles: u32) {
        if !self.enabled || cpu_cycles == 0 {
            return;
        }
        self.cpu_accum += cpu_cycles;
        while self.cpu_accum >= CPU_PER_APU {
            self.cpu_accum -= CPU_PER_APU;
            self.clock();
        }
    }

    /// 左声道、右声道。数值已经乘过左右音量和 `SOUNDCNT_H` 的比例。
    pub(crate) fn output(&self) -> (i16, i16) {
        let (left, right) = if self.enabled {
            (self.side(false), self.side(true))
        } else {
            (0, 0)
        };
        let (direct_left, direct_right) = self.direct.mix(self.regs[0x22], self.regs[0x23]);
        (
            left.saturating_add(direct_left),
            right.saturating_add(direct_right),
        )
    }

    /// 定时器 0、1 这次追上时各溢出了多少次。选中的那路每溢出一次弹出一个样本。
    pub(crate) fn clock_direct(&mut self, overflows: [u64; 2]) {
        for fifo in 0..2 {
            self.pop_direct(fifo, overflows[self.direct_timer(fifo)]);
        }
    }

    pub(crate) fn direct_timer(&self, fifo: usize) -> usize {
        let shift = if fifo == 0 { 2 } else { 6 };
        usize::from((self.regs[0x23] >> shift) & 1)
    }

    pub(crate) fn fifo_len(&self, fifo: usize) -> usize {
        self.direct.fifo[fifo].count
    }

    pub(crate) fn pop_direct(&mut self, fifo: usize, steps: u64) {
        self.direct.fifo[fifo].clock(steps);
    }

    /// `0x040000A0`–`0xA3` 推进 A，`0xA4`–`0xA7` 推进 B。地址低位不挑槽。
    pub(crate) fn push_sample(&mut self, off: usize, value: u8) {
        let index = usize::from(off >= 0xA4);
        self.direct.fifo[index].push(value);
    }

    pub(crate) fn bias(&self) -> u16 {
        u16::from(self.regs[0x28]) | (u16::from(self.regs[0x29]) << 8)
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::new();
    }

    pub(crate) fn read(&self, off: usize) -> u8 {
        if !(0x60..0xA0).contains(&off) {
            return 0;
        }
        if (0x90..0xA0).contains(&off) {
            return self.wave.ram[off - 0x90];
        }
        if off == 0x84 {
            let mut value = 0;
            if self.squares[0].enabled {
                value |= 1;
            }
            if self.squares[1].enabled {
                value |= 2;
            }
            if self.wave.enabled {
                value |= 4;
            }
            if self.noise.enabled {
                value |= 8;
            }
            if self.enabled {
                value |= 0x80;
            }
            return value;
        }
        let mut value = self.regs[off - 0x60];
        if matches!(off, 0x65 | 0x6D | 0x75 | 0x7D) {
            value &= !0x80;
        }
        value
    }

    pub(crate) fn write(&mut self, off: usize, value: u8) {
        if !(0x60..0xA0).contains(&off) {
            return;
        }
        if (0x90..0xA0).contains(&off) {
            self.wave.ram[off - 0x90] = value;
            return;
        }
        if off == 0x84 {
            self.write_master(value);
            return;
        }
        if off == 0x88 || off == 0x89 {
            self.regs[off - 0x60] = value;
            return;
        }
        if off == 0x82 || off == 0x83 {
            self.write_soundcnt_h(off, value);
            return;
        }
        if !self.enabled {
            return;
        }
        self.regs[off - 0x60] = value;
        match off {
            0x60 => self.squares[0].set_sweep(value),
            0x62 => self.squares[0].set_duty_length(value),
            0x63 => self.squares[0].set_envelope(value),
            0x64 => self.squares[0].set_freq_low(value),
            0x65 => self.trigger_square(0, value),
            0x68 => self.squares[1].set_duty_length(value),
            0x69 => self.squares[1].set_envelope(value),
            0x6C => self.squares[1].set_freq_low(value),
            0x6D => self.trigger_square(1, value),
            0x70 => {
                if value & 0x80 == 0 {
                    self.wave.enabled = false;
                }
            }
            0x72 => self.wave.length = 256 - u16::from(value),
            0x74 => self.wave.set_freq_low(value),
            0x75 => self.trigger_wave(value),
            0x78 => self.noise.set_length(value),
            0x79 => self.noise.set_envelope(value),
            0x7C => self.noise.set_poly(value),
            0x7D => self.trigger_noise(value),
            _ => {}
        }
    }

    fn write_master(&mut self, value: u8) {
        let on = value & 0x80 != 0;
        if self.enabled && !on {
            let bias = [self.regs[0x28], self.regs[0x29]];
            self.regs = [0; 0x40];
            self.regs[0x28] = bias[0];
            self.regs[0x29] = bias[1];
            self.squares = [Square::new(), Square::new()];
            let ram = self.wave.ram;
            self.wave = Wave::new();
            self.wave.ram = ram;
            self.noise = Noise::new();
            self.cpu_accum = 0;
        }
        if !self.enabled && on {
            self.frame_timer = FRAME_CLOCKS;
            self.frame_step = 0;
            self.cpu_accum = 0;
        }
        self.enabled = on;
    }

    fn write_soundcnt_h(&mut self, off: usize, value: u8) {
        if off == 0x82 {
            self.regs[0x22] = value;
            return;
        }
        if value & 0x08 != 0 {
            self.direct.fifo[0].reset();
        }
        if value & 0x80 != 0 {
            self.direct.fifo[1].reset();
        }
        self.regs[0x23] = value & !0x88;
    }

    fn trigger_square(&mut self, index: usize, high: u8) {
        let square = &mut self.squares[index];
        square.set_freq_high(high);
        if high & 0x80 == 0 {
            return;
        }
        square.enabled = square.dac;
        if square.length == 0 {
            square.length = 64;
        }
        square.volume = square.envelope_init;
        square.envelope_timer = square.envelope_period;
        square.timer = square.period();
        square.step = 0;
        if index == 0 {
            square.trigger_sweep();
        }
    }

    fn trigger_wave(&mut self, high: u8) {
        self.wave.set_freq_high(high);
        if high & 0x80 == 0 {
            return;
        }
        let dac = self.regs[0x10] & 0x80 != 0;
        self.wave.enabled = dac;
        if self.wave.length == 0 {
            self.wave.length = 256;
        }
        self.wave.timer = self.wave.period();
        self.wave.position = 0;
    }

    fn trigger_noise(&mut self, high: u8) {
        self.noise.length_enable = high & 0x40 != 0;
        if high & 0x80 == 0 {
            return;
        }
        self.noise.enabled = self.noise.dac;
        if self.noise.length == 0 {
            self.noise.length = 64;
        }
        self.noise.volume = self.noise.envelope_init;
        self.noise.envelope_timer = self.noise.envelope_period;
        self.noise.timer = self.noise.period();
        self.noise.lfsr = 0x7FFF;
    }

    fn clock(&mut self) {
        self.frame_timer -= 1;
        if self.frame_timer == 0 {
            self.frame_timer = FRAME_CLOCKS;
            let step = self.frame_step;
            self.frame_step = (self.frame_step + 1) & 7;
            self.frame(step);
        }
        for square in &mut self.squares {
            square.clock();
        }
        self.wave.clock();
        self.noise.clock();
    }

    fn frame(&mut self, step: u8) {
        if matches!(step, 0 | 2 | 4 | 6) {
            for square in &mut self.squares {
                square.clock_length();
            }
            self.wave.clock_length();
            self.noise.clock_length();
        }
        if matches!(step, 2 | 6) {
            self.squares[0].clock_sweep();
        }
        if step == 7 {
            for square in &mut self.squares {
                square.clock_envelope();
            }
            self.noise.clock_envelope();
        }
    }

    fn side(&self, right: bool) -> i16 {
        let cnt_l = u16::from(self.regs[0x20]) | (u16::from(self.regs[0x21]) << 8);
        let cnt_h = self.regs[0x22];
        let volume = if right { cnt_l & 7 } else { (cnt_l >> 4) & 7 };
        let route = if right {
            (cnt_l >> 8) & 0xF
        } else {
            (cnt_l >> 12) & 0xF
        };
        let scale = match cnt_h & 3 {
            1 => 2,
            2 => 4,
            _ => 1,
        };
        let mut sum = 0;
        for index in 0..4 {
            if route & (1 << index) != 0 {
                sum += self.level(index);
            }
        }
        (sum * i32::from(volume) * scale) as i16
    }

    #[cfg(test)]
    pub(crate) fn square_frequency(&self, index: usize) -> u16 {
        self.squares[index].freq
    }

    fn level(&self, index: usize) -> i32 {
        match index {
            0 | 1 => self.squares[index].level(),
            2 => self.wave.level(self.regs[0x13]),
            _ => self.noise.level(),
        }
    }
}

impl Square {
    fn new() -> Self {
        Self {
            enabled: false,
            dac: false,
            duty: 0,
            step: 0,
            timer: 1,
            length: 0,
            length_enable: false,
            volume: 0,
            envelope_init: 0,
            envelope_period: 0,
            envelope_timer: 0,
            envelope_add: false,
            freq: 0,
            sweep_period: 0,
            sweep_negate: false,
            sweep_shift: 0,
            sweep_timer: 0,
            sweep_enable: false,
            sweep_shadow: 0,
        }
    }

    fn set_sweep(&mut self, value: u8) {
        self.sweep_shift = value & 7;
        self.sweep_negate = value & 8 != 0;
        self.sweep_period = (value >> 4) & 7;
    }

    fn set_duty_length(&mut self, value: u8) {
        self.duty = value >> 6;
        self.length = 64 - u16::from(value & 0x3F);
    }

    fn set_envelope(&mut self, value: u8) {
        self.envelope_period = value & 7;
        self.envelope_add = value & 8 != 0;
        self.envelope_init = value >> 4;
        self.dac = self.envelope_init != 0 || self.envelope_add;
        if !self.dac {
            self.enabled = false;
        }
    }

    fn set_freq_low(&mut self, value: u8) {
        self.freq = (self.freq & 0x700) | u16::from(value);
    }

    fn set_freq_high(&mut self, value: u8) {
        self.freq = (self.freq & 0xFF) | (u16::from(value & 7) << 8);
        self.length_enable = value & 0x40 != 0;
    }

    fn trigger_sweep(&mut self) {
        self.sweep_shadow = self.freq;
        self.sweep_timer = if self.sweep_period == 0 {
            8
        } else {
            self.sweep_period
        };
        self.sweep_enable = self.sweep_period != 0 || self.sweep_shift != 0;
        if self.sweep_shift != 0 {
            let _ = self.next_frequency(false);
        }
    }

    fn period(&self) -> u32 {
        u32::from(2048 - (self.freq & 0x7FF)) * 4
    }

    fn clock(&mut self) {
        if !self.enabled {
            return;
        }
        self.timer = self.timer.saturating_sub(1);
        if self.timer == 0 {
            self.timer = self.period();
            self.step = (self.step + 1) & 7;
        }
    }

    fn clock_length(&mut self) {
        if self.length_enable && self.length > 0 {
            self.length -= 1;
            if self.length == 0 {
                self.enabled = false;
            }
        }
    }

    fn clock_envelope(&mut self) {
        if !self.enabled || self.envelope_period == 0 {
            return;
        }
        self.envelope_timer = self.envelope_timer.saturating_sub(1);
        if self.envelope_timer == 0 {
            self.envelope_timer = self.envelope_period;
            if self.envelope_add && self.volume < 15 {
                self.volume += 1;
            } else if !self.envelope_add && self.volume > 0 {
                self.volume -= 1;
            }
        }
    }

    fn clock_sweep(&mut self) {
        self.sweep_timer = self.sweep_timer.saturating_sub(1);
        if self.sweep_timer != 0 {
            return;
        }
        self.sweep_timer = if self.sweep_period == 0 {
            8
        } else {
            self.sweep_period
        };
        if !(self.sweep_enable && self.sweep_period != 0) {
            return;
        }
        if let Some(freq) = self.next_frequency(true) {
            if self.sweep_shift != 0 {
                self.freq = freq;
                self.sweep_shadow = freq;
                let _ = self.next_frequency(false);
            }
        }
    }

    fn next_frequency(&mut self, apply: bool) -> Option<u16> {
        let base = i32::from(self.sweep_shadow);
        let delta = base >> self.sweep_shift;
        let next = if self.sweep_negate {
            base - delta
        } else {
            base + delta
        };
        if next > 2047 {
            self.enabled = false;
            return None;
        }
        if next < 0 || !apply {
            return None;
        }
        Some(next as u16)
    }

    fn level(&self) -> i32 {
        if self.enabled && DUTY[self.duty as usize][self.step as usize] {
            i32::from(self.volume)
        } else {
            0
        }
    }
}

impl Wave {
    fn new() -> Self {
        Self {
            enabled: false,
            length: 0,
            length_enable: false,
            timer: 1,
            position: 0,
            ram: [0; 16],
            freq: 0,
        }
    }

    fn set_freq_low(&mut self, value: u8) {
        self.freq = (self.freq & 0x700) | u16::from(value);
    }

    fn set_freq_high(&mut self, value: u8) {
        self.freq = (self.freq & 0xFF) | (u16::from(value & 7) << 8);
        self.length_enable = value & 0x40 != 0;
    }

    fn period(&self) -> u32 {
        u32::from(2048 - (self.freq & 0x7FF)) * 2
    }

    fn clock(&mut self) {
        if !self.enabled {
            return;
        }
        self.timer = self.timer.saturating_sub(1);
        if self.timer == 0 {
            self.timer = self.period();
            self.position = (self.position + 1) % 32;
        }
    }

    fn clock_length(&mut self) {
        if self.length_enable && self.length > 0 {
            self.length -= 1;
            if self.length == 0 {
                self.enabled = false;
            }
        }
    }

    fn level(&self, volume: u8) -> i32 {
        if !self.enabled {
            return 0;
        }
        let byte = self.ram[self.position / 2];
        let nibble = if self.position % 2 == 0 {
            byte & 0xF
        } else {
            byte >> 4
        };
        let nibble = i32::from(nibble);
        if volume & 0x80 != 0 {
            return (nibble * 3) >> 2;
        }
        match (volume >> 5) & 3 {
            1 => nibble,
            2 => nibble >> 1,
            3 => nibble >> 2,
            _ => 0,
        }
    }
}

impl Noise {
    fn new() -> Self {
        Self {
            enabled: false,
            dac: false,
            timer: 1,
            length: 0,
            length_enable: false,
            volume: 0,
            envelope_init: 0,
            envelope_period: 0,
            envelope_timer: 0,
            envelope_add: false,
            divisor: 0,
            narrow: false,
            shift: 0,
            lfsr: 0x7FFF,
        }
    }

    fn set_length(&mut self, value: u8) {
        self.length = 64 - u16::from(value & 0x3F);
    }

    fn set_envelope(&mut self, value: u8) {
        self.envelope_period = value & 7;
        self.envelope_add = value & 8 != 0;
        self.envelope_init = value >> 4;
        self.dac = self.envelope_init != 0 || self.envelope_add;
        if !self.dac {
            self.enabled = false;
        }
    }

    fn set_poly(&mut self, value: u8) {
        self.divisor = value & 7;
        self.narrow = value & 8 != 0;
        self.shift = value >> 4;
    }

    fn period(&self) -> u32 {
        if self.shift >= 14 {
            return 0;
        }
        u32::from(NOISE_DIVISOR[self.divisor as usize]) << self.shift
    }

    fn clock(&mut self) {
        if !self.enabled {
            return;
        }
        let period = self.period();
        if period == 0 {
            return;
        }
        self.timer = self.timer.saturating_sub(1);
        if self.timer == 0 {
            self.timer = period;
            let bit = (self.lfsr ^ (self.lfsr >> 1)) & 1;
            self.lfsr = (self.lfsr >> 1) | (bit << 14);
            if self.narrow {
                self.lfsr = (self.lfsr & !(1 << 6)) | (bit << 6);
            }
        }
    }

    fn clock_length(&mut self) {
        if self.length_enable && self.length > 0 {
            self.length -= 1;
            if self.length == 0 {
                self.enabled = false;
            }
        }
    }

    fn clock_envelope(&mut self) {
        if !self.enabled || self.envelope_period == 0 {
            return;
        }
        self.envelope_timer = self.envelope_timer.saturating_sub(1);
        if self.envelope_timer == 0 {
            self.envelope_timer = self.envelope_period;
            if self.envelope_add && self.volume < 15 {
                self.volume += 1;
            } else if !self.envelope_add && self.volume > 0 {
                self.volume -= 1;
            }
        }
    }

    fn level(&self) -> i32 {
        if self.enabled && self.lfsr & 1 == 0 {
            i32::from(self.volume)
        } else {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Bus;

    const SOUND: u32 = 0x0400_0000;

    fn master(bus: &mut Bus, route: u16) {
        bus.write_u16(SOUND + 0x84, 0x0080);
        bus.write_u16(SOUND + 0x80, route | 0x0077);
        bus.write_u16(SOUND + 0x82, 2);
    }

    #[test]
    fn master_off_keeps_the_sample_silent() {
        let mut bus = Bus::new();
        bus.write_u16(SOUND + 0x68, 0xF180);
        bus.write_u16(SOUND + 0x6C, 0x8000 | 2047);
        assert_eq!(bus.audio(), (0, 0));
        assert_eq!(bus.read_u16(SOUND + 0x68), 0);
    }

    #[test]
    fn square_duty_follows_the_frequency() {
        let mut bus = Bus::new();
        master(&mut bus, 1 << 9);
        bus.write_u16(SOUND + 0x68, 0xF180);
        bus.write_u16(SOUND + 0x6C, 0x8000 | 2047);
        assert_eq!(bus.audio().1, 15 * 7 * 4);
        bus.advance(16);
        assert_eq!(bus.audio().1, 0);
        bus.advance(16 * 4);
        assert_eq!(bus.audio().1, 15 * 7 * 4);
        assert_eq!(bus.read_u16(SOUND + 0x84) & 2, 2);
    }

    #[test]
    fn envelope_lowers_the_volume_and_length_can_stop_it() {
        let mut bus = Bus::new();
        master(&mut bus, 1 << 9);
        bus.write_u16(SOUND + 0x68, 0xF180);
        bus.write_u16(SOUND + 0x6C, 0x8000 | 2047);
        bus.advance(8 * FRAME_CLOCKS * CPU_PER_APU);
        assert_eq!(bus.audio().1, 14 * 7 * 4);

        bus.write_u16(SOUND + 0x84, 0);
        assert_eq!(bus.audio(), (0, 0));
        master(&mut bus, 1 << 9);
        bus.write_u16(SOUND + 0x68, 0xF180 | 63);
        bus.write_u16(SOUND + 0x6C, 0xC000 | 2047);
        bus.advance(FRAME_CLOCKS * CPU_PER_APU);
        assert_eq!(bus.audio().1, 0);
        assert_eq!(bus.read_u16(SOUND + 0x84) & 2, 0);
    }

    #[test]
    fn sweep_raises_channel1_frequency() {
        let mut bus = Bus::new();
        master(&mut bus, 1 << 8);
        bus.write_u16(SOUND + 0x60, (1 << 4) | 1);
        bus.write_u16(SOUND + 0x62, 0xF180);
        bus.write_u16(SOUND + 0x64, 0x8000 | 0x400);
        assert_eq!(bus.square_frequency(0), 0x400);
        bus.advance(3 * FRAME_CLOCKS * CPU_PER_APU);
        assert_eq!(bus.square_frequency(0), 0x600);
    }

    #[test]
    fn wave_channel_reads_the_nibble() {
        let mut bus = Bus::new();
        master(&mut bus, 1 << 10);
        bus.write_u8(SOUND + 0x90, 0x12);
        bus.write_u16(SOUND + 0x70, 0x80);
        bus.write_u16(SOUND + 0x72, 0x2000);
        bus.write_u16(SOUND + 0x74, 0x8000 | 2047);
        assert_eq!(bus.audio().1, 2 * 7 * 4);
        bus.advance(8);
        assert_eq!(bus.audio().1, 7 * 4);
    }

    #[test]
    fn noise_shifts_the_lfsr() {
        let mut bus = Bus::new();
        master(&mut bus, 1 << 11);
        bus.write_u16(SOUND + 0x78, 0xF100);
        bus.write_u16(SOUND + 0x7C, 0x8000);
        assert_eq!(bus.audio().1, 0);
        bus.advance(8 * CPU_PER_APU);
        assert_eq!(bus.read_u16(SOUND + 0x84) & 8, 8);
        let mut saw_high = false;
        for _ in 0..32 {
            bus.advance(8 * CPU_PER_APU);
            if bus.audio().1 == 15 * 7 * 4 {
                saw_high = true;
            }
        }
        assert!(saw_high);
    }

    #[test]
    fn left_route_does_not_fill_the_right_side() {
        let mut bus = Bus::new();
        master(&mut bus, 1 << 13);
        bus.write_u16(SOUND + 0x68, 0xF180);
        bus.write_u16(SOUND + 0x6C, 0x8000 | 2047);
        assert_eq!(bus.audio(), (15 * 7 * 4, 0));
    }

    fn start_timer(bus: &mut Bus, index: u16, reload: u16) {
        let base = 0x0400_0100 + u32::from(index) * 4;
        bus.write_u16(base, reload);
        bus.write_u16(base + 2, 0x0080);
    }

    #[test]
    fn direct_sound_plays_signed_samples_from_timer_overflows() {
        let mut bus = Bus::new();
        bus.write_u32(SOUND + 0xA0, 0x0002_007F);
        bus.write_u16(SOUND + 0x82, 0x0304);
        assert_eq!(bus.read_u8(SOUND + 0xA0), 0);
        assert_eq!(bus.audio(), (0, 0));
        start_timer(&mut bus, 0, 0xFFFF);
        bus.advance(1);
        assert_eq!(bus.audio(), (0x7F, 0x7F));
        bus.advance(1);
        assert_eq!(bus.audio(), (0, 0));
        bus.advance(1);
        assert_eq!(bus.audio(), (2, 2));
        bus.advance(2);
        assert_eq!(bus.audio(), (0, 0));
    }

    #[test]
    fn direct_sound_half_volume_goes_to_one_side() {
        let mut bus = Bus::new();
        bus.write_u8(SOUND + 0xA0, 100);
        bus.write_u16(SOUND + 0x82, 0x0100);
        start_timer(&mut bus, 0, 0xFFFF);
        bus.advance(1);
        assert_eq!(bus.audio(), (0, 50));
        bus.advance(4);
        assert_eq!(bus.audio(), (0, 50));
    }

    #[test]
    fn direct_sound_follows_timer_1_and_reset_clears_it() {
        let mut bus = Bus::new();
        bus.write_u8(SOUND + 0xA4, 0x80);
        bus.write_u16(SOUND + 0x82, 0x7008);
        start_timer(&mut bus, 0, 0xFFFF);
        bus.advance(3);
        assert_eq!(bus.audio(), (0, 0));
        start_timer(&mut bus, 1, 0xFFFF);
        bus.advance(1);
        assert_eq!(bus.audio(), (-128, -128));
        bus.write_u16(SOUND + 0x82, 0xF008);
        assert_eq!(bus.read_u16(SOUND + 0x82), 0x7008);
        assert_eq!(bus.audio(), (0, 0));
        bus.advance(2);
        assert_eq!(bus.audio(), (0, 0));
    }

    #[test]
    fn direct_sound_drops_the_byte_past_32() {
        let mut bus = Bus::new();
        for value in 0..33 {
            bus.write_u8(SOUND + 0xA0, value);
        }
        bus.write_u16(SOUND + 0x82, 0x0304);
        start_timer(&mut bus, 0, 0xFFFF);
        bus.advance(31);
        assert_eq!(bus.audio(), (30, 30));
        bus.advance(1);
        assert_eq!(bus.audio(), (31, 31));
        bus.advance(5);
        assert_eq!(bus.audio(), (31, 31));
    }

    #[test]
    fn direct_sound_adds_onto_a_square() {
        let mut bus = Bus::new();
        master(&mut bus, 1 << 9);
        bus.write_u16(SOUND + 0x68, 0xF180);
        bus.write_u16(SOUND + 0x6C, 0x8000 | 2047);
        bus.write_u8(SOUND + 0xA0, 8);
        bus.write_u16(SOUND + 0x82, 0x0306);
        start_timer(&mut bus, 0, 0xFFFF);
        bus.advance(1);
        assert_eq!(bus.audio(), (8, 15 * 7 * 4 + 8));
    }

    #[test]
    fn direct_sound_is_latched_with_the_bias() {
        let mut bus = Bus::new();
        bus.write_u8(SOUND + 0xA0, 0x40);
        bus.write_u16(SOUND + 0x82, 0x0304);
        start_timer(&mut bus, 0, 0xFFFF);
        bus.advance(512);
        assert_eq!(bus.audio_samples().0, &[32]);
        assert_eq!(bus.audio_samples().1, &[32]);
    }
}
