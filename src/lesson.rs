//! 渐变程序只负责光束。半透明、马赛克、旋转、窗口各写一段短程序。

use crate::asm::{self, Shift};
use crate::bus::Bus;
use crate::cpu::run_until_self_branch;
use crate::registers::Registers;

const ENTRY: u32 = 0x0800_0000;
const AL: u8 = 0xE;

pub fn demo_rom() -> Vec<u8> {
    let at = |index: u32| ENTRY + index * 4;
    let mut words = Vec::new();
    let mut push = |word: u32| words.push(word);

    push(asm::dp_imm(AL, 13, false, 0, 0, 0x0400_0000).unwrap());
    push(asm::dp_imm(AL, 13, false, 0, 1, 0x400).unwrap());
    push(asm::dp_imm(AL, 12, false, 1, 1, 3).unwrap());
    push(asm::mem_imm(AL, false, false, true, true, false, 0, 1, 0));
    // 读 VCOUNT，小于 160 就继续转。空转本身把累计周期推向第 160 行。
    push(asm::half_imm(AL, true, true, true, false, 0, 2, 6));
    push(asm::dp_imm(AL, 10, true, 2, 0, 160).unwrap());
    push(asm::branch_address(3, false, at(6), at(4)));
    push(asm::dp_imm(AL, 13, false, 0, 4, 0x0600_0000).unwrap());
    push(asm::dp_imm(AL, 13, false, 0, 6, 0).unwrap());
    // row
    push(asm::dp_imm(AL, 0, false, 6, 8, 0x1F).unwrap());
    push(asm::dp_reg(AL, 13, false, 0, 8, 8, Shift::Lsl, 5));
    push(asm::dp_imm(AL, 12, false, 8, 8, 0x1F).unwrap());
    push(asm::dp_reg(AL, 12, false, 8, 9, 8, Shift::Lsl, 16));
    push(asm::dp_reg(AL, 13, false, 0, 10, 4, Shift::Lsl, 0));
    push(asm::dp_imm(AL, 13, false, 0, 7, 240).unwrap());
    // pix
    push(asm::mem_imm(AL, false, false, false, true, false, 10, 9, 4));
    push(asm::dp_imm(AL, 2, true, 7, 7, 2).unwrap());
    push(asm::branch_address(1, false, at(17), at(15)));
    push(asm::dp_imm(AL, 4, false, 4, 4, 480).unwrap());
    push(asm::dp_imm(AL, 4, false, 6, 6, 1).unwrap());
    push(asm::dp_imm(AL, 10, true, 6, 0, 160).unwrap());
    push(asm::branch_address(3, false, at(21), at(9)));
    push(asm::branch_address(AL, false, at(22), at(22)));

    let mut rom = Vec::with_capacity(words.len() * 4);
    for word in words {
        rom.extend(word.to_le_bytes());
    }
    rom
}

pub fn run_demo() -> Result<(Registers, Bus, u32), String> {
    let rom = demo_rom();
    let mut bus = Bus::new();
    bus.load_rom(&rom);
    let mut regs = Registers::arm_start(ENTRY);
    let steps = run_until_self_branch(&mut regs, &mut bus, 200_000)?;
    Ok((regs, bus, steps))
}

struct Rom {
    words: Vec<u32>,
}

impl Rom {
    fn new() -> Self {
        Self { words: Vec::new() }
    }

    fn here(&self) -> u32 {
        ENTRY + self.words.len() as u32 * 4
    }

    fn push(&mut self, word: u32) {
        self.words.push(word);
    }

    fn mov(&mut self, rd: u8, value: u32) {
        self.push(asm::dp_imm(AL, 13, false, 0, rd, value).unwrap());
    }

    fn add(&mut self, rd: u8, rn: u8, value: u32) {
        self.push(asm::dp_imm(AL, 4, false, rn, rd, value).unwrap());
    }

    fn add_reg(&mut self, rd: u8, rn: u8, rm: u8) {
        self.push(asm::dp_reg(AL, 4, false, rn, rd, rm, Shift::Lsl, 0));
    }

    fn subs(&mut self, rd: u8, rn: u8, value: u32) {
        self.push(asm::dp_imm(AL, 2, true, rn, rd, value).unwrap());
    }

