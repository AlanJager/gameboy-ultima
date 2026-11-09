//! BIOS 自带的软件混音和乐曲播放。
//!
//! `SoundDriverInit` 在调用者给的工作区里摆好 `Smsh` 头、十二路 PCM 声部和两块混音缓冲，
//! 并把指针写到 `0x03007FF0`。之后每帧先 `SoundDriverVSync` 再 `SoundDriverMain`：
//! 前者按周期重开 FIFO DMA，后者把已经打开的播放器往前推一拍，再把声部混进缓冲。
//! 乐曲数据是 MP2k 那一套：歌曲头、音色、轨道命令。

use crate::bus::Bus;

const IDENT: u32 = 0x6873_6D53;
const AREA_PTR: u32 = 0x0300_7FF0;
const AREA_BYTES: u32 = 0x0FB0;
const CHANNELS: usize = 12;
const CHANNEL_BYTES: u32 = 0x40;
const CHANNEL_BASE: u32 = 0x50;
const MIX_A: u32 = 0x350;
const MIX_BYTES: u32 = 0x630;
const MIX_B: u32 = 0x980;
const PLAYER_HEAD: u32 = 0x40;
const TRACK_BYTES: u32 = 0x50;
const FIFO_DMA: u16 = 0xB640;
const DMA1_CNT: u32 = 0x0400_00C6;
const DMA2_CNT: u32 = 0x0400_00D2;
const TIMER_RELOAD: u32 = 0x0400_0100;
const TIMER_CONTROL: u32 = 0x0400_0102;
const CYCLES_PER_FRAME: u32 = 280_896;

const SAMPLES_PER_FRAME: [u32; 13] = [
    0, 96, 132, 176, 224, 264, 304, 352, 448, 528, 608, 672, 704,
];

/// `0xD0..=0xFF` 的音符时值，单位是 tick。`0` 表示这一拍的等待写在后面的字节里。
const NOTE_TICKS: [u16; 48] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E,
    0x0F, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x1C, 0x1E, 0x20, 0x24, 0x28,
    0x2A, 0x2C, 0x30, 0x36, 0x38, 0x3C, 0x40, 0x48, 0x50, 0x54, 0x60, 0x6C, 0x70, 0x78, 0x80,
    0x90, 0xA0, 0xC0,
];

pub(crate) fn midi_freq(base: u32, key: i32, fine: i32) -> u32 {
    let steps = f64::from(key - 60) + f64::from(fine) / 256.0;
    let scaled = f64::from(base) * (steps / 12.0).exp2();
    if !scaled.is_finite() || scaled <= 0.0 {
        0
    } else if scaled >= f64::from(u32::MAX) {
        u32::MAX
    } else {
        scaled.round() as u32
    }
}

pub(crate) fn init(bus: &mut Bus, area: u32) {
    bus.write_u16(DMA1_CNT, 0);
    bus.write_u16(DMA2_CNT, 0);
    bus.write_u16(0x0400_0084, 0x008F);
    bus.write_u16(0x0400_0082, 0xA90E);
    let bias = bus.sound_bias();
    bus.set_sound_bias(bias | 0x4000);

    bus.write_u32(AREA_PTR, area);
    bus.set_sound_area(area);
    fill(bus, area, AREA_BYTES, 0);

    bus.write_u8(area + 0x06, 8);
    bus.write_u8(area + 0x07, 15);
    apply_rate(bus, area, 4);
    bus.write_u32(area + 0x40, 0);

    bus.write_u32(0x0400_00BC, area + MIX_A);
    bus.write_u32(0x0400_00C0, 0x0400_00A0);
    bus.write_u16(0x0400_00C4, 1);
    bus.write_u32(0x0400_00C8, area + MIX_B);
    bus.write_u32(0x0400_00CC, 0x0400_00A4);
    bus.write_u16(0x0400_00D0, 1);

    bus.write_u32(area, IDENT);
    bus.set_sound_vsync(true);
}

