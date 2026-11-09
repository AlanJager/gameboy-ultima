// 手动截取 swi 来执行我们自己的 BIOS 模拟

use crate::bus::{Bus, CpuStall};
use crate::registers::Registers;

const BIOS_END: u32 = 0x4000;

// 这个函数是 BIOS 模拟的核心，它根据传入的指令号执行相应的操作，并返回执行结果。
// 它接收三个参数：
// - regs: &mut Registers，寄存器状态的引用，用于读取和设置寄存器值
// - bus: &mut Bus，总线状态的引用，用于读取和写入内存
// - number: u8，指令号，用于确定要执行的操作
// 返回一个 Option<(u32, bool)>，其中 u32 表示执行指令所需的周期数，bool 表示是否需要继续执行
// 如果指令号不匹配任何已定义的操作，则返回 None
pub(crate) fn call(regs: &mut Registers, bus: &mut Bus, number: u8) -> Option<(u32, bool)> {
    Some(match number {
        0x00 => soft_reset(regs, bus),
        0x01 => register_ram_reset(regs, bus),
        0x02 => halt(bus, CpuStall::Halt),
        0x03 => halt(bus, CpuStall::Stop),
        0x04 => intr_wait(regs, bus, false),
        0x05 => vblank_intr_wait(regs, bus),
        0x06 => divide(regs, bus, false),
        0x07 => divide(regs, bus, true),
        0x08 => sqrt(regs, bus),
        0x09 => arctan(regs, bus),
        0x0A => arctan2(regs, bus),
        0x0B => cpu_set(regs, bus, false),
        0x0C => cpu_set(regs, bus, true),
        0x0D => bios_checksum(regs, bus),
        0x0E => bg_affine_set(regs, bus),
        0x0F => obj_affine_set(regs, bus),
        0x10 => bit_unpack(regs, bus),
        0x11 => lz77(regs, bus, false),
        0x12 => lz77(regs, bus, true),
        0x13 => huffman(regs, bus),
        0x14 => run_length(regs, bus, false),
        0x15 => run_length(regs, bus, true),
        0x16 => diff_filter(regs, bus, 1, false),
        0x17 => diff_filter(regs, bus, 1, true),
        0x18 => diff_filter(regs, bus, 2, true),
        0x19 => sound_bias(regs, bus),
        0x1A => sound_init(regs, bus),
        0x1B => sound_mode(regs, bus),
        0x1C => sound_main(bus),
        0x1D => sound_driver_vsync(bus),
        0x1E => sound_channel_clear(bus),
        0x1F => midi_key_to_freq(regs, bus),
        0x20 => sound_player_open(regs, bus),
        0x21 => sound_player_start(regs, bus),
        0x22 => sound_player_stop(regs, bus),
        0x23 => sound_player_continue(regs, bus),
        0x24 => sound_player_fade(regs, bus),
        0x25 => multiboot(regs, bus),
        0x26 => hard_reset(regs, bus),
        0x27 => custom_halt(regs, bus),
        0x28 => sound_vsync(bus, false),
        0x29 => sound_vsync(bus, true),
        0x2A => sound_jump_list(regs, bus),
        _ => return None,
    })
}

fn bios_checksum(regs: &mut Registers, bus: &mut Bus) -> (u32, bool) {
    regs.set(0, 0xBAAE_187F);
    idle(bus)
}

fn idle(bus: &mut Bus) -> (u32, bool) {
    bus.note_internal(2);
    (3, false)
}

fn soft_reset(regs: &mut Registers, bus: &mut Bus) -> (u32, bool) {
    let flag = bus.read_u8(0x0300_7FFA);
    bus.clear_iwram_tail();
    let target = if flag == 0 { 0x0800_0000 } else { 0x0200_0000 };
    regs.prepare_reset(target);
    bus.set_stall(CpuStall::Run);
    bus.note_internal(8);
    (8, true)
}

fn hard_reset(regs: &mut Registers, bus: &mut Bus) -> (u32, bool) {
    bus.clear_ewram();
    bus.clear_iwram();
    bus.clear_palette();
    bus.clear_vram();
    bus.clear_oam();
    reset_io(bus, true, true);
    bus.reset_apu();
    bus.write_u16(0x0400_0000, 0x0080);
    regs.prepare_reset(0x0800_0000);
    bus.set_stall(CpuStall::Run);
    bus.note_internal(8);
    (8, true)
}

fn register_ram_reset(regs: &mut Registers, bus: &mut Bus) -> (u32, bool) {
    let flags = regs.get(0);
    if flags & 1 != 0 {
        bus.clear_ewram();
    }
    if flags & 2 != 0 {
        bus.clear_iwram_keep_stack();
    }
    if flags & 4 != 0 {
        bus.clear_palette();
    }
    if flags & 8 != 0 {
        bus.clear_vram();
    }
    if flags & 0x10 != 0 {
        bus.clear_oam();
    }
    // `SIODATA32` 的低 16 位总会被清掉，哪怕没选串口。
    bus.write_u16(0x0400_0120, 0);
    if flags & 0x20 != 0 {
        for off in 0x120..0x12C {
            bus.write_u8(0x0400_0000 + off, 0);
        }
    }
    if flags & 0x40 != 0 {
        bus.reset_apu();
    }
    if flags & 0x80 != 0 {
        reset_io(bus, false, false);
    }
    bus.write_u16(0x0400_0000, 0x0080);
    bus.note_internal(8);
    (8, false)
}

/// `sio` 和 `sound` 为假时跳过这两段。`IF` 用写 1 清 0。
fn reset_io(bus: &mut Bus, sio: bool, sound: bool) {
    let ranges: &[(u32, u32)] = if sound && sio {
        &[(0x000, 0x400)]
    } else if sound {
        &[(0x000, 0x120), (0x12C, 0x400)]
    } else if sio {
        &[(0x000, 0x060), (0x0B0, 0x400)]
    } else {
        &[(0x000, 0x060), (0x0B0, 0x120), (0x12C, 0x400)]
    };
    for (start, end) in ranges.iter().copied() {
        for off in start..end {
            if (0x60..0xA8).contains(&off) && !sound {
                continue;
            }
            if (0x120..0x12C).contains(&off) && !sio {
                continue;
            }
            let value = if off == 0x202 || off == 0x203 {
                0xFF
            } else {
                0
            };
            bus.write_u8(0x0400_0000 + off, value);
        }
    }
}

fn halt(bus: &mut Bus, kind: CpuStall) -> (u32, bool) {
    if matches!(kind, CpuStall::Stop) {
        bus.write_u8(0x0400_0301, 0x80);
    } else {
        bus.write_u8(0x0400_0301, 0);
    }
    bus.set_stall(kind);
    bus.note_internal(2);
    (3, false)
}

