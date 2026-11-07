//! Thumb 是同一组寄存器上的 16 位指令。
//!
//! T=1 时 `step` 走这里。一条指令 2 字节，做完 PC 加 2。
//! 读 `r15` 得到的是这条指令的地址加 4。
//! `add rd, pc, #imm` 用的 PC 会先按 4 字节对齐。
//! `bl` 拆成两条：`blhi` 把高位加进 LR，`bllo` 再跳走，并把返回地址的最低位置 1。
//! 未定义编码进入向量 `0x04`，和 ARM 一样。

use crate::bus::Bus;
use crate::cpu::{add, shift, shift_reg, software_interrupt, sub};
use crate::disasm::{reg_name, Disassembly};
use crate::registers::Registers;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    ShiftImm {
        kind: u8,
        amount: u8,
        rm: u8,
        rd: u8,
    },
    AddSub {
        sub: bool,
        imm: bool,
        rhs: u8,
        rn: u8,
        rd: u8,
    },
    Imm {
        op: u8,
        rd: u8,
        imm: u8,
    },
    Alu {
        op: u8,
        rm: u8,
        rd: u8,
    },
    Hi {
        op: u8,
        rd: u8,
        rm: u8,
    },
    Bx {
        rm: u8,
    },
    LdrPc {
        rd: u8,
        imm: u8,
    },
    MemReg {
        op: u8,
        rm: u8,
        rn: u8,
        rd: u8,
    },
    MemImm {
        load: bool,
        size: u8,
        imm: u8,
        rn: u8,
        rd: u8,
    },
    MemSp {
        load: bool,
        rd: u8,
        imm: u8,
    },
    Addr {
        sp: bool,
        rd: u8,
        imm: u8,
    },
    AdjustSp {
        sub: bool,
        imm: u8,
    },
    Push {
        pop: bool,
        extra: bool,
        list: u8,
    },
    Block {
        load: bool,
        rn: u8,
        list: u8,
    },
    Cond {
        cond: u8,
        imm: u8,
    },
    Branch {
        imm: u16,
    },
    BlHi {
        imm: u16,
    },
    BlLo {
        imm: u16,
    },
    Swi {
        imm: u8,
    },
    Undefined,
}

pub fn disassemble_thumb(address: u32, half: u16) -> Disassembly {
    Disassembly {
        address,
        word: u32::from(half),
        text: format_at(address, half),
    }
}

pub(crate) fn format_at(address: u32, half: u16) -> String {
    format_op(address, &decode(half))
}

