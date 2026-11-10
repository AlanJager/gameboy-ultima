//! Thumb 直线上的低位寄存器运算，译成 x86_64。
//!
//! 客人 `r0`–`r7` 和 NZCV 的算法跟 Apple Silicon 那一版相同。访存、跳转、高位寄存器仍不在这里。
//! Unix 用 System V：`rdi` 是寄存器组，`rsi` 进来时是 CPSR 指针，进函数后 `rsi` 改存 CPSR 的值。
//! Windows 用微软约定：`rcx` 是寄存器组，`rdx` 是 CPSR 指针。

use crate::thumb::{self, Op};

#[cfg(windows)]
const BASE: u8 = 1;
#[cfg(not(windows))]
const BASE: u8 = 7;
const RES: u8 = 11;
#[cfg(not(windows))]
const T0: u8 = 0;
#[cfg(windows)]
const T0: u8 = 0;
#[cfg(not(windows))]
const T1: u8 = 1;
#[cfg(windows)]
const T1: u8 = 8;
#[cfg(not(windows))]
const T2: u8 = 2;
#[cfg(windows)]
const T2: u8 = 2;
const CPSR: u8 = 6;

fn g(reg: u8) -> u8 {
    #[cfg(not(windows))]
    const MAP: [u8; 8] = [3, 12, 13, 14, 15, 8, 9, 10];
    #[cfg(windows)]
    const MAP: [u8; 8] = [3, 7, 12, 13, 14, 15, 5, 9];
    MAP[reg as usize]
}

struct Asm {
    bytes: Vec<u8>,
}

impl Asm {
    fn prologue(&mut self) {
        #[cfg(not(windows))]
        {
            for reg in [3u8, 12, 13, 14, 15, 6] {
                self.push(reg);
            }
        }
        #[cfg(windows)]
        {
            for reg in [3u8, 7, 5, 6, 12, 13, 14, 15, 2] {
                self.push(reg);
            }
        }
        for reg in 0..8u8 {
            self.load(g(reg), BASE, reg * 4);
        }
        #[cfg(not(windows))]
        self.load(CPSR, CPSR, 0);
        #[cfg(windows)]
        self.load(CPSR, 2, 0);
    }

    fn epilogue(&mut self) {
        for reg in 0..8u8 {
            self.store(g(reg), BASE, reg * 4);
        }
        self.load64_rsp(T0);
        self.store(CPSR, T0, 0);
        #[cfg(not(windows))]
        {
            for reg in [6u8, 15, 14, 13, 12, 3] {
                self.pop(reg);
            }
        }
        #[cfg(windows)]
        {
            for reg in [2u8, 15, 14, 13, 12, 6, 5, 7, 3] {
                self.pop(reg);
            }
        }
        self.b(0xC3);
    }

    fn emit_op(&mut self, op: &Op) -> bool {
        match *op {
            Op::ShiftImm {
                kind,
                amount,
                rm,
                rd,
            } if kind <= 2 => {
                self.emit_shift(kind, amount, rm, rd);
            }
            Op::AddSub {
                sub,
                imm,
                rhs,
                rn,
                rd,
            } => self.emit_add_sub(sub, imm, rhs, rn, rd),
            Op::Imm { op, rd, imm } => self.emit_imm(op, rd, imm),
            Op::Alu { op, rm, rd } if !matches!(op, 2 | 3 | 4 | 7) => self.emit_alu(op, rm, rd),
            _ => return false,
        }
        true
    }

    fn emit_shift(&mut self, kind: u8, amount: u8, rm: u8, rd: u8) {
        let src = g(rm);
        let dst = g(rd);
        match (kind, amount) {
            (0, 0) => {
                self.mov(RES, src);
                self.mov(dst, RES);
                self.set_nz();
            }
            (0, _) => {
                self.carry_from_bit(src, 32 - u32::from(amount));
                self.mov(RES, src);
                self.sh(4, RES, amount);
                self.mov(dst, RES);
                self.set_nzc();
            }
            (1, 0) => {
                self.carry_from_bit(src, 31);
                self.mov_i(RES, 0);
                self.mov(dst, RES);
                self.set_nzc();
            }
            (1, _) => {
                self.carry_from_bit(src, u32::from(amount) - 1);
                self.mov(RES, src);
                self.sh(5, RES, amount);
                self.mov(dst, RES);
                self.set_nzc();
            }
            (_, 0) => {
                self.carry_from_bit(src, 31);
                self.mov(RES, src);
                self.sh(7, RES, 31);
                self.mov(dst, RES);
                self.set_nzc();
            }
            _ => {
                self.carry_from_bit(src, u32::from(amount) - 1);
                self.mov(RES, src);
                self.sh(7, RES, amount);
                self.mov(dst, RES);
                self.set_nzc();
            }
        }
    }

