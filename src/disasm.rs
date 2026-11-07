//! ARM 状态的反汇编。
//!
//! 一条指令是 4 个字节。这里把机器码译成文本；真正改寄存器的是 `step`。
//! 未定义编码印成 `udf`。`swi` 会译出来。执行时，`swi` 进管理向量，未定义编码进 `0x04`。

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Disassembly {
    pub address: u32,
    pub word: u32,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    Data {
        cond: u8,
        opcode: u8,
        set_flags: bool,
        rn: u8,
        rd: u8,
        shifter: Shifter,
    },
    Mem {
        cond: u8,
        load: bool,
        byte: bool,
        pre: bool,
        up: bool,
        writeback: bool,
        rn: u8,
        rd: u8,
        offset: MemOffset,
    },
    Half {
        cond: u8,
        load: bool,
        signed: bool,
        byte: bool,
        pre: bool,
        up: bool,
        writeback: bool,
        rn: u8,
        rd: u8,
        offset: HalfOffset,
    },
    Block {
        cond: u8,
        pre: bool,
        up: bool,
        user: bool,
        writeback: bool,
        load: bool,
        rn: u8,
        list: u16,
    },
    Branch {
        cond: u8,
        link: bool,
        imm24: u32,
    },
    Bx {
        cond: u8,
        rm: u8,
    },
    Mul {
        cond: u8,
        accumulate: bool,
        set_flags: bool,
        rd: u8,
        rn: u8,
        rs: u8,
        rm: u8,
    },
    MulLong {
        cond: u8,
        signed: bool,
        accumulate: bool,
        set_flags: bool,
        rd_hi: u8,
        rd_lo: u8,
        rs: u8,
        rm: u8,
    },
    Swap {
        cond: u8,
        byte: bool,
        rn: u8,
        rd: u8,
        rm: u8,
    },
    Mrs {
        cond: u8,
        spsr: bool,
        rd: u8,
    },
    Msr {
        cond: u8,
        spsr: bool,
        mask: u8,
        src: MsrSrc,
    },
    /// `swi` 进入管理向量。未定义编码进入未定义向量 `0x04`。
    Exception {
        cond: u8,
        swi: Option<u32>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MemOffset {
    Imm(u16),
    Reg { rm: u8, kind: u8, amount: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HalfOffset {
    Imm(u8),
    Reg(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MsrSrc {
    Reg(u8),
    Imm { imm8: u8, rot: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shifter {
    Imm {
        imm8: u8,
        rot: u8,
    },
    Reg {
        rm: u8,
        kind: u8,
        amount: u8,
    },
    /// 移位量的低 8 位在 `rs` 里。
    RegShift {
        rm: u8,
        kind: u8,
        rs: u8,
    },
}

impl Op {
    pub(crate) fn cond(&self) -> u8 {
        match *self {
            Op::Data { cond, .. }
            | Op::Mem { cond, .. }
            | Op::Half { cond, .. }
            | Op::Block { cond, .. }
            | Op::Branch { cond, .. }
            | Op::Bx { cond, .. }
            | Op::Mul { cond, .. }
            | Op::MulLong { cond, .. }
            | Op::Swap { cond, .. }
            | Op::Mrs { cond, .. }
            | Op::Msr { cond, .. }
            | Op::Exception { cond, .. } => cond,
        }
    }
}

pub fn disassemble(address: u32, word: u32) -> Disassembly {
    let text = match decode(word) {
        Some(op) => format_op(address, &op),
        None => format!(".word 0x{word:08X}"),
    };
    Disassembly {
        address,
        word,
        text,
    }
}

pub(crate) fn decode(word: u32) -> Option<Op> {
    let class = (word >> 25) & 7;
    if class == 0b101 {
        return Some(Op::Branch {
            cond: cond(word),
            link: word & (1 << 24) != 0,
            imm24: word & 0x00FF_FFFF,
        });
    }
    if class == 0b111 {
        let swi = if word & (1 << 24) != 0 {
            Some(word & 0x00FF_FFFF)
        } else {
            None
        };
        return Some(Op::Exception {
            cond: cond(word),
            swi,
        });
    }
    if class == 0b110 {
        return Some(Op::Exception {
            cond: cond(word),
            swi: None,
        });
    }
    if class == 0b100 {
        return Some(Op::Block {
            cond: cond(word),
            pre: word & (1 << 24) != 0,
            up: word & (1 << 23) != 0,
            user: word & (1 << 22) != 0,
            writeback: word & (1 << 21) != 0,
            load: word & (1 << 20) != 0,
            rn: reg(word, 16),
            list: word as u16,
        });
    }
    if (word >> 26) & 3 == 0b01 {
        if word & (1 << 25) != 0 && word & (1 << 4) != 0 {
            return Some(Op::Exception {
                cond: cond(word),
                swi: None,
            });
        }
        let offset = if word & (1 << 25) != 0 {
            MemOffset::Reg {
                rm: reg(word, 0),
                kind: ((word >> 5) & 3) as u8,
                amount: ((word >> 7) & 0x1F) as u8,
            }
        } else {
            MemOffset::Imm((word & 0x0FFF) as u16)
        };
        return Some(Op::Mem {
            cond: cond(word),
            load: word & (1 << 20) != 0,
            byte: word & (1 << 22) != 0,
            pre: word & (1 << 24) != 0,
            up: word & (1 << 23) != 0,
            writeback: word & (1 << 21) != 0,
            rn: reg(word, 16),
            rd: reg(word, 12),
            offset,
        });
    }
    if word & 0x0FFF_FFF0 == 0x012F_FF10 {
        return Some(Op::Bx {
            cond: cond(word),
            rm: reg(word, 0),
        });
    }
    if word & 0x0FBF_0FFF == 0x010F_0000 {
        return Some(Op::Mrs {
            cond: cond(word),
            spsr: word & (1 << 22) != 0,
            rd: reg(word, 12),
        });
    }
    if word & 0x0FB0_FFF0 == 0x0120_F000 {
        return Some(Op::Msr {
            cond: cond(word),
            spsr: word & (1 << 22) != 0,
            mask: ((word >> 16) & 0xF) as u8,
            src: MsrSrc::Reg(reg(word, 0)),
        });
    }
    if word & 0x0FB0_F000 == 0x0320_F000 {
        return Some(Op::Msr {
            cond: cond(word),
            spsr: word & (1 << 22) != 0,
            mask: ((word >> 16) & 0xF) as u8,
            src: MsrSrc::Imm {
                imm8: (word & 0xFF) as u8,
                rot: ((word >> 8) & 0xF) as u8,
            },
        });
    }
    if word & 0x0FB0_0FF0 == 0x0100_0090 {
        return Some(Op::Swap {
            cond: cond(word),
            byte: word & (1 << 22) != 0,
            rn: reg(word, 16),
            rd: reg(word, 12),
            rm: reg(word, 0),
        });
    }
    if word & 0x0F80_00F0 == 0x0080_0090 {
        return Some(Op::MulLong {
            cond: cond(word),
            signed: word & (1 << 22) != 0,
            accumulate: word & (1 << 21) != 0,
            set_flags: word & (1 << 20) != 0,
            rd_hi: reg(word, 16),
            rd_lo: reg(word, 12),
            rs: reg(word, 8),
            rm: reg(word, 0),
        });
    }
    if (word >> 24) & 0xF == 0 && word & 0xF0 == 0x90 {
        return Some(Op::Mul {
            cond: cond(word),
            accumulate: word & (1 << 21) != 0,
            set_flags: word & (1 << 20) != 0,
            rd: reg(word, 16),
            rn: reg(word, 12),
            rs: reg(word, 8),
            rm: reg(word, 0),
        });
    }
    // 半字传输的高三位是 000。数据处理的立即数会把第 25 位置 1，不能算进来。
    if class == 0 && word & 0x90 == 0x90 {
        let shape = (word >> 5) & 3;
        if shape == 0 || (word & (1 << 20) == 0 && shape != 1) {
            return Some(Op::Exception {
                cond: cond(word),
                swi: None,
            });
        }
        let offset = if word & (1 << 22) != 0 {
            let hi = (word >> 8) & 0xF;
            let lo = word & 0xF;
            HalfOffset::Imm(((hi << 4) | lo) as u8)
        } else if (word >> 8) & 0xF != 0 {
            return Some(Op::Exception {
                cond: cond(word),
                swi: None,
            });
        } else {
            HalfOffset::Reg(reg(word, 0))
        };
        return Some(Op::Half {
            cond: cond(word),
            load: word & (1 << 20) != 0,
            signed: shape & 2 != 0,
            byte: shape == 2,
            pre: word & (1 << 24) != 0,
            up: word & (1 << 23) != 0,
            writeback: word & (1 << 21) != 0,
            rn: reg(word, 16),
            rd: reg(word, 12),
            offset,
        });
    }
    if class & 0b110 == 0 {
        if word & (1 << 25) == 0 && word & (1 << 4) != 0 && word & (1 << 7) != 0 {
            return Some(Op::Exception {
                cond: cond(word),
                swi: None,
            });
        }
        let opcode = ((word >> 21) & 0xF) as u8;
        let set_flags = word & (1 << 20) != 0;
        if (8..12).contains(&opcode) && !set_flags {
            return Some(Op::Exception {
                cond: cond(word),
                swi: None,
            });
        }
        let shifter = if word & (1 << 25) != 0 {
            Shifter::Imm {
                imm8: (word & 0xFF) as u8,
                rot: ((word >> 8) & 0xF) as u8,
            }
        } else if word & (1 << 4) != 0 {
            Shifter::RegShift {
                rm: reg(word, 0),
                kind: ((word >> 5) & 3) as u8,
                rs: reg(word, 8),
            }
        } else {
            Shifter::Reg {
                rm: reg(word, 0),
                kind: ((word >> 5) & 3) as u8,
                amount: ((word >> 7) & 0x1F) as u8,
            }
        };
        return Some(Op::Data {
            cond: cond(word),
            opcode,
            set_flags,
            rn: reg(word, 16),
            rd: reg(word, 12),
            shifter,
        });
    }
    Some(Op::Exception {
        cond: cond(word),
        swi: None,
    })
}

pub(crate) fn format_op(address: u32, op: &Op) -> String {
    match op {
        Op::Data {
            cond,
            opcode,
            set_flags,
            rn,
            rd,
            shifter,
        } => format_data(*cond, *opcode, *set_flags, *rn, *rd, shifter),
        Op::Mem {
            cond,
            load,
            byte,
            pre,
            up,
            writeback,
            rn,
            rd,
            offset,
        } => {
            let name = mem_name(*load, *byte);
            format!(
                "{}{} {}, {}",
                name,
                cond_suffix(*cond),
                reg_name(*rd),
                format_mem(*rn, offset, *pre, *up, *writeback)
            )
        }
        Op::Half {
            cond,
            load,
            signed,
            byte,
            pre,
            up,
            writeback,
            rn,
            rd,
            offset,
        } => {
            let name = match (*load, *signed, *byte) {
                (false, _, _) => "strh",
                (true, false, _) => "ldrh",
                (true, true, true) => "ldrsb",
                (true, true, false) => "ldrsh",
            };
            let shown = match *offset {
                HalfOffset::Imm(value) => MemOffset::Imm(u16::from(value)),
                HalfOffset::Reg(rm) => MemOffset::Reg {
                    rm,
                    kind: 0,
                    amount: 0,
                },
            };
            format!(
                "{}{} {}, {}",
                name,
                cond_suffix(*cond),
                reg_name(*rd),
                format_mem(*rn, &shown, *pre, *up, *writeback)
            )
        }
        Op::Block {
            cond,
            pre,
            up,
            user,
            writeback,
            load,
            rn,
            list,
        } => {
            let op = if *load { "ldm" } else { "stm" };
            let mode = match (*pre, *up) {
                (false, true) => "ia",
                (true, true) => "ib",
                (false, false) => "da",
                (true, false) => "db",
            };
            let bang = if *writeback { "!" } else { "" };
            let hat = if *user { "^" } else { "" };
            format!(
                "{op}{mode}{} {}{bang}, {{{}}}{hat}",
                cond_suffix(*cond),
                reg_name(*rn),
                format_list(*list)
            )
        }
        Op::Branch { cond, link, imm24 } => {
            let name = if *link { "bl" } else { "b" };
            let target = branch_target(address, *imm24);
            format!("{name}{} 0x{target:08X}", cond_suffix(*cond))
        }
        Op::Mul {
            cond,
            accumulate,
            set_flags,
            rd,
            rn,
            rs,
            rm,
        } => {
            let name = if *accumulate { "mla" } else { "mul" };
            let suffix = format!("{}{}", cond_suffix(*cond), flag_suffix(*set_flags));
            if *accumulate {
                format!(
                    "{name}{suffix} {}, {}, {}, {}",
                    reg_name(*rd),
                    reg_name(*rm),
                    reg_name(*rs),
                    reg_name(*rn)
                )
            } else {
                format!(
                    "{name}{suffix} {}, {}, {}",
                    reg_name(*rd),
                    reg_name(*rm),
                    reg_name(*rs)
                )
            }
        }
        Op::Bx { cond, rm } => format!("bx{} {}", cond_suffix(*cond), reg_name(*rm)),
        Op::MulLong {
            cond,
            signed,
            accumulate,
            set_flags,
            rd_hi,
            rd_lo,
            rs,
            rm,
        } => {
            let name = match (*signed, *accumulate) {
                (false, false) => "umull",
                (true, false) => "smull",
                (false, true) => "umlal",
                (true, true) => "smlal",
            };
            format!(
                "{name}{}{} {}, {}, {}, {}",
                cond_suffix(*cond),
                flag_suffix(*set_flags),
                reg_name(*rd_lo),
                reg_name(*rd_hi),
                reg_name(*rm),
                reg_name(*rs)
            )
        }
        Op::Swap {
            cond,
            byte,
            rn,
            rd,
            rm,
        } => {
            let name = if *byte { "swpb" } else { "swp" };
            format!(
                "{name}{} {}, {}, [{}]",
                cond_suffix(*cond),
                reg_name(*rd),
                reg_name(*rm),
                reg_name(*rn)
            )
        }
        Op::Mrs { cond, spsr, rd } => {
            let which = if *spsr { "spsr" } else { "cpsr" };
            format!("mrs{} {}, {which}", cond_suffix(*cond), reg_name(*rd))
        }
        Op::Msr {
            cond,
            spsr,
            mask,
            src,
        } => {
            let which = if *spsr { "spsr" } else { "cpsr" };
            let fields = field_text(*mask);
            let operand = match *src {
                MsrSrc::Reg(rm) => reg_name(rm).to_string(),
                MsrSrc::Imm { imm8, rot } => {
                    let value = u32::from(imm8).rotate_right(u32::from(rot) * 2);
                    format!("#{}", imm_text(value))
                }
            };
            format!("msr{} {which}{fields}, {operand}", cond_suffix(*cond))
        }
        Op::Exception { swi, cond } => match swi {
            Some(imm) => format!("swi{} #{}", cond_suffix(*cond), imm_text(*imm)),
            None => "udf".to_string(),
        },
    }
}

pub(crate) fn branch_target(address: u32, imm24: u32) -> u32 {
    let offset = ((imm24 << 8) as i32) >> 6;
    address.wrapping_add(8).wrapping_add(offset as u32)
}

fn format_data(cond: u8, opcode: u8, set_flags: bool, rn: u8, rd: u8, shifter: &Shifter) -> String {
    let name = MNEMONIC[opcode as usize];
    let compare = (8..12).contains(&opcode);
    let suffix = format!(
        "{}{}",
        cond_suffix(cond),
        if set_flags && !compare { "s" } else { "" }
    );
    let operand = format_shifter(shifter);
    if opcode == 13 || opcode == 15 {
        format!("{name}{suffix} {}, {operand}", reg_name(rd))
    } else if compare {
        format!("{name}{suffix} {}, {operand}", reg_name(rn))
    } else {
        format!(
            "{name}{suffix} {}, {}, {operand}",
            reg_name(rd),
            reg_name(rn)
        )
    }
}

fn format_shifter(shifter: &Shifter) -> String {
    match *shifter {
        Shifter::Imm { imm8, rot } => {
            let value = u32::from(imm8).rotate_right(u32::from(rot) * 2);
            format!("#{}", imm_text(value))
        }
        Shifter::Reg { rm, kind, amount } => format_shift(rm, kind, ShiftBy::Imm(amount)),
        Shifter::RegShift { rm, kind, rs } => format_shift(rm, kind, ShiftBy::Reg(rs)),
    }
}

enum ShiftBy {
    Imm(u8),
    Reg(u8),
}

fn format_shift(rm: u8, kind: u8, by: ShiftBy) -> String {
    let reg = reg_name(rm);
    match (kind, by) {
        (0, ShiftBy::Imm(0)) => reg.to_string(),
        (3, ShiftBy::Imm(0)) => format!("{reg}, rrx"),
        (_, ShiftBy::Imm(amount)) => {
            let op = ["lsl", "lsr", "asr", "ror"][kind as usize];
            let shown = if amount == 0 { 32 } else { u32::from(amount) };
            format!("{reg}, {op} #{}", imm_text(shown))
        }
        (_, ShiftBy::Reg(rs)) => {
            let op = ["lsl", "lsr", "asr", "ror"][kind as usize];
            format!("{reg}, {op} {}", reg_name(rs))
        }
    }
}

fn format_mem(rn: u8, offset: &MemOffset, pre: bool, up: bool, writeback: bool) -> String {
    let base = reg_name(rn);
    let (text, zero) = match *offset {
        MemOffset::Imm(value) => {
            let signed = if up {
                i32::from(value)
            } else {
                -i32::from(value)
            };
            (format!("#{}", imm_text_signed(signed)), value == 0)
        }
        MemOffset::Reg { rm, kind, amount } => {
            (format_shift(rm, kind, ShiftBy::Imm(amount)), false)
        }
    };
    if pre && zero && !writeback {
        format!("[{base}]")
    } else if pre && !writeback {
        format!("[{base}, {text}]")
    } else if pre {
        format!("[{base}, {text}]!")
    } else {
        format!("[{base}], {text}")
    }
}

fn format_list(list: u16) -> String {
    let mut names = Vec::new();
    for index in 0..16 {
        if list & (1 << index) != 0 {
            names.push(reg_name(index));
        }
    }
    names.join(", ")
}

fn field_text(mask: u8) -> String {
    let mut letters = String::new();
    for (bit, letter) in [(1, 'c'), (2, 'x'), (4, 's'), (8, 'f')] {
        if mask & bit != 0 {
            letters.push(letter);
        }
    }
    if letters.is_empty() {
        String::new()
    } else {
        format!("_{letters}")
    }
}

fn mem_name(load: bool, byte: bool) -> &'static str {
    match (load, byte) {
        (true, false) => "ldr",
        (true, true) => "ldrb",
        (false, false) => "str",
        (false, true) => "strb",
    }
}

fn cond(word: u32) -> u8 {
    (word >> 28) as u8
}

fn reg(word: u32, shift: u32) -> u8 {
    ((word >> shift) & 0xF) as u8
}

fn cond_suffix(cond: u8) -> &'static str {
    [
        "eq", "ne", "hs", "lo", "mi", "pl", "vs", "vc", "hi", "ls", "ge", "lt", "gt", "le", "", "",
    ][cond as usize]
}

fn flag_suffix(set_flags: bool) -> &'static str {
    if set_flags {
        "s"
    } else {
        ""
    }
}

pub(crate) fn reg_name(index: u8) -> &'static str {
    match index {
        13 => "sp",
        14 => "lr",
        15 => "pc",
        0 => "r0",
        1 => "r1",
        2 => "r2",
        3 => "r3",
        4 => "r4",
        5 => "r5",
        6 => "r6",
        7 => "r7",
        8 => "r8",
        9 => "r9",
        10 => "r10",
        11 => "r11",
        _ => "r12",
    }
}

fn imm_text(value: u32) -> String {
    if value <= 9 {
        format!("{value}")
    } else {
        format!("0x{value:X}")
    }
}

fn imm_text_signed(value: i32) -> String {
    if value < 0 {
        format!("-{}", imm_text((-value) as u32))
    } else {
        imm_text(value as u32)
    }
}

const MNEMONIC: [&str; 16] = [
    "and", "eor", "sub", "rsb", "add", "adc", "sbc", "rsc", "tst", "teq", "cmp", "cmn", "orr",
    "mov", "bic", "mvn",
];