fn custom_halt(regs: &mut Registers, bus: &mut Bus) -> (u32, bool) {
    let value = regs.get(2) as u8;
    bus.write_u8(0x0400_0301, value);
    let kind = if value & 0x80 != 0 {
        CpuStall::Stop
    } else {
        CpuStall::Halt
    };
    bus.set_stall(kind);
    bus.note_internal(2);
    (3, false)
}

fn vblank_intr_wait(regs: &mut Registers, bus: &mut Bus) -> (u32, bool) {
    regs.set(0, 1);
    regs.set(1, 1);
    intr_wait(regs, bus, true)
}

fn intr_wait(regs: &mut Registers, bus: &mut Bus, vblank: bool) -> (u32, bool) {
    let discard = vblank || regs.get(0) != 0;
    let mask = if vblank { 1 } else { regs.get(1) as u16 };
    bus.write_u8(0x0400_0208, 1);
    if discard {
        bus.write_u16(0x0400_0202, mask);
        bus.clear_bios_flags(mask);
    }
    let (_, flags) = bus.irq_lines();
    let already = bus.bios_flags() & mask != 0 || (!discard && flags & mask != 0);
    if already {
        bus.clear_bios_flags(mask);
    } else {
        bus.set_stall(CpuStall::Intr(mask));
    }
    bus.note_internal(4);
    (4, false)
}

fn divide(regs: &mut Registers, bus: &mut Bus, swapped: bool) -> (u32, bool) {
    let (numer, denom) = if swapped {
        (regs.get(1) as i32, regs.get(0) as i32)
    } else {
        (regs.get(0) as i32, regs.get(1) as i32)
    };
    let (quot, rem) = if denom == 0 {
        (0, numer)
    } else {
        (numer.wrapping_div(denom), numer.wrapping_rem(denom))
    };
    regs.set(0, quot as u32);
    regs.set(1, rem as u32);
    regs.set(3, quot.unsigned_abs());
    bus.note_internal(if swapped { 4 } else { 2 });
    (if swapped { 6 } else { 3 }, false)
}

fn sqrt(regs: &mut Registers, bus: &mut Bus) -> (u32, bool) {
    regs.set(0, isqrt(regs.get(0)));
    bus.note_internal(8);
    (12, false)
}

fn isqrt(n: u32) -> u32 {
    let mut bit = 1u32 << 30;
    let mut rest = n;
    let mut result = 0u32;
    while bit > rest {
        bit >>= 2;
    }
    while bit != 0 {
        let trial = result.wrapping_add(bit);
        if rest >= trial {
            rest -= trial;
            result = (result >> 1).wrapping_add(bit);
        } else {
            result >>= 1;
        }
        bit >>= 2;
    }
    result
}

fn arctan(regs: &mut Registers, bus: &mut Bus) -> (u32, bool) {
    let angle = arctan_unit(regs.get(0) as i16);
    regs.set(0, i32::from(angle) as u32);
    bus.note_internal(8);
    (16, false)
}

fn arctan2(regs: &mut Registers, bus: &mut Bus) -> (u32, bool) {
    let angle = arctan2_unit(regs.get(0) as i16, regs.get(1) as i16);
    regs.set(0, u32::from(angle));
    bus.note_internal(12);
    (24, false)
}

/// 正切是 1 位整数、14 位小数。角度用有符号 16 位，`0x4000` 是 90 度。
fn arctan_unit(tan: i16) -> i16 {
    let tan = i32::from(tan);
    let square = -((tan * tan) >> 14);
    let mut factor = ((0x00A9 * square) >> 14) + 0x0390;
    factor = ((factor * square) >> 14) + 0x091C;
    factor = ((factor * square) >> 14) + 0x0FB6;
    factor = ((factor * square) >> 14) + 0x16AA;
    factor = ((factor * square) >> 14) + 0x2081;
    factor = ((factor * square) >> 14) + 0x3651;
    factor = ((factor * square) >> 14) + 0xA2F9;
    ((tan * factor) >> 16) as i16
}

/// `0x10000` 走完一圈。只看 X 或只看 Y 时落在坐标轴上。
fn arctan2_unit(x: i16, y: i16) -> u16 {
    if y == 0 {
        return if x >= 0 { 0 } else { 0x8000 };
    }
    if x == 0 {
        return if y >= 0 { 0x4000 } else { 0xC000 };
    }
    let x32 = i32::from(x);
    let y32 = i32::from(y);
    let angle = if x32.abs() > y32.abs() {
        let mut theta = i32::from(arctan_unit(((y32 << 14) / x32) as i16));
        if x < 0 {
            theta = theta.wrapping_add(0x8000);
        }
        theta
    } else {
        let mut theta = 0x4000 - i32::from(arctan_unit(((x32 << 14) / y32) as i16));
        if y < 0 {
            theta = theta.wrapping_add(0x8000);
        }
        theta
    };
    angle as u16
}

fn cpu_set(regs: &mut Registers, bus: &mut Bus, fast: bool) -> (u32, bool) {
    let src = regs.get(0);
    let mut dest = regs.get(1);
    let control = regs.get(2);
    let mut count = control & 0x001F_FFFF;
    let fill = control & (1 << 24) != 0;
    let wide = fast || control & (1 << 26) != 0;
    if fast {
        count = count.saturating_add(7) & !7;
    }
    let width: u32 = if wide { 4 } else { 2 };
    let bytes = count.saturating_mul(width);
    if bytes == 0 || hits_bios(src, bytes) {
        bus.note_internal(2);
        return (3, false);
    }
    let src = src & !(width - 1);
    dest &= !(width - 1);
    if wide {
        let fixed = bus.read_u32(src);
        let mut from = src;
        for _ in 0..count {
            let value = if fill {
                fixed
            } else {
                let word = bus.read_u32(from);
                from = from.wrapping_add(4);
                word
            };
            bus.write_u32(dest, value);
            dest = dest.wrapping_add(4);
        }
    } else {
        let fixed = bus.read_u16(src);
        let mut from = src;
        for _ in 0..count {
            let value = if fill {
                fixed
            } else {
                let half = bus.read_u16(from);
                from = from.wrapping_add(2);
                half
            };
            bus.write_u16(dest, value);
            dest = dest.wrapping_add(2);
        }
    }
    let cycles = 8 + count.saturating_mul(2);
    bus.note_internal(cycles.min(64));
    (cycles, false)
}

fn hits_bios(start: u32, bytes: u32) -> bool {
    if bytes == 0 {
        return false;
    }
    let end = start.wrapping_add(bytes - 1);
    end < start || start < BIOS_END
}