pub(crate) fn mode(bus: &mut Bus, mode: u32) {
    bus.set_sound_mode(mode);
    let Some(area) = ready(bus) else {
        return;
    };
    if mode & 0xFF != 0 {
        bus.write_u8(area + 0x05, (mode & 0x7F) as u8);
    }
    let channels = ((mode >> 8) & 0xF) as u8;
    if channels != 0 {
        bus.write_u8(area + 0x06, channels.min(CHANNELS as u8));
        clear_channels(bus);
    }
    let volume = ((mode >> 12) & 0xF) as u8;
    if volume != 0 {
        bus.write_u8(area + 0x07, volume);
    }
    if mode & 0x00B0_0000 != 0 {
        let high = (((mode & 0x0030_0000) >> 14) as u8) | ((bus.sound_bias() >> 8) as u8 & 0x3F);
        bus.set_sound_bias((bus.sound_bias() & 0x00FF) | (u16::from(high) << 8));
    }
    let rate = ((mode >> 16) & 0xF) as usize;
    if (1..=12).contains(&rate) {
        bus.write_u16(DMA1_CNT, 0);
        bus.write_u16(DMA2_CNT, 0);
        apply_rate(bus, area, rate as u8);
        bus.set_sound_vsync(true);
    }
}

pub(crate) fn vsync(bus: &mut Bus) {
    if !bus.sound_vsync() {
        return;
    }
    let Some(area) = ready(bus) else {
        return;
    };
    let left = bus.read_u8(area + 0x04);
    if left > 1 {
        bus.write_u8(area + 0x04, left - 1);
        return;
    }
    let period = bus.read_u8(area + 0x0B).max(1);
    bus.write_u8(area + 0x04, period);
    bus.write_u16(DMA1_CNT, 0);
    bus.write_u16(DMA2_CNT, 0);
    bus.write_u16(DMA1_CNT, FIFO_DMA);
    bus.write_u16(DMA2_CNT, FIFO_DMA);
    bus.write_u16(TIMER_CONTROL, 0x0080);
}

pub(crate) fn vsync_off(bus: &mut Bus) {
    bus.set_sound_vsync(false);
    bus.write_u16(DMA1_CNT, 0);
    bus.write_u16(DMA2_CNT, 0);
}

pub(crate) fn vsync_on(bus: &mut Bus) {
    bus.set_sound_vsync(true);
}

pub(crate) fn clear_channels(bus: &mut Bus) {
    let Some(area) = ready(bus) else {
        return;
    };
    for index in 0..CHANNELS {
        let channel = area + CHANNEL_BASE + index as u32 * CHANNEL_BYTES;
        bus.write_u8(channel, 0);
    }
}

pub(crate) fn main(bus: &mut Bus) {
    let Some(area) = ready(bus) else {
        return;
    };
    let mut player = bus.read_u32(area + PLAYER_HEAD);
    let mut guard = 0;
    while player != 0 && guard < 8 {
        step_player(bus, area, player);
        player = bus.read_u32(player + 0x3C);
        guard += 1;
    }
    mix(bus, area);
}

pub(crate) fn player_open(bus: &mut Bus, info: u32, tracks: u32, count: u32) {
    let count = count.clamp(1, 16) as u8;
    fill(bus, info, 0x40, 0);
    for index in 0..count {
        fill(bus, tracks.wrapping_add(u32::from(index) * TRACK_BYTES), TRACK_BYTES, 0);
    }
    bus.write_u32(info + 0x04, 0x8000);
    bus.write_u8(info + 0x08, count);
    bus.write_u32(info + 0x2C, tracks);
    bus.write_u32(info + 0x34, IDENT);
    if let Some(area) = ready(bus) {
        let previous = bus.read_u32(area + PLAYER_HEAD);
        bus.write_u32(info + 0x3C, previous);
        bus.write_u32(area + PLAYER_HEAD, info);
    }
}

