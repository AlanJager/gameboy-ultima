//! 处理 ARM 汇编指令的编码
//! 核心思路，处理输入的汇编，然后翻译成机器码

use crate::condition::Condition;

// 定义 数据处理指令的操作码
// 标记枚举明确使用 u8 作为底层类型
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opcode {
    AND = 0,
    EOR = 1,
    SUB = 2,
    RSB = 3,
    ADD = 4,
    ADC = 5,
    SBC = 6,
    RSC = 7,
    TST = 8,
    TEQ = 9,
    CMP = 10,
    CMN = 11,
    ORR = 12,
    MOV = 13,
    BIC = 14,
    MVN = 15,
}

impl Opcode {
    pub const fn from_bits(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::AND),
            1 => Some(Self::EOR),
            2 => Some(Self::SUB),
            3 => Some(Self::RSB),
            4 => Some(Self::ADD),
            5 => Some(Self::ADC),
            6 => Some(Self::SBC),
            7 => Some(Self::RSC),
            8 => Some(Self::TST),
            9 => Some(Self::TEQ),
            10 => Some(Self::CMP),
            11 => Some(Self::CMN),
            12 => Some(Self::ORR),
            13 => Some(Self::MOV),
            14 => Some(Self::BIC),
            15 => Some(Self::MVN),
            _ => None,
        }
    }
}

pub enum ShiftType {
    LSL = 0,
    LSR = 1,
    ASR = 2,
    ROR = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shift {
    Lsl = 0,
    Lsr = 1,
    Asr = 2,
    Ror = 3,
}

// 各指令格式的固定编码位。某些格式共享相同的前缀，完整区分还需要
// 检查其他位；这里的枚举用于给编码器提供固定的位模式，不用于解码分类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstructionClass {
    DataProcessingRegister,
    DataProcessingImmediate,
    Multiply,
    SingleDataTransfer,
    HalfwordDataTransfer,
    BlockDataTransfer,
    Branch,
    CoprocessorDataTransfer,
    CoprocessorDataProcessing,
    CoprocessorRegisterTransfer,
    SoftwareInterrupt,
}

impl InstructionClass {
    pub const fn bits(self) -> u32 {
        match self {
            Self::DataProcessingRegister => 0,
            Self::DataProcessingImmediate => 1 << 25,
            Self::Multiply => 0b1001 << 4,
            Self::SingleDataTransfer => 0b01 << 26,
            Self::HalfwordDataTransfer => (1 << 7) | (1 << 4),
            Self::BlockDataTransfer => 0b100 << 25,
            Self::Branch => 0b101 << 25,
            Self::CoprocessorDataTransfer => 0b110 << 25,
            Self::CoprocessorDataProcessing => 0b1110 << 24,
            Self::CoprocessorRegisterTransfer => (0b1110 << 24) | (1 << 4),
            Self::SoftwareInterrupt => 0b1111 << 24,
        }
    }
}

//
// 这里实现的是数据处理指令格式：
// - bits 27..26 固定为 00
// - bit 25 是 I：0 表示寄存器操作数，1 表示立即数
// - bits 24..21 是操作码
// - bit 20 是 S：1 表示更新条件码，0 表示不更新
// - bits 19..16 是 Rn：第一个操作数寄存器
// - bits 15..12 是 Rd：目标寄存器
// - bits 11..8 旋转量（rotate_imm）
// - bits 7..0 位立即数（imm8）
// 31..28 27..26 25 24..21 20 19..16 15..12 11..0
//  cond    00    I   opcode  S   Rn     Rd   Operand2
//
// 编码器把各字段放入 32 位机器码；Operand2 的具体格式由 I 位决定。

pub fn encode_imm(value: u32) -> Option<u32> {
    for rotate in 0..16 {
        // note: CPU 会右旋转 2 * rotate 位，所以我们这里左旋转 2 * rotate
        // 通过左旋转可以得到 一个 8 位立即数的表示形式
        let imm8 = value.rotate_left(rotate * 2);
        if imm8 <= 0xFF {
            return Some((rotate << 8) | imm8);
        }
    }
    None
}

