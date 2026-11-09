//! 传感器实验卡。游戏代码 `ULTS` 同时打开陀螺仪、倾斜和太阳能。
//!
//! 四条色带从上到下是陀螺仪、倾斜 X、倾斜 Y、日光。色块的位置就是读数。
//! 工作内存里也留着原始值：`0x00` 陀螺仪，`0x02` 倾斜 X，`0x04` 倾斜 Y，`0x06` 日光计数。

use crate::asm::{self, Shift};
use crate::bus::Bus;
use crate::cpu::step_quiet;
use crate::registers::Registers;

const CODE: u32 = 0x0800_0100;
const AL: u8 = 0xE;
const NE: u8 = 1;
const HI: u8 = 8;

const GYRO_Y: u32 = 24;
const TILT_X_Y: u32 = 64;
const TILT_Y_Y: u32 = 104;
const SOLAR_Y: u32 = 144;

struct Asm {
    words: Vec<u32>,
}

impl Asm {
    fn new() -> Self {
        Self { words: Vec::new() }
    }

    fn here(&self) -> u32 {
        CODE + self.words.len() as u32 * 4
    }

    fn emit(&mut self, word: u32) {
        self.words.push(word);
    }

    fn movi(&mut self, rd: u8, value: u32) {
        self.emit(asm::dp_imm(AL, 13, false, 0, rd, value).expect("mov"));
    }

    fn movi_if(&mut self, cond: u8, rd: u8, value: u32) {
        self.emit(asm::dp_imm(cond, 13, false, 0, rd, value).expect("mov"));
    }

    fn mov_lsl(&mut self, rd: u8, rm: u8, amount: u8) {
        self.emit(asm::dp_reg(AL, 13, false, 0, rd, rm, Shift::Lsl, amount));
    }

    fn mov_lsr(&mut self, rd: u8, rm: u8, amount: u8) {
        self.emit(asm::dp_reg(AL, 13, false, 0, rd, rm, Shift::Lsr, amount));
    }

    fn orr(&mut self, rd: u8, rn: u8, value: u32) {
        self.emit(asm::dp_imm(AL, 12, false, rn, rd, value).expect("orr"));
    }

    fn orr_lsl(&mut self, rd: u8, rn: u8, rm: u8, amount: u8) {
        self.emit(asm::dp_reg(AL, 12, false, rn, rd, rm, Shift::Lsl, amount));
    }

    fn add(&mut self, rd: u8, rn: u8, value: u32) {
        self.emit(asm::dp_imm(AL, 4, false, rn, rd, value).expect("add"));
    }

    fn add_reg(&mut self, rd: u8, rn: u8, rm: u8) {
        self.emit(asm::dp_reg(AL, 4, false, rn, rd, rm, Shift::Lsl, 0));
    }

    fn sub(&mut self, rd: u8, rn: u8, value: u32) {
        self.emit(asm::dp_imm(AL, 2, false, rn, rd, value).expect("sub"));
    }

    fn sub_reg(&mut self, rd: u8, rn: u8, rm: u8) {
        self.emit(asm::dp_reg(AL, 2, false, rn, rd, rm, Shift::Lsl, 0));
    }

    fn cmpi(&mut self, rn: u8, value: u32) {
        self.emit(asm::dp_imm(AL, 10, true, rn, 0, value).expect("cmp"));
    }

    fn tst(&mut self, rn: u8, value: u32) {
        self.emit(asm::dp_imm(AL, 8, true, rn, 0, value).expect("tst"));
    }

    fn andi(&mut self, rd: u8, rn: u8, value: u32) {
        self.emit(asm::dp_imm(AL, 0, false, rn, rd, value).expect("and"));
    }

    fn bic(&mut self, rd: u8, rn: u8, value: u32) {
        self.emit(asm::dp_imm(AL, 14, false, rn, rd, value).expect("bic"));
    }

    fn subs(&mut self, rd: u8, rn: u8, value: u32) {
        self.emit(asm::dp_imm(AL, 2, true, rn, rd, value).expect("sub"));
    }

    fn strh(&mut self, rn: u8, rd: u8, offset: u8) {
        self.emit(asm::half_imm(AL, false, true, true, false, rn, rd, offset));
    }

    fn ldrh(&mut self, rd: u8, rn: u8, offset: u8) {
        self.emit(asm::half_imm(AL, true, true, true, false, rn, rd, offset));
    }