pub(crate) fn player_start(bus: &mut Bus, info: u32, song: u32) {
    if bus.read_u32(info + 0x34) != IDENT {
        return;
    }
    let opened = bus.read_u8(info + 0x08).clamp(1, 16);
    let song_tracks = bus.read_u8(song).min(opened);
    bus.write_u32(info + 0x04, 0);
    bus.write_u32(info, song);
    bus.write_u32(info + 0x30, bus.read_u32(song + 4));
    bus.write_u8(info + 0x09, bus.read_u8(song + 2));
    bus.write_u16(info + 0x18, 0);
    bus.write_u16(info + 0x1C, 150);
    bus.write_u16(info + 0x1E, 0x100);
    bus.write_u16(info + 0x20, 150);
    bus.write_u16(info + 0x22, 0);
    bus.write_u8(info + 0x26, 16);
    let tracks = bus.read_u32(info + 0x2C);
    for index in 0..opened {
        let track = tracks.wrapping_add(u32::from(index) * TRACK_BYTES);
        bus.write_u8(track + 0x01, 0xFF);
        stop_track_voice(bus, track);
        if index < song_tracks {
            let cmd = bus.read_u32(song + 8 + u32::from(index) * 4);
            bus.write_u8(track, 0xC0);
            bus.write_u8(track + 0x01, 0xFF);
            bus.write_u16(track + 0x02, 0);
            bus.write_u8(track + 0x04, 0x7F);
            bus.write_u8(track + 0x05, 0x40);
            bus.write_u8(track + 0x0B, 0x40);
            bus.write_u8(track + 0x0C, 2);
            bus.write_u32(track + 0x40, cmd);
        } else {
            bus.write_u8(track, 0);
        }
    }
    if bus.read_u8(song + 3) & 0x80 != 0 {
        mode(bus, u32::from(bus.read_u8(song + 3)));
    }
}

pub(crate) fn player_stop(bus: &mut Bus, info: u32) {
    if bus.read_u32(info + 0x34) != IDENT {
        return;
    }
    let status = bus.read_u32(info + 0x04);
    bus.write_u32(info + 0x04, status | 1);
    release_player(bus, info);
}

pub(crate) fn player_continue(bus: &mut Bus, info: u32) {
    if bus.read_u32(info + 0x34) != IDENT {
        return;
    }
    let status = bus.read_u32(info + 0x04);
    bus.write_u32(info + 0x04, status & !1);
}

pub(crate) fn player_fade(bus: &mut Bus, info: u32, frames_per_step: u32) {
    if bus.read_u32(info + 0x34) != IDENT {
        return;
    }
    if frames_per_step == 0 {
        player_stop(bus, info);
        return;
    }
    bus.write_u16(info + 0x22, frames_per_step as u16);
    bus.write_u16(info + 0x24, frames_per_step as u16);
    bus.write_u8(info + 0x26, 16);
}

fn ready(bus: &Bus) -> Option<u32> {
    let area = bus.read_u32(AREA_PTR);
    if area != 0 && bus.read_u32(area) == IDENT {
        Some(area)
    } else {
        None
    }
}

fn apply_rate(bus: &mut Bus, area: u32, index: u8) {
    let samples = SAMPLES_PER_FRAME[usize::from(index.min(12))];
    let samples = if samples == 0 { 224 } else { samples };
    let period = (MIX_BYTES / samples).max(1) as u8;
    let hz = samples.saturating_mul(16_777_216) / CYCLES_PER_FRAME;
    bus.write_u8(area + 0x08, index);
    bus.write_u8(area + 0x04, period);
    bus.write_u8(area + 0x0B, period);
    bus.write_u32(area + 0x10, samples);
    bus.write_u32(area + 0x14, hz);
    // 周期是输出采样率对应的 CPU 拍数，装进定时器 0。真正启动留到下一次 VSync。
    let clocks = (16_777_216u32 / hz.max(1)).clamp(1, 65535) as u16;
    bus.write_u16(TIMER_RELOAD, 0u16.wrapping_sub(clocks));
}

