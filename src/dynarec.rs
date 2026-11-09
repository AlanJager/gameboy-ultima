//! Thumb 直线上的低位寄存器运算，在 Apple Silicon 上译成宿主指令。
//!
//! 访存、跳转、高位寄存器、寄存器移位和 ARM 仍逐条解释。
//! 译出来的代码只改 `r0`–`r7` 和 NZCV。拍数还是按原来的取指和空闲，一条条 `account` / `advance`，
//! 所以定时器、预取和画面跟解释器对齐。整段放得进下一次日程才跑，跑不了就退回解释。

use crate::bus::{Bus, CpuStall};
use crate::registers::Registers;
use crate::thumb::{self, Op};

const BLOCK: usize = 8;
/// 一条 Thumb 运算最坏的拍数。SRAM 上 16 位取指要 16 拍，再加乘法自己的 2 拍，去掉已经算进取指的 1 拍。
const WORST: u64 = 32;

#[derive(Clone, Copy)]
struct Plan {
    base: u8,
    idle: bool,
}

pub(crate) struct Dynarec {
    enabled: bool,
    compiled: u64,
    ran: u64,
    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    engine: Engine,
}

impl Default for Dynarec {
    fn default() -> Self {
        Self {
            enabled: true,
            compiled: 0,
            ran: 0,
            #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
            engine: Engine::empty(),
        }
    }
}

impl Clone for Dynarec {
    fn clone(&self) -> Self {
        Self {
            enabled: self.enabled,
            compiled: 0,
            ran: 0,
            #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
            engine: Engine::empty(),
        }
    }
}

impl std::fmt::Debug for Dynarec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dynarec")
            .field("enabled", &self.enabled)
            .finish()
    }
}

impl Dynarec {
    #[cfg(test)]
    pub(crate) fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    #[cfg(test)]
    pub(crate) fn compiled(&self) -> u64 {
        self.compiled
    }

    #[cfg(test)]
    pub(crate) fn ran(&self) -> u64 {
        self.ran
    }

    pub(crate) fn clear(&mut self) {
        #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
        self.engine.flush();
    }
}