    fn strb(&mut self, rn: u8, rd: u8, offset: u16) {
        self.emit(asm::mem_imm(
            AL, false, true, true, true, false, rn, rd, offset,
        ));
    }

    fn ldrb(&mut self, rd: u8, rn: u8, offset: u16) {
        self.emit(asm::mem_imm(AL, true, true, true, true, false, rn, rd, offset));
    }

    fn strw(&mut self, rn: u8, rd: u8, offset: u16) {
        self.emit(asm::mem_imm(
            AL, false, false, true, true, false, rn, rd, offset,
        ));
    }

    fn str_post(&mut self, rn: u8, rd: u8) {
        self.emit(asm::mem_imm(AL, false, false, false, true, false, rn, rd, 4));
    }

    fn b(&mut self, target: u32) {
        let from = self.here();
        self.emit(asm::branch_address(AL, false, from, target));
    }

    fn bne(&mut self, target: u32) {
        let from = self.here();
        self.emit(asm::branch_address(NE, false, from, target));
    }

    fn hole(&mut self) -> usize {
        let index = self.words.len();
        self.words.push(0);
        index
    }

    fn patch(&mut self, index: usize, link: bool, target: u32) {
        let from = CODE + index as u32 * 4;
        self.words[index] = asm::branch_address(AL, link, from, target);
    }

    fn color(&mut self, rd: u8, rgb: u32) {
        let lo = rgb & 0xFF;
        let hi = rgb & 0xFF00;
        if lo == 0 {
            self.movi(rd, hi);
        } else {
            self.movi(rd, lo);
            if hi != 0 {
                self.orr(rd, rd, hi);
            }
        }
        self.orr_lsl(rd, rd, rd, 16);
    }

    fn stash(&mut self, value: u8, offset: u8) {
        self.movi(8, 0x0300_0000);
        self.strh(8, value, offset);
    }
}