pub fn data_processing_immediate(
    cond: Condition,
    opcode: Opcode,
    s: bool,
    rn: u8,
    rd: u8,
    value: u32,
) -> Result<u32, String> {
    if rn > 15 || rd > 15 {
        return Err("寄存器编号必须在 0..=15 之间".into());
    }

    let oprand2 = encode_imm(value)
        .ok_or_else(|| format!("立即数 {:#X} 无法编码为 8 位立即数 + 旋转量", value))?;

    let mut instruction: u32 = 0;
    instruction |= (cond as u32) << 28;
    instruction |= InstructionClass::DataProcessingImmediate.bits();
    instruction |= (opcode as u32) << 21;
    if s {
        instruction |= 1 << 20;
    }
    instruction |= (rn as u32) << 16;
    instruction |= (rd as u32) << 12;
    instruction |= oprand2;

    Ok(instruction)
}

pub fn data_processing_register(
    cond: Condition,
    opcode: Opcode,
    s: bool,
    rn: u8,
    rd: u8,
    rm: u8,
    shift_type: ShiftType,
    shift_amount: u8,
) -> u32 {
    let mut instruction: u32 = 0;
    instruction |= (cond as u32) << 28;
    instruction |= InstructionClass::DataProcessingRegister.bits();
    instruction |= (opcode as u32) << 21;
    if s {
        instruction |= 1 << 20;
    }
    instruction |= (rn as u32) << 16;
    instruction |= (rd as u32) << 12;
    // Operand2 的格式是：shift_imm(5 bits) + shift(2 bits) + 0 + Rm(4 bits)
    instruction |= (shift_amount as u32 & 0x1F) << 7;
    instruction |= (shift_type as u32 & 0x3) << 5;
    instruction |= rm as u32 & 0xF;
    instruction
}

pub fn data_processing_register_immediate_shift(
    cond: Condition,
    opcode: Opcode,
    s: bool,
    rn: u8,
    rd: u8,
    rm: u8,
    shift_type: ShiftType,
    shift_amount: u8,
) -> Result<u32, String> {
    if rn > 15 || rd > 15 || rm > 15 {
        return Err("寄存器编号必须在 0..=15 之间".into());
    }
    if shift_amount > 31 {
        return Err("移位量必须在 0..=31 之间".into());
    }

    Ok((cond as u32) << 28
        | InstructionClass::DataProcessingRegister.bits()
        | (opcode as u32) << 21
        | u32::from(s) << 20
        | u32::from(rn) << 16
        | u32::from(rd) << 12
        | u32::from(shift_amount) << 7
        | (shift_type as u32) << 5
        | u32::from(rm))
}

pub fn data_processing_register_register_shift(
    cond: Condition,
    opcode: Opcode,
    s: bool,
    rn: u8,
    rd: u8,
    rm: u8,
    shift_type: ShiftType,
    rs: u8,
) -> Result<u32, String> {
    if rn > 15 || rd > 15 || rm > 15 || rs > 15 {
        return Err("寄存器编号必须在 0..=15 之间".into());
    }

    Ok((cond as u32) << 28
        | InstructionClass::DataProcessingRegister.bits()
        | (opcode as u32) << 21
        | u32::from(s) << 20
        | u32::from(rn) << 16
        | u32::from(rd) << 12
        | u32::from(rs) << 8
        | (shift_type as u32) << 5
        | 1 << 4
        | u32::from(rm))
}

pub fn multiply(
    cond: Condition,
    a: bool,
    s: bool,
    rd: u8,
    rn: u8,
    rs: u8,
    rm: u8,
) -> Result<u32, String> {
    if rd > 15 || rn > 15 || rs > 15 || rm > 15 {
        return Err("寄存器编号必须在 0..=15 之间".into());
    }

    let mut instruction: u32 = 0;
    instruction |= (cond as u32) << 28;
    instruction |= InstructionClass::Multiply.bits();
    if a {
        instruction |= 1 << 21;
    }
    if s {
        instruction |= 1 << 20;
    }
    instruction |= (rd as u32) << 16;
    instruction |= (rn as u32) << 12;
    instruction |= (rs as u32) << 8;
    instruction |= rm as u32 & 0xF;
    Ok(instruction)
}