/// 能译就跑掉 `limit` 条里的一段，返回实际条数。这段算进 `step_quiet` 的 8 条预算。
pub(crate) fn try_block(regs: &mut Registers, bus: &mut Bus, limit: u32) -> Option<u32> {
    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    {
        return try_block_host(regs, bus, limit);
    }
    #[cfg(not(all(target_arch = "aarch64", target_os = "macos")))]
    {
        let _ = (regs, bus, limit);
        None
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
fn try_block_host(regs: &mut Registers, bus: &mut Bus, limit: u32) -> Option<u32> {
    if !bus.dynarec.enabled || limit < 2 || !regs.thumb() || regs.pc() & 1 != 0 {
        return None;
    }
    if bus.stall() != CpuStall::Run || bus.dma_owing() {
        return None;
    }
    if !regs.irqs_blocked() && (bus.irq_requested() || bus.key_irq_would_fire()) {
        return None;
    }
    let room = bus.deadline().saturating_sub(bus.cycles());
    let mut max_n = usize::try_from(limit).unwrap_or(BLOCK).min(BLOCK);
    while max_n >= 2 && room < max_n as u64 * WORST {
        max_n -= 1;
    }
    if max_n < 2 {
        return None;
    }
    let pc = regs.pc();
    let mut halfs = [0u16; BLOCK];
    let mut plans = [Plan { base: 1, idle: false }; BLOCK];
    let mut n = 0;
    while n < max_n {
        let half = bus.read_u16(pc.wrapping_add((n as u32) * 2));
        let Some(plan) = classify(&thumb::decode(half)) else {
            break;
        };
        halfs[n] = half;
        plans[n] = plan;
        n += 1;
    }
    if n < 2 {
        return None;
    }
    let code = bus.dynarec.engine.lookup(pc, &halfs[..n])?;
    if bus.dynarec.engine.took_compile() {
        bus.dynarec.compiled += 1;
    }
    unsafe { invoke(code, regs) };
    bus.dynarec.ran += 1;
    for i in 0..n {
        let addr = pc.wrapping_add((i as u32) * 2);
        let fetch = bus.account(addr, 2, true);
        if plans[i].idle {
            bus.note_internal(1);
        }
        let cycles = u32::from(plans[i].base) + fetch.saturating_sub(1);
        bus.advance(cycles);
    }
    regs.set_pc(pc.wrapping_add((n as u32) * 2));
    Some(n as u32)
}

#[cfg_attr(
    not(all(target_arch = "aarch64", target_os = "macos")),
    allow(dead_code)
)]
fn classify(op: &Op) -> Option<Plan> {
    match *op {
        Op::ShiftImm { kind, .. } if kind <= 2 => Some(Plan { base: 1, idle: false }),
        Op::AddSub { .. } | Op::Imm { .. } => Some(Plan { base: 1, idle: false }),
        Op::Alu { op, .. } if !matches!(op, 2 | 3 | 4 | 7) => Some(Plan {
            base: if op == 13 { 2 } else { 1 },
            idle: op == 13,
        }),
        _ => None,
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
fn g(reg: u8) -> u8 {
    reg + 2
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
const RES: u8 = 11;
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
const T0: u8 = 12;
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
const T1: u8 = 13;
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
const T2: u8 = 14;
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
const CPSR: u8 = 15;
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
const CLEAR_NZCV: u32 = 0x1200_6DEF;

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
struct Asm {
    words: Vec<u32>,
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
impl Asm {
    fn prologue(&mut self) {
        for reg in 0..8u8 {
            self.ldr(g(reg), 0, u32::from(reg) * 4);
        }
        self.ldr(CPSR, 1, 0);
    }

    fn epilogue(&mut self) {
        for reg in 0..8u8 {
            self.str(g(reg), 0, u32::from(reg) * 4);
        }
        self.str(CPSR, 1, 0);
        self.emit(0xD65F_03C0);
    }

    fn emit_op(&mut self, op: &Op) -> bool {
        match *op {
            Op::ShiftImm { kind, amount, rm, rd } if kind <= 2 => {
                self.emit_shift(kind, amount, rm, rd);
            }
            Op::AddSub { sub, imm, rhs, rn, rd } => self.emit_add_sub(sub, imm, rhs, rn, rd),
            Op::Imm { op, rd, imm } => self.emit_imm(op, rd, imm),
            Op::Alu { op, rm, rd } if !matches!(op, 2 | 3 | 4 | 7) => self.emit_alu(op, rm, rd),
            _ => return false,
        }
        true
    }

    fn emit_shift(&mut self, kind: u8, amount: u8, rm: u8, rd: u8) {
        let src = g(rm);
        let dst = g(rd);
        let amount = u32::from(amount);
        match (kind, amount) {
            (0, 0) => {
                self.mov_reg(RES, src);
                self.mov_reg(dst, RES);
                self.set_nz();
            }
            (0, _) => {
                self.carry_from_bit(src, 32 - amount);
                self.lsl_imm(RES, src, amount);
                self.mov_reg(dst, RES);
                self.set_nzc();
            }
            (1, 0) => {
                self.carry_from_bit(src, 31);
                self.movz(RES, 0);
                self.mov_reg(dst, RES);
                self.set_nzc();
            }
            (1, _) => {
                self.carry_from_bit(src, amount - 1);
                self.lsr_imm(RES, src, amount);
                self.mov_reg(dst, RES);
                self.set_nzc();
            }
            (_, 0) => {
                self.carry_from_bit(src, 31);
                self.asr_imm(RES, src, 31);
                self.mov_reg(dst, RES);
                self.set_nzc();
            }
            _ => {
                self.carry_from_bit(src, amount - 1);
                self.asr_imm(RES, src, amount);
                self.mov_reg(dst, RES);
                self.set_nzc();
            }
        }
    }

    fn emit_add_sub(&mut self, sub: bool, imm: bool, rhs: u8, rn: u8, rd: u8) {
        let dst = g(rd);
        let left = g(rn);
        if imm {
            let imm = u32::from(rhs);
            if sub {
                self.subs_imm(RES, left, imm);
            } else {
                self.adds_imm(RES, left, imm);
            }
        } else {
            let right = g(rhs);
            if sub {
                self.subs_reg(RES, left, right);
            } else {
                self.adds_reg(RES, left, right);
            }
        }
        self.mov_reg(dst, RES);
        self.flags_from_host();
    }

    fn emit_imm(&mut self, op: u8, rd: u8, imm: u8) {
        let dst = g(rd);
        let imm = u32::from(imm);
        match op {
            0 => {
                self.movz(RES, imm);
                self.mov_reg(dst, RES);
                self.set_nz();
            }
            1 => {
                self.subs_imm(31, dst, imm);
                self.flags_from_host();
            }
            2 => {
                self.adds_imm(RES, dst, imm);
                self.mov_reg(dst, RES);
                self.flags_from_host();
            }
            _ => {
                self.subs_imm(RES, dst, imm);
                self.mov_reg(dst, RES);
                self.flags_from_host();
            }
        }
    }

    fn emit_alu(&mut self, op: u8, rm: u8, rd: u8) {
        let left = g(rd);
        let right = g(rm);
        match op {
            0 => self.logic(0x0A00_0000, left, right),
            1 => self.logic(0x4A00_0000, left, right),
            5 => self.adc_sbc(false, left, right),
            6 => self.adc_sbc(true, left, right),
            8 => {
                self.dp(0x0A00_0000, RES, left, right);
                self.set_nz();
            }
            9 => {
                self.subs_reg(RES, 31, right);
                self.mov_reg(left, RES);
                self.flags_from_host();
            }
            10 => {
                self.subs_reg(31, left, right);
                self.flags_from_host();
            }
            11 => {
                self.adds_reg(31, left, right);
                self.flags_from_host();
            }
            12 => self.logic(0x2A00_0000, left, right),
            13 => {
                self.mul(RES, left, right);
                self.mov_reg(left, RES);
                self.set_nz();
            }
            14 => self.logic(0x0A20_0000, left, right),
            _ => {
                self.dp(0x2A20_0000, RES, 31, right);
                self.mov_reg(left, RES);
                self.set_nz();
            }
        }
    }

    fn logic(&mut self, base: u32, left: u8, right: u8) {
        self.dp(base, RES, left, right);
        self.mov_reg(left, RES);
        self.set_nz();
    }

    fn adc_sbc(&mut self, sbc: bool, left: u8, right: u8) {
        self.host_carry_from_guest();
        if sbc {
            self.sbcs(RES, left, right);
        } else {
            self.adcs(RES, left, right);
        }
        self.mov_reg(left, RES);
        self.flags_from_host();
    }

    /// 结果在 `w11`。N、Z 按结果重写，C 和 V 留在客人的 CPSR 里。
    fn set_nz(&mut self) {
        self.cmp_imm(RES, 0);
        self.cset_eq(T0);
        self.lsl_imm(T0, T0, 30);
        self.lsr_imm(T1, RES, 31);
        self.lsl_imm(T1, T1, 31);
        self.orr(T0, T0, T1);
        self.lsr_imm(T1, CPSR, 28);
        self.and1(T2, T1);
        self.lsr_imm(T1, T1, 1);
        self.and1(T1, T1);
        self.lsl_imm(T2, T2, 28);
        self.lsl_imm(T1, T1, 29);
        self.orr(T0, T0, T2);
        self.orr(T0, T0, T1);
        self.merge_flags(T0);
    }

    /// 结果在 `w11`，新的 C 在 `w12` 的最低位。V 留在客人的 CPSR 里。
    fn set_nzc(&mut self) {
        self.lsl_imm(T0, T0, 29);
        self.cmp_imm(RES, 0);
        self.cset_eq(T1);
        self.lsl_imm(T1, T1, 30);
        self.orr(T0, T0, T1);
        self.lsr_imm(T1, RES, 31);
        self.lsl_imm(T1, T1, 31);
        self.orr(T0, T0, T1);
        self.lsr_imm(T1, CPSR, 28);
        self.and1(T1, T1);
        self.lsl_imm(T1, T1, 28);
        self.orr(T0, T0, T1);
        self.merge_flags(T0);
    }

    fn flags_from_host(&mut self) {
        self.mrs_nzcv(T0);
        self.merge_flags(T0);
    }

    fn host_carry_from_guest(&mut self) {
        self.lsr_imm(T0, CPSR, 29);
        self.and1(T0, T0);
        self.cmp_imm(T0, 1);
    }

    fn merge_flags(&mut self, flags: u8) {
        self.emit(CLEAR_NZCV);
        self.orr(CPSR, CPSR, flags);
    }

    fn carry_from_bit(&mut self, src: u8, bit: u32) {
        if bit == 0 {
            self.and1(T0, src);
        } else if bit >= 31 {
            self.lsr_imm(T0, src, 31);
        } else {
            self.lsr_imm(T0, src, bit);
            self.and1(T0, T0);
        }
    }

    fn emit(&mut self, word: u32) {
        self.words.push(word);
    }

    fn dp(&mut self, base: u32, rd: u8, rn: u8, rm: u8) {
        self.emit(base | (u32::from(rm) << 16) | (u32::from(rn) << 5) | u32::from(rd));
    }

    fn dpi(&mut self, base: u32, rd: u8, rn: u8, imm: u32) {
        self.emit(base | (imm << 10) | (u32::from(rn) << 5) | u32::from(rd));
    }

    fn ldr(&mut self, rt: u8, rn: u8, off: u32) {
        self.emit(0xB940_0000 | ((off / 4) << 10) | (u32::from(rn) << 5) | u32::from(rt));
    }

    fn str(&mut self, rt: u8, rn: u8, off: u32) {
        self.emit(0xB900_0000 | ((off / 4) << 10) | (u32::from(rn) << 5) | u32::from(rt));
    }

    fn adds_reg(&mut self, rd: u8, rn: u8, rm: u8) {
        self.dp(0x2B00_0000, rd, rn, rm);
    }

    fn subs_reg(&mut self, rd: u8, rn: u8, rm: u8) {
        self.dp(0x6B00_0000, rd, rn, rm);
    }

    fn adds_imm(&mut self, rd: u8, rn: u8, imm: u32) {
        self.dpi(0x3100_0000, rd, rn, imm);
    }

    fn subs_imm(&mut self, rd: u8, rn: u8, imm: u32) {
        self.dpi(0x7100_0000, rd, rn, imm);
    }

    fn adcs(&mut self, rd: u8, rn: u8, rm: u8) {
        self.dp(0x3A00_0000, rd, rn, rm);
    }

    fn sbcs(&mut self, rd: u8, rn: u8, rm: u8) {
        self.dp(0x7A00_0000, rd, rn, rm);
    }

    fn orr(&mut self, rd: u8, rn: u8, rm: u8) {
        self.dp(0x2A00_0000, rd, rn, rm);
    }

    fn mov_reg(&mut self, rd: u8, rm: u8) {
        self.orr(rd, 31, rm);
    }

    fn movz(&mut self, rd: u8, imm: u32) {
        self.emit(0x5280_0000 | (imm << 5) | u32::from(rd));
    }

    fn mul(&mut self, rd: u8, rn: u8, rm: u8) {
        self.emit(0x1B00_7C00 | (u32::from(rm) << 16) | (u32::from(rn) << 5) | u32::from(rd));
    }

    fn cmp_imm(&mut self, rn: u8, imm: u32) {
        self.subs_imm(31, rn, imm);
    }

    fn cset_eq(&mut self, rd: u8) {
        self.emit(0x1A9F_17E0 | u32::from(rd));
    }

    fn mrs_nzcv(&mut self, rt: u8) {
        self.emit(0xD53B_4200 | u32::from(rt));
    }

    fn and1(&mut self, rd: u8, rn: u8) {
        self.emit(0x1200_0000 | (u32::from(rn) << 5) | u32::from(rd));
    }

    fn ubfm(&mut self, rd: u8, rn: u8, immr: u32, imms: u32) {
        self.emit(0x5300_0000 | (immr << 16) | (imms << 10) | (u32::from(rn) << 5) | u32::from(rd));
    }

    fn sbfm(&mut self, rd: u8, rn: u8, immr: u32, imms: u32) {
        self.emit(0x1300_0000 | (immr << 16) | (imms << 10) | (u32::from(rn) << 5) | u32::from(rd));
    }

    fn lsl_imm(&mut self, rd: u8, rn: u8, shift: u32) {
        let immr = (32 - shift) & 31;
        let imms = 31 - shift;
        self.ubfm(rd, rn, immr, imms);
    }

    fn lsr_imm(&mut self, rd: u8, rn: u8, shift: u32) {
        self.ubfm(rd, rn, shift, 31);
    }

    fn asr_imm(&mut self, rd: u8, rn: u8, shift: u32) {
        self.sbfm(rd, rn, shift, 31);
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
impl Default for Asm {
    fn default() -> Self {
        Self { words: Vec::new() }
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
const MAP_LEN: usize = 1 << 20;
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
const SLOTS: usize = 1024;
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
const PROT_READ: i32 = 1;
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
const PROT_WRITE: i32 = 2;
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
const PROT_EXEC: i32 = 4;
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
const MAP_PRIVATE: i32 = 0x0002;
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
const MAP_JIT: i32 = 0x0800;
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
const MAP_ANON: i32 = 0x1000;

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
unsafe extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut u8;
    fn munmap(addr: *mut u8, len: usize) -> i32;
    fn pthread_jit_write_protect_np(enabled: i32);
    fn sys_icache_invalidate(start: *mut u8, len: usize);
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
struct Map {
    ptr: *mut u8,
    len: usize,
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
unsafe impl Send for Map {}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
impl Drop for Map {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                munmap(self.ptr, self.len);
            }
        }
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
impl Map {
    fn empty() -> Self {
        Self {
            ptr: std::ptr::null_mut(),
            len: 0,
        }
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[derive(Clone, Copy, Default)]
struct Slot {
    gen: u32,
    pc: u32,
    len: u8,
    halfs: [u16; BLOCK],
    offset: u32,
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
struct Engine {
    map: Map,
    used: usize,
    gen: u32,
    failed: bool,
    fresh: bool,
    slots: Vec<Slot>,
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
impl Engine {
    fn empty() -> Self {
        Self {
            map: Map::empty(),
            used: 0,
            gen: 1,
            failed: false,
            fresh: false,
            slots: Vec::new(),
        }
    }

    fn flush(&mut self) {
        self.used = 0;
        self.gen = self.gen.wrapping_add(1);
        if self.gen == 0 {
            self.gen = 1;
        }
    }

    fn took_compile(&self) -> bool {
        self.fresh
    }

    fn lookup(&mut self, pc: u32, halfs: &[u16]) -> Option<*const u8> {
        self.fresh = false;
        if self.failed || halfs.len() < 2 || halfs.len() > BLOCK {
            return None;
        }
        if self.slots.is_empty() {
            self.slots.resize(SLOTS, Slot::default());
        }
        let index = ((pc >> 1) as usize) & (SLOTS - 1);
        let slot = &self.slots[index];
        if slot.gen == self.gen
            && slot.pc == pc
            && slot.len == halfs.len() as u8
            && slot.halfs[..halfs.len()] == *halfs
        {
            return Some(unsafe { self.map.ptr.add(slot.offset as usize) });
        }
        self.compile(index, pc, halfs)
    }

    fn compile(&mut self, index: usize, pc: u32, halfs: &[u16]) -> Option<*const u8> {
        let mut asm = Asm::default();
        asm.prologue();
        for half in halfs {
            if !asm.emit_op(&thumb::decode(*half)) {
                return None;
            }
        }
        asm.epilogue();
        if asm.words.len() > 512 || !self.ensure_map() {
            return None;
        }
        let bytes = asm.words.len() * 4;
        if self.used + bytes > MAP_LEN {
            self.flush();
        }
        if self.used + bytes > MAP_LEN {
            return None;
        }
        let offset = self.used;
        self.patch(offset, &asm.words);
        self.used += bytes;
        let mut stored = [0u16; BLOCK];
        stored[..halfs.len()].copy_from_slice(halfs);
        self.slots[index] = Slot {
            gen: self.gen,
            pc,
            len: halfs.len() as u8,
            halfs: stored,
            offset: offset as u32,
        };
        self.fresh = true;
        Some(unsafe { self.map.ptr.add(offset) })
    }

    fn ensure_map(&mut self) -> bool {
        if self.failed {
            return false;
        }
        if !self.map.ptr.is_null() {
            return true;
        }
        let ptr = unsafe {
            mmap(
                std::ptr::null_mut(),
                MAP_LEN,
                PROT_READ | PROT_WRITE | PROT_EXEC,
                MAP_PRIVATE | MAP_ANON | MAP_JIT,
                -1,
                0,
            )
        };
        if ptr.is_null() || ptr == usize::MAX as *mut u8 {
            self.failed = true;
            return false;
        }
        self.map.ptr = ptr;
        self.map.len = MAP_LEN;
        true
    }

    fn patch(&self, offset: usize, words: &[u32]) {
        let bytes = words.len() * 4;
        unsafe {
            let dst = self.map.ptr.add(offset);
            pthread_jit_write_protect_np(0);
            std::ptr::copy_nonoverlapping(words.as_ptr() as *const u8, dst, bytes);
            sys_icache_invalidate(dst, bytes);
            pthread_jit_write_protect_np(1);
        }
    }
}

/// 宿主代码只碰 `r0`–`r7` 和 CPSR。调用约定用 `x0`、`x1` 传入这两处，其余只用调用者保存的寄存器。
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
unsafe fn invoke(code: *const u8, regs: &mut Registers) {
    let f: unsafe extern "C" fn(*mut u32, *mut u32) = std::mem::transmute(code);
    f(regs.low_regs_ptr(), regs.cpsr_ptr());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mov_imm(rd: u16, imm: u16) -> u16 {
        0x2000 | (rd << 8) | imm
    }

    fn cmp_imm(rd: u16, imm: u16) -> u16 {
        0x2800 | (rd << 8) | imm
    }

    fn add_imm(rd: u16, imm: u16) -> u16 {
        0x3000 | (rd << 8) | imm
    }

    fn sub_imm(rd: u16, imm: u16) -> u16 {
        0x3800 | (rd << 8) | imm
    }

    fn add_reg(rd: u16, rn: u16, rm: u16) -> u16 {
        0x1800 | (rm << 6) | (rn << 3) | rd
    }

    fn sub_reg(rd: u16, rn: u16, rm: u16) -> u16 {
        0x1A00 | (rm << 6) | (rn << 3) | rd
    }

    fn add3(rd: u16, rn: u16, imm: u16) -> u16 {
        0x1C00 | (imm << 6) | (rn << 3) | rd
    }

    fn sub3(rd: u16, rn: u16, imm: u16) -> u16 {
        0x1E00 | (imm << 6) | (rn << 3) | rd
    }

    fn shift(kind: u16, amount: u16, rm: u16, rd: u16) -> u16 {
        (kind << 11) | (amount << 6) | (rm << 3) | rd
    }

    fn alu(op: u16, rm: u16, rd: u16) -> u16 {
        0x4000 | (op << 6) | (rm << 3) | rd
    }

    fn ldr_word(rd: u16, rn: u16, words: u16) -> u16 {
        0x6800 | (words << 6) | (rn << 3) | rd
    }

    fn branch_to_start(index: u16) -> u16 {
        let imm = (-(i32::from(index) + 2)) as u16;
        0xE000 | (imm & 0x7FF)
    }

    fn run(code: &[u16], on: bool, steps: u32) -> (Registers, Bus) {
        let mut bus = Bus::new();
        install(&mut bus, code);
        bus.set_dynarec(on);
        let mut regs = Registers::arm_start(0x0300_0000);
        regs.set_thumb(true);
        for _ in 0..steps {
            crate::step_quiet(&mut regs, &mut bus).unwrap();
        }
        (regs, bus)
    }

    fn install(bus: &mut Bus, code: &[u16]) {
        for (index, half) in code.iter().enumerate() {
            bus.write_u16(0x0300_0000 + (index as u32) * 2, *half);
        }
    }

    fn assert_same(off: &Registers, on: &Registers, off_cycles: u64, on_cycles: u64) {
        assert_eq!(
            off_cycles, on_cycles,
            "cycles\noff {}\non  {}",
            off.format_line(),
            on.format_line()
        );
        assert_eq!(off.cpsr(), on.cpsr(), "cpsr {}", on.format_line());
        assert_eq!(off.pc(), on.pc(), "pc");
        for index in 0..16u8 {
            assert_eq!(
                off.get(index),
                on.get(index),
                "r{index}\n{}",
                on.format_line()
            );
        }
    }

    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    fn assert_ran(on: &Bus, off: &Bus) {
        assert!(on.dynarec_ran() > 0, "宿主代码没有跑起来");
        assert_eq!(off.dynarec_ran(), 0);
    }

    #[test]
    fn thumb_helpers_decode() {
        use crate::thumb::{decode, Op};
        assert!(matches!(
            decode(mov_imm(0, 1)),
            Op::Imm { op: 0, rd: 0, imm: 1 }
        ));
        assert!(matches!(
            decode(cmp_imm(2, 7)),
            Op::Imm { op: 1, rd: 2, imm: 7 }
        ));
        assert!(matches!(
            decode(add_imm(3, 4)),
            Op::Imm { op: 2, rd: 3, imm: 4 }
        ));
        assert!(matches!(
            decode(sub_imm(1, 9)),
            Op::Imm { op: 3, rd: 1, imm: 9 }
        ));
        assert!(matches!(
            decode(add_reg(2, 1, 1)),
            Op::AddSub {
                sub: false,
                imm: false,
                rhs: 1,
                rn: 1,
                rd: 2
            }
        ));
        assert!(matches!(
            decode(sub_reg(3, 0, 1)),
            Op::AddSub {
                sub: true,
                imm: false,
                rhs: 1,
                rn: 0,
                rd: 3
            }
        ));
        assert!(matches!(
            decode(add3(4, 0, 3)),
            Op::AddSub {
                sub: false,
                imm: true,
                rhs: 3,
                rn: 0,
                rd: 4
            }
        ));
        assert!(matches!(
            decode(sub3(5, 1, 1)),
            Op::AddSub {
                sub: true,
                imm: true,
                rhs: 1,
                rn: 1,
                rd: 5
            }
        ));
        assert!(matches!(
            decode(shift(0, 24, 1, 1)),
            Op::ShiftImm {
                kind: 0,
                amount: 24,
                rm: 1,
                rd: 1
            }
        ));
        assert!(matches!(
            decode(shift(1, 0, 1, 4)),
            Op::ShiftImm {
                kind: 1,
                amount: 0,
                rm: 1,
                rd: 4
            }
        ));
        assert!(matches!(
            decode(shift(2, 4, 3, 6)),
            Op::ShiftImm {
                kind: 2,
                amount: 4,
                rm: 3,
                rd: 6
            }
        ));
        assert!(matches!(decode(alu(5, 0, 3)), Op::Alu { op: 5, rm: 0, rd: 3 }));
        assert!(matches!(decode(alu(13, 0, 4)), Op::Alu { op: 13, rm: 0, rd: 4 }));
        assert!(matches!(
            decode(ldr_word(1, 0, 8)),
            Op::MemImm {
                load: true,
                size: 4,
                imm: 8,
                rn: 0,
                rd: 1
            }
        ));
        assert!(matches!(decode(0xE7FE), Op::Branch { imm: 0x7FE }));
    }

    #[test]
    fn thumb_alu_block_matches_the_interpreter() {
        let code = [
            mov_imm(0, 1),
            mov_imm(1, 0x80),
            shift(0, 24, 1, 1),
            add_reg(2, 1, 1),
            mov_imm(3, 1),
            alu(5, 0, 3),
            shift(1, 0, 1, 4),
            0xE7FE,
        ];
        let (off, off_bus) = run(&code, false, 1);
        let (on, on_bus) = run(&code, true, 1);
        assert_eq!(off.get(0), 1);
        assert_eq!(off.get(1), 0x8000_0000);
        assert_eq!(off.get(2), 0);
        assert_eq!(off.get(3), 3);
        assert_eq!(off.get(4), 0);
        assert_eq!(off.cpsr(), 0x6000_00F0, "{}", off.format_line());
        assert_eq!(off.pc(), 0x0300_000E);
        assert_same(&off, &on, off_bus.cycles(), on_bus.cycles());
        #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
        {
            assert_ran(&on_bus, &off_bus);
            assert_eq!(on_bus.dynarec_compiled(), 1);
        }
    }

    #[test]
    fn mixed_thumb_alu_stays_cycle_accurate() {
        let mut code = vec![
            mov_imm(0, 0xF0),
            mov_imm(1, 0x0F),
            mov_imm(2, 0xFF),
            mov_imm(3, 1),
            mov_imm(4, 2),
            mov_imm(5, 3),
            mov_imm(6, 4),
            mov_imm(7, 5),
            cmp_imm(0, 1),
            add_imm(0, 1),
            sub_imm(0, 1),
            add_reg(2, 0, 1),
            sub_reg(2, 0, 1),
            add3(2, 0, 1),
            sub3(2, 0, 1),
            shift(1, 4, 0, 1),
            shift(2, 4, 3, 6),
            shift(1, 0, 0, 7),
            shift(2, 0, 3, 5),
            alu(0, 0, 5),
            alu(1, 1, 6),
            alu(12, 0, 7),
            alu(14, 1, 4),
            alu(15, 1, 3),
            alu(13, 0, 4),
            alu(5, 0, 2),
            alu(6, 1, 2),
            alu(9, 1, 4),
            alu(8, 1, 0),
            alu(11, 1, 0),
            alu(10, 1, 5),
        ];
        assert_eq!(code.len(), 31);
        code.push(branch_to_start(code.len() as u16));
        let (off, off_bus) = run(&code, false, 8);
        let (on, on_bus) = run(&code, true, 8);
        assert!(off_bus.cycles() > 32, "{}", off.format_line());
        assert_same(&off, &on, off_bus.cycles(), on_bus.cycles());
        #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
        {
            assert_ran(&on_bus, &off_bus);
            assert_eq!(on_bus.dynarec_compiled(), 4);
            assert_eq!(on_bus.dynarec_ran(), 8);
        }
    }

    #[test]
    fn rom_and_ewram_keep_their_wait_states() {
        let code = [
            mov_imm(0, 1),
            mov_imm(1, 2),
            add_reg(2, 0, 1),
            add_imm(3, 4),
            shift(0, 4, 1, 4),
            alu(13, 1, 4),
            cmp_imm(0, 1),
            0xE7FE,
        ];
        let mut rom = vec![0u8; code.len() * 2];
        for (index, half) in code.iter().enumerate() {
            rom[index * 2..index * 2 + 2].copy_from_slice(&half.to_le_bytes());
        }
        let run_at = |origin: u32, on: bool, rom: Option<&[u8]>| {
            let mut bus = Bus::new();
            if let Some(bytes) = rom {
                bus.load_rom(bytes);
            } else {
                for (index, half) in code.iter().enumerate() {
                    bus.write_u16(origin + (index as u32) * 2, *half);
                }
            }
            bus.set_dynarec(on);
            let mut regs = Registers::arm_start(origin);
            regs.set_thumb(true);
            crate::step_quiet(&mut regs, &mut bus).unwrap();
            (regs, bus)
        };
        let (rom_off, rom_off_bus) = run_at(0x0800_0000, false, Some(&rom));
        let (rom_on, rom_on_bus) = run_at(0x0800_0000, true, Some(&rom));
        assert!(rom_off_bus.cycles() > 8, "rom {}", rom_off.format_line());
        assert_same(&rom_off, &rom_on, rom_off_bus.cycles(), rom_on_bus.cycles());
        let (ew_off, ew_off_bus) = run_at(0x0200_0000, false, None);
        let (ew_on, ew_on_bus) = run_at(0x0200_0000, true, None);
        assert!(ew_off_bus.cycles() > 8, "ewram {}", ew_off.format_line());
        assert_same(&ew_off, &ew_on, ew_off_bus.cycles(), ew_on_bus.cycles());
        #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
        {
            assert_ran(&rom_on_bus, &rom_off_bus);
            assert_ran(&ew_on_bus, &ew_off_bus);
        }
    }

    #[test]
    fn a_load_stays_outside_the_block() {
        let code = [
            mov_imm(0, 3),
            shift(0, 24, 0, 0),
            ldr_word(1, 0, 8),
            mov_imm(2, 7),
            0xE7FE,
        ];
        let mut off_bus = Bus::new();
        let mut on_bus = Bus::new();
        install(&mut off_bus, &code);
        install(&mut on_bus, &code);
        off_bus.write_u32(0x0300_0020, 0xA1B2_C3D4);
        on_bus.write_u32(0x0300_0020, 0xA1B2_C3D4);
        off_bus.set_dynarec(false);
        let mut off = Registers::arm_start(0x0300_0000);
        let mut on = Registers::arm_start(0x0300_0000);
        off.set_thumb(true);
        on.set_thumb(true);
        crate::step_quiet(&mut off, &mut off_bus).unwrap();
        crate::step_quiet(&mut on, &mut on_bus).unwrap();
        assert_eq!(off.get(0), 0x0300_0000);
        assert_eq!(off.get(1), 0xA1B2_C3D4);
        assert_eq!(off.get(2), 7);
        assert_same(&off, &on, off_bus.cycles(), on_bus.cycles());
        #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
        assert_ran(&on_bus, &off_bus);
    }

    #[test]
    fn rewritten_thumb_is_compiled_again() {
        let code = [
            mov_imm(0, 1),
            mov_imm(1, 2),
            add_reg(2, 0, 1),
            branch_to_start(3),
        ];
        let mut off_bus = Bus::new();
        let mut on_bus = Bus::new();
        install(&mut off_bus, &code);
        install(&mut on_bus, &code);
        off_bus.set_dynarec(false);
        let mut off = Registers::arm_start(0x0300_0000);
        let mut on = Registers::arm_start(0x0300_0000);
        off.set_thumb(true);
        on.set_thumb(true);
        crate::step_quiet(&mut off, &mut off_bus).unwrap();
        crate::step_quiet(&mut on, &mut on_bus).unwrap();
        assert_eq!(off.get(2), 3);
        assert_same(&off, &on, off_bus.cycles(), on_bus.cycles());
        #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
        {
            assert_eq!(on_bus.dynarec_compiled(), 1);
            assert_eq!(on_bus.dynarec_ran(), 2);
        }
        off_bus.write_u16(0x0300_0000, mov_imm(0, 5));
        on_bus.write_u16(0x0300_0000, mov_imm(0, 5));
        crate::step_quiet(&mut off, &mut off_bus).unwrap();
        crate::step_quiet(&mut on, &mut on_bus).unwrap();
        assert_eq!(off.get(2), 7);
        assert_same(&off, &on, off_bus.cycles(), on_bus.cycles());
        #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
        {
            assert_eq!(on_bus.dynarec_compiled(), 2);
            assert_eq!(on_bus.dynarec_ran(), 4);
        }
    }

    #[test]
    fn a_timer_irq_still_lands_on_the_same_cycle() {
        let code = [add_imm(0, 1), add_imm(0, 1), branch_to_start(2)];
        let prep = |on: bool| {
            let mut bus = Bus::new();
            install(&mut bus, &code);
            bus.write_u32(0x0300_1000, 0xE12F_FF1E);
            bus.write_u32(0x0300_7FFC, 0x0300_1000);
            bus.write_u16(0x0400_0200, 0x0008);
            bus.write_u16(0x0400_0208, 1);
            bus.write_u16(0x0400_0100, 0xFF00);
            bus.write_u16(0x0400_0102, 0x00C0);
            bus.set_dynarec(on);
            let mut regs = Registers::arm_start(0x0300_0000);
            regs.set_thumb(true);
            regs.set_boot_stacks();
            regs.set_cpsr(0x0000_003F);
            (regs, bus)
        };
        let (mut off, mut off_bus) = prep(false);
        let (mut on, mut on_bus) = prep(true);
        for _ in 0..40 {
            crate::step_quiet(&mut off, &mut off_bus).unwrap();
            crate::step_quiet(&mut on, &mut on_bus).unwrap();
        }
        assert!(off_bus.cycles() > 256, "{}", off.format_line());
        assert_same(&off, &on, off_bus.cycles(), on_bus.cycles());
        assert_eq!(off_bus.read_u16(0x0400_0202), on_bus.read_u16(0x0400_0202));
        #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
        assert_ran(&on_bus, &off_bus);
    }

    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    #[test]
    fn host_encodings_match_the_assembler() {
        let mut asm = Asm::default();
        asm.adds_reg(11, 2, 3);
        asm.subs_reg(11, 31, 3);
        asm.adds_imm(11, 2, 1);
        asm.subs_imm(31, 12, 1);
        asm.movz(11, 255);
        asm.ldr(2, 0, 0);
        asm.str(9, 0, 28);
        asm.mul(11, 2, 3);
        asm.lsl_imm(11, 2, 1);
        asm.lsr_imm(11, 2, 1);
        asm.asr_imm(11, 2, 31);
        asm.and1(12, 12);
        asm.cset_eq(12);
        asm.mrs_nzcv(12);
        asm.emit(CLEAR_NZCV);
        asm.emit(0xD65F_03C0);
        assert_eq!(
            asm.words,
            vec![
                0x2B03_004B,
                0x6B03_03EB,
                0x3100_044B,
                0x7100_059F,
                0x5280_1FEB,
                0xB940_0002,
                0xB900_1C09,
                0x1B03_7C4B,
                0x531F_784B,
                0x5301_7C4B,
                0x131F_7C4B,
                0x1200_018C,
                0x1A9F_17EC,
                0xD53B_420C,
                0x1200_6DEF,
                0xD65F_03C0,
            ]
        );
    }
}