fn bg_affine_set(regs: &mut Registers, bus: &mut Bus) -> (u32, bool) {
    let mut src = regs.get(0);
    let mut dest = regs.get(1);
    let count = regs.get(2);
    for _ in 0..count {
        let tex_x = bus.read_u32(src) as i32;
        let tex_y = bus.read_u32(src.wrapping_add(4)) as i32;
        let scr_x = bus.read_u16(src.wrapping_add(8)) as i16 as i32;
        let scr_y = bus.read_u16(src.wrapping_add(10)) as i16 as i32;
        let scale_x = bus.read_u16(src.wrapping_add(12)) as i16 as i32;
        let scale_y = bus.read_u16(src.wrapping_add(14)) as i16 as i32;
        let angle = bus.read_u16(src.wrapping_add(16));
        let (pa, pb, pc, pd) = affine_matrix(scale_x, scale_y, angle);
        let start_x = tex_x - scr_x * i32::from(pa) - scr_y * i32::from(pb);
        let start_y = tex_y - scr_x * i32::from(pc) - scr_y * i32::from(pd);
        bus.write_u16(dest, pa as u16);
        bus.write_u16(dest.wrapping_add(2), pb as u16);
        bus.write_u16(dest.wrapping_add(4), pc as u16);
        bus.write_u16(dest.wrapping_add(6), pd as u16);
        bus.write_u32(dest.wrapping_add(8), start_x as u32);
        bus.write_u32(dest.wrapping_add(12), start_y as u32);
        src = src.wrapping_add(20);
        dest = dest.wrapping_add(16);
    }
    let cycles = 16 + count.saturating_mul(24);
    bus.note_internal(cycles.min(64));
    (cycles, false)
}

fn obj_affine_set(regs: &mut Registers, bus: &mut Bus) -> (u32, bool) {
    let mut src = regs.get(0);
    let dest = regs.get(1);
    let count = regs.get(2);
    let offset = regs.get(3);
    for index in 0..count {
        let scale_x = bus.read_u16(src) as i16 as i32;
        let scale_y = bus.read_u16(src.wrapping_add(2)) as i16 as i32;
        let angle = bus.read_u16(src.wrapping_add(4));
        let (pa, pb, pc, pd) = affine_matrix(scale_x, scale_y, angle);
        let base = dest.wrapping_add(index.wrapping_mul(offset));
        bus.write_u16(base, pa as u16);
        bus.write_u16(base.wrapping_add(offset), pb as u16);
        bus.write_u16(base.wrapping_add(offset.wrapping_mul(2)), pc as u16);
        bus.write_u16(base.wrapping_add(offset.wrapping_mul(3)), pd as u16);
        src = src.wrapping_add(6);
    }
    let cycles = 16 + count.saturating_mul(16);
    bus.note_internal(cycles.min(64));
    (cycles, false)
}

fn affine_matrix(scale_x: i32, scale_y: i32, angle: u16) -> (i16, i16, i16, i16) {
    let (sin, cos) = sin_cos(angle);
    let pa = ((scale_x * cos) >> 8) as i16;
    let pb = ((scale_y * -sin) >> 8) as i16;
    let pc = ((scale_x * sin) >> 8) as i16;
    let pd = ((scale_y * cos) >> 8) as i16;
    (pa, pb, pc, pd)
}

fn sin_cos(angle: u16) -> (i32, i32) {
    let index = (angle >> 8) as u8;
    (sine(index), sine(index.wrapping_add(64)))
}

fn sine(index: u8) -> i32 {
    let turns = f64::from(index) * std::f64::consts::PI / 128.0;
    (turns.sin() * 256.0).round() as i32
}

fn bit_unpack(regs: &mut Registers, bus: &mut Bus) -> (u32, bool) {
    let src = regs.get(0);
    let mut dest = regs.get(1) & !3;
    let info = regs.get(2);
    let length = usize::from(bus.read_u16(info));
    let src_width = usize::from(bus.read_u8(info.wrapping_add(2)));
    let dst_width = usize::from(bus.read_u8(info.wrapping_add(3)));
    let offset_word = bus.read_u32(info.wrapping_add(4));
    let offset = offset_word & 0x7FFF_FFFF;
    let add_zero = offset_word & 0x8000_0000 != 0;
    if !matches!(src_width, 1 | 2 | 4 | 8) || !matches!(dst_width, 1 | 2 | 4 | 8 | 16 | 32) {
        return idle(bus);
    }
    let mask = if dst_width >= 32 {
        u32::MAX
    } else {
        (1u32 << dst_width) - 1
    };
    let total_bits = length * 8;
    let mut bit = 0usize;
    let mut packed = 0u32;
    let mut packed_bits = 0;
    let mut units = 0u32;
    while bit + src_width <= total_bits {
        let mut value = read_bits(bus, src, bit, src_width);
        bit += src_width;
        if value != 0 || add_zero {
            value = value.wrapping_add(offset);
        }
        value &= mask;
        packed |= value << packed_bits;
        packed_bits += dst_width;
        units += 1;
        if packed_bits >= 32 {
            bus.write_u32(dest, packed);
            dest = dest.wrapping_add(4);
            packed = 0;
            packed_bits = 0;
        }
    }
    if packed_bits > 0 {
        bus.write_u32(dest, packed);
    }
    let cycles = 8 + units.saturating_mul(2);
    bus.note_internal(cycles.min(64));
    (cycles, false)
}

fn read_bits(bus: &Bus, base: u32, bit: usize, width: usize) -> u32 {
    let mut value = 0u32;
    for index in 0..width {
        let at = bit + index;
        let byte = bus.read_u8(base.wrapping_add((at / 8) as u32));
        let one = u32::from((byte >> (at % 8)) & 1);
        value |= one << index;
    }
    value
}

fn lz77(regs: &mut Registers, bus: &mut Bus, vram: bool) -> (u32, bool) {
    let src = regs.get(0);
    let dest = regs.get(1);
    let header = bus.read_u32(src);
    let size = (header >> 8) as usize;
    let mut out = vec![0u8; size];
    let mut from = src.wrapping_add(4);
    let mut filled = 0usize;
    while filled < size {
        let flags = bus.read_u8(from);
        from = from.wrapping_add(1);
        for bit in (0..8).rev() {
            if filled >= size {
                break;
            }
            if flags & (1 << bit) == 0 {
                out[filled] = bus.read_u8(from);
                from = from.wrapping_add(1);
                filled += 1;
                continue;
            }
            let high = bus.read_u8(from);
            let low = bus.read_u8(from.wrapping_add(1));
            from = from.wrapping_add(2);
            let length = usize::from(high >> 4) + 3;
            let disp = (usize::from(high & 0x0F) << 8) | usize::from(low);
            for _ in 0..length {
                if filled >= size {
                    break;
                }
                let previous = filled.wrapping_sub(disp + 1);
                out[filled] = if previous < filled { out[previous] } else { 0 };
                filled += 1;
            }
        }
    }
    store_units(bus, dest, &out, if vram { 2 } else { 1 });
    let cycles = 16 + (size as u32).saturating_mul(2);
    bus.note_internal(cycles.min(64));
    (cycles, false)
}

