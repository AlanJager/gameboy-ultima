//! ARM7TDMI 的寄存器。
//!
//! 现在放五份。`r0`–`r15` 是用户和系统看到的那一份。
//! 管理模式、中断模式、未定义模式各有 SP、LR 和 SPSR。快速中断另有 `r8`–`r14` 和 SPSR。
//! 中止仍先共用用户这份，也没有 SPSR。
//!
//! 当前模式是 CPSR 的低 5 位。`get` / `set` 看这一位决定读写哪一份，
//! 换模式时旧的值留在原地。

const N: u32 = 1 << 31;
const Z: u32 = 1 << 30;
const C: u32 = 1 << 29;
const V: u32 = 1 << 28;
const I: u32 = 1 << 7;
const F: u32 = 1 << 6;
const T: u32 = 1 << 5;

pub(crate) const MODE_USER: u8 = 0x10;
pub(crate) const MODE_FIQ: u8 = 0x11;
pub(crate) const MODE_IRQ: u8 = 0x12;
pub(crate) const MODE_SVC: u8 = 0x13;
pub(crate) const MODE_UND: u8 = 0x1B;
const MODE_SYSTEM: u8 = 0x1F;

const SWI_VECTOR: u32 = 0x08;
const IRQ_VECTOR: u32 = 0x18;
pub(crate) const UND_VECTOR: u32 = 0x04;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Bank {
    sp: u32,
    lr: u32,
    spsr: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registers {
    /// 用户和系统看到的 `r0`–`r14`，以及所有模式共用的 PC。
    r: [u32; 16],
    fiq_high: [u32; 5],
    fiq: Bank,
    irq: Bank,
    svc: Bank,
    und: Bank,
    cpsr: u32,
}

impl Registers {
    /// ARM 状态、用户模式，PC 停在 `entry`。I 和 F 先置上，所以还不会响应 IRQ。
    pub fn arm_start(entry: u32) -> Self {
        let mut regs = Self {
            r: [0; 16],
            fiq_high: [0; 5],
            fiq: Bank::default(),
            irq: Bank::default(),
            svc: Bank::default(),
            und: Bank::default(),
            cpsr: 0x0000_00D0,
        };
        regs.r[15] = entry;
        regs
    }

    /// 卡带入口。系统模式，寄存器和用户模式同一份，但 `msr` 可以改模式。
    /// 用户栈 `0x03007F00`，中断栈 `0x03007FA0`，管理栈 `0x03007FE0`。I 和 F 保持开机时的屏蔽。
    pub fn set_boot_stacks(&mut self) {
        self.set_in(13, 0x0300_7F00, MODE_USER);
        self.set_in(13, 0x0300_7FA0, MODE_IRQ);
        self.set_in(13, 0x0300_7FE0, MODE_SVC);
        self.cpsr = (self.cpsr & !0x1F) | u32::from(MODE_SYSTEM);
    }

    pub fn pc(&self) -> u32 {
        self.r[15]
    }

    pub fn set_pc(&mut self, value: u32) {
        self.r[15] = value;
    }

    /// 按当前的 T 位对齐。回到 Thumb 时保留 bit 1，ARM 时按字对齐。
    pub(crate) fn set_pc_aligned(&mut self, value: u32) {
        let aligned = if self.thumb() { value & !1 } else { value & !3 };
        self.r[15] = aligned;
    }

    pub fn mode(&self) -> u8 {
        (self.cpsr & 0x1F) as u8
    }

    pub fn get(&self, index: u8) -> u32 {
        self.get_in(index, self.mode())
    }

    pub fn set(&mut self, index: u8, value: u32) {
        self.set_in(index, value, self.mode());
    }

    pub(crate) fn get_in(&self, index: u8, mode: u8) -> u32 {
        match index {
            8..=12 if mode == MODE_FIQ => self.fiq_high[index as usize - 8],
            13 | 14 if mode == MODE_SVC => self.svc.sp_lr(index),
            13 | 14 if mode == MODE_IRQ => self.irq.sp_lr(index),
            13 | 14 if mode == MODE_UND => self.und.sp_lr(index),
            13 | 14 if mode == MODE_FIQ => self.fiq.sp_lr(index),
            _ => self.r[index as usize],
        }
    }

    pub(crate) fn set_in(&mut self, index: u8, value: u32, mode: u8) {
        if (8..13).contains(&index) && mode == MODE_FIQ {
            self.fiq_high[index as usize - 8] = value;
        } else if matches!(index, 13 | 14) && mode == MODE_SVC {
            self.svc.set_sp_lr(index, value);
        } else if matches!(index, 13 | 14) && mode == MODE_IRQ {
            self.irq.set_sp_lr(index, value);
        } else if matches!(index, 13 | 14) && mode == MODE_UND {
            self.und.set_sp_lr(index, value);
        } else if matches!(index, 13 | 14) && mode == MODE_FIQ {
            self.fiq.set_sp_lr(index, value);
        } else {
            self.r[index as usize] = value;
        }
    }

    pub fn cpsr(&self) -> u32 {
        self.cpsr
    }

    /// 装入一整份 CPSR。模式位变了，之后的 `get` / `set` 就换到那一份。
    pub fn set_cpsr(&mut self, value: u32) {
        self.cpsr = value;
    }

    /// 当前模式的 SPSR。管理、中断、未定义和快速中断各有一份。
    pub(crate) fn spsr(&self) -> Option<u32> {
        match self.mode() {
            MODE_SVC => Some(self.svc.spsr),
            MODE_IRQ => Some(self.irq.spsr),
            MODE_UND => Some(self.und.spsr),
            MODE_FIQ => Some(self.fiq.spsr),
            _ => None,
        }
    }

    fn set_spsr(&mut self, value: u32) -> bool {
        match self.mode() {
            MODE_SVC => self.svc.spsr = value,
            MODE_IRQ => self.irq.spsr = value,
            MODE_UND => self.und.spsr = value,
            MODE_FIQ => self.fiq.spsr = value,
            _ => return false,
        }
        true
    }

    pub(crate) fn irqs_blocked(&self) -> bool {
        self.cpsr & I != 0
    }

    /// IRQ。返回地址是下一条指令再加 4，PC 指到 BIOS 向量 `0x18`。
    ///
    /// 用户处理函数不在这里跳。`0x18` 那段会把寄存器压栈，再 `ldr pc` 到
    /// `0x03FFFFFC`。加载进 PC 不看最低位，所以处理函数总是 ARM。
    /// 它用 `bx lr` 回到 `0x138`，再由 `subs pc, lr, #4` 恢复 SPSR，包括 T。
    pub(crate) fn enter_irq(&mut self, return_pc: u32) {
        self.enter_exception(IRQ_VECTOR, MODE_IRQ, return_pc, false);
    }

    /// 未定义指令进入未定义模式，向量 `0x04`。
    /// 返回地址是下一条指令，`movs pc, lr` 可以回来。清掉 T，置上 I，F 不动。
    pub(crate) fn enter_und(&mut self, return_pc: u32) {
        self.enter_exception(UND_VECTOR, MODE_UND, return_pc, false);
    }

    /// 进入异常：先换模式，再写新模式的 SPSR 和 LR，最后把 PC 指到向量。
    ///
    /// 旧模式的寄存器留在原地。`mask_fiq` 只在复位和 FIQ 时置上 F。
    pub(crate) fn enter_exception(
        &mut self,
        vector: u32,
        mode: u8,
        return_pc: u32,
        mask_fiq: bool,
    ) {
        let saved = self.cpsr;
        let mut next = saved | I;
        next &= !T;
        if mask_fiq {
            next |= F;
        }
        next = (next & !0x1F) | u32::from(mode);
        self.cpsr = next;
        assert!(self.set_spsr(saved), "这个模式没有 SPSR");
        self.set(14, return_pc);
        self.set_pc(vector);
    }

    /// `swi` 进入管理模式，向量 `0x08`。返回地址是下一条指令。
    pub(crate) fn enter_swi(&mut self, return_pc: u32) {
        self.enter_exception(SWI_VECTOR, MODE_SVC, return_pc, false);
    }

    /// 软复位和硬复位之后的寄存器。系统模式，ARM，I 和 F 置上，三条栈按开机的位置。
    /// `r14` 和 PC 都是 `target`。
    pub(crate) fn prepare_reset(&mut self, target: u32) {
        self.r[..13].fill(0);
        self.svc.lr = 0;
        self.svc.spsr = 0;
        self.irq.lr = 0;
        self.irq.spsr = 0;
        // 未定义组没有开机栈，复位时整份清掉。
        self.und = Bank::default();
        self.set_in(13, 0x0300_7F00, MODE_USER);
        self.set_in(13, 0x0300_7FA0, MODE_IRQ);
        self.set_in(13, 0x0300_7FE0, MODE_SVC);
        self.r[14] = target;
        self.r[15] = target;
        self.cpsr = I | F | u32::from(MODE_SYSTEM);
    }

    /// `msr` 按字段掩码写 CPSR 或当前模式的 SPSR。
    ///
    /// 掩码的 bit 0 是控制位，bit 3 是标志位。用户模式只能改标志。
    pub(crate) fn write_status(&mut self, spsr: bool, mask: u8, value: u32) -> Result<(), String> {
        if spsr {
            let Some(current) = self.spsr() else {
                return Err("这一步没有 SPSR".to_string());
            };
            self.set_spsr(merge_status(current, mask, value));
            return Ok(());
        }
        let allow = if self.mode() == MODE_USER {
            mask & 8
        } else {
            mask
        };
        self.cpsr = merge_status(self.cpsr, allow, value);
        Ok(())
    }

    pub fn thumb(&self) -> bool {
        self.cpsr & T != 0
    }

    pub fn set_thumb(&mut self, thumb: bool) {
        if thumb {
            self.cpsr |= T;
        } else {
            self.cpsr &= !T;
        }
    }

    pub fn flag_n(&self) -> bool {
        self.cpsr & N != 0
    }
    pub fn flag_z(&self) -> bool {
        self.cpsr & Z != 0
    }
    pub fn flag_c(&self) -> bool {
        self.cpsr & C != 0
    }
    pub fn flag_v(&self) -> bool {
        self.cpsr & V != 0
    }

    pub fn set_nzcv(&mut self, n: bool, z: bool, c: bool, v: bool) {
        let mut cpsr = self.cpsr & !(N | Z | C | V);
        if n {
            cpsr |= N;
        }
        if z {
            cpsr |= Z;
        }
        if c {
            cpsr |= C;
        }
        if v {
            cpsr |= V;
        }
        self.cpsr = cpsr;
    }

    pub fn set_nzc(&mut self, n: bool, z: bool, c: bool) {
        self.set_nzcv(n, z, c, self.flag_v());
    }

    pub fn format_line(&self) -> String {
        format!(
            "r0:{:08X} r1:{:08X} r2:{:08X} r3:{:08X} SP:{:08X} LR:{:08X} PC:{:08X} CPSR:{:08X} NZCV:{}{}{}{}",
            self.get(0),
            self.get(1),
            self.get(2),
            self.get(3),
            self.get(13),
            self.get(14),
            self.r[15],
            self.cpsr,
            u8::from(self.flag_n()),
            u8::from(self.flag_z()),
            u8::from(self.flag_c()),
            u8::from(self.flag_v()),
        )
    }
}

impl Bank {
    fn sp_lr(&self, index: u8) -> u32 {
        if index == 13 {
            self.sp
        } else {
            self.lr
        }
    }

    fn set_sp_lr(&mut self, index: u8, value: u32) {
        if index == 13 {
            self.sp = value;
        } else {
            self.lr = value;
        }
    }
}

fn merge_status(current: u32, mask: u8, value: u32) -> u32 {
    let mut out = current;
    if mask & 1 != 0 {
        out = (out & !0xFF) | (value & 0xFF);
    }
    if mask & 2 != 0 {
        out = (out & !0xFF00) | (value & 0xFF00);
    }
    if mask & 4 != 0 {
        out = (out & !0x00FF_0000) | (value & 0x00FF_0000);
    }
    if mask & 8 != 0 {
        out = (out & !0xFF00_0000) | (value & 0xFF00_0000);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switching_mode_keeps_the_svc_bank() {
        let mut regs = Registers::arm_start(0);
        regs.set(8, 8);
        regs.set(13, 13);
        regs.set(14, 14);
        regs.set_cpsr(0x0000_00D3);
        assert_eq!(regs.mode(), MODE_SVC);
        assert_eq!(regs.get(8), 8);
        assert_eq!(regs.get(13), 0);
        assert_eq!(regs.get(14), 0);
        regs.set(13, 2);
        regs.set(14, 3);
        regs.set_cpsr(0x0000_00D1);
        assert_eq!(regs.mode(), MODE_FIQ);
        assert_eq!(regs.get(8), 0);
        assert!(regs.spsr().is_some());
        regs.set(8, 64);
        regs.set_cpsr(0x0000_00D0);
        assert_eq!(regs.get(8), 8);
        regs.set_cpsr(0x0000_00D1);
        assert_eq!(regs.get(8), 64);
        regs.set_cpsr(0x0000_00D2);
        assert_eq!(regs.mode(), MODE_IRQ);
        assert_eq!(regs.get(13), 0);
        assert!(regs.spsr().is_some());
        regs.set(13, 7);
        regs.set_cpsr(0x0000_00D0);
        assert_eq!(regs.get(13), 13);
        assert_eq!(regs.get(14), 14);
        regs.set_cpsr(0x0000_00D2);
        assert_eq!(regs.get(13), 7);
        regs.set_cpsr(0x0000_00D0);
        regs.set_cpsr(0x0000_00D3);
        assert_eq!(regs.get(13), 2);
        assert_eq!(regs.get(14), 3);
    }

    #[test]
    fn cartridge_boot_sets_the_irq_stack() {
        let mut regs = Registers::arm_start(0x0800_0000);
        regs.set_boot_stacks();
        regs.set_cpsr(0x12);
        assert_eq!(regs.get(13), 0x0300_7FA0);
        assert_eq!(regs.get_in(13, MODE_USER), 0x0300_7F00);
        assert_eq!(regs.get_in(13, MODE_SVC), 0x0300_7FE0);
    }

    #[test]
    fn system_shares_the_user_bank_and_has_no_spsr() {
        let mut regs = Registers::arm_start(0);
        regs.set(14, 5);
        regs.set_cpsr(0x0000_00DF);
        assert_eq!(regs.mode(), 0x1F);
        assert_eq!(regs.get(14), 5);
        assert!(regs.spsr().is_none());
        regs.set(13, 9);
        regs.set_cpsr(0x0000_00D0);
        assert_eq!(regs.get(13), 9);
    }

    #[test]
    fn swi_entry_saves_cpsr_and_clears_thumb() {
        let mut regs = Registers::arm_start(0x0300_0000);
        regs.set(13, 0x1111);
        regs.set_thumb(true);
        regs.set_nzcv(true, false, false, false);
        regs.enter_swi(0x0300_0002);
        assert_eq!(regs.mode(), MODE_SVC);
        assert!(!regs.thumb());
        assert_eq!(regs.cpsr(), 0x8000_00D3);
        assert_eq!(regs.spsr(), Some(0x8000_00F0));
        assert_eq!(regs.get(14), 0x0300_0002);
        assert_eq!(regs.get(13), 0);
        assert_eq!(regs.pc(), SWI_VECTOR);
        assert_eq!(regs.get_in(13, MODE_USER), 0x1111);
    }

    #[test]
    fn undefined_entry_banks_sp_and_lr() {
        let mut regs = Registers::arm_start(0x0800_0000);
        regs.set(13, 0x1111);
        regs.set(14, 0x2222);
        regs.set_thumb(true);
        regs.set_nzcv(true, false, true, false);
        regs.enter_und(0x0800_0004);
        assert_eq!(regs.mode(), MODE_UND);
        assert!(!regs.thumb());
        assert!(regs.irqs_blocked());
        assert_eq!(regs.cpsr() & (1 << 6), 1 << 6);
        assert_eq!(regs.spsr(), Some(0xA000_00F0));
        assert_eq!(regs.get(14), 0x0800_0004);
        assert_eq!(regs.get(13), 0);
        assert_eq!(regs.pc(), UND_VECTOR);
        assert_eq!(regs.get_in(13, MODE_USER), 0x1111);
        assert_eq!(regs.get_in(14, MODE_USER), 0x2222);
        regs.set(13, 0x3333);
        regs.set_cpsr(0x0000_00D0);
        assert_eq!(regs.get(13), 0x1111);
        assert_eq!(regs.get(14), 0x2222);
        regs.set_cpsr(0x0000_00DB);
        assert_eq!(regs.get(13), 0x3333);
        assert_eq!(regs.spsr(), Some(0xA000_00F0));
    }
}
use serde::{Deserialize, Serialize};
