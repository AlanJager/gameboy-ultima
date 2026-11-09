//! 一张自己画的联机测试卡。没有 GBA 编译器，指令直接编码。
//!
//! 1 号机每次送出 `0x0001`。2、3、4 号机分别送出 `0x1111`、`0x2222`、`0x3333`。
//! 画面上方的色块是自己的机号，下面四条是四个座位：黑的是空位。

use crate::asm::{self, Shift};
use crate::bus::Bus;
use crate::cpu::step_quiet;
use crate::registers::Registers;

const CODE: u32 = 0x0800_00C0;
const AL: u8 = 0xE;
const NE: u8 = 1;

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

    fn movr(&mut self, rd: u8, rm: u8, amount: u8) {
        self.emit(asm::dp_reg(AL, 13, false, 0, rd, rm, Shift::Lsl, amount));
    }

    fn movr_if(&mut self, cond: u8, rd: u8, rm: u8) {
        self.emit(asm::dp_reg(cond, 13, false, 0, rd, rm, Shift::Lsl, 0));
    }

    fn orr(&mut self, rd: u8, rn: u8, value: u32) {
        self.emit(asm::dp_imm(AL, 12, false, rn, rd, value).expect("orr"));
    }

    fn orr_lsl(&mut self, rd: u8, rn: u8, rm: u8, amount: u8) {
        self.emit(asm::dp_reg(
            AL,
            12,
            false,
            rn,
            rd,
            rm,
            Shift::Lsl,
            amount,
        ));
    }

    fn add(&mut self, rd: u8, rn: u8, value: u32) {
        self.emit(asm::dp_imm(AL, 4, false, rn, rd, value).expect("add"));
    }

    fn add_reg(&mut self, rd: u8, rn: u8, rm: u8) {
        self.emit(asm::dp_reg(AL, 4, false, rn, rd, rm, Shift::Lsl, 0));
    }

    fn cmp(&mut self, rn: u8, rm: u8) {
        self.emit(asm::dp_reg(AL, 10, true, rn, 0, rm, Shift::Lsl, 0));
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

    fn subs(&mut self, rd: u8, rn: u8, value: u32) {
        self.emit(asm::dp_imm(AL, 2, true, rn, rd, value).expect("sub"));
    }

    fn strh(&mut self, rn: u8, rd: u8, offset: u8) {
        self.emit(asm::half_imm(AL, false, true, true, false, rn, rd, offset));
    }

    fn ldrh(&mut self, rd: u8, rn: u8, offset: u8) {
        self.emit(asm::half_imm(AL, true, true, true, false, rn, rd, offset));
    }

    fn str_post(&mut self, rn: u8, rd: u8) {
        self.emit(asm::mem_imm(AL, false, false, false, true, false, rn, rd, 4));
    }

    fn strw(&mut self, rn: u8, rd: u8, offset: u16) {
        self.emit(asm::mem_imm(
            AL, false, false, true, true, false, rn, rd, offset,
        ));
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

    fn patch(&mut self, index: usize, cond: u8, link: bool, target: u32) {
        let from = CODE + index as u32 * 4;
        self.words[index] = asm::branch_address(cond, link, from, target);
    }
}

pub fn link_test_rom() -> Vec<u8> {
    let mut code = Asm::new();

    code.movi(2, 0x0400_0000);
    code.movi(3, 0x0600_0000);
    code.movi(0, 0x400);
    code.orr(0, 0, 3);
    code.strw(2, 0, 0);
    code.movi(4, 0x0400_0000);
    code.add(4, 4, 0x100);
    code.add(4, 4, 0x20);
    code.movi(0, 0);
    code.strh(4, 0, 0x14);
    code.movi(0, 0x2000);
    code.orr(0, 0, 3);
    code.strh(4, 0, 8);

    let head = code.here();
    code.ldrh(5, 4, 8);
    code.movr(6, 5, 4);
    code.andi(6, 6, 3);
    code.cmpi(6, 0);
    let to_child = code.hole();

    code.movi(0, 1);
    code.strh(4, 0, 0x0A);
    code.movi(0, 0x2000);
    code.orr(0, 0, 0x83);
    code.strh(4, 0, 8);
    let parent_wait = code.here();
    code.ldrh(5, 4, 8);
    code.tst(5, 0x80);
    code.bne(parent_wait);
    let to_draw = code.hole();

    let child = code.here();
    code.patch(to_child, NE, false, child);
    code.cmpi(6, 1);
    let not_one = code.hole();
    code.movi(0, 0x1100);
    code.orr(0, 0, 0x11);
    let sent_one = code.hole();
    let two = code.here();
    code.patch(not_one, NE, false, two);
    code.cmpi(6, 2);
    let not_two = code.hole();
    code.movi(0, 0x2200);
    code.orr(0, 0, 0x22);
    let sent_two = code.hole();
    let three = code.here();
    code.patch(not_two, NE, false, three);
    code.movi(0, 0x3300);
    code.orr(0, 0, 0x33);
    let send_child = code.here();
    code.patch(sent_one, AL, false, send_child);
    code.patch(sent_two, AL, false, send_child);
    code.strh(4, 0, 0x0A);
    let child_wait = code.here();
    code.ldrh(5, 4, 8);
    code.tst(5, 0x80);
    code.bne(child_wait);

    let draw = code.here();
    code.patch(to_draw, AL, false, draw);
    // 下一次传输一开始会把寄存器清成空位。先抄走，画面才不会跟着闪。
    code.movi(7, 0x0300_0000);
    for offset in [0u8, 2, 4, 6] {
        code.ldrh(0, 4, offset);
        code.strh(7, 0, offset);
    }
    code.movi(0, 0x1F);
    code.movr(1, 0, 5);
    code.movr(10, 0, 10);
    code.orr_lsl(12, 0, 1, 0);
    code.movr(9, 0, 0);
    code.cmpi(6, 1);
    code.movr_if(0, 9, 1);
    code.cmpi(6, 2);
    code.movr_if(0, 9, 10);
    code.cmpi(6, 3);
    code.movr_if(0, 9, 12);
    code.orr_lsl(9, 9, 9, 16);
    code.movr(8, 3, 0);
    code.add(8, 8, 16);
    code.movi(5, 8);
    code.movi(11, 8);
    code.movi(12, 0x1C0);
    let badge = code.hole();

    let mut bars = Vec::new();
    let specs: [(u8, u8, &[u32]); 4] = [
        (0, 0, &[0x1E00]),
        (2, 5, &[0x1E00, 0x3C00]),
        (4, 10, &[0x1E00, 0x3C00, 0x3C00]),
        (6, 0, &[0x1E00, 0x3C00, 0x3C00, 0x3C00]),
    ];
    for (offset, shifts, y_parts) in specs {
        code.movi(1, 0x1F);
        if offset == 6 {
            code.movr(1, 1, 5);
            code.movi(0, 0x1F);
            code.orr_lsl(1, 1, 0, 0);
        } else if shifts != 0 {
            code.movr(1, 1, shifts);
        }
        code.ldrh(0, 7, offset);
        code.movi(10, 0xFF);
        code.orr(10, 10, 0xFF00);
        code.cmp(0, 10);
        code.movi_if(0, 9, 0);
        code.movr_if(NE, 9, 1);
        code.orr_lsl(9, 9, 9, 16);
        code.movr(8, 3, 0);
        code.add(8, 8, 32);
        for part in y_parts {
            if *part != 0 {
                code.add(8, 8, *part);
            }
        }
        code.movi(5, 32);
        code.movi(11, 20);
        code.movi(12, 0x160);
        bars.push(code.hole());
    }
    code.b(head);

    let fill = code.here();
    code.patch(badge, AL, true, fill);
    for hole in bars {
        code.patch(hole, AL, true, fill);
    }
    let row = code.here();
    code.movr(6, 5, 0);
    let pixel = code.here();
    code.str_post(8, 9);
    code.subs(6, 6, 1);
    code.bne(pixel);
    code.add_reg(8, 8, 12);
    code.subs(11, 11, 1);
    code.bne(row);
    code.movr(15, 14, 0);

    let mut body = Vec::with_capacity(code.words.len() * 4);
    for word in code.words {
        body.extend(word.to_le_bytes());
    }
    finish_header(&body)
}

fn finish_header(code: &[u8]) -> Vec<u8> {
    let mut rom = vec![0u8; 0xC0];
    let branch = asm::branch_address(AL, false, 0x0800_0000, CODE);
    rom[..4].copy_from_slice(&branch.to_le_bytes());
    rom[4..0xA0].copy_from_slice(&include_bytes!("../roms/arm.gba")[4..0xA0]);
    rom[0xA0..0xA9].copy_from_slice(b"LINK TEST");
    rom[0xAC..0xB0].copy_from_slice(b"LNKU");
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
    use crate::link::LinkHub;
    use std::time::Duration;

    #[test]
    fn two_machines_paint_the_first_two_seats() {
        let rom = link_test_rom();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("roms/link.gba");
        std::fs::write(&path, &rom).expect("write link.gba");

        let mut host_link = LinkHub::new();
        let port = host_link.host_on(0, "link-test").expect("host");
        let guest = std::thread::spawn(move || {
            let mut link = LinkHub::new();
            link.join(&format!("127.0.0.1:{port}"), "link-test")
                .expect("join");
            let mut bus = Bus::new();
            bus.load_rom(&rom);
            let mut regs = Registers::arm_start(0x0800_0000);
            for _ in 0..80 {
                for _ in 0..2_000 {
                    step_quiet(&mut regs, &mut bus).expect("guest step");
                }
                link.service(&mut bus);
                if bus.read_u16(0x0600_5A20) == 0x03E0 {
                    return bus.read_u16(0x0600_1E20);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!(
                "guest bars {:04x} {:04x}",
                bus.read_u16(0x0600_1E20),
                bus.read_u16(0x0600_5A20)
            );
        });

        let rom = link_test_rom();
        let mut bus = Bus::new();
        bus.load_rom(&rom);
        let mut regs = Registers::arm_start(0x0800_0000);
        let mut first = 0u16;
        let mut second = 0u16;
        for _ in 0..80 {
            for _ in 0..2_000 {
                step_quiet(&mut regs, &mut bus).expect("host step");
            }
            host_link.service(&mut bus);
            first = bus.read_u16(0x0600_1E20);
            second = bus.read_u16(0x0600_5A20);
            if first == 0x001F && second == 0x03E0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(first, 0x001F, "host seat 1");
        assert_eq!(second, 0x03E0, "host seat 2");
        assert_eq!(bus.read_u16(0x0600_9620), 0, "host seat 3 stays empty");
        let guest_first = guest.join().expect("guest");
        assert_eq!(guest_first, 0x001F);
    }
}