// 示例格式：
// 31..28 27..26 25 24..21 20 19..16 15..12 11..0
// cond    01    I   P U B W L   Rn     Rd   offset
pub fn memory_immediate(
    cond: Condition,
    load: bool,
    byte: bool,
    pre: bool,
    add: bool,
    writeback: bool,
    rn: u8,
    rd: u8,
    offset: u32,
) -> Result<u32, String> {
    if rn > 15 || rd > 15 {
        return Err("寄存器编号必须在 0..=15 之间".into());
    }
    if offset > 0xFFF {
        return Err("立即数偏移超出 12 位范围".into());
    }

    let mut instruction = (cond as u32) << 28;
    instruction |= InstructionClass::SingleDataTransfer.bits();
    instruction |= u32::from(pre) << 24;
    instruction |= u32::from(add) << 23; // up so match U bit
    instruction |= u32::from(byte) << 22;
    instruction |= u32::from(writeback) << 21;
    instruction |= u32::from(load) << 20;
    instruction |= u32::from(rn) << 16;
    instruction |= u32::from(rd) << 12;
    instruction |= offset;

    Ok(instruction)
}

pub enum HalfwordKind {
    Halfword,       // LDRH / STRH
    SignedByte,     // LDRSB / STRSB
    SignedHalfword, // LDRSH / STRSH
}

pub enum HalfwordOffset {
    Immediate(u8),
    Register(u8),
}

// 处理半字数据传输指令的编码
// 编码格式：
// 31..28 27..25 24 23 22 21 20 19..16 15..12 11..8 7 6 5 4 3..0
// cond    000   P  U  I  W  L   Rn     Rd   immH  1 S H 1 immL/Rm
pub fn halfword_transfer(
    cond: Condition,
    kind: HalfwordKind,
    load: bool,
    pre: bool,
    add: bool,
    writeback: bool,
    rn: u8,
    rd: u8,
    offset: HalfwordOffset,
) -> Result<u32, String> {
    // 拒绝所有符号类型用于存储
    if matches!(
        kind,
        HalfwordKind::SignedByte | HalfwordKind::SignedHalfword
    ) && !load
    {
        return Err("不允许使用符号类型进行存储".into());
    }

    // 检查rn rd 和 寄存器的偏移范围
    if rn > 15 || rd > 15 {
        return Err("寄存器编号必须在 0..=15 之间".into());
    }

    let mut instruction: u32 = 0;
    instruction |= (cond as u32) << 28;
    instruction |= InstructionClass::HalfwordDataTransfer.bits();
    if load {
        instruction |= 1 << 20;
    }
    if pre {
        instruction |= 1 << 24;
    }
    if add {
        instruction |= 1 << 23;
    }
    if writeback {
        instruction |= 1 << 21;
    }
    match kind {
        HalfwordKind::Halfword => instruction |= 0b01 << 5, // LDRH / STRH
        HalfwordKind::SignedByte => instruction |= 0b10 << 5, // LDRSB / STRSB
        HalfwordKind::SignedHalfword => instruction |= 0b11 << 5, // LDRSH / STRSH
    }
    instruction |= (rn as u32) << 16;
    instruction |= (rd as u32) << 12;
    match offset {
        HalfwordOffset::Immediate(value) => {
            instruction |= 1 << 22;
            instruction |= (u32::from(value & 0xF0) << 4) | u32::from(value & 0x0F);
        }
        HalfwordOffset::Register(rm) => {
            instruction |= u32::from(rm & 0x0F);
        }
    }
    Ok(instruction)
}

// B/BL 的 imm24 是有符号 24 位字偏移：CPU 符号扩展后左移 2 位，
// 加到 PC（当前指令地址 + 8）得到分支目标。
// 因此 byte offset 必须 4 字节对齐，范围为
// -0x0200_0000..=0x01FF_FFFC。
//
// 参考：ARM7TDMI Technical Reference Manual (DDI 0029E),
// “ARM Instruction Set” -> “Branch and Branch with Link”.
pub fn branch_offset(cond: Condition, link: bool, offset: i32) -> Result<u32, String> {
    if offset % 4 != 0 {
        return Err("Branch offset must be 4-byte aligned".into());
    }

    if offset < -0x0200_0000 || offset > 0x01FF_FFFC {
        return Err(format!("Branch offset out of range: {:#X}", offset));
    }

    let mut instruction: u32 = 0;
    instruction |= (cond as u32) << 28;
    instruction |= InstructionClass::Branch.bits();
    if link {
        instruction |= 1 << 24; // bit 24 = 1 表示 BL
    }
    // offset 偏移相对于 PC + 8，因此需要右移 2 位以获得字偏移
    let offset = (offset >> 2) & 0x00FFFFFF; // 24 位偏移量
    instruction |= offset as u32;
    Ok(instruction)
}