pub(crate) fn decode(half: u16) -> Op {
    match half >> 13 {
        0b000 if half & 0x1800 == 0x1800 => Op::AddSub {
            sub: half & (1 << 9) != 0,
            imm: half & (1 << 10) != 0,
            rhs: field(half, 6, 3),
            rn: field(half, 3, 3),
            rd: field(half, 0, 3),
        },
        0b000 => Op::ShiftImm {
            kind: field(half, 11, 2),
            amount: field(half, 6, 5),
            rm: field(half, 3, 3),
            rd: field(half, 0, 3),
        },
        0b001 => Op::Imm {
            op: field(half, 11, 2),
            rd: field(half, 8, 3),
            imm: (half & 0xFF) as u8,
        },
        0b010 if half & 0xFC00 == 0x4000 => Op::Alu {
            op: field(half, 6, 4),
            rm: field(half, 3, 3),
            rd: field(half, 0, 3),
        },
        0b010 if half & 0xFC00 == 0x4400 => {
            let op = field(half, 8, 2);
            let rm = field(half, 3, 3) | (field(half, 6, 1) << 3);
            let rd = field(half, 0, 3) | (field(half, 7, 1) << 3);
            if op == 3 {
                if half & (1 << 7) != 0 {
                    Op::Undefined
                } else {
                    Op::Bx { rm }
                }
            } else {
                Op::Hi { op, rd, rm }
            }
        }
        0b010 if half & 0xF800 == 0x4800 => Op::LdrPc {
            rd: field(half, 8, 3),
            imm: (half & 0xFF) as u8,
        },
        0b010 => Op::MemReg {
            op: field(half, 9, 3),
            rm: field(half, 6, 3),
            rn: field(half, 3, 3),
            rd: field(half, 0, 3),
        },
        0b011 => Op::MemImm {
            load: half & (1 << 11) != 0,
            size: if half & (1 << 12) != 0 { 1 } else { 4 },
            imm: field(half, 6, 5),
            rn: field(half, 3, 3),
            rd: field(half, 0, 3),
        },
        0b100 if half & 0xF000 == 0x8000 => Op::MemImm {
            load: half & (1 << 11) != 0,
            size: 2,
            imm: field(half, 6, 5),
            rn: field(half, 3, 3),
            rd: field(half, 0, 3),
        },
        0b100 => Op::MemSp {
            load: half & (1 << 11) != 0,
            rd: field(half, 8, 3),
            imm: (half & 0xFF) as u8,
        },
        0b101 if half & 0xF000 == 0xA000 => Op::Addr {
            sp: half & (1 << 11) != 0,
            rd: field(half, 8, 3),
            imm: (half & 0xFF) as u8,
        },
        0b101 if half & 0xFF00 == 0xB000 => Op::AdjustSp {
            sub: half & 0x80 != 0,
            imm: (half & 0x7F) as u8,
        },
        0b101 if half & 0xF600 == 0xB400 => Op::Push {
            pop: half & (1 << 11) != 0,
            extra: half & 0x100 != 0,
            list: (half & 0xFF) as u8,
        },
        0b101 => Op::Undefined,
        0b110 if half & 0xF000 == 0xC000 => Op::Block {
            load: half & (1 << 11) != 0,
            rn: field(half, 8, 3),
            list: (half & 0xFF) as u8,
        },
        0b110 => {
            let cond = field(half, 8, 4);
            if cond == 15 {
                Op::Swi {
                    imm: (half & 0xFF) as u8,
                }
            } else if cond == 14 {
                Op::Undefined
            } else {
                Op::Cond {
                    cond,
                    imm: (half & 0xFF) as u8,
                }
            }
        }
        _ => match field(half, 11, 2) {
            0 => Op::Branch { imm: half & 0x7FF },
            1 => Op::Undefined,
            2 => Op::BlHi { imm: half & 0x7FF },
            _ => Op::BlLo { imm: half & 0x7FF },
        },
    }
}