fn step_player(bus: &mut Bus, area: u32, info: u32) {
    if bus.read_u32(info + 0x34) != IDENT {
        return;
    }
    let status = bus.read_u32(info + 0x04);
    if status & 0x8001 != 0 {
        return;
    }
    fade_tick(bus, info);
    if bus.read_u32(info + 0x04) & 1 != 0 {
        return;
    }
    let tempo_d = u32::from(bus.read_u16(info + 0x1C).max(1));
    let tempo_u = u32::from(bus.read_u16(info + 0x1E).max(1));
    let mut acc = u32::from(bus.read_u16(info + 0x18));
    acc += tempo_d.saturating_mul(tempo_u) / 256;
    let ticks = acc / 150;
    bus.write_u16(info + 0x18, (acc % 150) as u16);
    if ticks == 0 {
        return;
    }
    let count = bus.read_u8(info + 0x08).min(16);
    let tracks = bus.read_u32(info + 0x2C);
    for index in 0..count {
        step_track(bus, area, info, tracks.wrapping_add(u32::from(index) * TRACK_BYTES), ticks);
    }
}

fn fade_tick(bus: &mut Bus, info: u32) {
    let interval = bus.read_u16(info + 0x22);
    if interval == 0 {
        return;
    }
    let left = bus.read_u16(info + 0x24);
    if left > 1 {
        bus.write_u16(info + 0x24, left - 1);
        return;
    }
    bus.write_u16(info + 0x24, interval);
    let level = bus.read_u8(info + 0x26);
    if level <= 1 {
        bus.write_u8(info + 0x26, 0);
        bus.write_u16(info + 0x22, 0);
        player_stop(bus, info);
        return;
    }
    bus.write_u8(info + 0x26, level - 1);
}

fn step_track(bus: &mut Bus, area: u32, info: u32, track: u32, mut ticks: u32) {
    if bus.read_u8(track) & 0xC0 != 0xC0 {
        return;
    }
    loop {
        let wait = u32::from(bus.read_u16(track + 0x02));
        if wait > 0 {
            if ticks == 0 {
                return;
            }
            if wait > ticks {
                bus.write_u16(track + 0x02, (wait - ticks) as u16);
                return;
            }
            ticks -= wait;
            bus.write_u16(track + 0x02, 0);
            if bus.read_u8(track + 0x0A) != 0 {
                key_off(bus, track);
                bus.write_u8(track + 0x0A, 0);
            }
        }
        if !run_commands(bus, area, info, track) {
            return;
        }
        if bus.read_u16(track + 0x02) == 0 || ticks == 0 {
            return;
        }
    }
}