fn huffman(regs: &mut Registers, bus: &mut Bus) -> (u32, bool) {
    let src = regs.get(0);
    let dest = regs.get(1);
    let header = bus.read_u32(src);
    let data_bits = (header & 0xF) as usize;
    let size = (header >> 8) as usize;
    if data_bits == 0 || size == 0 {
        return idle(bus);
    }
    let tree_size = bus.read_u8(src.wrapping_add(4));
    let tree_bytes = (usize::from(tree_size) + 1) * 2;
    let root = src.wrapping_add(5);
    let data_at = src.wrapping_add(5 + tree_bytes as u32);
    let bitstream = (data_at.wrapping_add(3)) & !3;
    let mut out = vec![0u8; size];
    let mut out_bit = 0usize;
    let mut node = root;
    let mut word_index = 0u32;
    let mut word = bus.read_u32(bitstream);
    let mut bit_in_word = 31i32;
    while out_bit / 8 < size {
        if bit_in_word < 0 {
            word_index += 1;
            word = bus.read_u32(bitstream.wrapping_add(word_index * 4));
            bit_in_word = 31;
        }
        let bit = (word >> bit_in_word) & 1;
        bit_in_word -= 1;
        let node_byte = bus.read_u8(node);
        let offset = u32::from(node_byte & 0x3F);
        let data_node = if bit == 0 {
            node_byte & 0x80 != 0
        } else {
            node_byte & 0x40 != 0
        };
        let child0 = (node & !1).wrapping_add(offset * 2 + 2);
        let child = if bit == 0 { child0 } else { child0 + 1 };
        if data_node {
            let symbol = bus.read_u8(child);
            for index in 0..data_bits {
                if out_bit / 8 >= size {
                    break;
                }
                let one = (symbol >> index) & 1;
                out[out_bit / 8] |= one << (out_bit % 8);
                out_bit += 1;
            }
            node = root;
        } else {
            node = child;
        }
    }
    store_units(bus, dest, &out, 4);
    let cycles = 16 + (size as u32).saturating_mul(4);
    bus.note_internal(cycles.min(64));
    (cycles, false)
}

fn run_length(regs: &mut Registers, bus: &mut Bus, vram: bool) -> (u32, bool) {
    let src = regs.get(0);
    let dest = regs.get(1);
    let header = bus.read_u32(src);
    let size = (header >> 8) as usize;
    let mut out = vec![0u8; size];
    let mut from = src.wrapping_add(4);
    let mut filled = 0usize;
    while filled < size {
        let flag = bus.read_u8(from);
        from = from.wrapping_add(1);
        if flag & 0x80 == 0 {
            let length = usize::from(flag & 0x7F) + 1;
            for _ in 0..length {
                if filled >= size {
                    break;
                }
                out[filled] = bus.read_u8(from);
                from = from.wrapping_add(1);
                filled += 1;
            }
        } else {
            let length = usize::from(flag & 0x7F) + 3;
            let byte = bus.read_u8(from);
            from = from.wrapping_add(1);
            for _ in 0..length {
                if filled >= size {
                    break;
                }
                out[filled] = byte;
                filled += 1;
            }
        }
    }
    store_units(bus, dest, &out, if vram { 2 } else { 1 });
    let cycles = 16 + (size as u32).saturating_mul(2);
    bus.note_internal(cycles.min(64));
    (cycles, false)
}

fn diff_filter(regs: &mut Registers, bus: &mut Bus, unit: usize, vram: bool) -> (u32, bool) {
    let src = regs.get(0);
    let dest = regs.get(1);
    let header = bus.read_u32(src);
    let size = (header >> 8) as usize;
    let mut out = vec![0u8; size];
    let mut from = src.wrapping_add(4);
    if unit == 1 {
        let mut previous = 0u8;
        for slot in out.iter_mut() {
            previous = previous.wrapping_add(bus.read_u8(from));
            from = from.wrapping_add(1);
            *slot = previous;
        }
        store_units(bus, dest, &out, if vram { 2 } else { 1 });
    } else {
        let mut previous = 0u16;
        let mut index = 0;
        while index + 1 < size {
            previous = previous.wrapping_add(bus.read_u16(from));
            from = from.wrapping_add(2);
            out[index] = previous as u8;
            out[index + 1] = (previous >> 8) as u8;
            index += 2;
        }
        store_units(bus, dest, &out[..index], 2);
    }
    let cycles = 16 + (size as u32);
    bus.note_internal(cycles.min(64));
    (cycles, false)
}

fn store_units(bus: &mut Bus, mut dest: u32, data: &[u8], width: u32) {
    if width == 1 {
        for byte in data {
            bus.write_u8(dest, *byte);
            dest = dest.wrapping_add(1);
        }
        return;
    }
    if width == 2 {
        dest &= !1;
        for pair in data.chunks(2) {
            let high = pair.get(1).copied().unwrap_or(0);
            let half = u16::from(pair[0]) | (u16::from(high) << 8);
            bus.write_u16(dest, half);
            dest = dest.wrapping_add(2);
        }
        return;
    }
    dest &= !3;
    for quad in data.chunks(4) {
        let mut word = 0u32;
        for (index, byte) in quad.iter().enumerate() {
            word |= u32::from(*byte) << (8 * index);
        }
        bus.write_u32(dest, word);
        dest = dest.wrapping_add(4);
    }
}

fn sound_bias(regs: &mut Registers, bus: &mut Bus) -> (u32, bool) {
    let target: u16 = if regs.get(0) == 0 { 0 } else { 0x200 };
    loop {
        let raw = bus.sound_bias();
        let level = raw & 0x03FF;
        if level == target {
            break;
        }
        let next = if level < target { level + 1 } else { level - 1 };
        bus.set_sound_bias((raw & 0xC000) | next);
        bus.advance(16);
    }
    (1, false)
}

fn sound_init(regs: &mut Registers, bus: &mut Bus) -> (u32, bool) {
    crate::music::init(bus, regs.get(0));
    idle(bus)
}

fn sound_mode(regs: &mut Registers, bus: &mut Bus) -> (u32, bool) {
    crate::music::mode(bus, regs.get(0));
    idle(bus)
}

fn sound_main(bus: &mut Bus) -> (u32, bool) {
    crate::music::main(bus);
    idle(bus)
}

fn sound_driver_vsync(bus: &mut Bus) -> (u32, bool) {
    crate::music::vsync(bus);
    idle(bus)
}

fn sound_channel_clear(bus: &mut Bus) -> (u32, bool) {
    crate::music::clear_channels(bus);
    idle(bus)
}