pub fn sensor_test_rom() -> Vec<u8> {
    let mut code = Asm::new();

    code.movi(2, 0x0400_0000);
    code.movi(0, 0x400);
    code.orr(0, 0, 3);
    code.strw(2, 0, 0);
    code.movi(3, 0x0600_0000);
    code.movi(4, 0x0800_0000);
    code.add(4, 4, 0xC4);
    code.movi(7, 0x0E00_0000);
    code.add(7, 7, 0x8000);

    let head = code.here();
    let to_gyro = code.hole();
    code.mov_lsr(1, 0, 4);
    code.sub(1, 1, 0x30);
    code.movi(10, GYRO_Y);
    code.color(9, 0x03E0);
    let paint_gyro = code.hole();

    let to_latch = code.hole();
    let to_tilt_x = code.hole();
    code.mov_lsr(1, 0, 3);
    code.sub(1, 1, 0x40);
    code.movi(10, TILT_X_Y);
    code.color(9, 0x7C00);
    let paint_tilt_x = code.hole();

    let to_tilt_y = code.hole();
    code.mov_lsr(1, 0, 3);
    code.sub(1, 1, 0x40);
    code.movi(10, TILT_Y_Y);
    code.color(9, 0x001F);
    let paint_tilt_y = code.hole();

    let to_solar = code.hole();
    code.mov_lsl(1, 0, 0);
    code.cmpi(1, 220);
    code.movi_if(HI, 1, 220);
    code.movi(10, SOLAR_Y);
    code.color(9, 0x7FFF);
    let paint_solar = code.hole();
    code.b(head);

    let gyro = code.here();
    code.patch(to_gyro, true, gyro);
    code.movi(0, 1);
    code.strh(4, 0, 4);
    code.movi(0, 3);
    code.strh(4, 0, 2);
    code.movi(0, 2);
    code.strh(4, 0, 0);
    code.movi(0, 3);
    code.strh(4, 0, 0);
    code.ldrh(0, 4, 0);
    code.mov_lsr(1, 0, 2);
    code.andi(1, 1, 1);
    code.movi(0, 2);
    code.strh(4, 0, 0);
    code.movi(6, 15);
    let bit = code.here();
    code.movi(0, 0);
    code.strh(4, 0, 0);
    code.ldrh(0, 4, 0);
    code.mov_lsr(0, 0, 2);
    code.andi(0, 0, 1);
    code.mov_lsl(1, 1, 1);
    code.orr_lsl(1, 1, 0, 0);
    code.movi(0, 2);
    code.strh(4, 0, 0);
    code.subs(6, 6, 1);
    code.bne(bit);
    code.mov_lsl(0, 1, 0);
    code.stash(0, 0);
    code.mov_lsl(15, 14, 0);

    let latch = code.here();
    code.patch(to_latch, true, latch);
    code.movi(0, 0x55);
    code.strb(7, 0, 0);
    code.movi(0, 0xAA);
    code.strb(7, 0, 0x100);
    code.mov_lsl(15, 14, 0);

    let tilt_x = code.here();
    code.patch(to_tilt_x, true, tilt_x);
    code.ldrb(0, 7, 0x200);
    code.ldrb(1, 7, 0x300);
    code.andi(1, 1, 0x0F);
    code.orr_lsl(0, 0, 1, 8);
    code.stash(0, 2);
    code.mov_lsl(15, 14, 0);

    let tilt_y = code.here();
    code.patch(to_tilt_y, true, tilt_y);
    code.ldrb(0, 7, 0x400);
    code.ldrb(1, 7, 0x500);
    code.andi(1, 1, 0x0F);
    code.orr_lsl(0, 0, 1, 8);
    code.stash(0, 4);
    code.mov_lsl(15, 14, 0);

    let solar = code.here();
    code.patch(to_solar, true, solar);
    code.movi(0, 1);
    code.strh(4, 0, 4);
    code.movi(0, 7);
    code.strh(4, 0, 2);
    code.movi(0, 2);
    code.strh(4, 0, 0);
    code.movi(0, 0);
    code.strh(4, 0, 0);
    code.movi(1, 0);
    let poll = code.here();
    code.movi(0, 1);
    code.strh(4, 0, 0);
    code.movi(0, 0);
    code.strh(4, 0, 0);
    code.ldrh(0, 4, 0);
    code.tst(0, 8);
    let solar_done = code.hole();
    code.add(1, 1, 1);
    code.cmpi(1, 0x100);
    code.bne(poll);
    let solar_out = code.here();
    let from = CODE + solar_done as u32 * 4;
    code.words[solar_done] = asm::branch_address(NE, false, from, solar_out);
    code.mov_lsl(0, 1, 0);
    code.stash(0, 6);
    code.mov_lsl(15, 14, 0);

    let paint = code.here();
    for hole in [paint_gyro, paint_tilt_x, paint_tilt_y, paint_solar] {
        code.patch(hole, true, paint);
    }
    code.mov_lsl(2, 14, 0);
    code.bic(1, 1, 1);
    code.mov_lsl(0, 9, 0);
    let fill_clear = emit_row(&mut code, true);
    code.mov_lsl(9, 0, 0);
    let fill_mark = emit_row(&mut code, false);
    code.mov_lsl(15, 2, 0);

    let fill = code.here();
    code.patch(fill_clear, true, fill);
    code.patch(fill_mark, true, fill);
    let row = code.here();
    code.mov_lsl(6, 5, 0);
    let pixel = code.here();
    code.str_post(8, 9);
    code.subs(6, 6, 1);
    code.bne(pixel);
    code.add_reg(8, 8, 12);
    code.subs(11, 11, 1);
    code.bne(row);
    code.mov_lsl(15, 14, 0);

    let mut body = Vec::with_capacity(code.words.len() * 4);
    for word in code.words {
        body.extend(word.to_le_bytes());
    }
    finish_header(&body)
}

fn emit_row(code: &mut Asm, clear: bool) -> usize {
    code.mov_lsl(8, 10, 9);
    code.mov_lsl(5, 10, 5);
    code.sub_reg(8, 8, 5);
    code.add_reg(8, 8, 3);
    if clear {
        code.movi(9, 0);
        code.movi(5, 120);
        code.movi(11, 16);
        code.movi(12, 0);
    } else {
        code.mov_lsl(5, 1, 1);
        code.add_reg(8, 8, 5);
        code.movi(5, 8);
        code.movi(11, 16);
        code.movi(12, 0x1C0);
    }
    code.hole()
}