pub fn branch(cond: Condition, link: bool, offset: i32) -> Result<u32, String> {
    branch_offset(cond, link, offset)
}

pub fn dp_imm(
    cond: u8,
    opcode: u8,
    set_flags: bool,
    rn: u8,
    rd: u8,
    value: u32,
) -> Result<u32, String> {
    if rn > 15 || rd > 15 || opcode > 15 || cond > 15 {
        return Err("invalid ARM data-processing field".into());
    }
    let operand2 = encode_imm(value)
        .ok_or_else(|| format!("immediate {value:#X} cannot be encoded in ARM format"))?;
    Ok((u32::from(cond) << 28)
        | InstructionClass::DataProcessingImmediate.bits()
        | (u32::from(opcode) << 21)
        | (u32::from(set_flags) << 20)
        | (u32::from(rn) << 16)
        | (u32::from(rd) << 12)
        | operand2)
}

pub fn dp_reg(
    cond: u8,
    opcode: u8,
    set_flags: bool,
    rn: u8,
    rd: u8,
    rm: u8,
    shift: Shift,
    amount: u8,
) -> u32 {
    (u32::from(cond & 0xF) << 28)
        | InstructionClass::DataProcessingRegister.bits()
        | (u32::from(opcode & 0xF) << 21)
        | (u32::from(set_flags) << 20)
        | (u32::from(rn & 0xF) << 16)
        | (u32::from(rd & 0xF) << 12)
        | (u32::from(amount & 0x1F) << 7)
        | ((shift as u32) << 5)
        | u32::from(rm & 0xF)
}

pub fn mem_imm(
    cond: u8,
    load: bool,
    byte: bool,
    pre: bool,
    add: bool,
    writeback: bool,
    rn: u8,
    rd: u8,
    offset: u16,
) -> u32 {
    (u32::from(cond & 0xF) << 28)
        | InstructionClass::SingleDataTransfer.bits()
        | (u32::from(pre) << 24)
        | (u32::from(add) << 23)
        | (u32::from(byte) << 22)
        | (u32::from(writeback) << 21)
        | (u32::from(load) << 20)
        | (u32::from(rn & 0xF) << 16)
        | (u32::from(rd & 0xF) << 12)
        | (u32::from(offset) & 0xFFF)
}

pub fn half_imm(
    cond: u8,
    load: bool,
    pre: bool,
    add: bool,
    writeback: bool,
    rn: u8,
    rd: u8,
    offset: u8,
) -> u32 {
    (u32::from(cond & 0xF) << 28)
        | InstructionClass::HalfwordDataTransfer.bits()
        | (u32::from(pre) << 24)
        | (u32::from(add) << 23)
        | (1 << 22)
        | (u32::from(writeback) << 21)
        | (u32::from(load) << 20)
        | (u32::from(rn & 0xF) << 16)
        | (u32::from(rd & 0xF) << 12)
        | (u32::from(offset & 0xF0) << 4)
        | (1 << 5)
        | (1 << 4)
        | u32::from(offset & 0x0F)
}

pub fn branch_address(cond: u8, link: bool, from: u32, target: u32) -> u32 {
    let offset = target.wrapping_sub(from.wrapping_add(8));
    (u32::from(cond & 0xF) << 28)
        | InstructionClass::Branch.bits()
        | (u32::from(link) << 24)
        | ((offset >> 2) & 0x00FF_FFFF)
}

pub fn software_interrupt(cond: Condition, swi_number: u32) -> Result<u32, String> {
    let mut instruction: u32 = 0;
    instruction |= (cond as u32) << 28;
    instruction |= InstructionClass::SoftwareInterrupt.bits();
    instruction |= swi_number & 0x00FFFFFF;
    Ok(instruction)
}