/// 读命令，直到写出一个等待或者轨道结束。返回 false 表示轨道已经停了。
fn run_commands(bus: &mut Bus, area: u32, info: u32, track: u32) -> bool {
    for _ in 0..64 {
        if bus.read_u8(track) & 0x80 == 0 {
            return false;
        }
        let mut cmd_ptr = bus.read_u32(track + 0x40);
        let cmd = bus.read_u8(cmd_ptr);
        cmd_ptr = cmd_ptr.wrapping_add(1);
        bus.write_u32(track + 0x40, cmd_ptr);
        if cmd < 0x80 {
            let wait = if cmd == 0 { 0x100 } else { u16::from(cmd) };
            bus.write_u16(track + 0x02, wait);
            return true;
        }
        match cmd {
            0xB1 => {
                stop_track_voice(bus, track);
                bus.write_u8(track, 0);
                return false;
            }
            0xB2 => {
                let dest = bus.read_u32(cmd_ptr);
                bus.write_u32(track + 0x40, dest);
            }
            0xB3 => {
                let dest = bus.read_u32(cmd_ptr);
                let ret = cmd_ptr.wrapping_add(4);
                bus.write_u32(track + 0x10, ret);
                bus.write_u32(track + 0x40, dest);
            }
            0xB4 => {
                let ret = bus.read_u32(track + 0x10);
                if ret != 0 {
                    bus.write_u32(track + 0x40, ret);
                    bus.write_u32(track + 0x10, 0);
                }
            }
            0xBB => {
                let tempo = bus.read_u16(cmd_ptr);
                bus.write_u32(track + 0x40, cmd_ptr.wrapping_add(2));
                bus.write_u16(info + 0x1C, tempo.max(1));
                bus.write_u16(info + 0x20, tempo.max(1));
            }
            0xBC => {
                bus.write_u8(track + 0x07, bus.read_u8(cmd_ptr));
                bus.write_u32(track + 0x40, cmd_ptr.wrapping_add(1));
            }
            0xBD => {
                bus.write_u8(track + 0x06, bus.read_u8(cmd_ptr));
                bus.write_u32(track + 0x40, cmd_ptr.wrapping_add(1));
            }
            0xBE => {
                bus.write_u8(track + 0x04, bus.read_u8(cmd_ptr));
                bus.write_u32(track + 0x40, cmd_ptr.wrapping_add(1));
            }
            0xBF => {
                bus.write_u8(track + 0x05, bus.read_u8(cmd_ptr));
                bus.write_u32(track + 0x40, cmd_ptr.wrapping_add(1));
            }
            0xC0 => {
                bus.write_u8(track + 0x0B, bus.read_u8(cmd_ptr));
                bus.write_u32(track + 0x40, cmd_ptr.wrapping_add(1));
            }
            0xC1 => {
                bus.write_u8(track + 0x0C, bus.read_u8(cmd_ptr).max(1));
                bus.write_u32(track + 0x40, cmd_ptr.wrapping_add(1));
            }
            0xCE => {
                if bus.read_u8(cmd_ptr) < 0x80 {
                    bus.write_u32(track + 0x40, cmd_ptr.wrapping_add(1));
                }
                key_off(bus, track);
            }
            0xCF => {
                let key = bus.read_u8(cmd_ptr);
                let vel = bus.read_u8(cmd_ptr.wrapping_add(1));
                bus.write_u32(track + 0x40, cmd_ptr.wrapping_add(2));
                key_on(bus, area, info, track, key, vel, false);
            }
            0xD0..=0xFF => {
                let key = bus.read_u8(cmd_ptr);
                let vel = bus.read_u8(cmd_ptr.wrapping_add(1));
                bus.write_u32(track + 0x40, cmd_ptr.wrapping_add(2));
                let dur = NOTE_TICKS[usize::from(cmd - 0xD0)];
                key_on(bus, area, info, track, key, vel, dur > 0);
                if dur > 0 {
                    bus.write_u16(track + 0x02, dur);
                    return true;
                }
            }
            _ => {}
        }
    }
    true
}

fn key_on(bus: &mut Bus, area: u32, info: u32, track: u32, key: u8, velocity: u8, release_on_wait: bool) {
    let tone = bus.read_u32(info + 0x30);
    if tone == 0 {
        return;
    }
    let voice = u32::from(bus.read_u8(track + 0x06));
    let entry = tone.wrapping_add(voice * 12);
    if bus.read_u8(entry) & 0xC0 != 0 {
        return;
    }
    let wave = bus.read_u32(entry + 4);
    if wave == 0 || bus.read_u32(wave + 0x0C) == 0 {
        return;
    }
    let root = bus.read_u8(entry + 1) as i32;
    let root = if root == 0 { 60 } else { root };
    let shift = bus.read_u8(track + 0x07) as i8 as i32;
    let bend = bus.read_u8(track + 0x0B) as i32 - 0x40;
    let bendr = bus.read_u8(track + 0x0C).max(1) as i32;
    let fine_total = bend * bendr * 256 / 64;
    let midi = i32::from(key) + shift + (60 - root) + fine_total.div_euclid(256);
    let fine = fine_total.rem_euclid(256);
    let freq = midi_freq(bus.read_u32(wave + 4), midi, fine);
    let channel = alloc_channel(bus, area, track);
    let Some(channel) = channel else {
        return;
    };
    let vol = u32::from(bus.read_u8(track + 0x04));
    let pan = u32::from(bus.read_u8(track + 0x05));
    let vel = u32::from(velocity);
    let rv = (vol * pan * vel / (128 * 127)).min(255) as u8;
    let lv = (vol * (128 - pan.min(128)) * vel / (128 * 127)).min(255) as u8;
    bus.write_u8(channel + 0x02, rv);
    bus.write_u8(channel + 0x03, lv);
    bus.write_u8(channel + 0x04, bus.read_u8(entry + 8));
    bus.write_u8(channel + 0x05, bus.read_u8(entry + 9));
    bus.write_u8(channel + 0x06, bus.read_u8(entry + 10));
    bus.write_u8(channel + 0x07, bus.read_u8(entry + 11));
    bus.write_u8(channel + 0x10, 0);
    bus.write_u8(channel + 0x11, 0);
    bus.write_u8(channel + 0x12, bus.read_u8(info + 0x26).max(1));
    bus.write_u32(channel + 0x20, freq);
    bus.write_u32(channel + 0x24, wave);
    bus.write_u32(channel + 0x28, 0);
    bus.write_u8(channel, 0x80);
    bus.write_u8(track + 0x0A, u8::from(release_on_wait));
}

