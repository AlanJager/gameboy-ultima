//! ARM7TDMI 走一步。
//!
//! T=0 时从 PC 取出 4 个字节，条件不成立就只把 PC 加 4。
//! 读 `r15` 得到的是「这条指令的地址 + 8」。`str` 把 PC 存出去时是地址 + 12。
//! 普通指令结束时 PC 加 4。写进 PC 的结果就是下一条要执行的地址，不再额外加 4。
//! T=1 时改走 Thumb：一次取 2 个字节。
//! 这一步成功时，返回的周期数会加进总线的累计时钟。停住的指令不加。
//! 未定义编码进入向量 `0x04`，这一步仍然成功。

use std::cmp::Ordering;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use crate::bios;
use crate::bus::{Bus, CpuStall};
use crate::disasm::{format_op, HalfOffset, MemOffset, MsrSrc, Op, Shifter};
use crate::registers::{Registers, MODE_UND, MODE_USER, UND_VECTOR};

#[derive(Debug)]
pub struct Stepped {
    pub address: u32,
    pub word: u32,
    pub text: String,
    pub cycles: u32,
}

pub fn step(regs: &mut Registers, bus: &mut Bus) -> Result<Stepped, String> {
    step_at(regs, bus, true)
}

/// 和 `step` 做同一件事，但不把指令译成文本。窗口按帧跑时用这个。
///
/// 下一件日程还远时，最多连跑 8 条再看一眼缓存的截止拍。
/// 每条仍按自己的拍数 `advance`，所以不会跨过定时器或采样。
pub fn step_quiet(regs: &mut Registers, bus: &mut Bus) -> Result<(), String> {
    const BATCH: u32 = 8;
    for _ in 0..BATCH {
        let pc = regs.pc();
        step_at(regs, bus, false)?;
        if regs.pc() == pc || bus.cycles() >= bus.deadline() {
            break;
        }
    }
    Ok(())
}

fn step_at(regs: &mut Registers, bus: &mut Bus, explain: bool) -> Result<Stepped, String> {
    if let Some(paused) = consume_stall(regs, bus) {
        return Ok(paused);
    }
    if !regs.irqs_blocked() && bus.irq_requested() {
        bus.or_bios_flags({
            let (ie, flags) = bus.irq_lines();
            ie & flags
        });
        return take_irq(regs, bus);
    }
    if regs.thumb() {
        return step_thumb(regs, bus, explain);
    }
    let address = regs.pc();
    if address & 3 != 0 {
        return Err(format!("PC 0x{address:08X} 没有按 4 字节对齐"));
    }
    let word = bus.read_u32(address);
    // 同一地址、同一机器码译过一次就留着。写到代码区会作废。
    let Some(op) = bus.decode_arm(address, word) else {
        return Err(format!("0x{word:08X} 这条 ARM 指令还没有实现"));
    };
    let text = if explain {
        format_op(address, &op)
    } else {
        String::new()
    };
    let snap = bus.snap_timing();
    let fetch = bus.account(address, 4, true);
    if !condition(regs, op.cond()) {
        regs.set_pc(address.wrapping_add(4));
        bus.advance(fetch);
        return Ok(Stepped {
            address,
            word,
            text,
            cycles: fetch,
        });
    }
    let (base, branched) = match execute(regs, bus, address, word, &op) {
        Ok(done) => done,
        Err(err) => {
            bus.restore_timing(snap);
            return Err(err);
        }
    };
    let cycles = base + fetch.saturating_sub(1);
    if !branched {
        regs.set_pc(address.wrapping_add(4));
    }
    bus.advance(cycles);
    Ok(Stepped {
        address,
        word,
        text,
        cycles,
    })
}

/// 停机还没结束就推进一行。结束了，或者这次该进 IRQ，就返回 `None`。
fn consume_stall(regs: &Registers, bus: &mut Bus) -> Option<Stepped> {
    let waiting = match bus.stall() {
        CpuStall::Run => return None,
        CpuStall::Halt => {
            let (ie, flags) = bus.irq_lines();
            if ie & flags != 0 {
                bus.set_stall(CpuStall::Run);
                return None;
            }
            true
        }
        CpuStall::Stop => {
            let (ie, flags) = bus.irq_lines();
            // 串口、按键、卡带。
            if ie & flags & 0x3080 != 0 {
                bus.set_stall(CpuStall::Run);
                return None;
            }
            true
        }
        CpuStall::Intr(mask) => !intr_done(regs, bus, mask),
    };
    if !waiting {
        return None;
    }
    let address = regs.pc();
    let cycles = bus.idle_line();
    Some(Stepped {
        address,
        word: 0,
        text: "halt".to_string(),
        cycles,
    })
}

/// `true` 表示这次 `IntrWait` 可以返回，或者该把控制权交给 IRQ。
/// 处理函数还能进时，停机状态留着，等 IRQ 回来再看 `0x03007FF8`。
fn intr_done(regs: &Registers, bus: &mut Bus, mask: u16) -> bool {
    if bus.bios_flags() & mask != 0 {
        bus.clear_bios_flags(mask);
        bus.set_stall(CpuStall::Run);
        return true;
    }
    let (ie, flags) = bus.irq_lines();
    let fired = ie & flags;
    if fired & mask == 0 {
        return false;
    }
    if !regs.irqs_blocked() && bus.ime() {
        return true;
    }
    bus.or_bios_flags(fired);
    bus.clear_bios_flags(mask);
    bus.set_stall(CpuStall::Run);
    true
}

fn take_irq(regs: &mut Registers, bus: &mut Bus) -> Result<Stepped, String> {
    let address = regs.pc();
    // 先到 `0x18`。处理函数地址在 BIOS 里用 `ldr pc` 读，回来再恢复 T。
    regs.enter_irq(address.wrapping_add(4));
    let cycles = 3;
    bus.note_internal(cycles);
    bus.advance(cycles);
    Ok(Stepped {
        address,
        word: 0,
        text: "irq".to_string(),
        cycles,
    })
}

fn step_thumb(regs: &mut Registers, bus: &mut Bus, explain: bool) -> Result<Stepped, String> {
    let address = regs.pc();
    if address & 1 != 0 {
        return Err(format!("PC 0x{address:08X} 没有按 2 字节对齐"));
    }
    let half = bus.read_u16(address);
    let op = bus.decode_thumb(address, half);
    let text = if explain {
        crate::thumb::format_at(address, half)
    } else {
        String::new()
    };
    let snap = bus.snap_timing();
    let fetch = bus.account(address, 2, true);
    let (base, branched) = match crate::thumb::execute(regs, bus, address, half, &op) {
        Ok(done) => done,
        Err(err) => {
            bus.restore_timing(snap);
            return Err(err);
        }
    };
    let cycles = base + fetch.saturating_sub(1);
    if !branched {
        regs.set_pc(address.wrapping_add(2));
    }
    bus.advance(cycles);
    Ok(Stepped {
        address,
        word: u32::from(half),
        text,
        cycles,
    })
}