    fn emit_add_sub(&mut self, sub: bool, imm: bool, rhs: u8, rn: u8, rd: u8) {
        let dst = g(rd);
        let left = g(rn);
        self.mov(RES, left);
        if imm {
            self.alu_i(if sub { 5 } else { 0 }, RES, u32::from(rhs));
        } else if sub {
            self.rr(0x2B, RES, g(rhs));
        } else {
            self.rr(0x03, RES, g(rhs));
        }
        self.mov(dst, RES);
        self.flags_from_host(sub);
    }

    fn emit_imm(&mut self, op: u8, rd: u8, imm: u8) {
        let dst = g(rd);
        let imm = u32::from(imm);
        match op {
            0 => {
                self.mov_i(RES, imm);
                self.mov(dst, RES);
                self.set_nz();
            }
            1 => {
                self.alu_i(7, dst, imm);
                self.flags_from_host(true);
            }
            2 => {
                self.mov(RES, dst);
                self.alu_i(0, RES, imm);
                self.mov(dst, RES);
                self.flags_from_host(false);
            }
            _ => {
                self.mov(RES, dst);
                self.alu_i(5, RES, imm);
                self.mov(dst, RES);
                self.flags_from_host(true);
            }
        }
    }

    fn emit_alu(&mut self, op: u8, rm: u8, rd: u8) {
        let left = g(rd);
        let right = g(rm);
        match op {
            0 => self.logic(0x23, left, right),
            1 => self.logic(0x33, left, right),
            5 => self.adc_sbc(false, left, right),
            6 => self.adc_sbc(true, left, right),
            8 => {
                self.mov(RES, left);
                self.rr(0x23, RES, right);
                self.set_nz();
            }
            9 => {
                self.mov_i(RES, 0);
                self.rr(0x2B, RES, right);
                self.mov(left, RES);
                self.flags_from_host(true);
            }
            10 => {
                self.alu_rr_flags(0x3B, left, right, true);
            }
            11 => {
                self.alu_rr_flags(0x03, left, right, false);
            }
            12 => self.logic(0x0B, left, right),
            13 => {
                self.mov(RES, left);
                self.imul(RES, right);
                self.mov(left, RES);
                self.set_nz();
            }
            14 => {
                self.mov(RES, right);
                self.not(RES);
                self.rr(0x23, RES, left);
                self.mov(left, RES);
                self.set_nz();
            }
            _ => {
                self.mov(RES, right);
                self.not(RES);
                self.mov(left, RES);
                self.set_nz();
            }
        }
    }

    fn logic(&mut self, opcode: u8, left: u8, right: u8) {
        self.mov(RES, left);
        self.rr(opcode, RES, right);
        self.mov(left, RES);
        self.set_nz();
    }

    fn alu_rr_flags(&mut self, opcode: u8, left: u8, right: u8, invert_carry: bool) {
        if opcode == 0x03 {
            self.mov(RES, left);
            self.rr(0x03, RES, right);
        } else {
            self.rr(opcode, left, right);
        }
        self.flags_from_host(invert_carry);
    }

    fn adc_sbc(&mut self, sbc: bool, left: u8, right: u8) {
        self.bt(CPSR, 29);
        if sbc {
            self.b(0xF5);
        }
        self.mov(RES, left);
        self.rr(if sbc { 0x1B } else { 0x13 }, RES, right);
        self.mov(left, RES);
        self.flags_from_host(sbc);
    }

