//! 译码缓存。解释器还是逐条执行，只是同一条机器码不必每次重拆位域。
//!
//! 表是直接映射的。一行记下折回后的地址、原始机器码，以及译好的结构。
//! 命中要三者对上：地址、机器码、世代。写到能放代码的区域时只丢掉重叠的行；
//! 重新装入卡带时把世代加一，整表作废。

use crate::disasm::{self, Op as ArmOp};
use crate::thumb::{self, Op as ThumbOp};

/// 片内 32KB 的 ARM 指令正好一行一条。
const ARM_LINES: usize = 8192;
const THUMB_LINES: usize = 8192;

#[derive(Clone, Debug)]
struct ArmLine {
    pc: u32,
    word: u32,
    op: ArmOp,
    gen: u32,
}

#[derive(Clone, Debug)]
struct ThumbLine {
    pc: u32,
    half: u16,
    op: ThumbOp,
    gen: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct ICache {
    arm: Vec<Option<ArmLine>>,
    thumb: Vec<Option<ThumbLine>>,
    /// 换卡时加一。旧行的世代对不上，就是没命中。
    generation: u32,
    #[cfg(test)]
    hits: u64,
    #[cfg(test)]
    misses: u64,
}

impl ICache {
    pub(crate) fn new() -> Self {
        Self {
            arm: vec![None; ARM_LINES],
            thumb: vec![None; THUMB_LINES],
            generation: 1,
            #[cfg(test)]
            hits: 0,
            #[cfg(test)]
            misses: 0,
        }
    }

    pub(crate) fn arm(&mut self, pc: u32, word: u32) -> Option<ArmOp> {
        let Some(tag) = canon(pc) else {
            return disasm::decode(word);
        };
        let index = arm_index(tag);
        if let Some(line) = &self.arm[index] {
            if line.gen == self.generation && line.pc == tag && line.word == word {
                #[cfg(test)]
                {
                    self.hits += 1;
                }
                return Some(line.op);
            }
        }
        let op = disasm::decode(word)?;
        self.arm[index] = Some(ArmLine {
            pc: tag,
            word,
            op,
            gen: self.generation,
        });
        #[cfg(test)]
        {
            self.misses += 1;
        }
        Some(op)
    }

    pub(crate) fn thumb(&mut self, pc: u32, half: u16) -> ThumbOp {
        let Some(tag) = canon(pc) else {
            return thumb::decode(half);
        };
        let index = thumb_index(tag);
        if let Some(line) = &self.thumb[index] {
            if line.gen == self.generation && line.pc == tag && line.half == half {
                #[cfg(test)]
                {
                    self.hits += 1;
                }
                return line.op;
            }
        }
        let op = thumb::decode(half);
        self.thumb[index] = Some(ThumbLine {
            pc: tag,
            half,
            op,
            gen: self.generation,
        });
        #[cfg(test)]
        {
            self.misses += 1;
        }
        op
    }

    /// 换卡。以前译过的指令，不管在哪一块，都不再使用。
    pub(crate) fn clear(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.arm.fill(None);
            self.thumb.fill(None);
            self.generation = 1;
        }
    }

    /// 这次写入盖住的指令行作废。别的地址留着。
    pub(crate) fn invalidate(&mut self, addr: u32, bytes: u32) {
        if bytes == 0 {
            return;
        }
        let end = addr.saturating_add(bytes);
        let mut word = addr & !3;
        while word < end {
            if let Some(tag) = canon(word) {
                let index = arm_index(tag);
                if let Some(line) = &self.arm[index] {
                    if line.pc == tag {
                        self.arm[index] = None;
                    }
                }
            }
            let next = word.saturating_add(4);
            if next <= word {
                break;
            }
            word = next;
        }
        let mut half = addr & !1;
        while half < end {
            if let Some(tag) = canon(half) {
                let index = thumb_index(tag);
                if let Some(line) = &self.thumb[index] {
                    if line.pc == tag {
                        self.thumb[index] = None;
                    }
                }
            }
            let next = half.saturating_add(2);
            if next <= half {
                break;
            }
            half = next;
        }
    }

    #[cfg(test)]
    pub(crate) fn hits(&self) -> u64 {
        self.hits
    }

    #[cfg(test)]
    pub(crate) fn misses(&self) -> u64 {
        self.misses
    }
}

fn arm_index(tag: u32) -> usize {
    ((tag >> 2) as usize) & (ARM_LINES - 1)
}

fn thumb_index(tag: u32) -> usize {
    ((tag >> 1) as usize) & (THUMB_LINES - 1)
}

/// 镜像折回真正的那一份。查表用这个地址，执行仍用原来的 PC。
fn canon(addr: u32) -> Option<u32> {
    match addr >> 24 {
        0x00 if addr < 0x4000 => Some(addr),
        0x02 => Some(0x0200_0000 | (addr & 0x3_FFFF)),
        0x03 => Some(0x0300_0000 | (addr & 0x7FFF)),
        0x06 => Some(0x0600_0000 | vram_off(addr)),
        0x08..=0x0D => Some(0x0800_0000 | (addr & 0x01FF_FFFF)),
        _ => None,
    }
}