pub(crate) fn execute(
    regs: &mut Registers,
    bus: &mut Bus,
    address: u32,
    half: u16,
    op: &Op,
) -> Result<(u32, bool), String> {
    match *op {
        Op::ShiftImm {
            kind,
            amount,
            rm,
            rd,
        } => {
            let (result, carry) = shift(regs.get(rm), kind, amount, regs.flag_c());
            set_nzc(regs, result, carry);
            regs.set(rd, result);
            Ok((1, false))
        }
        Op::AddSub {
            sub,
            imm,
            rhs,
            rn,
            rd,
        } => {
            let right = if imm { u32::from(rhs) } else { regs.get(rhs) };
            let (result, carry, overflow) = arith(sub, regs.get(rn), right, false);
            set_nzcv(regs, result, carry, overflow);
            regs.set(rd, result);
            Ok((1, false))
        }
        Op::Imm { op, rd, imm } => {
            let imm = u32::from(imm);
            match op {
                0 => {
                    set_nz(regs, imm);
                    regs.set(rd, imm);
                }
                1 => {
                    let (result, carry, overflow) = sub(regs.get(rd), imm, true);
                    set_nzcv(regs, result, carry, overflow);
                }
                2 => {
                    let (result, carry, overflow) = add(regs.get(rd), imm, false);
                    set_nzcv(regs, result, carry, overflow);
                    regs.set(rd, result);
                }
                _ => {
                    let (result, carry, overflow) = sub(regs.get(rd), imm, true);
                    set_nzcv(regs, result, carry, overflow);
                    regs.set(rd, result);
                }
            }
            Ok((1, false))
        }
        Op::Alu { op, rm, rd } => alu(regs, bus, op, rm, rd),
        Op::Hi { op, rd, rm } => {
            let left = read_pc(regs, address, rd);
            let right = read_pc(regs, address, rm);
            match op {
                0 => Ok((1, write_pc(regs, rd, left.wrapping_add(right)))),
                1 => {
                    let (result, carry, overflow) = sub(left, right, true);
                    set_nzcv(regs, result, carry, overflow);
                    Ok((1, false))
                }
                _ => Ok((1, write_pc(regs, rd, right))),
            }
        }
        Op::Bx { rm } => {
            let target = read_pc(regs, address, rm);
            let (land, width) = if target & 1 != 0 {
                regs.set_thumb(true);
                (target & !1, 2)
            } else {
                regs.set_thumb(false);
                (target & !3, 4)
            };
            regs.set_pc(land);
            Ok((3 + bus.refill_stall(land, width), true))
        }
        Op::LdrPc { rd, imm } => {
            let base = address.wrapping_add(4) & !3;
            let addr = base.wrapping_add(u32::from(imm) << 2);
            let stall = bus.data_stall(addr, 4);
            bus.note_internal(1);
            regs.set(rd, load_word(bus, addr));
            Ok((3 + stall, false))
        }
        Op::MemReg { op, rm, rn, rd } => {
            let addr = regs.get(rn).wrapping_add(regs.get(rm));
            access(regs, bus, op, rd, addr)
        }
        Op::MemImm {
            load,
            size,
            imm,
            rn,
            rd,
        } => {
            let addr = regs.get(rn).wrapping_add(u32::from(imm) * u32::from(size));
            let op = match (load, size) {
                (false, 4) => 0,
                (false, 2) => 1,
                (false, _) => 2,
                (true, 4) => 4,
                (true, 2) => 5,
                _ => 6,
            };
            access(regs, bus, op, rd, addr)
        }
        Op::MemSp { load, rd, imm } => {
            let addr = regs.get(13).wrapping_add(u32::from(imm) << 2);
            access(regs, bus, if load { 4 } else { 0 }, rd, addr)
        }
        Op::Addr { sp, rd, imm } => {
            let base = if sp {
                regs.get(13)
            } else {
                address.wrapping_add(4) & !3
            };
            regs.set(rd, base.wrapping_add(u32::from(imm) << 2));
            Ok((1, false))
        }
        Op::AdjustSp { sub, imm } => {
            let off = u32::from(imm) << 2;
            let sp = regs.get(13);
            regs.set(
                13,
                if sub {
                    sp.wrapping_sub(off)
                } else {
                    sp.wrapping_add(off)
                },
            );
            Ok((1, false))
        }
        Op::Push { pop, extra, list } => stack(regs, bus, pop, extra, list),
        Op::Block { load, rn, list } => block(regs, bus, address, load, rn, list),
        Op::Cond { cond, imm } => {
            if !meets(regs, cond) {
                return Ok((1, false));
            }
            let target = branch_target(address, u32::from(imm), 8);
            regs.set_pc(target);
            Ok((3 + bus.refill_stall(target, 2), true))
        }
        Op::Branch { imm } => {
            let target = branch_target(address, u32::from(imm), 11);
            regs.set_pc(target);
            Ok((3 + bus.refill_stall(target, 2), true))
        }
        Op::BlHi { imm } => {
            let addend = (sext(u32::from(imm), 11) as u32) << 12;
            regs.set(14, address.wrapping_add(4).wrapping_add(addend));
            Ok((1, false))
        }
        Op::BlLo { imm } => {
            let target = regs.get(14).wrapping_add(u32::from(imm) << 1) & !1;
            regs.set(14, address.wrapping_add(2) | 1);
            regs.set_pc(target);
            Ok((3 + bus.refill_stall(target, 2), true))
        }
        Op::Swi { imm } => Ok(software_interrupt(regs, bus, imm, address.wrapping_add(2))),
        Op::Undefined => Ok(crate::cpu::undefined_instruction(
            regs,
            bus,
            address,
            u32::from(half),
            true,
        )),
    }
}

fn alu(regs: &mut Registers, bus: &mut Bus, op: u8, rm: u8, rd: u8) -> Result<(u32, bool), String> {
    let left = regs.get(rd);
    let right = regs.get(rm);
    let mut cycles = 1;
    match op {
        0 => {
            let result = left & right;
            set_nz(regs, result);
            regs.set(rd, result);
        }
        1 => {
            let result = left ^ right;
            set_nz(regs, result);
            regs.set(rd, result);
        }
        2 | 3 | 4 | 7 => {
            let kind = if op == 7 { 3 } else { op - 2 };
            let (result, carry) = shift_reg(left, kind, (right & 0xFF) as u8, regs.flag_c());
            set_nzc(regs, result, carry);
            regs.set(rd, result);
            bus.note_internal(1);
            cycles = 2;
        }
        5 => {
            let (result, carry, overflow) = add(left, right, regs.flag_c());
            set_nzcv(regs, result, carry, overflow);
            regs.set(rd, result);
        }
        6 => {
            let (result, carry, overflow) = sub(left, right, regs.flag_c());
            set_nzcv(regs, result, carry, overflow);
            regs.set(rd, result);
        }
        8 => set_nz(regs, left & right),
        9 => {
            let (result, carry, overflow) = sub(0, right, true);
            set_nzcv(regs, result, carry, overflow);
            regs.set(rd, result);
        }
        10 => {
            let (result, carry, overflow) = sub(left, right, true);
            set_nzcv(regs, result, carry, overflow);
        }
        11 => {
            let (result, carry, overflow) = add(left, right, false);
            set_nzcv(regs, result, carry, overflow);
        }
        12 => {
            let result = left | right;
            set_nz(regs, result);
            regs.set(rd, result);
        }
        13 => {
            let result = left.wrapping_mul(right);
            set_nz(regs, result);
            regs.set(rd, result);
            bus.note_internal(1);
            cycles = 2;
        }
        14 => {
            let result = left & !right;
            set_nz(regs, result);
            regs.set(rd, result);
        }
        _ => {
            let result = !right;
            set_nz(regs, result);
            regs.set(rd, result);
        }
    }
    Ok((cycles, false))
}