    /// 结果在 `RES`。N、Z 按结果重写，C 和 V 留在客人的 CPSR 里。
    fn set_nz(&mut self) {
        self.alu_i(7, RES, 0);
        self.sete(T0);
        self.movzx(T0);
        self.sh(4, T0, 30);
        self.mov(T1, RES);
        self.sh(5, T1, 31);
        self.sh(4, T1, 31);
        self.rr(0x0B, T0, T1);
        self.mov(T1, CPSR);
        self.sh(5, T1, 28);
        self.mov(T2, T1);
        self.and_i(T2, 1);
        self.sh(5, T1, 1);
        self.and_i(T1, 1);
        self.sh(4, T2, 28);
        self.sh(4, T1, 29);
        self.rr(0x0B, T0, T2);
        self.rr(0x0B, T0, T1);
        self.merge_flags(T0);
    }

    /// 结果在 `RES`，新的 C 在 `T0` 的最低位。V 留在客人的 CPSR 里。
    fn set_nzc(&mut self) {
        self.sh(4, T0, 29);
        self.alu_i(7, RES, 0);
        self.sete(T1);
        self.movzx(T1);
        self.sh(4, T1, 30);
        self.rr(0x0B, T0, T1);
        self.mov(T1, RES);
        self.sh(5, T1, 31);
        self.sh(4, T1, 31);
        self.rr(0x0B, T0, T1);
        self.mov(T1, CPSR);
        self.sh(5, T1, 28);
        self.and_i(T1, 1);
        self.sh(4, T1, 28);
        self.rr(0x0B, T0, T1);
        self.merge_flags(T0);
    }

    /// x86 的 CF 在减法里是借位。ARM 的 C 是「没有借位」，减法要反过来。
    fn flags_from_host(&mut self, invert_carry: bool) {
        self.b(0x9C);
        self.pop(T0);
        self.mov(T1, T0);
        self.sh(5, T1, 6);
        self.and_i(T1, 1);
        self.sh(4, T1, 30);
        self.mov(T2, T0);
        self.sh(5, T2, 7);
        self.and_i(T2, 1);
        self.sh(4, T2, 31);
        self.rr(0x0B, T1, T2);
        self.mov(T2, T0);
        self.and_i(T2, 1);
        self.sh(4, T2, 29);
        if invert_carry {
            self.xor_i(T2, 1 << 29);
        }
        self.rr(0x0B, T1, T2);
        self.mov(T2, T0);
        self.sh(5, T2, 11);
        self.and_i(T2, 1);
        self.sh(4, T2, 28);
        self.rr(0x0B, T1, T2);
        self.merge_flags(T1);
    }

    fn merge_flags(&mut self, flags: u8) {
        self.alu_i(4, CPSR, 0x0FFF_FFFF);
        self.rr(0x0B, CPSR, flags);
    }

    fn carry_from_bit(&mut self, src: u8, bit: u32) {
        self.mov(T0, src);
        if bit == 0 {
            self.and_i(T0, 1);
        } else if bit >= 31 {
            self.sh(5, T0, 31);
        } else {
            self.sh(5, T0, bit as u8);
            self.and_i(T0, 1);
        }
    }

    fn b(&mut self, byte: u8) {
        self.bytes.push(byte);
    }

    fn rex(&mut self, reg: u8, rm: u8) {
        let byte = 0x40 | (u8::from(reg >= 8) << 2) | u8::from(rm >= 8);
        if byte != 0x40 {
            self.b(byte);
        }
    }

    fn rex_b(&mut self, rm: u8) {
        if rm >= 8 {
            self.b(0x41);
        }
    }

    fn modrm(&mut self, mode: u8, reg: u8, rm: u8) {
        self.b((mode << 6) | ((reg & 7) << 3) | (rm & 7));
    }

    fn rr(&mut self, opcode: u8, dst: u8, src: u8) {
        self.rex(dst, src);
        self.b(opcode);
        self.modrm(3, dst, src);
    }

    fn mov(&mut self, dst: u8, src: u8) {
        if dst != src {
            self.rr(0x8B, dst, src);
        }
    }

    fn mov_i(&mut self, rd: u8, imm: u32) {
        self.rex_b(rd);
        self.b(0xB8 + (rd & 7));
        self.bytes.extend_from_slice(&imm.to_le_bytes());
    }

    fn alu_i(&mut self, ext: u8, rm: u8, imm: u32) {
        if imm <= 127 && ext != 4 {
            self.rex_b(rm);
            self.b(0x83);
            self.modrm(3, ext, rm);
            self.b(imm as u8);
            return;
        }
        self.rex_b(rm);
        self.b(0x81);
        self.modrm(3, ext, rm);
        self.bytes.extend_from_slice(&imm.to_le_bytes());
    }