/// 显存 128KB 窗口里，高 32KB 折回 `0x06010000`。和总线同一条规则。
fn vram_off(addr: u32) -> u32 {
    let off = addr & 0x1_FFFF;
    if off >= 0x1_8000 {
        off - 0x8000
    } else {
        off
    }
}

#[cfg(test)]
mod tests {
    use crate::asm::{self, branch_address};
    use crate::bus::Bus;
    use crate::cpu::step;
    use crate::registers::Registers;

    const AL: u8 = 0xE;

    fn mov_imm(rd: u8, value: u32) -> u32 {
        asm::dp_imm(AL, 13, false, 0, rd, value).unwrap()
    }

    #[test]
    fn the_same_arm_word_is_decoded_once() {
        let mut bus = Bus::new();
        let word = mov_imm(0, 1);
        bus.write_u32(0x0300_0000, word);
        let mut regs = Registers::arm_start(0x0300_0000);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.get(0), 1);
        assert_eq!(bus.decode_misses(), 1);
        regs.set_pc(0x0300_0000);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(bus.decode_hits(), 1);
        assert_eq!(bus.decode_misses(), 1);
    }

    #[test]
    fn writing_the_same_bits_still_drops_the_line() {
        let mut bus = Bus::new();
        let word = mov_imm(0, 1);
        bus.write_u32(0x0300_0000, word);
        let mut regs = Registers::arm_start(0x0300_0000);
        step(&mut regs, &mut bus).unwrap();
        bus.write_u32(0x0300_0000, word);
        regs.set_pc(0x0300_0000);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(bus.decode_hits(), 0);
        assert_eq!(bus.decode_misses(), 2);
        assert_eq!(regs.get(0), 1);
    }

    #[test]
    fn a_store_elsewhere_keeps_the_decoded_word() {
        let mut bus = Bus::new();
        let word = mov_imm(0, 1);
        bus.write_u32(0x0300_0000, word);
        let mut regs = Registers::arm_start(0x0300_0000);
        step(&mut regs, &mut bus).unwrap();
        bus.write_u32(0x0200_0100, 0x1111_1111);
        regs.set_pc(0x0300_0000);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(bus.decode_hits(), 1);
        assert_eq!(bus.decode_misses(), 1);
    }

    #[test]
    fn the_high_iwram_mirror_invalidates_the_low_copy() {
        let mut bus = Bus::new();
        let first = mov_imm(0, 1);
        let second = mov_imm(0, 2);
        bus.write_u32(0x0300_0000, first);
        let mut regs = Registers::arm_start(0x0300_0000);
        step(&mut regs, &mut bus).unwrap();
        bus.write_u32(0x0300_8000, second);
        regs.set_pc(0x0300_0000);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.get(0), 2);
        assert_eq!(bus.decode_misses(), 2);
        assert_eq!(bus.decode_hits(), 0);
    }

    #[test]
    fn reloading_the_cart_forgets_decoded_rom() {
        let mut bus = Bus::new();
        bus.load_rom(&mov_imm(0, 1).to_le_bytes());
        let mut regs = Registers::arm_start(0x0800_0000);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.get(0), 1);
        regs.set_pc(0x0800_0000);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(bus.decode_hits(), 1);
        bus.load_rom(&mov_imm(0, 7).to_le_bytes());
        regs.set_pc(0x0800_0000);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.get(0), 7);
        assert_eq!(bus.decode_misses(), 2);
        assert_eq!(bus.decode_hits(), 1);
    }

    #[test]
    fn a_thumb_half_is_decoded_once_until_it_is_overwritten() {
        let mut bus = Bus::new();
        bus.write_u16(0x0300_0000, 0x2001);
        let mut regs = Registers::arm_start(0x0300_0000);
        regs.set_thumb(true);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.get(0), 1);
        assert_eq!(bus.decode_misses(), 1);
        regs.set_pc(0x0300_0000);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(bus.decode_hits(), 1);
        bus.write_u16(0x0300_0000, 0x2002);
        regs.set_pc(0x0300_0000);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.get(0), 2);
        assert_eq!(bus.decode_misses(), 2);
    }

    #[test]
    fn cached_branches_still_use_the_pc_they_were_fetched_from() {
        let mut bus = Bus::new();
        let skip = branch_address(AL, false, 0x0300_0000, 0x0300_0008);
        let again = branch_address(AL, false, 0x0300_0010, 0x0300_0018);
        bus.write_u32(0x0300_0000, skip);
        bus.write_u32(0x0300_0004, mov_imm(0, 9));
        bus.write_u32(0x0300_0008, mov_imm(1, 4));
        bus.write_u32(0x0300_0010, again);
        bus.write_u32(0x0300_0014, mov_imm(2, 9));
        bus.write_u32(0x0300_0018, mov_imm(3, 4));
        let mut regs = Registers::arm_start(0x0300_0000);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.pc(), 0x0300_0008);
        regs.set_pc(0x0300_0010);
        step(&mut regs, &mut bus).unwrap();
        assert_eq!(regs.pc(), 0x0300_0018);
        assert_eq!(regs.get(0), 0);
        assert_eq!(regs.get(2), 0);
    }
}