fn access(
    regs: &mut Registers,
    bus: &mut Bus,
    op: u8,
    rd: u8,
    addr: u32,
) -> Result<(u32, bool), String> {
    let load = matches!(op, 3..=7);
    let width = match op {
        0 | 4 => 4,
        1 | 5 | 7 => 2,
        _ => 1,
    };
    let stall = bus.data_stall(addr, width);
    if load {
        bus.note_internal(1);
    }
    match op {
        0 => bus.write_u32(addr & !3, regs.get(rd)),
        1 => bus.write_u16(addr & !1, regs.get(rd) as u16),
        2 => bus.write_u8(addr, regs.get(rd) as u8),
        3 => regs.set(rd, bus.read_u8(addr) as i8 as i32 as u32),
        4 => regs.set(rd, load_word(bus, addr)),
        5 => regs.set(rd, bus.read_half(addr, false)),
        6 => regs.set(rd, u32::from(bus.read_u8(addr))),
        _ => regs.set(rd, bus.read_half(addr, true)),
    }
    Ok((if load { 3 } else { 2 } + stall, false))
}

fn stack(
    regs: &mut Registers,
    bus: &mut Bus,
    pop: bool,
    extra: bool,
    list: u8,
) -> Result<(u32, bool), String> {
    let mut mask = u16::from(list);
    if extra {
        mask |= 1 << if pop { 15 } else { 14 };
    }
    let count = mask.count_ones();
    if count == 0 {
        return Ok((1, false));
    }
    let sp = regs.get(13);
    let start = if pop { sp } else { sp.wrapping_sub(count * 4) };
    let stall = bus.burst_stall(start, count, 4);
    let mut branched = false;
    if pop {
        let mut cursor = sp;
        for index in 0..16 {
            if mask & (1 << index) == 0 {
                continue;
            }
            let value = bus.read_u32(cursor & !3);
            if write_pc(regs, index, value) {
                branched = true;
            }
            cursor = cursor.wrapping_add(4);
        }
        regs.set(13, sp.wrapping_add(count * 4));
        bus.note_internal(1);
        Ok((count + 2 + stall, branched))
    } else {
        let mut cursor = sp.wrapping_sub(count * 4);
        for index in 0..16 {
            if mask & (1 << index) == 0 {
                continue;
            }
            bus.write_u32(cursor & !3, regs.get(index));
            cursor = cursor.wrapping_add(4);
        }
        regs.set(13, sp.wrapping_sub(count * 4));
        Ok((count + 1 + stall, false))
    }
}