fn key_off(bus: &mut Bus, track: u32) {
    let channel = channel_of(bus, track);
    if let Some(channel) = channel {
        let status = bus.read_u8(channel);
        if status != 0 {
            bus.write_u8(channel, status | 0x40);
        }
    }
}

fn stop_track_voice(bus: &mut Bus, track: u32) {
    if let Some(channel) = channel_of(bus, track) {
        bus.write_u8(channel, 0);
    }
    bus.write_u8(track + 0x01, 0xFF);
    bus.write_u8(track + 0x0A, 0);
}

fn release_player(bus: &mut Bus, info: u32) {
    let count = bus.read_u8(info + 0x08).min(16);
    let tracks = bus.read_u32(info + 0x2C);
    for index in 0..count {
        stop_track_voice(bus, tracks.wrapping_add(u32::from(index) * TRACK_BYTES));
    }
}

fn channel_of(bus: &Bus, track: u32) -> Option<u32> {
    let index = bus.read_u8(track + 0x01);
    if index == 0xFF {
        None
    } else if let Some(area) = ready(bus) {
        Some(area + CHANNEL_BASE + u32::from(index) * CHANNEL_BYTES)
    } else {
        None
    }
}

fn alloc_channel(bus: &mut Bus, area: u32, track: u32) -> Option<u32> {
    if let Some(channel) = channel_of(bus, track) {
        return Some(channel);
    }
    let limit = usize::from(bus.read_u8(area + 0x06).clamp(1, CHANNELS as u8));
    for index in 0..limit {
        let channel = area + CHANNEL_BASE + index as u32 * CHANNEL_BYTES;
        if bus.read_u8(channel) == 0 {
            bus.write_u8(track + 0x01, index as u8);
            return Some(channel);
        }
    }
    None
}

fn mix(bus: &mut Bus, area: u32) {
    let samples = bus.read_u32(area + 0x10).clamp(1, MIX_BYTES);
    let period = u32::from(bus.read_u8(area + 0x0B).max(1));
    let counter = u32::from(bus.read_u8(area + 0x04).max(1));
    let slot = period - counter.min(period);
    let dest = slot * samples;
    if dest.saturating_add(samples) > MIX_BYTES {
        return;
    }
    let hz = bus.read_u32(area + 0x14).max(1);
    let master = u32::from(bus.read_u8(area + 0x07).clamp(1, 15));
    let limit = usize::from(bus.read_u8(area + 0x06).clamp(1, CHANNELS as u8));
    for index in 0..limit {
        prepare_channel(bus, area + CHANNEL_BASE + index as u32 * CHANNEL_BYTES);
    }
    for sample_index in 0..samples {
        let mut left = 0i32;
        let mut right = 0i32;
        for index in 0..limit {
            let channel = area + CHANNEL_BASE + index as u32 * CHANNEL_BYTES;
            if bus.read_u8(channel) & 0x01 == 0 {
                continue;
            }
            let (sample, alive) = take_sample(bus, channel, hz);
            if !alive {
                bus.write_u8(channel, 0);
                continue;
            }
            let env = u32::from(bus.read_u8(channel + 0x10));
            let scale = u32::from(bus.read_u8(channel + 0x12).max(1));
            let rv = u32::from(bus.read_u8(channel + 0x02));
            let lv = u32::from(bus.read_u8(channel + 0x03));
            let gain = env * scale * master;
            left += i32::from(sample) * (lv * gain) as i32 / (255 * 16 * 15);
            right += i32::from(sample) * (rv * gain) as i32 / (255 * 16 * 15);
        }
        let at = dest + sample_index;
        bus.write_u8(area + MIX_B + at, left.clamp(-128, 127) as i8 as u8);
        bus.write_u8(area + MIX_A + at, right.clamp(-128, 127) as i8 as u8);
    }
}