fn sound_vsync(bus: &mut Bus, on: bool) -> (u32, bool) {
    if on {
        crate::music::vsync_on(bus);
    } else {
        crate::music::vsync_off(bus);
    }
    idle(bus)
}

fn sound_player_open(regs: &mut Registers, bus: &mut Bus) -> (u32, bool) {
    crate::music::player_open(bus, regs.get(0), regs.get(1), regs.get(2));
    idle(bus)
}

fn sound_player_start(regs: &mut Registers, bus: &mut Bus) -> (u32, bool) {
    crate::music::player_start(bus, regs.get(0), regs.get(1));
    idle(bus)
}

fn sound_player_stop(regs: &mut Registers, bus: &mut Bus) -> (u32, bool) {
    crate::music::player_stop(bus, regs.get(0));
    idle(bus)
}

fn sound_player_continue(regs: &mut Registers, bus: &mut Bus) -> (u32, bool) {
    crate::music::player_continue(bus, regs.get(0));
    idle(bus)
}

fn sound_player_fade(regs: &mut Registers, bus: &mut Bus) -> (u32, bool) {
    crate::music::player_fade(bus, regs.get(0), regs.get(1));
    idle(bus)
}

fn midi_key_to_freq(regs: &mut Registers, bus: &mut Bus) -> (u32, bool) {
    let wave = regs.get(0);
    let key = (regs.get(1) & 0xFF) as i32;
    let fine = (regs.get(2) & 0xFF) as i32;
    let freq = bus.read_u32(wave.wrapping_add(4));
    regs.set(0, crate::music::midi_freq(freq, key, fine));
    bus.note_internal(16);
    (20, false)
}

fn multiboot(regs: &mut Registers, bus: &mut Bus) -> (u32, bool) {
    regs.set(0, 1);
    idle(bus)
}

