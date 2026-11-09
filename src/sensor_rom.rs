//! 传感器实验卡。游戏代码 `ULTS` 同时打开陀螺仪、倾斜和太阳能。
//!
//! 画面是三块仪表。左边的陀螺仪转盘跟着读数往两侧倒，中间的小球在槽里滚，
//! 右边的太阳随日光变大。工作内存里留着原始值：`0x00` 陀螺仪，`0x02` 倾斜 X，
//! `0x04` 倾斜 Y，`0x06` 日光计数。

use crate::asm::{self, Shift};
use crate::bus::Bus;
use crate::cpu::step_quiet;
use crate::registers::Registers;

const CODE: u32 = 0x0800_0100;
const AL: u8 = 0xE;
const NE: u8 = 1;
const LO: u8 = 3;
const HI: u8 = 8;

const PANEL_W: i32 = 64;
const PANEL_H: i32 = 128;
const GYRO_AT: u32 = 8;
const TILT_AT: u32 = 88;
const SUN_AT: u32 = 168;
const PANEL_AT: u32 = 16;
const WIN_X: i32 = 6;
const WIN_Y: i32 = 22;
const WIN_W: i32 = 52;
const WIN_H: i32 = 100;

const DESK: u16 = rgb(2, 3, 6);
const CARD: u16 = rgb(5, 7, 12);
const GOLD: u16 = rgb(24, 18, 6);
const GOLD_HI: u16 = rgb(31, 26, 14);
const GOLD_DIM: u16 = rgb(14, 10, 4);
const LIP: u16 = rgb(8, 10, 14);
const VIEW: u16 = rgb(2, 4, 8);
const BRASS: u16 = rgb(22, 16, 6);
const BRASS_HI: u16 = rgb(31, 24, 12);
const STEEL: u16 = rgb(12, 16, 18);
const ROTOR: u16 = rgb(6, 20, 12);
const ROTOR_HI: u16 = rgb(16, 30, 18);
const HUB: u16 = rgb(22, 31, 22);
const JEWEL: u16 = rgb(28, 6, 8);
const SLOT: u16 = rgb(4, 8, 10);
const WELL: u16 = rgb(1, 2, 5);
const WELL_RIM: u16 = rgb(8, 12, 18);
const WELL_HI: u16 = rgb(16, 20, 26);
const DOT: u16 = rgb(6, 10, 16);
const BALL: u16 = rgb(30, 26, 20);
const BALL_HI: u16 = rgb(31, 31, 26);
const BALL_EDGE: u16 = rgb(14, 10, 8);
const SHADOW: u16 = rgb(0, 0, 2);
const SCREW: u16 = rgb(8, 8, 10);
const SCREW_HI: u16 = rgb(20, 20, 18);
const STAR: u16 = rgb(22, 24, 28);
const EYE: u16 = rgb(14, 6, 2);

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

    fn add_lsl(&mut self, rd: u8, rn: u8, rm: u8, amount: u8) {
        self.emit(asm::dp_reg(AL, 4, false, rn, rd, rm, Shift::Lsl, amount));
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

    fn ldr_post(&mut self, rd: u8, rn: u8) {
        self.emit(asm::mem_imm(AL, true, false, false, true, false, rn, rd, 4));
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

    /// `ldr rd, [pc, #0]` 后面紧跟一条跳过字面量的分支。字面量在指令地址 + 8。
    fn pool(&mut self, rd: u8) -> usize {
        self.emit(asm::mem_imm(AL, true, false, true, true, false, 15, rd, 0));
        let skip = self.hole();
        let data = self.words.len();
        self.words.push(0);
        let after = self.here();
        self.patch(skip, false, after);
        data
    }
}

pub fn sensor_test_rom() -> Vec<u8> {
    let mut code = Asm::new();
    let mut pools = Vec::new();

    code.movi(2, 0x0400_0000);
    code.movi(0, 0x400);
    code.orr(0, 0, 3);
    code.strw(2, 0, 0);
    code.movi(3, 0x0600_0000);
    code.movi(4, 0x0800_0000);
    code.add(4, 4, 0xC4);
    code.movi(7, 0x0E00_0000);
    code.add(7, 7, 0x8000);

    code.color(9, u32::from(DESK));
    code.mov_lsl(8, 3, 0);
    code.movi(11, 160);
    let clear_row = code.here();
    code.movi(6, 120);
    let clear_pix = code.here();
    code.str_post(8, 9);
    code.subs(6, 6, 1);
    code.bne(clear_pix);
    code.subs(11, 11, 1);
    code.bne(clear_row);

    let head = code.here();
    let to_gyro = code.hole();
    code.cmpi(0, 0x700);
    code.movi(0, 1);
    code.movi_if(LO, 0, 0);
    code.movi_if(HI, 0, 2);
    let to_show_gyro = code.hole();

    let to_latch = code.hole();
    let to_tilt_x = code.hole();
    code.cmpi(0, 0x3A0);
    code.movi(6, 1);
    code.movi_if(LO, 6, 0);
    code.movi_if(HI, 6, 2);
    let to_tilt_y = code.hole();
    code.cmpi(0, 0x3A0);
    code.movi(1, 1);
    code.movi_if(LO, 1, 0);
    code.movi_if(HI, 1, 2);
    code.add_lsl(0, 6, 1, 1);
    code.add_reg(0, 0, 1);
    let to_show_tilt = code.hole();

    let to_solar = code.hole();
    code.mov_lsr(1, 0, 5);
    code.cmpi(1, 7);
    code.movi_if(HI, 1, 7);
    code.movi(0, 7);
    code.sub_reg(0, 0, 1);
    let to_show_sun = code.hole();
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

    let show_gyro = code.here();
    code.patch(to_show_gyro, true, show_gyro);
    let copy_gyro = emit_show(&mut code, &mut pools, 0, GYRO_AT, PANEL_AT);
    let show_tilt = code.here();
    code.patch(to_show_tilt, true, show_tilt);
    let copy_tilt = emit_show(&mut code, &mut pools, 0xC000, TILT_AT, PANEL_AT);
    let show_sun = code.here();
    code.patch(to_show_sun, true, show_sun);
    let copy_sun = emit_show(&mut code, &mut pools, 0x3_0000, SUN_AT, PANEL_AT);

    let copy = code.here();
    for hole in [copy_gyro, copy_tilt, copy_sun] {
        code.patch(hole, false, copy);
    }
    // 一行 64 像素，32 个字。写完后补到下一行：480 - 128 = 352。
    code.mov_lsl(8, 10, 9);
    code.mov_lsl(5, 10, 5);
    code.sub_reg(8, 8, 5);
    code.mov_lsl(5, 1, 1);
    code.add_reg(8, 8, 5);
    code.add_reg(8, 8, 3);
    code.movi(5, 32);
    code.movi(11, 128);
    code.movi(12, 0x160);
    let row = code.here();
    code.mov_lsl(6, 5, 0);
    let pixel = code.here();
    code.ldr_post(9, 0);
    code.str_post(8, 9);
    code.subs(6, 6, 1);
    code.bne(pixel);
    code.add_reg(8, 8, 12);
    code.subs(11, 11, 1);
    code.bne(row);
    code.mov_lsl(15, 2, 0);

    let asset_addr = CODE + code.words.len() as u32 * 4;
    for slot in pools {
        code.words[slot] = asset_addr;
    }
    let mut body = Vec::with_capacity(code.words.len() * 4);
    for word in code.words {
        body.extend(word.to_le_bytes());
    }
    body.extend(assets());
    finish_header(&body)
}

fn emit_show(code: &mut Asm, pools: &mut Vec<usize>, group: u32, x: u32, y: u32) -> usize {
    code.mov_lsl(2, 14, 0);
    code.mov_lsl(1, 0, 14);
    pools.push(code.pool(5));
    code.add_reg(0, 5, 1);
    if group != 0 {
        code.add(0, 0, group);
    }
    code.movi(1, x);
    code.movi(10, y);
    code.hole()
}

struct Pix {
    px: Vec<u16>,
}

impl Pix {
    fn new(fill: u16) -> Self {
        Self {
            px: vec![fill; (PANEL_W * PANEL_H) as usize],
        }
    }

    fn set(&mut self, x: i32, y: i32, color: u16) {
        if (0..PANEL_W).contains(&x) && (0..PANEL_H).contains(&y) {
            self.px[(y * PANEL_W + x) as usize] = color;
        }
    }

    fn get(&self, x: i32, y: i32) -> u16 {
        self.px[(y * PANEL_W + x) as usize]
    }
}

fn rect(p: &mut Pix, x: i32, y: i32, w: i32, h: i32, color: u16) {
    for yy in y..y + h {
        for xx in x..x + w {
            p.set(xx, yy, color);
        }
    }
}

fn disc(p: &mut Pix, cx: i32, cy: i32, r: i32, color: u16) {
    let r2 = r * r;
    for y in -r..=r {
        for x in -r..=r {
            if x * x + y * y <= r2 {
                p.set(cx + x, cy + y, color);
            }
        }
    }
}

fn ring(p: &mut Pix, cx: i32, cy: i32, r: i32, thick: i32, color: u16, hi: Option<u16>) {
    let inner = (r - thick) * (r - thick);
    let outer = r * r;
    for y in -r..=r {
        for x in -r..=r {
            let d = x * x + y * y;
            if d <= outer && d >= inner {
                let paint = if x < -1 && y < -1 {
                    hi.unwrap_or(color)
                } else {
                    color
                };
                p.set(cx + x, cy + y, paint);
            }
        }
    }
}

fn line(p: &mut Pix, x0: i32, y0: i32, x1: i32, y1: i32, color: u16) {
    let n = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
    for i in 0..=n {
        p.set(x0 + (x1 - x0) * i / n, y0 + (y1 - y0) * i / n, color);
    }
}

fn glyph(ch: char) -> [&'static str; 5] {
    match ch {
        'G' => ["####", "#   ", "# ##", "#  #", "####"],
        'Y' => ["#  #", "#  #", " ## ", " #  ", " #  "],
        'R' => ["### ", "#  #", "### ", "# # ", "#  #"],
        'O' => [" ## ", "#  #", "#  #", "#  #", " ## "],
        'T' => ["####", " #  ", " #  ", " #  ", " #  "],
        'I' => ["### ", " #  ", " #  ", " #  ", "### "],
        'L' => ["#   ", "#   ", "#   ", "#   ", "####"],
        'S' => [" ###", "#   ", " ## ", "   #", "### "],
        'U' => ["#  #", "#  #", "#  #", "#  #", " ## "],
        'N' => ["#  #", "## #", "# ##", "#  #", "#  #"],
        _ => ["    ", "    ", "    ", "    ", "    "],
    }
}

fn label(p: &mut Pix, text: &str, y0: i32, color: u16) {
    let width = text.len() as i32 * 5 - 1;
    let mut x = (PANEL_W - width) / 2;
    for ch in text.chars() {
        for (dy, row) in glyph(ch).iter().enumerate() {
            for (dx, bit) in row.chars().enumerate() {
                if bit == '#' {
                    p.set(x + dx as i32, y0 + dy as i32, color);
                }
            }
        }
        x += 5;
    }
}

fn punch_card(p: &mut Pix) {
    for y in 0..PANEL_H {
        for x in 0..PANEL_W {
            let inside = (WIN_X..WIN_X + WIN_W).contains(&x) && (WIN_Y..WIN_Y + WIN_H).contains(&y);
            if !inside {
                p.set(x, y, CARD);
            }
        }
    }
}

fn lip(p: &mut Pix) {
    let x1 = WIN_X + WIN_W - 1;
    let y1 = WIN_Y + WIN_H - 1;
    for x in WIN_X..=x1 {
        p.set(x, WIN_Y, LIP);
        p.set(x, y1, LIP);
    }
    for y in WIN_Y..=y1 {
        p.set(WIN_X, y, LIP);
        p.set(x1, y, LIP);
    }
}

fn chrome(p: &mut Pix) {
    let w = PANEL_W - 1;
    let h = PANEL_H - 1;
    for x in 0..PANEL_W {
        p.set(x, 0, GOLD_HI);
        p.set(x, 1, GOLD);
        p.set(x, h - 1, GOLD);
        p.set(x, h, GOLD_DIM);
    }
    for y in 0..PANEL_H {
        p.set(0, y, GOLD_HI);
        p.set(1, y, GOLD);
        p.set(w - 1, y, GOLD);
        p.set(w, y, GOLD_DIM);
    }
    for (x, y) in [(4, 4), (w - 5, 4), (4, h - 5), (w - 5, h - 5)] {
        p.set(x, y, SCREW);
        p.set(x + 1, y, SCREW_HI);
        p.set(x, y + 1, SCREW);
    }
}

fn finish_panel(p: &mut Pix, title: &str) {
    punch_card(p);
    lip(p);
    label(p, title, 9, rgb(1, 1, 2));
    label(p, title, 8, GOLD_HI);
    chrome(p);
}

fn draw_gyro(frame: usize) -> Pix {
    let mut p = Pix::new(CARD);
    rect(&mut p, WIN_X, WIN_Y, WIN_W, WIN_H, VIEW);
    let cx = 32;
    let cy = 64;
    let shift = [-6, 0, 6][frame];
    for x in -16..=16 {
        p.set(cx + x, cy, SLOT);
    }
    ring(&mut p, cx, cy, 20, 3, BRASS, Some(BRASS_HI));
    ring(&mut p, cx + shift, cy, 10, 2, STEEL, Some(rgb(22, 26, 28)));
    let ends = match frame {
        0 => [(-4, -8), (-8, 2), (5, 7)],
        2 => [(4, -8), (8, 2), (-5, 7)],
        _ => [(0, -8), (-7, 4), (7, 4)],
    };
    for (dx, dy) in ends {
        line(&mut p, cx + shift, cy, cx + shift + dx, cy + dy, ROTOR_HI);
    }
    disc(&mut p, cx + shift, cy, 5, ROTOR);
    disc(&mut p, cx + shift - 1, cy - 1, 2, HUB);
    p.set(cx + shift, cy, JEWEL);
    rect(&mut p, cx - 3, cy + 16, 6, 16, STEEL);
    rect(&mut p, cx - 14, cy + 30, 28, 5, BRASS);
    for x in cx - 14..cx + 14 {
        p.set(x, cy + 30, BRASS_HI);
    }
    for (index, x) in [16, 30, 44].iter().enumerate() {
        let color = if index == frame { BRASS_HI } else { STEEL };
        rect(&mut p, *x, 110, 3, 3, color);
    }
    finish_panel(&mut p, "GYRO");
    p
}

fn draw_tilt(x_index: usize, y_index: usize) -> Pix {
    let mut p = Pix::new(CARD);
    rect(&mut p, 10, 40, 44, 64, WELL_RIM);
    rect(&mut p, 13, 43, 38, 58, WELL);
    for x in 13..51 {
        p.set(x, 43, WELL_HI);
    }
    let cx = 32;
    let cy = 72;
    for x in cx - 16..=cx + 16 {
        p.set(x, cy, DOT);
    }
    for y in cy - 16..=cy + 16 {
        p.set(cx, y, DOT);
    }
    for yi in 0..3 {
        for xi in 0..3 {
            let x = cx + (xi - 1) * 12;
            let y = cy + (yi - 1) * 12;
            p.set(x, y, WELL_HI);
        }
    }
    let bx = cx + (x_index as i32 - 1) * 12;
    let by = cy + (y_index as i32 - 1) * 12;
    rect(&mut p, bx - 3, by + 5, 7, 2, SHADOW);
    disc(&mut p, bx, by, 6, BALL_EDGE);
    disc(&mut p, bx, by, 5, BALL);
    disc(&mut p, bx - 2, by - 2, 2, BALL_HI);
    finish_panel(&mut p, "TILT");
    p
}

struct SunLook {
    high: u16,
    sky: u16,
    ground: u16,
    radius: i32,
    core: i32,
    ray: i32,
    edge: u16,
    body: u16,
    hot: u16,
    beam: u16,
    face: bool,
    stars: bool,
}

fn sun_look(frame: usize) -> SunLook {
    match frame {
        0 => SunLook {
            high: rgb(0, 1, 4),
            sky: rgb(1, 2, 6),
            ground: rgb(1, 2, 2),
            radius: 3,
            core: 1,
            ray: 0,
            edge: rgb(10, 5, 2),
            body: rgb(16, 8, 4),
            hot: rgb(20, 10, 4),
            beam: rgb(0, 0, 0),
            face: false,
            stars: true,
        },
        1 => SunLook {
            high: rgb(2, 2, 6),
            sky: rgb(3, 3, 8),
            ground: rgb(3, 3, 3),
            radius: 4,
            core: 2,
            ray: 0,
            edge: rgb(12, 6, 2),
            body: rgb(20, 10, 4),
            hot: rgb(26, 14, 6),
            beam: rgb(0, 0, 0),
            face: false,
            stars: true,
        },
        2 => SunLook {
            high: rgb(4, 2, 8),
            sky: rgb(6, 4, 12),
            ground: rgb(5, 4, 3),
            radius: 5,
            core: 2,
            ray: 2,
            edge: rgb(16, 6, 2),
            body: rgb(26, 12, 4),
            hot: rgb(31, 18, 6),
            beam: rgb(28, 14, 4),
            face: false,
            stars: false,
        },
        3 => SunLook {
            high: rgb(4, 6, 14),
            sky: rgb(6, 10, 18),
            ground: rgb(4, 8, 4),
            radius: 6,
            core: 3,
            ray: 4,
            edge: rgb(20, 8, 2),
            body: rgb(31, 16, 4),
            hot: rgb(31, 24, 8),
            beam: rgb(31, 20, 6),
            face: true,
            stars: false,
        },
        4 => SunLook {
            high: rgb(4, 10, 20),
            sky: rgb(6, 14, 22),
            ground: rgb(4, 12, 6),
            radius: 7,
            core: 3,
            ray: 5,
            edge: rgb(22, 10, 2),
            body: rgb(31, 20, 4),
            hot: rgb(31, 28, 12),
            beam: rgb(31, 24, 8),
            face: true,
            stars: false,
        },
        5 => SunLook {
            high: rgb(6, 14, 24),
            sky: rgb(8, 18, 26),
            ground: rgb(8, 16, 8),
            radius: 8,
            core: 4,
            ray: 7,
            edge: rgb(24, 12, 2),
            body: rgb(31, 22, 6),
            hot: rgb(31, 30, 16),
            beam: rgb(31, 26, 10),
            face: true,
            stars: false,
        },
        6 => SunLook {
            high: rgb(10, 18, 26),
            sky: rgb(12, 22, 28),
            ground: rgb(10, 18, 8),
            radius: 9,
            core: 5,
            ray: 8,
            edge: rgb(26, 16, 4),
            body: rgb(31, 26, 8),
            hot: rgb(31, 31, 20),
            beam: rgb(31, 28, 14),
            face: true,
            stars: false,
        },
        _ => SunLook {
            high: rgb(16, 24, 31),
            sky: rgb(20, 28, 31),
            ground: rgb(16, 22, 12),
            radius: 11,
            core: 6,
            ray: 10,
            edge: rgb(28, 20, 6),
            body: rgb(31, 28, 12),
            hot: rgb(31, 31, 26),
            beam: rgb(31, 30, 18),
            face: true,
            stars: false,
        },
    }
}

fn channel(color: u16, shift: u32) -> i32 {
    i32::from((color >> shift) & 31)
}

fn blend(from: u16, to: u16, t: i32, n: i32) -> u16 {
    let mix = |shift: u32| {
        let a = channel(from, shift);
        let b = channel(to, shift);
        u16::try_from(a + (b - a) * t / n).unwrap_or(0) << shift
    };
    mix(0) | mix(5) | mix(10)
}

fn lighten(color: u16, by: u16) -> u16 {
    let mix = |shift: u32| (((color >> shift) & 31) + by).min(31) << shift;
    mix(0) | mix(5) | mix(10)
}

fn draw_sun(frame: usize) -> Pix {
    let look = sun_look(frame);
    let mut p = Pix::new(CARD);
    let sky_rows = 100 - WIN_Y;
    for y in WIN_Y..WIN_Y + WIN_H {
        let color = if y == 100 {
            lighten(look.ground, 8)
        } else if y > 100 {
            look.ground
        } else {
            blend(look.high, look.sky, y - WIN_Y, sky_rows)
        };
        for x in WIN_X..WIN_X + WIN_W {
            p.set(x, y, color);
        }
    }
    if look.stars {
        for (x, y) in [(16, 34), (46, 30), (22, 48), (48, 52), (18, 70), (44, 66)] {
            p.set(x, y, STAR);
            p.set(x - 1, y, STAR);
            p.set(x + 1, y, STAR);
            p.set(x, y - 1, STAR);
            p.set(x, y + 1, STAR);
        }
    }
    let cx = 32;
    let cy = 68;
    if look.ray > 0 {
        let gap = look.radius + 2;
        for (dx, dy) in [
            (1, 0),
            (-1, 0),
            (0, 1),
            (0, -1),
            (1, 1),
            (1, -1),
            (-1, 1),
            (-1, -1),
        ] {
            let steps = if dx != 0 && dy != 0 {
                look.ray * 2 / 3
            } else {
                look.ray
            };
            for i in 0..steps {
                let x = cx + dx * (gap + i);
                let y = cy + dy * (gap + i);
                p.set(x, y, look.beam);
                if dy == 0 {
                    p.set(x, y - 1, look.beam);
                } else if dx == 0 {
                    p.set(x - 1, y, look.beam);
                }
            }
        }
    }
    disc(&mut p, cx, cy, look.radius + 1, look.edge);
    disc(&mut p, cx, cy, look.radius, look.body);
    disc(&mut p, cx, cy, look.core, look.hot);
    if look.radius >= 6 {
        let shine = rgb(31, 31, 22);
        p.set(cx - look.radius + 3, cy - look.radius + 4, shine);
        p.set(cx - look.radius + 4, cy - look.radius + 4, shine);
        p.set(cx - look.radius + 3, cy - look.radius + 5, shine);
    }
    if look.face {
        p.set(cx - 3, cy - 1, EYE);
        p.set(cx - 2, cy - 1, EYE);
        p.set(cx + 2, cy - 1, EYE);
        p.set(cx + 3, cy - 1, EYE);
        p.set(cx - 2, cy + 3, EYE);
        p.set(cx - 1, cy + 4, EYE);
        p.set(cx, cy + 4, EYE);
        p.set(cx + 1, cy + 4, EYE);
        p.set(cx + 2, cy + 3, EYE);
    }
    finish_panel(&mut p, "SUN");
    p
}

fn assets() -> Vec<u8> {
    let mut out = Vec::new();
    for frame in 0..3 {
        push_pix(&mut out, &draw_gyro(frame));
    }
    for y in 0..3 {
        for x in 0..3 {
            push_pix(&mut out, &draw_tilt(x, y));
        }
    }
    for frame in 0..8 {
        push_pix(&mut out, &draw_sun(frame));
    }
    out
}

fn push_pix(out: &mut Vec<u8>, pix: &Pix) {
    for color in &pix.px {
        out.extend(color.to_le_bytes());
    }
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

    fn sun_frame(count: u16) -> usize {
        7 - usize::from(count >> 5).min(7)
    }

    fn axis_index(sample: u16) -> usize {
        if sample < 0x3A0 {
            0
        } else if sample > 0x3A0 {
            2
        } else {
            1
        }
    }

    fn pixel(bus: &Bus, x: u32, y: u32) -> u16 {
        bus.read_u16(0x0600_0000 + y * 480 + x * 2)
    }

    fn panel_pixel(bus: &Bus, origin: u32, x: i32, y: i32) -> u16 {
        pixel(bus, origin + u32::try_from(x).unwrap(), PANEL_AT + u32::try_from(y).unwrap())
    }

    fn first_diff(a: &Pix, b: &Pix) -> (i32, i32) {
        for y in 0..PANEL_H {
            for x in 0..PANEL_W {
                if a.get(x, y) != b.get(x, y) {
                    return (x, y);
                }
            }
        }
        panic!("两帧画得一样");
    }

    fn run_until(regs: &mut Registers, bus: &mut Bus, mut done: impl FnMut(&Bus) -> bool) {
        for _ in 0..800_000 {
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

        let gyro_rest = draw_gyro(1);
        let gyro_right = draw_gyro(2);
        let tilt_rest = draw_tilt(1, 1);
        let tilt_moved = draw_tilt(0, 2);
        let sun_day = draw_sun(sun_frame(solar_count(128)));
        let sun_noon = draw_sun(sun_frame(0));
        let (gx, gy) = first_diff(&gyro_right, &gyro_rest);
        let (tx, ty) = first_diff(&tilt_moved, &tilt_rest);
        let (sx, sy) = first_diff(&sun_noon, &sun_day);

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
                && panel_pixel(bus, GYRO_AT, gx, gy) == gyro_rest.get(gx, gy)
                && panel_pixel(bus, TILT_AT, tx, ty) == tilt_rest.get(tx, ty)
                && panel_pixel(bus, SUN_AT, sx, sy) == sun_day.get(sx, sy)
                && panel_pixel(bus, SUN_AT, 63, 127) == GOLD_DIM
                && pixel(bus, 0, 0) == DESK
                && pixel(bus, 80, 16) == DESK
        });

        bus.set_gyro_rate(1);
        bus.set_tilt(-1, 1);
        bus.set_solar(255);
        let gyro = gyro_sample(1);
        let tilt_left = tilt_sample(-1);
        let tilt_down = tilt_sample(1);
        run_until(&mut regs, &mut bus, |bus| {
            bus.read_u16(0x0300_0000) == gyro
                && bus.read_u16(0x0300_0002) == tilt_left
                && bus.read_u16(0x0300_0004) == tilt_down
                && bus.read_u16(0x0300_0006) == 0
                && panel_pixel(bus, GYRO_AT, gx, gy) == gyro_right.get(gx, gy)
                && panel_pixel(bus, TILT_AT, tx, ty) == tilt_moved.get(tx, ty)
                && panel_pixel(bus, SUN_AT, sx, sy) == sun_noon.get(sx, sy)
                && panel_pixel(bus, GYRO_AT, 63, 127) == GOLD_DIM
                && axis_index(tilt_left) == 0
                && axis_index(tilt_down) == 2
        });
    }
}