fn block(
    regs: &mut Registers,
    bus: &mut Bus,
    address: u32,
    load: bool,
    rn: u8,
    list: u8,
) -> Result<(u32, bool), String> {
    let count = list.count_ones();
    let base = regs.get(rn);
    if count == 0 {
        // 空列表按 16 个寄存器算。存出去的是这条指令的地址加 6，读则把那个字装进 PC。
        let stall = bus.burst_stall(base & !3, 16, 4);
        if load {
            let value = bus.read_u32(base & !3);
            regs.set(rn, base.wrapping_add(0x40));
            regs.set_pc(value & !1);
            bus.note_internal(1);
            return Ok((18 + stall, true));
        }
        bus.write_u32(base & !3, address.wrapping_add(6));
        regs.set(rn, base.wrapping_add(0x40));
        return Ok((17 + stall, false));
    }
    let written = base.wrapping_add(count * 4);
    let stall = bus.burst_stall(base & !3, count, 4);
    let mut cursor = base;
    if load {
        for index in 0..8 {
            if list & (1 << index) == 0 {
                continue;
            }
            regs.set(index, bus.read_u32(cursor & !3));
            cursor = cursor.wrapping_add(4);
        }
        if list & (1 << rn) == 0 {
            regs.set(rn, written);
        }
        bus.note_internal(1);
        Ok((count + 2 + stall, false))
    } else {
        let mut seen = false;
        for index in 0..8 {
            if list & (1 << index) == 0 {
                continue;
            }
            // 基址在列表里又不是第一个时，存回写之后的值。
            let value = if index == rn && seen {
                written
            } else {
                regs.get(index)
            };
            bus.write_u32(cursor & !3, value);
            cursor = cursor.wrapping_add(4);
            seen = true;
        }
        regs.set(rn, written);
        Ok((count + 1 + stall, false))
    }
}

fn format_op(address: u32, op: &Op) -> String {
    match *op {
        Op::ShiftImm {
            kind,
            amount,
            rm,
            rd,
        } => {
            let name = ["lsls", "lsrs", "asrs"][kind as usize];
            let shown = if kind != 0 && amount == 0 {
                32
            } else {
                u32::from(amount)
            };
            format!(
                "{name} {}, {}, #{}",
                reg_name(rd),
                reg_name(rm),
                imm_text(shown)
            )
        }
        Op::AddSub {
            sub,
            imm,
            rhs,
            rn,
            rd,
        } => {
            let name = if sub { "subs" } else { "adds" };
            let right = if imm {
                format!("#{}", imm_text(u32::from(rhs)))
            } else {
                reg_name(rhs).to_string()
            };
            format!("{name} {}, {}, {right}", reg_name(rd), reg_name(rn))
        }
        Op::Imm { op, rd, imm } => {
            let name = ["movs", "cmp", "adds", "subs"][op as usize];
            format!("{name} {}, #{}", reg_name(rd), imm_text(u32::from(imm)))
        }
        Op::Alu { op, rm, rd } => {
            let name = [
                "ands", "eors", "lsls", "lsrs", "asrs", "adcs", "sbcs", "rors", "tst", "negs",
                "cmp", "cmn", "orrs", "muls", "bics", "mvns",
            ][op as usize];
            format!("{name} {}, {}", reg_name(rd), reg_name(rm))
        }
        Op::Hi { op, rd, rm } => {
            let name = ["add", "cmp", "mov"][op as usize];
            format!("{name} {}, {}", reg_name(rd), reg_name(rm))
        }
        Op::Bx { rm } => format!("bx {}", reg_name(rm)),
        Op::LdrPc { rd, imm } => mem_text("ldr", rd, 15, u32::from(imm) << 2),
        Op::MemReg { op, rm, rn, rd } => {
            let name = [
                "str", "strh", "strb", "ldrsb", "ldr", "ldrh", "ldrb", "ldrsh",
            ][op as usize];
            format!(
                "{name} {}, [{}, {}]",
                reg_name(rd),
                reg_name(rn),
                reg_name(rm)
            )
        }
        Op::MemImm {
            load,
            size,
            imm,
            rn,
            rd,
        } => {
            let name = match (load, size) {
                (false, 4) => "str",
                (false, 2) => "strh",
                (false, _) => "strb",
                (true, 4) => "ldr",
                (true, 2) => "ldrh",
                _ => "ldrb",
            };
            mem_text(name, rd, rn, u32::from(imm) * u32::from(size))
        }
        Op::MemSp { load, rd, imm } => mem_text(
            if load { "ldr" } else { "str" },
            rd,
            13,
            u32::from(imm) << 2,
        ),
        Op::Addr { sp, rd, imm } => format!(
            "add {}, {}, #{}",
            reg_name(rd),
            if sp { "sp" } else { "pc" },
            imm_text(u32::from(imm) << 2)
        ),
        Op::AdjustSp { sub, imm } => {
            let name = if sub { "sub" } else { "add" };
            format!("{name} sp, #{}", imm_text(u32::from(imm) << 2))
        }
        Op::Push { pop, extra, list } => {
            let name = if pop { "pop" } else { "push" };
            let high = if extra {
                Some(if pop { 15 } else { 14 })
            } else {
                None
            };
            format!("{name} {{{}}}", format_list(list, high))
        }
        Op::Block { load, rn, list } => {
            let name = if load { "ldmia" } else { "stmia" };
            format!("{name} {}!, {{{}}}", reg_name(rn), format_list(list, None))
        }
        Op::Cond { cond, imm } => format!(
            "b{} 0x{:08X}",
            COND[cond as usize],
            branch_target(address, u32::from(imm), 8)
        ),
        Op::Branch { imm } => format!("b 0x{:08X}", branch_target(address, u32::from(imm), 11)),
        Op::BlHi { imm } => {
            let addend = sext(u32::from(imm), 11) << 12;
            format!("blhi #{}", imm_signed(addend))
        }
        Op::BlLo { imm } => format!("bllo #{}", imm_text(u32::from(imm) << 1)),
        Op::Swi { imm } => format!("swi #{}", imm_text(u32::from(imm))),
        Op::Undefined => "udf".to_string(),
    }
}