fn sound_jump_list(regs: &mut Registers, bus: &mut Bus) -> (u32, bool) {
    let dest = regs.get(0);
    let stub = bus.sound_return_stub();
    for index in 0..36u32 {
        bus.write_u32(dest.wrapping_add(index * 4), stub);
    }
    let cycles = 8 + 36 * 2;
    bus.note_internal(32);
    (cycles, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm;
    use crate::cpu::step;
    use crate::registers::Registers;

    fn run_swi(word: u32, setup: impl FnOnce(&mut Registers, &mut Bus)) -> (Registers, Bus) {
        let mut regs = Registers::arm_start(0x0300_0000);
        let mut bus = Bus::new();
        bus.write_u32(0x0300_0000, word);
        setup(&mut regs, &mut bus);
        step(&mut regs, &mut bus).unwrap();
        (regs, bus)
    }

    #[test]
    fn div_arm_swaps_the_operands() {
        let (regs, _) = run_swi(0xEF07_0000, |regs, _| {
            regs.set(0, 10);
            regs.set(1, -1234i32 as u32);
        });
        assert_eq!(regs.get(0) as i32, -123);
        assert_eq!(regs.get(1) as i32, -4);
        assert_eq!(regs.get(3), 123);
        assert_eq!(regs.pc(), 0x0300_0004);
        assert_eq!(regs.cpsr() & 0x1F, 0x10);
    }

    #[test]
    fn sqrt_rounds_down() {
        let (regs, _) = run_swi(0xEF08_0000, |regs, _| regs.set(0, 2));
        assert_eq!(regs.get(0), 1);
        let (regs, _) = run_swi(0xEF08_0000, |regs, _| regs.set(0, 4));
        assert_eq!(regs.get(0), 2);
        let (regs, _) = run_swi(0xEF08_0000, |regs, _| regs.set(0, 0));
        assert_eq!(regs.get(0), 0);
    }

    #[test]
    fn arctan_of_zero_and_one() {
        let (regs, _) = run_swi(0xEF09_0000, |regs, _| regs.set(0, 0));
        assert_eq!(regs.get(0), 0);
        let (regs, _) = run_swi(0xEF09_0000, |regs, _| regs.set(0, 0x4000));
        let angle = regs.get(0) as i16;
        assert!((i32::from(angle) - 0x2000).abs() < 0x80, "{angle:04X}");
    }

    #[test]
    fn arctan2_lands_on_the_axes() {
        let (regs, _) = run_swi(0xEF0A_0000, |regs, _| {
            regs.set(0, 0x100);
            regs.set(1, 0);
        });
        assert_eq!(regs.get(0), 0);
        let (regs, _) = run_swi(0xEF0A_0000, |regs, _| {
            regs.set(0, 0);
            regs.set(1, 0x100);
        });
        assert_eq!(regs.get(0), 0x4000);
        let (regs, _) = run_swi(0xEF0A_0000, |regs, _| {
            regs.set(0, -1i32 as u32);
            regs.set(1, 0);
        });
        assert_eq!(regs.get(0), 0x8000);
    }

    #[test]
    fn cpu_set_copies_and_fills() {
        let (regs, bus) = run_swi(0xEF0B_0000, |regs, bus| {
            bus.write_u32(0x0200_0000, 0x1122_3344);
            bus.write_u32(0x0200_0004, 0x5566_7788);
            regs.set(0, 0x0200_0000);
            regs.set(1, 0x0200_0010);
            regs.set(2, (1 << 26) | 2);
        });
        assert_eq!(bus.read_u32(0x0200_0010), 0x1122_3344);
        assert_eq!(bus.read_u32(0x0200_0014), 0x5566_7788);
        assert_eq!(regs.pc(), 0x0300_0004);

        let (_, bus) = run_swi(0xEF0C_0000, |regs, bus| {
            bus.write_u32(0x0200_0000, 0xAABB_CCDD);
            regs.set(0, 0x0200_0000);
            regs.set(1, 0x0200_0040);
            regs.set(2, (1 << 24) | 1);
        });
        for index in 0..8 {
            assert_eq!(bus.read_u32(0x0200_0040 + index * 4), 0xAABB_CCDD);
        }
    }

    #[test]
    fn cpu_set_ignores_a_bios_source() {
        let (_, bus) = run_swi(0xEF0B_0000, |regs, bus| {
            bus.write_u32(0x0200_0100, 0x1111_1111);
            regs.set(0, 0x0000_0000);
            regs.set(1, 0x0200_0100);
            regs.set(2, (1 << 26) | 1);
        });
        assert_eq!(bus.read_u32(0x0200_0100), 0x1111_1111);
    }

    #[test]
    fn lz77_and_run_length_expand() {
        let (_, bus) = run_swi(0xEF11_0000, |regs, bus| {
            bus.write_u32(0x0200_0000, (4 << 8) | 0x10);
            bus.write_u8(0x0200_0004, 0x40);
            bus.write_u8(0x0200_0005, b'A');
            bus.write_u16(0x0200_0006, 0x0000);
            regs.set(0, 0x0200_0000);
            regs.set(1, 0x0200_0020);
        });
        assert_eq!(&bus_bytes(&bus, 0x0200_0020, 4), b"AAAA");

        let (_, bus) = run_swi(0xEF14_0000, |regs, bus| {
            bus.write_u32(0x0200_0000, (4 << 8) | 0x30);
            bus.write_u8(0x0200_0004, 0x81);
            bus.write_u8(0x0200_0005, 0x11);
            regs.set(0, 0x0200_0000);
            regs.set(1, 0x0200_0030);
        });
        assert_eq!(bus_bytes(&bus, 0x0200_0030, 4), [0x11, 0x11, 0x11, 0x11]);
    }

    #[test]
    fn lz77_vram_writes_halfwords() {
        let (_, bus) = run_swi(0xEF12_0000, |regs, bus| {
            bus.write_u32(0x0200_0000, (4 << 8) | 0x10);
            bus.write_u8(0x0200_0004, 0x00);
            bus.write_u32(0x0200_0005, u32::from_le_bytes(*b"WXYZ"));
            regs.set(0, 0x0200_0000);
            regs.set(1, 0x0600_0000);
        });
        assert_eq!(bus.read_u32(0x0600_0000), u32::from_le_bytes(*b"WXYZ"));
    }

    #[test]
    fn huffman_decodes_a_single_symbol() {
        let (_, bus) = run_swi(0xEF13_0000, |regs, bus| {
            bus.write_u32(0x0200_0000, (4 << 8) | 0x28);
            bus.write_u8(0x0200_0004, 0);
            bus.write_u8(0x0200_0005, 0x80);
            bus.write_u8(0x0200_0006, 0xAB);
            bus.write_u32(0x0200_0008, 0);
            regs.set(0, 0x0200_0000);
            regs.set(1, 0x0200_0020);
        });
        assert_eq!(bus_bytes(&bus, 0x0200_0020, 4), [0xAB, 0xAB, 0xAB, 0xAB]);
    }

    #[test]
    fn diff_adds_each_unit_to_the_previous() {
        let (_, bus) = run_swi(0xEF16_0000, |regs, bus| {
            bus.write_u32(0x0200_0000, (4 << 8) | 0x81);
            bus.write_u8(0x0200_0004, 10);
            bus.write_u8(0x0200_0005, 1);
            bus.write_u8(0x0200_0006, 1);
            bus.write_u8(0x0200_0007, 1);
            regs.set(0, 0x0200_0000);
            regs.set(1, 0x0200_0010);
        });
        assert_eq!(bus_bytes(&bus, 0x0200_0010, 4), [10, 11, 12, 13]);
    }

    #[test]
    fn bit_unpack_expands_low_bits_first() {
        let (_, bus) = run_swi(0xEF10_0000, |regs, bus| {
            bus.write_u8(0x0200_0000, 0b0000_0101);
            bus.write_u16(0x0200_0010, 1);
            bus.write_u8(0x0200_0012, 1);
            bus.write_u8(0x0200_0013, 4);
            bus.write_u32(0x0200_0014, 0);
            regs.set(0, 0x0200_0000);
            regs.set(1, 0x0200_0020);
            regs.set(2, 0x0200_0010);
        });
        assert_eq!(bus.read_u32(0x0200_0020), 0x0000_0101);
    }

    #[test]
    fn affine_identity_and_object_parameters() {
        let (_, bus) = run_swi(0xEF0E_0000, |regs, bus| {
            bus.write_u32(0x0200_0000, 0);
            bus.write_u32(0x0200_0004, 0);
            bus.write_u16(0x0200_0008, 0);
            bus.write_u16(0x0200_000A, 0);
            bus.write_u16(0x0200_000C, 0x100);
            bus.write_u16(0x0200_000E, 0x100);
            bus.write_u16(0x0200_0010, 0);
            regs.set(0, 0x0200_0000);
            regs.set(1, 0x0200_0020);
            regs.set(2, 1);
        });
        assert_eq!(bus.read_u16(0x0200_0020) as i16, 0x100);
        assert_eq!(bus.read_u16(0x0200_0022) as i16, 0);
        assert_eq!(bus.read_u16(0x0200_0024) as i16, 0);
        assert_eq!(bus.read_u16(0x0200_0026) as i16, 0x100);
        assert_eq!(bus.read_u32(0x0200_0028), 0);
        assert_eq!(bus.read_u32(0x0200_002C), 0);

        let (_, bus) = run_swi(0xEF0F_0000, |regs, bus| {
            bus.write_u16(0x0200_0000, 0x100);
            bus.write_u16(0x0200_0002, 0x100);
            bus.write_u16(0x0200_0004, 0);
            regs.set(0, 0x0200_0000);
            regs.set(1, 0x0700_0006);
            regs.set(2, 1);
            regs.set(3, 8);
        });
        assert_eq!(bus.read_u16(0x0700_0006) as i16, 0x100);
        assert_eq!(bus.read_u16(0x0700_000E) as i16, 0);
        assert_eq!(bus.read_u16(0x0700_0016) as i16, 0);
        assert_eq!(bus.read_u16(0x0700_001E) as i16, 0x100);
    }

    #[test]
    fn halt_wakes_on_vblank_and_runs_the_handler() {
        let mut regs = Registers::arm_start(0x0800_0000);
        let mut bus = Bus::new();
        regs.set_cpsr(0x0000_001F);
        regs.set_boot_stacks();
        let handler = ack_vblank();
        for (index, word) in handler.iter().enumerate() {
            bus.write_u32(0x0300_0000 + index as u32 * 4, *word);
        }
        bus.write_u32(0x0300_7FFC, 0x0300_0000);
        bus.write_u16(0x0400_0200, 1);
        bus.write_u16(0x0400_0208, 1);
        bus.write_u16(0x0400_0004, 0x0008);
        let program = [
            0xEF02_0000,
            asm::dp_imm(0xE, 13, false, 0, 0, 1).unwrap(),
            asm::branch_address(0xE, false, 0x0800_0008, 0x0800_0008),
        ];
        bus.load_rom(&words(&program));
        let mut saw_handler = false;
        for _ in 0..500 {
            step(&mut regs, &mut bus).unwrap();
            if regs.pc() == 0x0300_0000 {
                saw_handler = true;
            }
            if regs.get(0) == 1 && regs.pc() == 0x0800_0008 {
                break;
            }
        }
        assert!(saw_handler);
        assert_eq!(regs.get(0), 1);
        assert_eq!(bus.read_u16(0x0400_0202) & 1, 0);
    }

    #[test]
    fn vblank_intr_wait_returns_after_the_new_blank() {
        let mut regs = Registers::arm_start(0x0800_0000);
        let mut bus = Bus::new();
        regs.set_cpsr(0x0000_001F);
        regs.set_boot_stacks();
        bus.write_u32(0x0300_7FFC, 0x0300_0000);
        for (index, word) in ack_vblank().iter().enumerate() {
            bus.write_u32(0x0300_0000 + index as u32 * 4, *word);
        }
        bus.write_u16(0x0400_0200, 1);
        bus.write_u16(0x0400_0004, 0x0008);
        bus.write_u16(0x0400_0202, 0xFFFF);
        bus.write_u16(0x0300_7FF8, 1);
        let program = [0xEF05_0000, asm::dp_imm(0xE, 13, false, 0, 0, 7).unwrap()];
        bus.load_rom(&words(&program));
        for _ in 0..800 {
            step(&mut regs, &mut bus).unwrap();
            if regs.get(0) == 7 {
                break;
            }
        }
        assert_eq!(regs.get(0), 7);
        assert_eq!(bus.bios_flags() & 1, 0);
        assert!(bus.ime());
    }

    #[test]
    fn intr_wait_returns_immediately_when_the_flag_is_already_set() {
        let (regs, bus) = run_swi(0xEF04_0000, |regs, bus| {
            regs.set(0, 0);
            regs.set(1, 1);
            bus.write_u16(0x0300_7FF8, 1);
        });
        assert_eq!(regs.pc(), 0x0300_0004);
        assert_eq!(bus.stall(), CpuStall::Run);
        assert_eq!(bus.bios_flags() & 1, 0);
    }

    #[test]
    fn soft_reset_clears_the_stack_page_and_jumps() {
        let (regs, bus) = run_swi(0xEF00_0000, |_, bus| {
            bus.write_u8(0x0300_7E10, 0xAA);
            bus.write_u8(0x0300_7FFA, 0);
        });
        assert_eq!(regs.pc(), 0x0800_0000);
        assert_eq!(regs.get(14), 0x0800_0000);
        assert_eq!(regs.cpsr() & 0xFF, 0xDF);
        assert_eq!(bus.read_u8(0x0300_7E10), 0);
        assert_eq!(regs.get_in(13, 0x10), 0x0300_7F00);

        let (regs, _) = run_swi(0xEF00_0000, |_, bus| {
            bus.write_u8(0x0300_7FFA, 1);
        });
        assert_eq!(regs.pc(), 0x0200_0000);
    }

    #[test]
    fn register_ram_reset_keeps_the_stack_and_blanks_the_screen() {
        let (_, bus) = run_swi(0xEF01_0000, |regs, bus| {
            bus.write_u8(0x0200_0010, 0x5A);
            bus.write_u8(0x0300_0010, 0x5A);
            bus.write_u8(0x0300_7E10, 0x5A);
            regs.set(0, 0x03);
        });
        assert_eq!(bus.read_u8(0x0200_0010), 0);
        assert_eq!(bus.read_u8(0x0300_0010), 0);
        assert_eq!(bus.read_u8(0x0300_7E10), 0x5A);
        assert_eq!(bus.read_u16(0x0400_0000) & 0x80, 0x80);
    }

    #[test]
    fn sound_bias_reaches_the_requested_level() {
        let (_, bus) = run_swi(0xEF19_0000, |regs, bus| {
            bus.set_sound_bias(0xC000);
            regs.set(0, 1);
        });
        assert_eq!(bus.sound_bias() & 0xC3FF, 0xC200);

        let (_, bus) = run_swi(0xEF19_0000, |regs, bus| {
            bus.set_sound_bias(0x0200);
            regs.set(0, 0);
        });
        assert_eq!(bus.sound_bias() & 0x03FF, 0);
    }

    #[test]
    fn midi_key_keeps_the_sample_frequency_at_middle_c() {
        let (regs, _) = run_swi(0xEF1F_0000, |regs, bus| {
            bus.write_u32(0x0200_0004, 44100);
            regs.set(0, 0x0200_0000);
            regs.set(1, 60);
            regs.set(2, 0);
        });
        assert_eq!(regs.get(0), 44100);
    }

    #[test]
    fn jump_list_points_at_the_return_stub() {
        let (_, bus) = run_swi(0xEF2A_0000, |regs, _| regs.set(0, 0x0200_0100));
        assert_eq!(bus.read_u32(0x0200_0100), 0x188);
        assert_eq!(bus.read_u32(0x0200_0100 + 35 * 4), 0x188);
        assert_eq!(bus.read_u32(0x188), 0xE12F_FF1E);
    }

    #[test]
    fn bios_song_mixes_a_note_and_the_transport_stops_it() {
        const AREA: u32 = 0x0200_0000;
        const WAVE: u32 = 0x0200_1000;
        const TONE: u32 = 0x0200_1100;
        const SCORE: u32 = 0x0200_1200;
        const SONG: u32 = 0x0200_1300;
        const PLAYER: u32 = 0x0200_1400;
        const TRACKS: u32 = 0x0200_1500;

        let mut regs = Registers::arm_start(0x0800_0000);
        let mut bus = Bus::new();
        regs.set(0, AREA);
        call(&mut regs, &mut bus, 0x1A).unwrap();
        assert_eq!(bus.read_u32(AREA), 0x6873_6D53);
        assert_eq!(bus.read_u32(0x0300_7FF0), AREA);
        assert_eq!(bus.sound_area(), AREA);
        assert!(bus.sound_vsync());
        assert_eq!(bus.read_u8(AREA + 0x06), 8);
        assert_eq!(bus.read_u8(AREA + 0x07), 15);
        assert_eq!(bus.read_u32(AREA + 0x10), 224);
        assert_eq!(bus.read_u16(0x0400_0084) & 0x80, 0x80);

        regs.set(0, 0x0002_3800);
        call(&mut regs, &mut bus, 0x1B).unwrap();
        assert_eq!(bus.sound_mode(), 0x0002_3800);
        assert_eq!(bus.read_u8(AREA + 0x06), 8);
        assert_eq!(bus.read_u8(AREA + 0x07), 3);
        assert_eq!(bus.read_u32(AREA + 0x10), 132);
        assert!(bus.sound_vsync());

        bus.write_u16(WAVE + 2, 0xC000);
        bus.write_u32(WAVE + 4, 13_379);
        bus.write_u32(WAVE + 8, 0);
        bus.write_u32(WAVE + 0x0C, 8);
        for index in 0..8u32 {
            bus.write_u8(WAVE + 0x10 + index, 0x7F);
        }
        bus.write_u8(TONE + 1, 60);
        bus.write_u32(TONE + 4, WAVE);
        bus.write_u8(TONE + 8, 255);
        bus.write_u8(TONE + 9, 255);
        bus.write_u8(TONE + 10, 255);
        bus.write_u8(TONE + 11, 255);
        let score = [
            0xBDu8, 0x00, 0xBE, 0x7F, 0xBF, 0x40, 0xCF, 60, 0x7F, 10, 0xCE, 0xB1,
        ];
        for (index, byte) in score.iter().enumerate() {
            bus.write_u8(SCORE + index as u32, *byte);
        }
        bus.write_u8(SONG, 1);
        bus.write_u32(SONG + 4, TONE);
        bus.write_u32(SONG + 8, SCORE);

        regs.set(0, PLAYER);
        regs.set(1, TRACKS);
        regs.set(2, 1);
        call(&mut regs, &mut bus, 0x20).unwrap();
        assert_eq!(bus.read_u32(PLAYER + 0x34), 0x6873_6D53);
        assert_eq!(bus.read_u32(PLAYER + 0x04), 0x8000);
        assert_eq!(bus.read_u32(PLAYER + 0x2C), TRACKS);
        assert_eq!(bus.read_u32(AREA + 0x40), PLAYER);
        assert_eq!(bus.read_u8(TRACKS), 0);

        regs.set(0, PLAYER);
        regs.set(1, SONG);
        call(&mut regs, &mut bus, 0x21).unwrap();
        assert_eq!(bus.read_u32(PLAYER), SONG);
        assert_eq!(bus.read_u8(TRACKS), 0xC0);
        assert_eq!(bus.read_u32(TRACKS + 0x40), SCORE);

        call(&mut regs, &mut bus, 0x1C).unwrap();
        let loud = (0..132u32).any(|index| {
            bus.read_u8(AREA + 0x350 + index) != 0 || bus.read_u8(AREA + 0x980 + index) != 0
        });
        assert!(loud, "音符应该写进左右两块混音缓冲");
        assert_ne!(bus.read_u8(AREA + 0x50) & 0x01, 0);

        let period = u32::from(bus.read_u8(AREA + 0x0B));
        for _ in 0..period {
            call(&mut regs, &mut bus, 0x1D).unwrap();
        }
        assert_eq!(bus.read_u16(0x0400_00C6), 0xB640);
        assert_eq!(bus.read_u16(0x0400_00D2), 0xB640);
        assert_eq!(bus.read_u16(0x0400_0102) & 0x80, 0x80);

        for _ in 0..16 {
            call(&mut regs, &mut bus, 0x1C).unwrap();
        }
        assert_eq!(bus.read_u8(TRACKS), 0);

        regs.set(0, PLAYER);
        regs.set(1, SONG);
        call(&mut regs, &mut bus, 0x21).unwrap();
        call(&mut regs, &mut bus, 0x1C).unwrap();
        regs.set(0, PLAYER);
        call(&mut regs, &mut bus, 0x1E).unwrap();
        assert_eq!(bus.read_u8(AREA + 0x50), 0);

        regs.set(0, PLAYER);
        regs.set(1, SONG);
        call(&mut regs, &mut bus, 0x21).unwrap();
        call(&mut regs, &mut bus, 0x1C).unwrap();
        regs.set(0, PLAYER);
        call(&mut regs, &mut bus, 0x22).unwrap();
        assert_eq!(bus.read_u32(PLAYER + 0x04) & 1, 1);
        assert_eq!(bus.read_u8(AREA + 0x50), 0);
        let cmd = bus.read_u32(TRACKS + 0x40);
        call(&mut regs, &mut bus, 0x1C).unwrap();
        assert_eq!(bus.read_u32(TRACKS + 0x40), cmd);
        call(&mut regs, &mut bus, 0x23).unwrap();
        assert_eq!(bus.read_u32(PLAYER + 0x04) & 1, 0);

        regs.set(1, 1);
        call(&mut regs, &mut bus, 0x24).unwrap();
        for _ in 0..16 {
            call(&mut regs, &mut bus, 0x1C).unwrap();
        }
        assert_eq!(bus.read_u32(PLAYER + 0x04) & 1, 1);
    }

    #[test]
    fn sound_driver_records_the_work_area_and_returns() {
        let (regs, bus) = run_swi(0xEF1A_0000, |regs, _| regs.set(0, 0x0200_2000));
        assert_eq!(bus.sound_area(), 0x0200_2000);
        assert!(bus.sound_vsync());
        assert_eq!(regs.pc(), 0x0300_0004);
        let (_, bus) = run_swi(0xEF1B_0000, |regs, _| regs.set(0, 0x0000_0B80));
        assert_eq!(bus.sound_mode(), 0xB80);
        let (_, bus) = run_swi(0xEF28_0000, |_, _| {});
        assert!(!bus.sound_vsync());
        let (_, bus) = run_swi(0xEF29_0000, |_, _| {});
        assert!(bus.sound_vsync());
    }

    #[test]
    fn bios_checksum_returns_without_entering_svc() {
        let (regs, _) = run_swi(0xEF0D_0000, |_, _| {});
        assert_eq!(regs.get(0), 0xBAAE_187F);
        assert_eq!(regs.pc(), 0x0300_0004);
        assert_eq!(regs.cpsr() & 0x1F, 0x10);
    }

    #[test]
    fn multiboot_fails_without_another_machine() {
        let (regs, _) = run_swi(0xEF25_0000, |_, _| {});
        assert_eq!(regs.get(0), 1);
        assert_eq!(regs.pc(), 0x0300_0004);
    }

    #[test]
    fn hard_reset_boots_the_cartridge() {
        let (regs, bus) = run_swi(0xEF26_0000, |_, bus| {
            bus.write_u8(0x0200_0000, 1);
            bus.write_u8(0x0300_0004, 1);
        });
        assert_eq!(regs.pc(), 0x0800_0000);
        assert_eq!(bus.read_u8(0x0200_0000), 0);
        assert_eq!(bus.read_u8(0x0300_0004), 0);
        assert_eq!(bus.read_u16(0x0400_0000) & 0x80, 0x80);
    }

    fn ack_vblank() -> [u32; 5] {
        [
            asm::dp_imm(0xE, 13, false, 0, 12, 0x0400_0000).unwrap(),
            asm::dp_imm(0xE, 4, false, 12, 12, 0x200).unwrap(),
            asm::dp_imm(0xE, 13, false, 0, 1, 1).unwrap(),
            asm::half_imm(0xE, false, true, true, false, 12, 1, 2),
            0xE12F_FF1E,
        ]
    }

    fn bus_bytes(bus: &Bus, addr: u32, len: usize) -> Vec<u8> {
        (0..len)
            .map(|index| bus.read_u8(addr + index as u32))
            .collect()
    }

    fn words(program: &[u32]) -> Vec<u8> {
        program.iter().flat_map(|word| word.to_le_bytes()).collect()
    }
}