fn prepare_channel(bus: &mut Bus, channel: u32) {
    let status = bus.read_u8(channel);
    if status == 0 {
        return;
    }
    let mut status = status;
    if status & 0x80 != 0 {
        bus.write_u8(channel + 0x10, 0);
        bus.write_u8(channel + 0x11, 0);
        bus.write_u32(channel + 0x28, 0);
        status = (status & !0x80) | 0x01;
    }
    if status & 0x40 != 0 {
        bus.write_u8(channel + 0x11, 3);
        status = (status & !0x40) | 0x01;
    }
    if status & 0x01 == 0 {
        bus.write_u8(channel, status);
        return;
    }
    let mut level = u32::from(bus.read_u8(channel + 0x10));
    let phase = bus.read_u8(channel + 0x11);
    let attack = u32::from(bus.read_u8(channel + 0x04));
    let decay = u32::from(bus.read_u8(channel + 0x05));
    let sustain = u32::from(bus.read_u8(channel + 0x06));
    let release = u32::from(bus.read_u8(channel + 0x07));
    let mut phase = phase;
    match phase {
        0 => {
            level = level.saturating_add(attack).min(255);
            if level >= 255 {
                level = 255;
                phase = 1;
            }
        }
        1 => {
            level = level * decay / 256;
            if level <= sustain {
                level = sustain;
                phase = 2;
            }
        }
        3 => {
            level = level * release / 256;
            if level == 0 {
                bus.write_u8(channel, 0);
                return;
            }
        }
        _ => {}
    }
    bus.write_u8(channel + 0x10, level as u8);
    bus.write_u8(channel + 0x11, phase);
    bus.write_u8(channel, status);
}

fn take_sample(bus: &mut Bus, channel: u32, hz: u32) -> (i8, bool) {
    let wave = bus.read_u32(channel + 0x24);
    let length = bus.read_u32(wave + 0x0C);
    if wave == 0 || length == 0 {
        return (0, false);
    }
    let looped = bus.read_u16(wave + 2) & 0xC000 != 0;
    let loop_at = bus.read_u32(wave + 8).min(length.saturating_sub(1));
    let mut pos = bus.read_u32(channel + 0x28);
    let mut index = u64::from(pos) >> 16;
    if index >= u64::from(length) {
        if !looped {
            return (0, false);
        }
        let span = u64::from(length - loop_at).max(1);
        index = u64::from(loop_at) + (index - u64::from(length)) % span;
        pos = ((index as u32) << 16) | (pos & 0xFFFF);
    }
    let sample = bus.read_u8(wave + 0x10 + index as u32) as i8;
    let step = ((u64::from(bus.read_u32(channel + 0x20)) << 16) / u64::from(hz.max(1))) as u32;
    bus.write_u32(channel + 0x28, pos.wrapping_add(step.max(1)));
    (sample, true)
}

fn fill(bus: &mut Bus, addr: u32, len: u32, value: u8) {
    let mut at = addr;
    let end = addr.wrapping_add(len);
    while at != end {
        bus.write_u8(at, value);
        at = at.wrapping_add(1);
    }
}
