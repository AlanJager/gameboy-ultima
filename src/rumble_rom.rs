//! 震动实验卡。游戏代码 `V49J`，只打开马达。
//!
//! 画面是一张固定的靶。马达每 16 帧转、16 帧停。靶本身不滚动，
//! 开着「画面震动」时整幅画面跟着晃。左上角的灯亮表示马达正在转。

use crate::asm::{self, Shift};
use crate::bus::Bus;
use crate::cpu::step_quiet;
use crate::registers::Registers;

const CODE: u32 = 0x0800_0100;
const AL: u8 = 0xE;
const EQ: u8 = 0;
const NE: u8 = 1;
const HS: u8 = 2;
const LO: u8 = 3;

const WIDTH: i32 = 240;
const HEIGHT: i32 = 160;
const LAMP_X: i32 = 10;
const LAMP_Y: i32 = 10;
const LAMP_W: i32 = 28;
const LAMP_H: i32 = 16;

const DESK: u16 = rgb(2, 3, 8);
const GRID: u16 = rgb(7, 11, 16);
const EDGE: u16 = rgb(28, 26, 18);
const RING: u16 = rgb(18, 24, 28);
const CROSS: u16 = rgb(31, 31, 24);
const HUB: u16 = rgb(31, 8, 6);
const LAMP_OFF: u16 = rgb(10, 2, 2);
const LAMP_ON: u16 = rgb(31, 4, 2);
const BEZEL: u16 = rgb(20, 16, 8);