    fn str(&mut self, rn: u8, rd: u8, offset: u16) {
        self.push(asm::mem_imm(
            AL, false, false, true, true, false, rn, rd, offset,
        ));
    }

    fn strh(&mut self, rn: u8, rd: u8, offset: u8) {
        self.push(asm::half_imm(AL, false, true, true, false, rn, rd, offset));
    }

    fn bne(&mut self, target: u32) {
        let from = self.here();
        self.push(asm::branch_address(1, false, from, target));
    }

    fn halt(mut self) -> Vec<u8> {
        let from = self.here();
        self.push(asm::branch_address(AL, false, from, from));
        let mut rom = Vec::with_capacity(self.words.len() * 4);
        for word in self.words {
            rom.extend(word.to_le_bytes());
        }
        rom
    }
}

fn hide_sprites(rom: &mut Rom) {
    rom.mov(0, 0x0700_0000);
    rom.mov(1, 1 << 9);
    rom.mov(2, 128);
    let again = rom.here();
    rom.strh(0, 1, 0);
    rom.add(0, 0, 8);
    rom.subs(2, 2, 1);
    rom.bne(again);
}

fn obj_tiles(rom: &mut Rom) {
    rom.mov(3, 0x0600_0000);
    rom.mov(2, 0x1_0000);
    rom.add(2, 2, 0x4000);
    rom.add_reg(3, 3, 2);
}

fn enable_objects(rom: &mut Rom) {
    rom.mov(4, 0x0400_0000);
    rom.mov(1, 0x1000);
    rom.add(1, 1, 3);
    rom.str(4, 1, 0);
}

/// 半透明写在 0 号精灵上，混合系数写在 `0x04000050`。位图像素保持原色。
pub fn semi_sprite_rom() -> Vec<u8> {
    let mut rom = Rom::new();
    hide_sprites(&mut rom);
    rom.mov(4, 0x0400_0000);
    rom.mov(1, 0x1000);
    rom.add(1, 1, 0x400);
    rom.add(1, 1, 3);
    rom.str(4, 1, 0);
    rom.mov(3, 0x0600_0000);
    rom.mov(1, 0x001F);
    rom.strh(3, 1, 0);
    rom.mov(1, 0x03E0);
    rom.strh(3, 1, 2);
    rom.mov(3, 0x0500_0000);
    rom.add(3, 3, 0x200);
    rom.mov(1, 0x7C00);
    rom.strh(3, 1, 2);
    obj_tiles(&mut rom);
    rom.mov(1, 1);
    rom.strh(3, 1, 0);
    rom.mov(3, 0x0700_0000);
    rom.mov(1, 1 << 10);
    rom.strh(3, 1, 0);
    rom.mov(1, 0);
    rom.strh(3, 1, 2);
    rom.strh(3, 1, 4);
    rom.mov(1, 1 << 10);
    rom.strh(4, 1, 0x50);
    rom.mov(1, 0x800);
    rom.add(1, 1, 8);
    rom.strh(4, 1, 0x52);
    rom.halt()
}

/// 马赛克宽度写在 `MOSAIC`，开不开写在这个精灵的属性 0。`sprite_x` 是它的横坐标。
pub fn mosaic_sprite_rom(sprite_x: u32) -> Vec<u8> {
    let mut rom = Rom::new();
    hide_sprites(&mut rom);
    enable_objects(&mut rom);
    rom.mov(1, 0x100);
    rom.strh(4, 1, 0x4C);
    rom.mov(3, 0x0500_0000);
    rom.mov(1, 0x7C00);
    rom.strh(3, 1, 0);
    rom.add(3, 3, 0x200);
    rom.mov(1, 0x001F);
    rom.strh(3, 1, 2);
    rom.mov(1, 0x03E0);
    rom.strh(3, 1, 4);
    obj_tiles(&mut rom);
    rom.mov(1, 0x21);
    rom.strh(3, 1, 0);
    rom.mov(3, 0x0700_0000);
    rom.mov(1, 1 << 12);
    rom.strh(3, 1, 0);
    rom.mov(1, sprite_x);
    rom.strh(3, 1, 2);
    rom.mov(1, 0);
    rom.strh(3, 1, 4);
    rom.halt()
}