fn finish_header(code: &[u8]) -> Vec<u8> {
    let mut rom = vec![0u8; CODE as usize - 0x0800_0000];
    let branch = asm::branch_address(AL, false, 0x0800_0000, CODE);
    rom[..4].copy_from_slice(&branch.to_le_bytes());
    rom[4..0xA0].copy_from_slice(&include_bytes!("../roms/arm.gba")[4..0xA0]);
    rom[0xA0..0xAC].copy_from_slice(b"SENSOR LAB\0\0");
    rom[0xAC..0xB0].copy_from_slice(b"ULTS");
    rom[0xB0..0xB2].copy_from_slice(b"UT");
    rom[0xB2] = 0x96;
    let sum = rom[0xA0..=0xBC]
        .iter()
        .fold(0u8, |sum, byte| sum.wrapping_add(*byte));
    rom[0xBD] = sum.wrapping_neg();
    rom.extend_from_slice(code);
    rom
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gyro_sample(rate: i8) -> u16 {
        0x700u16.wrapping_add((i16::from(rate) * 0x180) as u16)
    }

    fn tilt_sample(rate: i8) -> u16 {
        0x3A0u16.wrapping_add((i16::from(rate) * 0xE0) as u16)
    }

    fn solar_count(brightness: u8) -> u16 {
        (0xFFu16 - u16::from(brightness)).saturating_sub(1)
    }

    fn gyro_x(sample: u16) -> u32 {
        u32::from(sample >> 4) - 0x30
    }

    fn tilt_x(sample: u16) -> u32 {
        u32::from(sample >> 3) - 0x40
    }

    fn pixel(bus: &Bus, x: u32, y: u32) -> u16 {
        bus.read_u16(0x0600_0000 + y * 480 + x * 2)
    }

    fn run_until(regs: &mut Registers, bus: &mut Bus, mut done: impl FnMut(&Bus) -> bool) {
        for _ in 0..400_000 {
            step_quiet(regs, bus).expect("step");
            if done(bus) {
                return;
            }
        }
        panic!(
            "pc {:08x} samples {:04x} {:04x} {:04x} {:04x}",
            regs.pc(),
            bus.read_u16(0x0300_0000),
            bus.read_u16(0x0300_0002),
            bus.read_u16(0x0300_0004),
            bus.read_u16(0x0300_0006),
        );
    }

    #[test]
    fn the_lab_cart_moves_gyro_tilt_and_sunlight_together() {
        let rom = sensor_test_rom();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("roms/sensors.gba");
        std::fs::write(&path, &rom).expect("write sensors.gba");

        let mut bus = Bus::new();
        bus.load_rom(&rom);
        assert!(bus.solar_present());
        bus.set_gyro_rate(0);
        bus.set_tilt(0, 0);
        bus.set_solar(128);
        let mut regs = Registers::arm_start(0x0800_0000);
        let gyro = gyro_sample(0);
        let tilt = tilt_sample(0);
        let sun = solar_count(128);
        run_until(&mut regs, &mut bus, |bus| {
            bus.read_u16(0x0300_0000) == gyro
                && bus.read_u16(0x0300_0002) == tilt
                && bus.read_u16(0x0300_0004) == tilt
                && bus.read_u16(0x0300_0006) == sun
                && pixel(bus, gyro_x(gyro), GYRO_Y) == 0x03E0
                && pixel(bus, tilt_x(tilt), TILT_X_Y) == 0x7C00
                && pixel(bus, tilt_x(tilt), TILT_Y_Y) == 0x001F
                && pixel(bus, u32::from(sun), SOLAR_Y) == 0x7FFF
        });

        bus.set_gyro_rate(1);
        bus.set_tilt(-1, 1);
        bus.set_solar(255);
        let gyro = gyro_sample(1);
        let tilt_left = tilt_sample(-1);
        let tilt_down = tilt_sample(1);
        let still = gyro_sample(0);
        run_until(&mut regs, &mut bus, |bus| {
            bus.read_u16(0x0300_0000) == gyro
                && bus.read_u16(0x0300_0002) == tilt_left
                && bus.read_u16(0x0300_0004) == tilt_down
                && bus.read_u16(0x0300_0006) == 0
                && pixel(bus, gyro_x(gyro), GYRO_Y) == 0x03E0
                && pixel(bus, gyro_x(still), GYRO_Y) == 0
                && pixel(bus, tilt_x(tilt_left), TILT_X_Y) == 0x7C00
                && pixel(bus, tilt_x(tilt_down), TILT_Y_Y) == 0x001F
                && pixel(bus, 0, SOLAR_Y) == 0x7FFF
        });
    }
}