const fn rgb(r: u16, g: u16, b: u16) -> u16 {
    (r & 31) | ((g & 31) << 5) | ((b & 31) << 10)
}

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

    fn mov_reg_if(&mut self, cond: u8, rd: u8, rm: u8) {
        self.emit(asm::dp_reg(cond, 13, false, 0, rd, rm, Shift::Lsl, 0));
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

    fn cmpi(&mut self, rn: u8, value: u32) {
        self.emit(asm::dp_imm(AL, 10, true, rn, 0, value).expect("cmp"));
    }

    fn tst(&mut self, rn: u8, value: u32) {
        self.emit(asm::dp_imm(AL, 8, true, rn, 0, value).expect("tst"));
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

    fn strw(&mut self, rn: u8, rd: u8, offset: u16) {
        self.emit(asm::mem_imm(
            AL, false, false, true, true, false, rn, rd, offset,
        ));
    }

    fn str_post(&mut self, rn: u8, rd: u8) {
        self.emit(asm::mem_imm(AL, false, false, false, true, false, rn, rd, 4));
    }

    fn ldr_post(&mut self, rd: u8, rn: u8) {
        self.emit(asm::mem_imm(AL, true, false, false, true, false, rn, rd, 4));
    }

    fn b(&mut self, target: u32) {
        let from = self.here();
        self.emit(asm::branch_address(AL, false, from, target));
    }

    fn b_if(&mut self, cond: u8, target: u32) {
        let from = self.here();
        self.emit(asm::branch_address(cond, false, from, target));
    }

    fn hole(&mut self) -> usize {
        let index = self.words.len();
        self.words.push(0);
        index
    }

    fn patch(&mut self, index: usize, target: u32) {
        let from = CODE + index as u32 * 4;
        self.words[index] = asm::branch_address(AL, false, from, target);
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

    /// `ldr rd, [pc, #0]` 后面紧跟一条跳过字面量的分支。字面量在指令地址 + 8。
    fn pool(&mut self, rd: u8) -> usize {
        self.emit(asm::mem_imm(AL, true, false, true, true, false, 15, rd, 0));
        let skip = self.hole();
        let data = self.words.len();
        self.words.push(0);
        let after = self.here();
        self.patch(skip, after);
        data
    }
}

pub fn rumble_test_rom() -> Vec<u8> {
    let mut code = Asm::new();

    code.movi(2, 0x0400_0000);
    code.movi(0, 0x400);
    code.orr(0, 0, 3);
    code.strw(2, 0, 0);

    code.movi(4, 0x0800_0000);
    code.add(4, 4, 0xC4);
    code.movi(0, 1);
    code.strh(4, 0, 4);
    code.movi(0, 8);
    code.strh(4, 0, 2);

    let asset = code.pool(0);
    code.movi(8, 0x0600_0000);
    code.movi(11, 160);
    let copy_row = code.here();
    code.movi(6, 120);
    let copy_word = code.here();
    code.ldr_post(9, 0);
    code.str_post(8, 9);
    code.subs(6, 6, 1);
    code.b_if(NE, copy_word);
    code.subs(11, 11, 1);
    code.b_if(NE, copy_row);

    code.color(7, u32::from(LAMP_ON));
    code.color(9, u32::from(LAMP_OFF));
    code.movi(1, 0x0300_0000);
    code.movi(5, 0);

    let frame = code.here();
    let wait_in = code.here();
    code.ldrh(0, 2, 6);
    code.cmpi(0, 160);
    code.b_if(LO, wait_in);
    let wait_out = code.here();
    code.ldrh(0, 2, 6);
    code.cmpi(0, 160);
    code.b_if(HS, wait_out);

    code.add(5, 5, 1);
    code.strh(1, 5, 0);
    code.tst(5, 16);
    code.movi_if(EQ, 0, 0);
    code.movi_if(NE, 0, 8);
    code.strh(4, 0, 0);
    code.mov_reg_if(AL, 0, 9);
    code.mov_reg_if(NE, 0, 7);

    // 灯在 (10, 10)，28×16。字节偏移 10*480 + 20 = 0x12D4。
    code.movi(8, 0x0600_0000);
    code.add(8, 8, 0x1000);
    code.add(8, 8, 0x2C0);
    code.add(8, 8, 0x14);
    code.movi(11, LAMP_H as u32);
    let lamp_row = code.here();
    code.movi(6, (LAMP_W / 2) as u32);
    let lamp_word = code.here();
    code.str_post(8, 0);
    code.subs(6, 6, 1);
    code.b_if(NE, lamp_word);
    code.add(8, 8, 0x1A0);
    code.add(8, 8, 8);
    code.subs(11, 11, 1);
    code.b_if(NE, lamp_row);
    code.b(frame);

    let asset_addr = CODE + code.words.len() as u32 * 4;
    code.words[asset] = asset_addr;
    let mut body = Vec::with_capacity(code.words.len() * 4 + WIDTH as usize * HEIGHT as usize * 2);
    for word in code.words {
        body.extend(word.to_le_bytes());
    }
    for color in picture() {
        body.extend(color.to_le_bytes());
    }
    finish_header(&body)
}

fn picture() -> Vec<u16> {
    let mut px = vec![DESK; (WIDTH * HEIGHT) as usize];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            if x % 16 == 0 || y % 16 == 0 {
                put(&mut px, x, y, GRID);
            }
        }
    }
    for t in 0..2 {
        hline(&mut px, t, EDGE);
        hline(&mut px, HEIGHT - 1 - t, EDGE);
        vline(&mut px, t, EDGE);
        vline(&mut px, WIDTH - 1 - t, EDGE);
    }
    for radius in [54, 36, 18] {
        ring(&mut px, 120, 80, radius, RING);
    }
    for t in 0..2 {
        hline(&mut px, 80 + t, CROSS);
        vline(&mut px, 120 + t, CROSS);
    }
    fill(&mut px, 118, 78, 4, 4, HUB);
    fill(&mut px, LAMP_X - 2, LAMP_Y - 2, LAMP_W + 4, LAMP_H + 4, BEZEL);
    fill(&mut px, LAMP_X, LAMP_Y, LAMP_W, LAMP_H, LAMP_OFF);
    px
}