fn mem_text(name: &str, rd: u8, rn: u8, offset: u32) -> String {
    let base = reg_name(rn);
    let addr = if offset == 0 {
        format!("[{base}]")
    } else {
        format!("[{base}, #{}]", imm_text(offset))
    };
    format!("{name} {}, {addr}", reg_name(rd))
}

fn format_list(list: u8, extra: Option<u8>) -> String {
    let mut names = Vec::new();
    for index in 0..8 {
        if list & (1 << index) != 0 {
            names.push(reg_name(index));
        }
    }
    if let Some(index) = extra {
        names.push(reg_name(index));
    }
    names.join(", ")
}

fn load_word(bus: &Bus, addr: u32) -> u32 {
    bus.read_u32(addr & !3).rotate_right((addr & 3) * 8)
}

fn read_pc(regs: &Registers, address: u32, index: u8) -> u32 {
    if index == 15 {
        address.wrapping_add(4)
    } else {
        regs.get(index)
    }
}

fn write_pc(regs: &mut Registers, index: u8, value: u32) -> bool {
    if index == 15 {
        regs.set_pc(value & !1);
        true
    } else {
        regs.set(index, value);
        false
    }
}

fn arith(is_sub: bool, left: u32, right: u32, carry: bool) -> (u32, bool, bool) {
    if is_sub {
        sub(left, right, !carry)
    } else {
        add(left, right, carry)
    }
}

fn branch_target(address: u32, imm: u32, bits: u32) -> u32 {
    let off = sext(imm, bits) as u32;
    address.wrapping_add(4).wrapping_add(off << 1)
}

fn sext(value: u32, bits: u32) -> i32 {
    let shift = 32 - bits;
    ((value << shift) as i32) >> shift
}

fn set_nz(regs: &mut Registers, result: u32) {
    regs.set_nzcv(
        result & 0x8000_0000 != 0,
        result == 0,
        regs.flag_c(),
        regs.flag_v(),
    );
}

fn set_nzc(regs: &mut Registers, result: u32, carry: bool) {
    regs.set_nzc(result & 0x8000_0000 != 0, result == 0, carry);
}

fn set_nzcv(regs: &mut Registers, result: u32, carry: bool, overflow: bool) {
    regs.set_nzcv(result & 0x8000_0000 != 0, result == 0, carry, overflow);
}

fn meets(regs: &Registers, cond: u8) -> bool {
    let n = regs.flag_n();
    let z = regs.flag_z();
    let c = regs.flag_c();
    let v = regs.flag_v();
    match cond {
        0 => z,
        1 => !z,
        2 => c,
        3 => !c,
        4 => n,
        5 => !n,
        6 => v,
        7 => !v,
        8 => c && !z,
        9 => !c || z,
        10 => n == v,
        11 => n != v,
        12 => !z && n == v,
        13 => z || n != v,
        14 => true,
        _ => false,
    }
}

fn field(half: u16, shift: u32, width: u32) -> u8 {
    ((u32::from(half) >> shift) & ((1u32 << width) - 1)) as u8
}

const COND: [&str; 16] = [
    "eq", "ne", "hs", "lo", "mi", "pl", "vs", "vc", "hi", "ls", "ge", "lt", "gt", "le", "", "",
];

fn imm_text(value: u32) -> String {
    if value <= 9 {
        format!("{value}")
    } else {
        format!("0x{value:X}")
    }
}

fn imm_signed(value: i32) -> String {
    if value < 0 {
        format!("-{}", imm_text((-value) as u32))
    } else {
        imm_text(value as u32)
    }
}