/// 顺时针转 90 度。系数在精灵表的矩阵 0：`pb = 1`，`pc = -1`。
pub fn rotate_sprite_rom() -> Vec<u8> {
    let mut rom = Rom::new();
    hide_sprites(&mut rom);
    enable_objects(&mut rom);
    rom.mov(3, 0x0500_0000);
    rom.mov(1, 0x03E0);
    rom.strh(3, 1, 0);
    rom.add(3, 3, 0x200);
    rom.mov(1, 0x001F);
    rom.strh(3, 1, 2);
    obj_tiles(&mut rom);
    rom.mov(1, 1);
    rom.strh(3, 1, 16);
    rom.mov(3, 0x0700_0000);
    rom.mov(1, 1 << 8);
    rom.strh(3, 1, 0);
    rom.mov(1, 0);
    rom.strh(3, 1, 2);
    rom.strh(3, 1, 4);
    rom.strh(3, 1, 6);
    rom.mov(1, 0x100);
    rom.strh(3, 1, 0x0E);
    rom.mov(1, 0xFF00);
    rom.strh(3, 1, 0x16);
    rom.mov(1, 0);
    rom.strh(3, 1, 0x1E);
    rom.halt()
}

/// 窗口矩形和层开关在 `0x04000040`，不写进位图。
pub fn window_rom() -> Vec<u8> {
    let mut rom = Rom::new();
    rom.mov(4, 0x0400_0000);
    rom.mov(1, 0x2000);
    rom.add(1, 1, 0x400);
    rom.add(1, 1, 3);
    rom.str(4, 1, 0);
    rom.mov(3, 0x0600_0000);
    rom.mov(1, 0x001F);
    rom.strh(3, 1, 0);
    rom.strh(3, 1, 2);
    rom.mov(3, 0x0500_0000);
    rom.mov(1, 0x03E0);
    rom.strh(3, 1, 0);
    rom.mov(1, 1);
    rom.strh(4, 1, 0x40);
    rom.strh(4, 1, 0x44);
    rom.mov(1, 4);
    rom.strh(4, 1, 0x48);
    rom.halt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::step;
    use crate::disasm::disassemble;
    use crate::ppu::{finished_lines, render_frame, CYCLES_PER_LINE, VISIBLE_LINES};

    const FILL: u32 = ENTRY + 7 * 4;

    #[test]
    fn first_word_is_the_display_pointer() {
        let rom = demo_rom();
        let word = u32::from_le_bytes(rom[0..4].try_into().unwrap());
        assert_eq!(disassemble(ENTRY, word).text, "mov r0, #0x4000000");
    }

    #[test]
    fn the_program_fills_mode3_with_a_gradient() {
        let (_regs, mut bus, steps) = run_demo().unwrap();
        assert!(steps < 200_000);
        assert_eq!(bus.read_u16(0x0400_0000), 0x0403);
        let frame = render_frame(&mut bus);
        assert_eq!(frame.pixel(0, 0), 0x001F);
        assert_eq!(frame.pixel(239, 0), 0x001F);
        assert_eq!(frame.pixel(0, 1), 0x003F);
        assert_eq!(frame.pixel(0, 31), 0x03FF);
        assert_eq!(frame.pixel(0, 32), 0x001F);
        assert_eq!(frame.pixel(0, 159), 0x03FF);
    }

    #[test]
    fn vram_stays_empty_until_vcount_reaches_160() {
        let mut bus = Bus::new();
        bus.load_rom(&demo_rom());
        let mut regs = Registers::arm_start(ENTRY);
        for _ in 0..200_000 {
            if regs.pc() == FILL {
                break;
            }
            step(&mut regs, &mut bus).unwrap();
        }
        assert_eq!(regs.pc(), FILL);
        assert_eq!(bus.read_u16(0x0600_0000), 0);
        assert!(bus.cycles() >= VISIBLE_LINES * CYCLES_PER_LINE);
        assert!(u64::from(bus.read_u8(0x0400_0006)) >= VISIBLE_LINES);
        assert_eq!(finished_lines(bus.cycles()), 160);
        assert_eq!(bus.frame().pixel(0, 0), 0);
        assert_eq!(bus.frame().pixel(239, 159), 0);

        let steps = run_until_self_branch(&mut regs, &mut bus, 200_000).unwrap();
        assert!(steps < 200_000);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x001F);

        let lines = finished_lines(bus.cycles());
        assert_eq!(bus.cycles(), 761_423);
        assert_eq!(lines, 160);
        assert_eq!(bus.frame().pixel(0, 0), 0x001F);
        assert_eq!(bus.frame().pixel(239, 157), row_color(157));
        assert_eq!(bus.frame().pixel(0, 158), row_color(158));
        assert_eq!(bus.frame().pixel(239, 158), 0);
        assert_eq!(bus.frame().pixel(0, 159), 0);
        assert_eq!(bus.frame().pixel(239, 159), 0);
        assert_eq!(render_frame(&mut bus).pixel(0, 159), row_color(159));
        assert_eq!(render_frame(&mut bus).pixel(239, 159), row_color(159));
    }

    fn row_color(y: usize) -> u16 {
        ((y as u16) & 31) << 5 | 0x001F
    }

    fn run_rom(rom: &[u8]) -> Bus {
        let mut bus = Bus::new();
        bus.load_rom(rom);
        let mut regs = Registers::arm_start(ENTRY);
        run_until_self_branch(&mut regs, &mut bus, 5_000).unwrap();
        bus
    }

    #[test]
    fn semi_transparent_sprite_blends_over_the_bitmap() {
        let mut bus = run_rom(&semi_sprite_rom());
        assert_eq!(bus.read_u16(0x0700_0000), 1 << 10);
        assert_eq!(bus.read_u16(0x0700_0008), 1 << 9);
        assert_eq!(bus.read_u16(0x0400_0050), 1 << 10);
        assert_eq!(bus.read_u16(0x0400_0052), 0x0808);
        assert_eq!(bus.read_u16(0x0600_0000), 0x001F);
        let frame = render_frame(&mut bus);
        assert_eq!(frame.pixel(0, 0), 0x3C0F);
        assert_eq!(frame.pixel(1, 0), 0x03E0);
    }

    #[test]
    fn mosaic_sprite_repeats_its_anchor() {
        let mut bus = run_rom(&mosaic_sprite_rom(0));
        assert_eq!(bus.read_u16(0x0400_004C), 0x0100);
        assert_eq!(bus.read_u16(0x0700_0000), 1 << 12);
        let frame = render_frame(&mut bus);
        assert_eq!(frame.pixel(0, 0), 0x001F);
        assert_eq!(frame.pixel(1, 0), 0x001F);
        assert_eq!(frame.pixel(2, 0), 0x7C00);
    }

    #[test]
    fn mosaic_sprite_hides_when_the_anchor_is_outside() {
        let mut bus = run_rom(&mosaic_sprite_rom(1));
        assert_eq!(bus.read_u16(0x0700_0002), 1);
        assert_eq!(bus.read_u16(0x0700_0010), 1 << 9);
        assert_eq!(render_frame(&mut bus).pixel(1, 0), 0x7C00);
    }

    #[test]
    fn rotate_sprite_uses_the_oam_matrix() {
        let mut bus = run_rom(&rotate_sprite_rom());
        assert_eq!(bus.read_u16(0x0700_0000), 1 << 8);
        assert_eq!(bus.read_u16(0x0700_0006), 0);
        assert_eq!(bus.read_u16(0x0700_000E), 0x0100);
        assert_eq!(bus.read_u16(0x0700_0016), 0xFF00);
        let frame = render_frame(&mut bus);
        assert_eq!(frame.pixel(4, 0), 0x001F);
        assert_eq!(frame.pixel(0, 0), 0x03E0);
    }

    #[test]
    fn window_clips_the_bitmap() {
        let mut bus = run_rom(&window_rom());
        assert_eq!(bus.read_u16(0x0400_0000), 0x2403);
        assert_eq!(bus.read_u16(0x0400_0040), 1);
        assert_eq!(bus.read_u16(0x0400_0044), 1);
        assert_eq!(bus.read_u16(0x0400_0048), 4);
        assert_eq!(bus.read_u16(0x0600_0000), 0x001F);
        assert_eq!(bus.read_u16(0x0600_0002), 0x001F);
        let frame = render_frame(&mut bus);
        assert_eq!(frame.pixel(0, 0), 0x001F);
        assert_eq!(frame.pixel(1, 0), 0x03E0);
    }
}