fn execute(
    regs: &mut Registers,
    bus: &mut Bus,
    address: u32,
    word: u32,
    op: &Op,
) -> Result<(u32, bool), String> {
    match op {
        Op::Data {
            opcode,
            set_flags,
            rn,
            rd,
            shifter,
            ..
        } => {
            let carry_in = regs.flag_c();
            let pc_plus = if matches!(shifter, Shifter::RegShift { .. }) {
                12
            } else {
                8
            };
            let (operand, shifter_carry) = eval_shifter(regs, address, shifter, carry_in);
            let left = read_reg(regs, address, *rn, pc_plus);
            let (result, carry, overflow, logical) =
                alu(*opcode, left, operand, carry_in, shifter_carry);
            let compare = (8..12).contains(opcode);
            let restore = *set_flags && *rd == 15 && regs.spsr().is_some();
            if *set_flags && !restore && (*rd != 15 || compare) {
                if logical {
                    regs.set_nzc(result & 0x8000_0000 != 0, result == 0, carry);
                } else {
                    regs.set_nzcv(result & 0x8000_0000 != 0, result == 0, carry, overflow);
                }
            }
            let branched = if restore {
                let spsr = regs.spsr().expect("上面已经确认有 SPSR");
                regs.set_cpsr(spsr);
                if compare {
                    false
                } else {
                    regs.set_pc_aligned(result);
                    true
                }
            } else if compare {
                false
            } else {
                write_reg(regs, *rd, result)
            };
            let cycles = if matches!(shifter, Shifter::RegShift { .. }) {
                bus.note_internal(1);
                2
            } else {
                1
            };
            Ok((cycles, branched))
        }
        Op::Mem {
            load,
            byte,
            pre,
            up,
            writeback,
            rn,
            rd,
            offset,
            ..
        } => {
            let base = read_reg(regs, address, *rn, 8);
            let off = mem_offset(regs, address, offset);
            let effective = if *up {
                base.wrapping_add(off)
            } else {
                base.wrapping_sub(off)
            };
            let addr = if *pre { effective } else { base };
            let width = if *byte { 1 } else { 4 };
            let stall = bus.data_stall(addr, width);
            if *load {
                bus.note_internal(1);
            }
            let branched = if *load {
                let value = if *byte {
                    u32::from(bus.read_u8(addr))
                } else {
                    let aligned = addr & !3;
                    let word = bus.read_u32(aligned);
                    word.rotate_right((addr & 3) * 8)
                };
                write_reg(regs, *rd, value)
            } else {
                let value = read_reg(regs, address, *rd, 12);
                if *byte {
                    bus.write_u8(addr, value as u8);
                } else {
                    bus.write_u32(addr & !3, value);
                }
                false
            };
            let cycles = if *load { 3 } else { 2 } + stall;
            // 加载和回写是同一个寄存器时，留下读到的值。
            if (!*pre || *writeback) && !(*load && *rn == *rd) {
                let branched_base = write_reg(regs, *rn, effective);
                Ok((cycles, branched || branched_base))
            } else {
                Ok((cycles, branched))
            }
        }
        Op::Half {
            load,
            signed,
            byte,
            pre,
            up,
            writeback,
            rn,
            rd,
            offset,
            ..
        } => {
            let base = read_reg(regs, address, *rn, 8);
            let off = half_offset(regs, address, offset);
            let effective = if *up {
                base.wrapping_add(off)
            } else {
                base.wrapping_sub(off)
            };
            let addr = if *pre { effective } else { base };
            let width = if *byte || (*load && *signed && addr & 1 != 0) {
                1
            } else if *load && addr & 1 != 0 {
                4
            } else {
                2
            };
            let stall = bus.data_stall(addr, width);
            if *load {
                bus.note_internal(1);
            }
            let branched = if *load {
                let value = if *byte {
                    let raw = bus.read_u8(addr);
                    if *signed {
                        raw as i8 as i32 as u32
                    } else {
                        u32::from(raw)
                    }
                } else {
                    bus.read_half(addr, *signed)
                };
                write_reg(regs, *rd, value)
            } else {
                bus.write_u16(addr & !1, read_reg(regs, address, *rd, 12) as u16);
                false
            };
            let cycles = if *load { 3 } else { 2 } + stall;
            if (!*pre || *writeback) && !(*load && *rn == *rd) {
                let branched_base = write_reg(regs, *rn, effective);
                Ok((cycles, branched || branched_base))
            } else {
                Ok((cycles, branched))
            }
        }
        Op::Branch { link, imm24, .. } => {
            if *link {
                regs.set(14, address.wrapping_add(4));
            }
            let offset = ((*imm24 << 8) as i32) >> 6;
            let target = address.wrapping_add(8).wrapping_add(offset as u32);
            regs.set_pc(target);
            Ok((3 + bus.refill_stall(target, 4), true))
        }
        Op::Mul {
            accumulate,
            set_flags,
            rd,
            rn,
            rs,
            rm,
            ..
        } => {
            let product =
                read_reg(regs, address, *rm, 8).wrapping_mul(read_reg(regs, address, *rs, 8));
            let result = if *accumulate {
                product.wrapping_add(read_reg(regs, address, *rn, 8))
            } else {
                product
            };
            if *set_flags {
                regs.set_nzc(result & 0x8000_0000 != 0, result == 0, regs.flag_c());
            }
            let branched = write_reg(regs, *rd, result);
            bus.note_internal(1);
            Ok((2, branched))
        }
        Op::MulLong {
            signed,
            accumulate,
            set_flags,
            rd_hi,
            rd_lo,
            rs,
            rm,
            ..
        } => {
            let left = read_reg(regs, address, *rm, 8);
            let right = read_reg(regs, address, *rs, 8);
            let mut product = if *signed {
                (left as i32 as i64).wrapping_mul(right as i32 as i64) as u64
            } else {
                u64::from(left).wrapping_mul(u64::from(right))
            };
            if *accumulate {
                let acc = (u64::from(read_reg(regs, address, *rd_hi, 8)) << 32)
                    | u64::from(read_reg(regs, address, *rd_lo, 8));
                product = product.wrapping_add(acc);
            }
            let lo = product as u32;
            let hi = (product >> 32) as u32;
            if *set_flags {
                regs.set_nzc(hi & 0x8000_0000 != 0, product == 0, regs.flag_c());
            }
            let low = write_reg(regs, *rd_lo, lo);
            let high = write_reg(regs, *rd_hi, hi);
            bus.note_internal(2);
            Ok((3, low || high))
        }
        Op::Swap {
            byte, rn, rd, rm, ..
        } => {
            let addr = read_reg(regs, address, *rn, 8);
            let stored = read_reg(regs, address, *rm, 8);
            let width = if *byte { 1 } else { 4 };
            let stall = bus.data_stall(addr, width) + bus.data_stall(addr, width);
            bus.note_internal(1);
            let loaded = if *byte {
                u32::from(bus.read_u8(addr))
            } else {
                let aligned = addr & !3;
                bus.read_u32(aligned).rotate_right((addr & 3) * 8)
            };
            if *byte {
                bus.write_u8(addr, stored as u8);
            } else {
                bus.write_u32(addr & !3, stored);
            }
            Ok((4 + stall, write_reg(regs, *rd, loaded)))
        }
        Op::Bx { rm, .. } => {
            let target = read_reg(regs, address, *rm, 8);
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
        Op::Mrs { spsr, rd, .. } => {
            let value = if *spsr {
                regs.spsr().ok_or_else(|| "这一步没有 SPSR".to_string())?
            } else {
                regs.cpsr()
            };
            Ok((1, write_reg(regs, *rd, value)))
        }
        Op::Msr {
            spsr, mask, src, ..
        } => {
            let value = match *src {
                MsrSrc::Reg(rm) => read_reg(regs, address, rm, 8),
                MsrSrc::Imm { imm8, rot } => u32::from(imm8).rotate_right(u32::from(rot) * 2),
            };
            regs.write_status(*spsr, *mask, value)?;
            Ok((1, false))
        }
        Op::Block {
            pre,
            up,
            user,
            writeback,
            load,
            rn,
            list,
            ..
        } => transfer_block(
            regs,
            bus,
            address,
            BlockTransfer {
                pre: *pre,
                up: *up,
                user: *user,
                writeback: *writeback,
                load: *load,
                rn: *rn,
                list: *list,
            },
        ),
        Op::Exception { swi, .. } => match *swi {
            Some(imm) => Ok(software_interrupt(
                regs,
                bus,
                (imm >> 16) as u8,
                address.wrapping_add(4),
            )),
            None => Ok(undefined_instruction(regs, bus, address, word, false)),
        },
    }
}

fn mem_offset(regs: &Registers, address: u32, offset: &MemOffset) -> u32 {
    match *offset {
        MemOffset::Imm(value) => u32::from(value),
        MemOffset::Reg { rm, kind, amount } => {
            let value = read_reg(regs, address, rm, 8);
            shift(value, kind, amount, regs.flag_c()).0
        }
    }
}

fn half_offset(regs: &Registers, address: u32, offset: &HalfOffset) -> u32 {
    match *offset {
        HalfOffset::Imm(value) => u32::from(value),
        HalfOffset::Reg(rm) => read_reg(regs, address, rm, 8),
    }
}

struct BlockTransfer {
    pre: bool,
    up: bool,
    user: bool,
    writeback: bool,
    load: bool,
    rn: u8,
    list: u16,
}

fn transfer_block(
    regs: &mut Registers,
    bus: &mut Bus,
    address: u32,
    block: BlockTransfer,
) -> Result<(u32, bool), String> {
    let BlockTransfer {
        pre,
        up,
        user,
        writeback,
        load,
        rn,
        list,
    } = block;
    let mut mask = list;
    let mut count = mask.count_ones();
    if count == 0 {
        mask = 1 << 15;
        count = 16;
    }
    let includes_pc = mask & (1 << 15) != 0;
    if load && user && includes_pc && regs.spsr().is_none() {
        return Err("LDM 带 S 并且包含 PC，这一步没有 SPSR".to_string());
    }
    // 带 S 又不包含 PC 时，列表用用户组。包含 PC 的 ldm 仍用当前组，做完再恢复 SPSR。
    let user_bank = user && !(load && includes_pc);
    let base = read_reg(regs, address, rn, 8);
    let mut cursor = if up {
        if pre {
            base.wrapping_add(4)
        } else {
            base
        }
    } else if pre {
        base.wrapping_sub(count * 4)
    } else {
        base.wrapping_sub(count * 4).wrapping_add(4)
    };
    let written = if up {
        base.wrapping_add(count * 4)
    } else {
        base.wrapping_sub(count * 4)
    };
    let stall = bus.burst_stall(cursor & !3, count, 4);
    let mut branched = false;
    if load {
        let mut loaded_pc = None;
        for index in 0..16 {
            if mask & (1 << index) == 0 {
                continue;
            }
            // 批量传送忽略地址低两位，读出来的字不旋转。
            let value = bus.read_u32(cursor & !3);
            if index == 15 {
                loaded_pc = Some(value);
            } else if user_bank {
                regs.set_in(index, value, MODE_USER);
            } else {
                regs.set(index, value);
            }
            cursor = cursor.wrapping_add(4);
        }
        if let Some(value) = loaded_pc {
            if user && includes_pc {
                let spsr = regs.spsr().expect("上面已经确认有 SPSR");
                regs.set_cpsr(spsr);
            }
            regs.set_pc_aligned(value);
            branched = true;
        }
        if writeback && mask & (1 << rn) == 0 && write_reg(regs, rn, written) {
            branched = true;
        }
        bus.note_internal(1);
        Ok((count + 2 + stall, branched))
    } else {
        let mut seen = false;
        for index in 0..16 {
            if mask & (1 << index) == 0 {
                continue;
            }
            // 基址在列表里又不是第一个时，存回写之后的值。带 ^ 时仍存用户那一份。
            let value = if index == 15 {
                read_reg(regs, address, 15, 12)
            } else if user_bank {
                regs.get_in(index, MODE_USER)
            } else if index == rn && seen {
                written
            } else {
                read_reg(regs, address, index, 12)
            };
            bus.write_u32(cursor & !3, value);
            cursor = cursor.wrapping_add(4);
            seen = true;
        }
        if writeback {
            branched = write_reg(regs, rn, written);
        }
        Ok((count + 1 + stall, branched))
    }
}

fn alu(
    opcode: u8,
    left: u32,
    right: u32,
    carry_in: bool,
    shifter_carry: bool,
) -> (u32, bool, bool, bool) {
    match opcode {
        0 => (left & right, shifter_carry, false, true),
        1 => (left ^ right, shifter_carry, false, true),
        2 | 10 => {
            let (result, carry, overflow) = sub(left, right, true);
            (result, carry, overflow, false)
        }
        3 => {
            let (result, carry, overflow) = sub(right, left, true);
            (result, carry, overflow, false)
        }
        4 | 11 => {
            let (result, carry, overflow) = add(left, right, false);
            (result, carry, overflow, false)
        }
        5 => {
            let (result, carry, overflow) = add(left, right, carry_in);
            (result, carry, overflow, false)
        }
        6 => {
            let (result, carry, overflow) = sub(left, right, carry_in);
            (result, carry, overflow, false)
        }
        7 => {
            let (result, carry, overflow) = sub(right, left, carry_in);
            (result, carry, overflow, false)
        }
        8 => (left & right, shifter_carry, false, true),
        9 => (left ^ right, shifter_carry, false, true),
        12 => (left | right, shifter_carry, false, true),
        13 => (right, shifter_carry, false, true),
        14 => (left & !right, shifter_carry, false, true),
        _ => (!right, shifter_carry, false, true),
    }
}

pub(crate) fn add(left: u32, right: u32, carry: bool) -> (u32, bool, bool) {
    let (sum, carry_a) = left.overflowing_add(right);
    let (result, carry_b) = sum.overflowing_add(u32::from(carry));
    let overflow = (left ^ right) & 0x8000_0000 == 0 && (left ^ result) & 0x8000_0000 != 0;
    (result, carry_a || carry_b, overflow)
}

pub(crate) fn sub(left: u32, right: u32, carry: bool) -> (u32, bool, bool) {
    // carry 在这里是「没有借位」。SBC 传入的是 CPSR 的 C。
    let (result, carry_out, overflow) = add(left, !right, carry);
    (result, carry_out, overflow)
}

fn eval_shifter(regs: &Registers, address: u32, shifter: &Shifter, carry_in: bool) -> (u32, bool) {
    match *shifter {
        Shifter::Imm { imm8, rot } => {
            if rot == 0 {
                (u32::from(imm8), carry_in)
            } else {
                let result = u32::from(imm8).rotate_right(u32::from(rot) * 2);
                (result, result & 0x8000_0000 != 0)
            }
        }
        Shifter::Reg { rm, kind, amount } => {
            let value = read_reg(regs, address, rm, 8);
            shift(value, kind, amount, carry_in)
        }
        Shifter::RegShift { rm, kind, rs } => {
            let amount = (read_reg(regs, address, rs, 8) & 0xFF) as u8;
            let value = read_reg(regs, address, rm, 12);
            shift_reg(value, kind, amount, carry_in)
        }
    }
}

pub(crate) fn shift_reg(value: u32, kind: u8, amount: u8, carry_in: bool) -> (u32, bool) {
    let amount = u32::from(amount);
    if amount == 0 {
        return (value, carry_in);
    }
    match kind {
        0 | 1 => match amount.cmp(&32) {
            Ordering::Less => shift(value, kind, amount as u8, carry_in),
            Ordering::Equal => {
                let carry = if kind == 0 {
                    value & 1 != 0
                } else {
                    value & 0x8000_0000 != 0
                };
                (0, carry)
            }
            Ordering::Greater => (0, false),
        },
        2 => {
            if amount < 32 {
                shift(value, kind, amount as u8, carry_in)
            } else {
                let sign = value & 0x8000_0000 != 0;
                (if sign { 0xFFFF_FFFF } else { 0 }, sign)
            }
        }
        _ => {
            let low = amount & 31;
            if low == 0 {
                (value, value & 0x8000_0000 != 0)
            } else {
                (value.rotate_right(low), value & (1 << (low - 1)) != 0)
            }
        }
    }
}

pub(crate) fn shift(value: u32, kind: u8, amount: u8, carry_in: bool) -> (u32, bool) {
    let amount = u32::from(amount);
    match kind {
        0 => {
            if amount == 0 {
                (value, carry_in)
            } else if amount < 32 {
                (value << amount, (value & (1 << (32 - amount))) != 0)
            } else if amount == 32 {
                (0, value & 1 != 0)
            } else {
                (0, false)
            }
        }
        1 => {
            let amount = if amount == 0 { 32 } else { amount };
            match amount.cmp(&32) {
                Ordering::Less => (value >> amount, (value & (1 << (amount - 1))) != 0),
                Ordering::Equal => (0, value & 0x8000_0000 != 0),
                Ordering::Greater => (0, false),
            }
        }
        2 => {
            let amount = if amount == 0 { 32 } else { amount };
            if amount < 32 {
                let result = ((value as i32) >> amount) as u32;
                (result, (value & (1 << (amount - 1))) != 0)
            } else {
                let sign = value & 0x8000_0000 != 0;
                (if sign { 0xFFFF_FFFF } else { 0 }, sign)
            }
        }
        _ => {
            if amount == 0 {
                let carry_bit = u32::from(carry_in) << 31;
                ((value >> 1) | carry_bit, value & 1 != 0)
            } else {
                (
                    value.rotate_right(amount),
                    (value & (1 << (amount - 1))) != 0,
                )
            }
        }
    }
}

fn read_reg(regs: &Registers, address: u32, index: u8, pc_plus: u32) -> u32 {
    if index == 15 {
        address.wrapping_add(pc_plus)
    } else {
        regs.get(index)
    }
}

fn write_reg(regs: &mut Registers, index: u8, value: u32) -> bool {
    if index == 15 {
        regs.set_pc_aligned(value);
        true
    } else {
        regs.set(index, value);
        false
    }
}

/// 未定义编码。进入未定义向量 `0x04`，这一步返回成功。
///
/// 已经在未定义模式里又碰到未定义编码时，不再套一层：否则 LR 和 SPSR 会被盖掉，
/// PC 又回到 `0x04`，死转在向量上。这里只按指令宽度把 PC 往前挪，第一次进入时
/// 留下的返回信息还在，后面的 `movs pc, lr` 仍能回去。
/// 默认的 `0x04` 就是这条返回指令，所以通常走不到这条退路。
pub(crate) fn undefined_instruction(
    regs: &mut Registers,
    bus: &mut Bus,
    address: u32,
    encoding: u32,
    thumb: bool,
) -> (u32, bool) {
    let width = if thumb { 2 } else { 4 };
    let nested = regs.mode() == MODE_UND;
    log_undefined(
        if thumb { "Thumb" } else { "ARM" },
        address,
        encoding,
        regs,
        nested,
    );
    if nested {
        regs.set_pc(address.wrapping_add(width));
        return (1, true);
    }
    regs.enter_und(address.wrapping_add(width));
    (3 + bus.refill_stall(UND_VECTOR, 4), true)
}

/// 前 8 次都记。之后每 256 次再记一条，避免商业卡带把标准输出灌满。
fn log_undefined(isa: &str, address: u32, encoding: u32, regs: &Registers, nested: bool) {
    static SEEN: AtomicU64 = AtomicU64::new(0);
    let n = SEEN.fetch_add(1, AtomicOrdering::Relaxed);
    if n >= 8 && n % 256 != 0 {
        return;
    }
    let note = if nested {
        "（已在未定义模式，跳过这条）"
    } else if n == 7 {
        "（之后每 256 次再记一条）"
    } else {
        ""
    };
    let count = if n >= 8 {
        format!("（第 {} 次）", n + 1)
    } else {
        String::new()
    };
    eprintln!(
        "未定义指令：{isa} pc=0x{address:08X} encoding=0x{encoding:08X} cpsr=0x{:08X} mode=0x{:02X} lr=0x{:08X}{note}{count}",
        regs.cpsr(),
        regs.mode(),
        regs.get(14),
    );
}

/// `swi`。认得出的编号在 `bios` 里做完，模式不变。其余进入管理向量。
pub(crate) fn software_interrupt(
    regs: &mut Registers,
    bus: &mut Bus,
    number: u8,
    return_pc: u32,
) -> (u32, bool) {
    if bus.bios_hle() {
        if let Some(done) = bios::call(regs, bus, number) {
            done
        } else {
            regs.enter_swi(return_pc);
            (3 + bus.refill_stall(0x08, 4), true)
        }
    } else {
        regs.enter_swi(return_pc);
        (3 + bus.refill_stall(0x08, 4), true)
    }
}

fn condition(regs: &Registers, cond: u8) -> bool {
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

pub fn run_until_self_branch(
    regs: &mut Registers,
    bus: &mut Bus,
    max_steps: u32,
) -> Result<u32, String> {
    for count in 1..=max_steps {
        let address = regs.pc();
        // 一次一条，这样停在原地跳转上时不会把那条再执行一遍。
        step_at(regs, bus, false)?;
        if regs.pc() == address {
            return Ok(count);
        }
    }
    Err(format!("跑了 {max_steps} 条指令，还没有停在原地跳转上"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::{self, Shift};
    use crate::bus::Bus;
    use crate::disasm::disassemble;
    use crate::registers::{Registers, MODE_IRQ, MODE_SVC, MODE_UND, MODE_USER};

    const AL: u8 = 0xE;

    fn boot(words: &[u32]) -> (Registers, Bus) {
        let mut bytes = Vec::new();
        for word in words {
            bytes.extend(word.to_le_bytes());
        }
        let mut bus = Bus::new();
        bus.load_rom(&bytes);
        (Registers::arm_start(0x0800_0000), bus)
    }

    #[test]
    fn mov_immediate_uses_the_rotated_form() {
        let word = asm::dp_imm(AL, 13, false, 0, 0, 0x0400_0000).unwrap();
        assert_eq!(disassemble(0x0800_0000, word).text, "mov r0, #0x4000000");
        let (mut regs, mut bus) = boot(&[word]);
        let stepped = step(&mut regs, &mut bus).unwrap();
        assert_eq!(stepped.cycles, 6);
        assert_eq!(regs.get(0), 0x0400_0000);
        assert_eq!(regs.pc(), 0x0800_0004);
    }

    #[test]
    fn adds_sets_carry_zero_and_overflow() {
        let word = asm::dp_imm(AL, 4, true, 0, 0, 1).unwrap();
        let (mut regs, mut bus) = boot(&[word]);
        regs.set(0, 0xFFFF_FFFF);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.get(0), 0);
        assert!(regs.flag_z() && regs.flag_c() && !regs.flag_v());

        regs.set_pc(0x0800_0000);
        regs.set(0, 0x7FFF_FFFF);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.get(0), 0x8000_0000);
        assert!(regs.flag_n() && regs.flag_v() && !regs.flag_c());
    }

    #[test]
    fn cmp_does_not_write_the_register() {
        let (mut regs, mut bus) = boot(&[asm::dp_imm(AL, 10, true, 0, 0, 5).unwrap()]);
        regs.set(0, 5);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.get(0), 5);
        assert!(regs.flag_z() && regs.flag_c());
    }

    #[test]
    fn str_and_ldr_are_little_endian() {
        let (mut regs, mut bus) = boot(&[
            asm::mem_imm(AL, false, false, true, true, false, 0, 1, 0),
            asm::mem_imm(AL, true, false, true, true, false, 0, 2, 0),
        ]);
        regs.set(0, 0x0200_0000);
        regs.set(1, 0x0403);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(bus.read_u32(0x0200_0000), 0x0403);
        assert_eq!(bus.read_u8(0x0200_0000), 0x03);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.get(2), 0x0403);
    }

    #[test]
    fn post_indexed_store_writes_back_the_base() {
        let (mut regs, mut bus) =
            boot(&[asm::mem_imm(AL, false, false, false, true, false, 0, 1, 4)]);
        regs.set(0, 0x0600_0000);
        regs.set(1, 0x001F_001F);
        let stepped = step(&mut regs, &mut bus).unwrap();
        assert_eq!(stepped.text, "str r1, [r0], #4");
        assert_eq!(bus.read_u32(0x0600_0000), 0x001F_001F);
        assert_eq!(regs.get(0), 0x0600_0004);
        assert_eq!(stepped.cycles, 10);
    }

    #[test]
    fn reading_pc_is_eight_bytes_ahead() {
        let (mut regs, mut bus) = boot(&[asm::dp_reg(AL, 13, false, 0, 0, 15, Shift::Lsl, 0)]);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.get(0), 0x0800_0008);
    }

    #[test]
    fn branch_to_self_leaves_pc_in_place() {
        let word = asm::branch_address(AL, false, 0x0800_0000, 0x0800_0000);
        assert_eq!(disassemble(0x0800_0000, word).text, "b 0x08000000");
        let (mut regs, mut bus) = boot(&[word]);
        let stepped = step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.pc(), 0x0800_0000);
        assert_eq!(stepped.cycles, 16);
    }

    #[test]
    fn failed_condition_only_advances_pc() {
        let (mut regs, mut bus) = boot(&[asm::dp_imm(0, 13, false, 0, 0, 2).unwrap()]);
        regs.set(0, 1);
        let stepped = step(&mut regs, &mut bus).unwrap();
        assert_eq!(stepped.text, "moveq r0, #2");
        assert_eq!(regs.get(0), 1);
        assert_eq!(regs.pc(), 0x0800_0004);
        assert_eq!(stepped.cycles, 6);
        assert_eq!(bus.cycles(), 6);
        assert_eq!(bus.read_u8(0x0400_0006), 0);
    }

    #[test]
    fn bl_saves_the_next_instruction_in_lr() {
        let (mut regs, mut bus) = boot(&[asm::branch_address(AL, true, 0x0800_0000, 0x0800_0010)]);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.get(14), 0x0800_0004);
        assert_eq!(regs.pc(), 0x0800_0010);
    }

    #[test]
    fn arm_undefined_enters_und_and_handler_can_return() {
        let (mut regs, mut bus) = boot(&[0xEE00_0010]);
        let adds = asm::dp_imm(AL, 4, true, 0, 0, 1).unwrap();
        bus.poke_bios_word(0x04, adds);
        regs.set(0, 5);
        regs.set(13, 0x1111);
        regs.set(14, 0x2222);
        let entered = step(&mut regs, &mut bus).unwrap();
        assert_eq!(entered.text, "udf");
        assert_eq!(entered.cycles, 8);
        assert_eq!(regs.mode(), MODE_UND);
        assert!(!regs.thumb());
        assert!(regs.irqs_blocked());
        assert_eq!(regs.pc(), 0x04);
        assert_eq!(regs.get(14), 0x0800_0004);
        assert_eq!(regs.spsr(), Some(0x0000_00D0));
        assert_eq!(regs.get(0), 5);
        assert_eq!(regs.get(13), 0);
        assert_eq!(regs.get_in(13, MODE_USER), 0x1111);
        assert_eq!(regs.get_in(14, MODE_USER), 0x2222);

        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.get(0), 6);
        assert_eq!(regs.pc(), 0x08);
        assert_eq!(regs.mode(), MODE_UND);

        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.pc(), 0x0800_0004);
        assert_eq!(regs.mode(), MODE_USER);
        assert_eq!(regs.cpsr(), 0x0000_00D0);
        assert_eq!(regs.get(13), 0x1111);
        assert_eq!(regs.get(14), 0x2222);
        assert_eq!(regs.get(0), 6);
    }

    #[test]
    fn thumb_undefined_enters_und_and_can_return() {
        let (mut regs, mut bus) = boot(&[0x0000_DE00]);
        regs.set_thumb(true);
        regs.set(13, 0x3333);
        regs.set(14, 0x4444);
        let entered = step(&mut regs, &mut bus).unwrap();
        assert_eq!(entered.text, "udf");
        assert_eq!(regs.mode(), MODE_UND);
        assert!(!regs.thumb());
        assert!(regs.irqs_blocked());
        assert_eq!(regs.pc(), 0x04);
        assert_eq!(regs.get(14), 0x0800_0002);
        assert_eq!(regs.spsr(), Some(0x0000_00F0));
        assert_eq!(regs.get_in(13, MODE_USER), 0x3333);
        assert_eq!(regs.get_in(14, MODE_USER), 0x4444);

        step(&mut regs, &mut bus).unwrap();
        assert!(regs.thumb());
        assert_eq!(regs.pc(), 0x0800_0002);
        assert_eq!(regs.mode(), MODE_USER);
        assert_eq!(regs.cpsr(), 0x0000_00F0);
        assert_eq!(regs.get(13), 0x3333);
        assert_eq!(regs.get(14), 0x4444);
    }

    #[test]
    fn undefined_vector_does_not_reenter() {
        let (mut regs, mut bus) = boot(&[0xEE00_0010]);
        bus.poke_bios_word(0x04, 0xEE00_0010);
        regs.set(14, 0x5555);
        let entered = step(&mut regs, &mut bus).unwrap();
        assert_eq!(entered.text, "udf");
        assert_eq!(regs.mode(), MODE_UND);
        assert_eq!(regs.pc(), 0x04);
        assert_eq!(regs.get(14), 0x0800_0004);
        assert_eq!(regs.spsr(), Some(0x0000_00D0));

        let skipped = step(&mut regs, &mut bus).unwrap();
        assert_eq!(skipped.text, "udf");
        assert_eq!(regs.mode(), MODE_UND);
        assert_eq!(regs.pc(), 0x08);
        assert_eq!(regs.get(14), 0x0800_0004);
        assert_eq!(regs.spsr(), Some(0x0000_00D0));

        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.pc(), 0x0800_0004);
        assert_eq!(regs.mode(), MODE_USER);
        assert_eq!(regs.get(14), 0x5555);
    }

    /// `0x18` 的分支、压栈、`ldr pc`。处理函数的最低位留着也不进 Thumb。
    fn enter_user_irq(regs: &mut Registers, bus: &mut Bus) {
        let entered = step(regs, bus).unwrap();
        assert_eq!(entered.text, "irq");
        assert_eq!(regs.pc(), 0x18);
        assert_eq!(regs.mode(), MODE_IRQ);
        assert!(!regs.thumb());
        for _ in 0..5 {
            step(regs, bus).unwrap();
        }
    }

    #[test]
    fn irq_handler_ignores_the_thumb_bit_and_returns_to_thumb() {
        // 被打断的是 Thumb，地址最低两位是 2。处理函数指针带 Thumb 位，
        // BIOS 的 `ldr pc` 仍按 ARM 进入，对齐后的地方是 `bx lr`。
        let (mut regs, mut bus) = boot(&[0x2102_2001]);
        regs.set_boot_stacks();
        regs.set_cpsr(0x3F);
        regs.set_pc(0x0800_0002);
        regs.set(0, 0x1111);
        bus.write_u32(0x0300_1000, 0xE12F_FF1E);
        bus.write_u32(0x0300_7FFC, 0x0300_1001);
        bus.write_u16(0x0400_0004, 0x0008);
        bus.write_u16(0x0400_0200, 1);
        bus.write_u16(0x0400_0208, 1);
        bus.advance(160 * 1232);

        enter_user_irq(&mut regs, &mut bus);
        assert_eq!(regs.pc(), 0x0300_1000);
        assert!(!regs.thumb());
        assert_eq!(regs.mode(), MODE_IRQ);
        assert_eq!(regs.get(14), 0x138);
        assert_eq!(regs.spsr(), Some(0x3F));
        assert_eq!(regs.get(0), 0x0400_0000);

        bus.write_u16(0x0400_0202, 1);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.pc(), 0x138);
        step(&mut regs, &mut bus).unwrap();
        step(&mut regs, &mut bus).unwrap();
        assert!(regs.thumb());
        assert_eq!(regs.mode(), 0x1F);
        assert_eq!(regs.pc(), 0x0800_0002);
        assert_eq!(regs.get(0), 0x1111);
        assert!(!regs.irqs_blocked());

        let next = step(&mut regs, &mut bus).unwrap();
        assert_eq!(next.text, "movs r1, #2");
        assert_eq!(regs.get(1), 2);
        assert_eq!(regs.pc(), 0x0800_0004);
    }

    #[test]
    fn irq_master_calls_thumb_and_bios_restores_it() {
        // 和商业卡带的主处理函数一样：先 `bx` 进 Thumb，再 `bx lr` 回 BIOS。
        let (mut regs, mut bus) = boot(&[0x2001]);
        regs.set_boot_stacks();
        regs.set_cpsr(0x3F);
        regs.set(4, 0);
        let master = [
            0xE92D_4000u32, // stmfd sp!, {lr}
            0xE59F_200C,    // ldr r2, [pc, #12]
            0xE28F_E000,    // add lr, pc, #0
            0xE12F_FF12,    // bx r2
            0xE8BD_4000,    // ldmfd sp!, {lr}
            0xE12F_FF1E,    // bx lr
            0x0300_2001,    // Thumb 函数，最低位是 1
        ];
        for (index, word) in master.iter().enumerate() {
            bus.write_u32(0x0300_1000 + index as u32 * 4, *word);
        }
        bus.write_u16(0x0300_2000, 0x2407);
        bus.write_u16(0x0300_2002, 0x4770);
        bus.write_u32(0x0300_7FFC, 0x0300_1000);
        bus.write_u16(0x0400_0004, 0x0008);
        bus.write_u16(0x0400_0200, 1);
        bus.write_u16(0x0400_0208, 1);
        bus.advance(160 * 1232);

        enter_user_irq(&mut regs, &mut bus);
        assert_eq!(regs.pc(), 0x0300_1000);
        for _ in 0..4 {
            step(&mut regs, &mut bus).unwrap();
        }
        assert!(regs.thumb());
        assert_eq!(regs.pc(), 0x0300_2000);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.get(4), 7);
        bus.write_u16(0x0400_0202, 1);
        for _ in 0..5 {
            step(&mut regs, &mut bus).unwrap();
        }
        assert!(regs.thumb());
        assert_eq!(regs.mode(), 0x1F);
        assert_eq!(regs.pc(), 0x0800_0000);
        assert_eq!(regs.get(4), 7);
        let next = step(&mut regs, &mut bus).unwrap();
        assert_eq!(next.text, "movs r0, #1");
    }

    #[test]
    fn swi_returns_with_movs_pc_lr() {
        let (mut regs, mut bus) = boot(&[0xEFFF_0000, 0xE1B0_F00E]);
        regs.set(13, 0x1111_1111);
        regs.set(14, 0x2222_2222);
        let entered = step(&mut regs, &mut bus).unwrap();
        assert_eq!(entered.text, "swi #0xFF0000");
        assert_eq!(entered.cycles, 8);
        assert_eq!(regs.pc(), 0x08);
        assert_eq!(regs.mode(), MODE_SVC);
        assert_eq!(regs.get(14), 0x0800_0004);
        assert_eq!(regs.get(13), 0);
        assert_eq!(regs.get_in(13, MODE_USER), 0x1111_1111);
        regs.set_pc(0x0800_0004);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.pc(), 0x0800_0004);
        assert_eq!(regs.mode(), MODE_USER);
        assert_eq!(regs.cpsr(), 0x0000_00D0);
        assert_eq!(regs.get(13), 0x1111_1111);
        assert_eq!(regs.get(14), 0x2222_2222);
    }

    #[test]
    fn disabling_bios_hle_routes_implemented_swi_through_the_vector() {
        let (mut regs, mut bus) = boot(&[0xEF06_0000]);
        bus.set_bios_hle(false);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.pc(), 0x08);
        assert_eq!(regs.mode(), MODE_SVC);
        assert_eq!(regs.get(14), 0x0800_0004);
    }

    #[test]
    fn thumb_swi_returns_to_thumb() {
        let (mut regs, mut bus) = boot(&[0x0000_DFFF, 0xE1B0_F00E]);
        regs.set_thumb(true);
        regs.set(13, 0x3333);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.pc(), 0x08);
        assert_eq!(regs.mode(), MODE_SVC);
        assert!(!regs.thumb());
        assert_eq!(regs.get(14), 0x0800_0002);
        regs.set_pc(0x0800_0004);
        step(&mut regs, &mut bus).unwrap();
        assert!(regs.thumb());
        assert_eq!(regs.pc(), 0x0800_0002);
        assert_eq!(regs.mode(), MODE_USER);
        assert_eq!(regs.get(13), 0x3333);
    }

    #[test]
    fn ldm_user_bank_and_exception_return() {
        let (mut regs, mut bus) = boot(&[0xE8D0_2000, 0xE8D0_8000]);
        regs.set_cpsr(0x0000_00D3);
        regs.set(0, 0x0200_0000);
        regs.set(13, 0x2222);
        regs.set_in(13, 0x1111, MODE_USER);
        bus.write_u32(0x0200_0000, 0x3333);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.mode(), MODE_SVC);
        assert_eq!(regs.get(13), 0x2222);
        assert_eq!(regs.get_in(13, MODE_USER), 0x3333);

        regs.set(0, 0x0200_0004);
        bus.write_u32(0x0200_0004, 0x0800_0010);
        regs.write_status(true, 0xF, 0x0000_00D0).unwrap();
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.pc(), 0x0800_0010);
        assert_eq!(regs.mode(), MODE_USER);
        assert_eq!(regs.get(13), 0x3333);
        assert_eq!(regs.get_in(13, MODE_SVC), 0x2222);
    }

    #[test]
    fn storing_pc_writes_twelve_bytes_ahead() {
        let (mut regs, mut bus) =
            boot(&[asm::mem_imm(AL, false, false, true, true, false, 0, 15, 0)]);
        regs.set(0, 0x0200_0000);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(bus.read_u32(0x0200_0000), 0x0800_000C);
    }

    #[test]
    fn bx_lands_in_thumb_and_the_next_step_runs() {
        let (mut regs, mut bus) = boot(&[0xE12F_FF11, 0x2001]);
        regs.set(1, 0x0800_0005);
        let first = step(&mut regs, &mut bus).unwrap();
        assert_eq!(first.text, "bx r1");
        assert!(regs.thumb());
        assert_eq!(regs.pc(), 0x0800_0004);
        let second = step(&mut regs, &mut bus).unwrap();
        assert_eq!(second.text, "movs r0, #1");
        assert_eq!(regs.get(0), 1);
        assert_eq!(regs.pc(), 0x0800_0006);
    }

    #[test]
    fn a_scanline_of_movs_bumps_vcount() {
        let word = asm::dp_imm(AL, 13, false, 0, 0, 0).unwrap();
        let mut bus = Bus::new();
        for index in 0..1232 {
            bus.write_u32(0x0300_0000 + index * 4, word);
        }
        let mut regs = Registers::arm_start(0x0300_0000);
        for _ in 0..1231 {
            step(&mut regs, &mut bus).unwrap();
        }
        assert_eq!(bus.cycles(), 1231);
        assert_eq!(bus.read_u8(0x0400_0006), 0);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(bus.cycles(), 1232);
        assert_eq!(bus.read_u8(0x0400_0006), 1);
    }

    #[test]
    fn vblank_irq_runs_the_handler_and_returns() {
        let add = asm::dp_imm(AL, 4, false, 0, 0, 1).unwrap();
        let (mut regs, mut bus) = boot(&[add]);
        bus.write_u32(0x0300_1000, 0xE12F_FF1E);
        bus.write_u32(0x0300_7FFC, 0x0300_1000);
        regs.set_boot_stacks();
        regs.set_cpsr(0x1F);
        bus.write_u16(0x0400_0004, 0x0008);
        bus.write_u16(0x0400_0200, 1);
        bus.advance(160 * 1232);
        assert_eq!(bus.read_u16(0x0400_0202), 1);

        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.get(0), 1);
        assert_eq!(regs.pc(), 0x0800_0004);

        regs.set(0, 0);
        regs.set_pc(0x0800_0000);
        bus.write_u16(0x0400_0208, 1);
        enter_user_irq(&mut regs, &mut bus);
        assert!(regs.irqs_blocked());
        assert_eq!(regs.get(14), 0x138);
        assert_eq!(regs.spsr(), Some(0x1F));
        assert_eq!(regs.get(0), 0x0400_0000);
        assert!(bus.cycles() > 160 * 1232);

        bus.write_u16(0x0400_0202, 1);
        step(&mut regs, &mut bus).unwrap();
        step(&mut regs, &mut bus).unwrap();
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.mode(), 0x1F);
        assert!(!regs.irqs_blocked());
        assert_eq!(regs.pc(), 0x0800_0000);
        assert_eq!(regs.get(0), 0);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.get(0), 1);
        assert_eq!(regs.pc(), 0x0800_0004);
    }

    #[test]
    fn timer_overflow_enters_the_handler() {
        let add = asm::dp_imm(AL, 4, false, 0, 0, 1).unwrap();
        let (mut regs, mut bus) = boot(&[add, add]);
        bus.write_u32(0x0300_1000, 0xE12F_FF1E);
        bus.write_u32(0x0300_7FFC, 0x0300_1000);
        regs.set_boot_stacks();
        regs.set_cpsr(0x1F);
        bus.write_u16(0x0400_0200, 0x0008);
        bus.write_u16(0x0400_0208, 1);
        bus.write_u16(0x0400_0100, 0xFFFF);
        bus.write_u16(0x0400_0102, 0x00C0);

        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.get(0), 1);
        assert_eq!(bus.read_u16(0x0400_0202), 0x0008);

        enter_user_irq(&mut regs, &mut bus);
        assert_eq!(regs.get(14), 0x138);
        assert_eq!(regs.get(0), 0x0400_0000);

        bus.write_u16(0x0400_0102, 0);
        bus.write_u16(0x0400_0202, 0x0008);
        step(&mut regs, &mut bus).unwrap();
        step(&mut regs, &mut bus).unwrap();
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.pc(), 0x0800_0004);
        assert_eq!(regs.get(0), 1);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.get(0), 2);
        assert_eq!(regs.pc(), 0x0800_0008);
    }

    /// 计时：`cargo test --release --lib iwram_movs_two_frames -- --ignored --nocapture`
    #[test]
    #[ignore = "只用来量一帧要多久"]
    fn iwram_movs_two_frames() {
        use std::time::Instant;
        let mov = asm::dp_imm(AL, 13, false, 0, 0, 0).unwrap();
        let mut bus = Bus::new();
        for index in 0..8191 {
            bus.write_u32(0x0300_0000 + index * 4, mov);
        }
        let back = asm::branch_address(AL, false, 0x0300_7FFC, 0x0300_0000);
        bus.write_u32(0x0300_7FFC, back);
        bus.write_u16(0x0400_0000, 0x1000);
        let mut regs = Registers::arm_start(0x0300_0000);
        regs.set_cpsr(0x0000_00DF);
        let target = 2 * 280896;
        let started = Instant::now();
        while bus.cycles() < target {
            step_quiet(&mut regs, &mut bus).unwrap();
        }
        let elapsed = started.elapsed();
        let frames = bus.cycles() as f64 / 280896.0;
        eprintln!(
            "iwram movs: {frames:.2} 帧，{elapsed:.3?}，{:.1} FPS",
            frames / elapsed.as_secs_f64()
        );
    }

    /// 计时：`cargo test --release --lib fifo_timer_two_frames -- --ignored --nocapture`
    #[test]
    #[ignore = "只用来量带声音队列的一帧要多久"]
    fn fifo_timer_two_frames() {
        use std::time::Instant;
        let mov = asm::dp_imm(AL, 13, false, 0, 0, 0).unwrap();
        let mut bus = Bus::new();
        for index in 0..8191 {
            bus.write_u32(0x0300_0000 + index * 4, mov);
        }
        let back = asm::branch_address(AL, false, 0x0300_7FFC, 0x0300_0000);
        bus.write_u32(0x0300_7FFC, back);
        for value in 1u8..=64 {
            bus.write_u8(0x0200_0000 + u32::from(value) - 1, value);
        }
        let dma = 0x0400_00BC;
        bus.write_u32(dma, 0x0200_0000);
        bus.write_u32(dma + 4, 0x0400_00A0);
        bus.write_u32(dma + 8, 1 | (u32::from(0xB640u16) << 16));
        bus.write_u16(0x0400_0082, 0x0304);
        bus.write_u16(0x0400_0100, 0xFE00);
        bus.write_u16(0x0400_0102, 0x0080);
        bus.write_u16(0x0400_0000, 0x1000);
        let mut regs = Registers::arm_start(0x0300_0000);
        regs.set_cpsr(0x0000_00DF);
        let target = 2 * 280896;
        let started = Instant::now();
        while bus.cycles() < target {
            step_quiet(&mut regs, &mut bus).unwrap();
        }
        let elapsed = started.elapsed();
        let frames = bus.cycles() as f64 / 280896.0;
        eprintln!(
            "fifo timer: {frames:.2} 帧，{elapsed:.3?}，{:.1} FPS",
            frames / elapsed.as_secs_f64()
        );
    }

    /// 计时：`cargo test --release --lib fifo_fast_two_frames -- --ignored --nocapture`
    #[test]
    #[ignore = "只用来量每拍溢出的声音队列"]
    fn fifo_fast_two_frames() {
        use std::time::Instant;
        let mov = asm::dp_imm(AL, 13, false, 0, 0, 0).unwrap();
        let mut bus = Bus::new();
        for index in 0..8191 {
            bus.write_u32(0x0300_0000 + index * 4, mov);
        }
        let back = asm::branch_address(AL, false, 0x0300_7FFC, 0x0300_0000);
        bus.write_u32(0x0300_7FFC, back);
        for value in 1u8..=64 {
            bus.write_u8(0x0200_0000 + u32::from(value) - 1, value);
        }
        let dma = 0x0400_00BC;
        bus.write_u32(dma, 0x0200_0000);
        bus.write_u32(dma + 4, 0x0400_00A0);
        bus.write_u32(dma + 8, 1 | (u32::from(0xB640u16) << 16));
        bus.write_u16(0x0400_0082, 0x0304);
        bus.write_u16(0x0400_0100, 0xFFFF);
        bus.write_u16(0x0400_0102, 0x0080);
        bus.write_u16(0x0400_0000, 0x1000);
        let mut regs = Registers::arm_start(0x0300_0000);
        regs.set_cpsr(0x0000_00DF);
        let target = 2 * 280896;
        let started = Instant::now();
        while bus.cycles() < target {
            step_quiet(&mut regs, &mut bus).unwrap();
        }
        let elapsed = started.elapsed();
        let frames = bus.cycles() as f64 / 280896.0;
        eprintln!(
            "fifo fast: {frames:.2} 帧，{elapsed:.3?}，{:.1} FPS",
            frames / elapsed.as_secs_f64()
        );
    }
}