    fn and_i(&mut self, rm: u8, imm: u8) {
        self.rex_b(rm);
        self.b(0x83);
        self.modrm(3, 4, rm);
        self.b(imm);
    }

    fn xor_i(&mut self, rm: u8, imm: u32) {
        self.rex_b(rm);
        self.b(0x81);
        self.modrm(3, 6, rm);
        self.bytes.extend_from_slice(&imm.to_le_bytes());
    }

    fn sh(&mut self, ext: u8, rm: u8, amount: u8) {
        self.rex_b(rm);
        self.b(0xC1);
        self.modrm(3, ext, rm);
        self.b(amount);
    }

    fn load(&mut self, dst: u8, base: u8, disp: u8) {
        self.rex(dst, base);
        self.b(0x8B);
        self.addr(dst, base, disp);
    }

    fn store(&mut self, src: u8, base: u8, disp: u8) {
        self.rex(src, base);
        self.b(0x89);
        self.addr(src, base, disp);
    }

    fn addr(&mut self, reg: u8, base: u8, disp: u8) {
        if disp == 0 {
            self.modrm(0, reg, base);
        } else {
            self.modrm(1, reg, base);
            self.b(disp);
        }
    }

    fn load64_rsp(&mut self, dst: u8) {
        self.b(0x48 | (u8::from(dst >= 8) << 2));
        self.b(0x8B);
        self.modrm(0, dst, 4);
        self.b(0x24);
    }

    fn push(&mut self, reg: u8) {
        self.rex_b(reg);
        self.b(0x50 + (reg & 7));
    }

    fn pop(&mut self, reg: u8) {
        self.rex_b(reg);
        self.b(0x58 + (reg & 7));
    }

    fn bt(&mut self, rm: u8, bit: u8) {
        self.rex_b(rm);
        self.b(0x0F);
        self.b(0xBA);
        self.modrm(3, 4, rm);
        self.b(bit);
    }

    fn sete(&mut self, reg: u8) {
        self.rex_b(reg);
        self.b(0x0F);
        self.b(0x94);
        self.modrm(3, 0, reg);
    }

    fn movzx(&mut self, reg: u8) {
        self.rex(reg, reg);
        self.b(0x0F);
        self.b(0xB6);
        self.modrm(3, reg, reg);
    }

    fn imul(&mut self, dst: u8, src: u8) {
        self.rex(dst, src);
        self.b(0x0F);
        self.b(0xAF);
        self.modrm(3, dst, src);
    }

    fn not(&mut self, rm: u8) {
        self.rex_b(rm);
        self.b(0xF7);
        self.modrm(3, 2, rm);
    }
}

pub(crate) fn encode(halfs: &[u16]) -> Option<Vec<u8>> {
    if !(2..=8).contains(&halfs.len()) {
        return None;
    }
    let mut asm = Asm { bytes: Vec::new() };
    asm.prologue();
    for half in halfs {
        if !asm.emit_op(&thumb::decode(*half)) {
            return None;
        }
    }
    asm.epilogue();
    if asm.bytes.len() > 4096 {
        return None;
    }
    Some(asm.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn prologue_loads_low_regs_and_cpsr() {
        let bytes = encode(&[0x2001, 0x2102]).expect("两条 mov 应该能译");
        assert_eq!(
            &bytes[..32],
            &[
                0x53, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57, 0x56, 0x8B, 0x1F, 0x44, 0x8B,
                0x67, 0x04, 0x44, 0x8B, 0x6F, 0x08, 0x44, 0x8B, 0x77, 0x0C, 0x44, 0x8B, 0x7F, 0x10,
                0x44, 0x8B, 0x47, 0x14,
            ]
        );
        assert_eq!(
            &bytes[bytes.len() - 17..],
            &[
                0x48, 0x8B, 0x04, 0x24, 0x89, 0x30, 0x5E, 0x41, 0x5F, 0x41, 0x5E, 0x41, 0x5D, 0x41,
                0x5C, 0x5B, 0xC3,
            ]
        );
        assert!(
            bytes
                .windows(6)
                .any(|window| window == [0x41, 0xBB, 0x01, 0x00, 0x00, 0x00]),
            "mov r11d, 1"
        );
    }
}