fn put(px: &mut [u16], x: i32, y: i32, color: u16) {
    if (0..WIDTH).contains(&x) && (0..HEIGHT).contains(&y) {
        px[(y * WIDTH + x) as usize] = color;
    }
}

fn fill(px: &mut [u16], x: i32, y: i32, w: i32, h: i32, color: u16) {
    for yy in y..y + h {
        for xx in x..x + w {
            put(px, xx, yy, color);
        }
    }
}

fn hline(px: &mut [u16], y: i32, color: u16) {
    for x in 0..WIDTH {
        put(px, x, y, color);
    }
}

fn vline(px: &mut [u16], x: i32, color: u16) {
    for y in 0..HEIGHT {
        put(px, x, y, color);
    }
}

fn ring(px: &mut [u16], cx: i32, cy: i32, radius: i32, color: u16) {
    let outer = radius * radius;
    let inner = (radius - 1) * (radius - 1);
    for y in cy - radius..=cy + radius {
        for x in cx - radius..=cx + radius {
            let dx = x - cx;
            let dy = y - cy;
            let dist = dx * dx + dy * dy;
            if dist <= outer && dist > inner {
                put(px, x, y, color);
            }
        }
    }
}

fn finish_header(code: &[u8]) -> Vec<u8> {
    let mut rom = vec![0u8; CODE as usize - 0x0800_0000];
    let branch = asm::branch_address(AL, false, 0x0800_0000, CODE);
    rom[..4].copy_from_slice(&branch.to_le_bytes());
    rom[4..0xA0].copy_from_slice(&include_bytes!("../roms/arm.gba")[4..0xA0]);
    rom[0xA0..0xAC].copy_from_slice(b"RUMBLE TEST\0");
    rom[0xAC..0xB0].copy_from_slice(b"V49J");
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

    fn pixel(bus: &Bus, x: u32, y: u32) -> u16 {
        bus.read_u16(0x0600_0000 + y * 480 + x * 2)
    }

    fn run_until(regs: &mut Registers, bus: &mut Bus, mut done: impl FnMut(&mut Bus) -> bool) {
        for _ in 0..6_000_000 {
            step_quiet(regs, bus).expect("step");
            if done(bus) {
                return;
            }
        }
        panic!(
            "pc {:08x} frame {} hub {:04x} lamp {:04x}",
            regs.pc(),
            bus.read_u16(0x0300_0000),
            pixel(bus, 120, 80),
            pixel(bus, LAMP_X as u32, LAMP_Y as u32),
        );
    }

    #[test]
    fn the_motor_pulses_and_the_target_stays_put() {
        let rom = rumble_test_rom();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("roms/rumble.gba");
        std::fs::write(&path, &rom).expect("write rumble.gba");

        let drawn = picture();
        let hub_at = (80 * WIDTH + 120) as usize;
        let lamp_at = (LAMP_Y * WIDTH + LAMP_X) as usize;
        assert_eq!(drawn[hub_at], HUB);
        assert_eq!(drawn[lamp_at], LAMP_OFF);
        assert_eq!(drawn[0], EDGE);

        let mut bus = Bus::new();
        bus.load_rom(&rom);
        assert!(bus.rumble_present());
        assert!(!bus.solar_present());
        let mut regs = Registers::arm_start(0x0800_0000);

        run_until(&mut regs, &mut bus, |bus| {
            pixel(bus, 120, 80) == HUB
                && pixel(bus, 0, 0) == EDGE
                && pixel(bus, LAMP_X as u32, LAMP_Y as u32) == LAMP_ON
                && bus.read_u16(0x0300_0000) >= 16
                && bus.take_rumble()
        });

        run_until(&mut regs, &mut bus, |bus| {
            pixel(bus, LAMP_X as u32, LAMP_Y as u32) == LAMP_OFF
                && pixel(bus, 120, 80) == HUB
                && !bus.take_rumble()
        });
    }
}
