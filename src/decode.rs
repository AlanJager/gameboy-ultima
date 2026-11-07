//! Decode ARM data-processing instructions.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataProcessing {
    pub condition: u8,
    pub opcode: u8,
    pub set_flags: bool,
    pub rn: u8,
    pub rd: u8,
    pub operand2: Operand2,
    pub operand2_carry: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Branch {
    pub condition: Option<u8>,
    pub link: bool,
    pub offset: i32,
    pub pc_bias: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BranchExchange {
    pub condition: u8,
    pub rm: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Multiply {
    pub condition: u8,
    pub accumulate: bool,
    pub set_flags: bool,
    pub rd: u8,
    pub rn: u8,
    pub rs: u8,
    pub rm: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LongMultiply {
    pub condition: u8,
    pub signed: bool,
    pub accumulate: bool,
    pub set_flags: bool,
    pub rd_hi: u8,
    pub rd_lo: u8,
    pub rs: u8,
    pub rm: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HalfwordTransferKind {
    Halfword,
    SignedByte,
    SignedHalfword,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HalfwordOffset {
    Immediate(u8),
    Register(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HalfwordTransfer {
    pub condition: u8,
    pub kind: HalfwordTransferKind,
    pub load: bool,
    pub pre_index: bool,
    pub add_offset: bool,
    pub writeback: bool,
    pub rn: u8,
    pub rd: u8,
    pub offset: HalfwordOffset,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockTransfer {
    pub condition: u8,
    pub pre_index: bool,
    pub increment: bool,
    pub user_registers: bool,
    pub writeback: bool,
    pub load: bool,
    pub rn: u8,
    pub register_list: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressOffset {
    Immediate(u16),
    Register { rm: u8, shift_type: u8, amount: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SingleTransfer {
    pub condition: u8,
    pub pre_index: bool,
    pub add_offset: bool,
    pub byte: bool,
    pub writeback: bool,
    pub load: bool,
    pub rn: u8,
    pub rd: u8,
    pub offset: AddressOffset,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusRegisterSource {
    Register(u8),
    Immediate(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusRegisterTransfer {
    MoveFrom {
        condition: u8,
        saved: bool,
        rd: u8,
    },
    MoveTo {
        condition: u8,
        saved: bool,
        field_mask: u8,
        source: StatusRegisterSource,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Instruction {
    DataProcessing(DataProcessing),
    Branch(Branch),
    BranchExchange(BranchExchange),
    Multiply(Multiply),
    LongMultiply(LongMultiply),
    HalfwordTransfer(HalfwordTransfer),
    BlockTransfer(BlockTransfer),
    SingleTransfer(SingleTransfer),
    StatusRegisterTransfer(StatusRegisterTransfer),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operand2 {
    Immediate(u32),
    RegisterImmediateShift { rm: u8, shift_type: u8, amount: u8 },
    RegisterShift { rm: u8, shift_type: u8, rs: u8 },
}

pub fn decode_arm(word: u32) -> Option<Instruction> {
    // BX Rm: fixed opcode with a variable condition and source register.
    if word & 0x0FFF_FFF0 == 0x012F_FF10 {
        return Some(Instruction::BranchExchange(BranchExchange {
            condition: (word >> 28) as u8,
            rm: (word & 0xF) as u8,
        }));
    }

    // MRS reads CPSR/SPSR; MSR writes selected bytes from a register or immediate.
    if word & 0x0FBF_0FFF == 0x010F_0000 {
        return Some(Instruction::StatusRegisterTransfer(
            StatusRegisterTransfer::MoveFrom {
                condition: (word >> 28) as u8,
                saved: word & (1 << 22) != 0,
                rd: ((word >> 12) & 0xF) as u8,
            },
        ));
    }
    if word & 0x0FB0_FFF0 == 0x0120_F000 {
        return Some(Instruction::StatusRegisterTransfer(
            StatusRegisterTransfer::MoveTo {
                condition: (word >> 28) as u8,
                saved: word & (1 << 22) != 0,
                field_mask: ((word >> 16) & 0xF) as u8,
                source: StatusRegisterSource::Register((word & 0xF) as u8),
            },
        ));
    }
    if word & 0x0FB0_F000 == 0x0320_F000 {
        let immediate = word & 0xFF;
        let rotation = ((word >> 8) & 0xF) * 2;
        return Some(Instruction::StatusRegisterTransfer(
            StatusRegisterTransfer::MoveTo {
                condition: (word >> 28) as u8,
                saved: word & (1 << 22) != 0,
                field_mask: ((word >> 16) & 0xF) as u8,
                source: StatusRegisterSource::Immediate(immediate.rotate_right(rotation)),
            },
        ));
    }

    // MUL/MLA: multiply two 32-bit operands, optionally adding Rn.
    if word & 0x0FC0_00F0 == 0x0000_0090 {
        return Some(Instruction::Multiply(Multiply {
            condition: (word >> 28) as u8,
            accumulate: word & (1 << 21) != 0,
            set_flags: word & (1 << 20) != 0,
            rd: ((word >> 16) & 0xF) as u8,
            rn: ((word >> 12) & 0xF) as u8,
            rs: ((word >> 8) & 0xF) as u8,
            rm: (word & 0xF) as u8,
        }));
    }

    // UMULL/UMLAL/SMULL/SMLAL: 64-bit multiply, optionally accumulating.
    if word & 0x0F80_00F0 == 0x0080_0090 {
        return Some(Instruction::LongMultiply(LongMultiply {
            condition: (word >> 28) as u8,
            signed: word & (1 << 22) != 0,
            accumulate: word & (1 << 21) != 0,
            set_flags: word & (1 << 20) != 0,
            rd_hi: ((word >> 16) & 0xF) as u8,
            rd_lo: ((word >> 12) & 0xF) as u8,
            rs: ((word >> 8) & 0xF) as u8,
            rm: (word & 0xF) as u8,
        }));
    }

    // LDRH/STRH/LDRSB/LDRSH use the halfword-transfer encoding.
    if word & 0x0E00_0090 == 0x0000_0090 && word & (0b11 << 5) != 0 {
        let kind = match (word >> 5) & 0b11 {
            0b01 => HalfwordTransferKind::Halfword,
            0b10 => HalfwordTransferKind::SignedByte,
            0b11 => HalfwordTransferKind::SignedHalfword,
            _ => unreachable!("checked nonzero halfword transfer kind"),
        };
        let immediate = word & (1 << 22) != 0;
        if !immediate && word & 0x0000_0F00 != 0 {
            return None;
        }
        return Some(Instruction::HalfwordTransfer(HalfwordTransfer {
            condition: (word >> 28) as u8,
            kind,
            load: word & (1 << 20) != 0,
            pre_index: word & (1 << 24) != 0,
            add_offset: word & (1 << 23) != 0,
            writeback: word & (1 << 21) != 0,
            rn: ((word >> 16) & 0xF) as u8,
            rd: ((word >> 12) & 0xF) as u8,
            offset: if immediate {
                HalfwordOffset::Immediate((((word >> 4) & 0xF0) | (word & 0xF)) as u8)
            } else {
                HalfwordOffset::Register((word & 0xF) as u8)
            },
        }));
    }

    // LDR/STR and LDRB/STRB use an immediate or shifted-register offset.
    if (word >> 26) & 0b11 == 0b01 {
        let register_offset = word & (1 << 25) != 0;
        if register_offset && word & (1 << 4) != 0 {
            return None;
        }
        return Some(Instruction::SingleTransfer(SingleTransfer {
            condition: (word >> 28) as u8,
            pre_index: word & (1 << 24) != 0,
            add_offset: word & (1 << 23) != 0,
            byte: word & (1 << 22) != 0,
            writeback: word & (1 << 21) != 0,
            load: word & (1 << 20) != 0,
            rn: ((word >> 16) & 0xF) as u8,
            rd: ((word >> 12) & 0xF) as u8,
            offset: if register_offset {
                AddressOffset::Register {
                    rm: (word & 0xF) as u8,
                    shift_type: ((word >> 5) & 0b11) as u8,
                    amount: ((word >> 7) & 0x1F) as u8,
                }
            } else {
                AddressOffset::Immediate((word & 0xFFF) as u16)
            },
        }));
    }

    // B and BL use a signed 24-bit word offset relative to PC + 8.
    if (word >> 25) & 0b111 == 0b101 {
        let imm24 = word & 0x00FF_FFFF;
        let offset = ((imm24 << 8) as i32) >> 6;
        return Some(Instruction::Branch(Branch {
            condition: Some((word >> 28) as u8),
            link: word & (1 << 24) != 0,
            offset,
            pc_bias: 8,
        }));
    }

    // LDM/STM transfer an ascending register list to/from consecutive words.
    if (word >> 25) & 0b111 == 0b100 {
        let register_list = word as u16;
        if register_list == 0 {
            return None;
        }
        return Some(Instruction::BlockTransfer(BlockTransfer {
            condition: (word >> 28) as u8,
            pre_index: word & (1 << 24) != 0,
            increment: word & (1 << 23) != 0,
            user_registers: word & (1 << 22) != 0,
            writeback: word & (1 << 21) != 0,
            load: word & (1 << 20) != 0,
            rn: ((word >> 16) & 0xF) as u8,
            register_list,
        }));
    }

    // Data-processing instructions have 00 in bits 27..26.
    if (word >> 26) & 0b11 != 0 {
        return None;
    }

    // These instruction formats overlap the broad data-processing class.
    let multiply_or_swap = word & 0x0FC0_00F0 == 0x0000_0090 || word & 0x0FB0_0FF0 == 0x0100_0090;
    let halfword_transfer = word & 0x0E00_0090 == 0x0000_0090;
    let status_register_transfer = word & 0x0FBF_0FFF == 0x010F_0000
        || word & 0x0FB0_FFF0 == 0x0120_F000
        || word & 0x0FB0_F000 == 0x0320_F000;
    if multiply_or_swap || halfword_transfer || status_register_transfer {
        return None;
    }

    let immediate = (word >> 25) & 1 != 0;
    let (operand2, operand2_carry) = if immediate {
        let imm8 = word & 0xFF;
        let rotate = ((word >> 8) & 0xF) * 2;
        let value = imm8.rotate_right(rotate);
        (
            Operand2::Immediate(value),
            (rotate != 0).then_some(value & 0x8000_0000 != 0),
        )
    } else {
        let rm = (word & 0xF) as u8;
        let shift_type = ((word >> 5) & 0b11) as u8;
        if word & (1 << 4) == 0 {
            let amount = ((word >> 7) & 0x1F) as u8;
            (
                Operand2::RegisterImmediateShift {
                    rm,
                    shift_type,
                    amount,
                },
                None,
            )
        } else {
            // Register-controlled shifts require bit 7 to be zero.
            if word & (1 << 7) != 0 {
                return None;
            }
            let rs = ((word >> 8) & 0xF) as u8;
            (Operand2::RegisterShift { rm, shift_type, rs }, None)
        }
    };

    Some(Instruction::DataProcessing(DataProcessing {
        condition: ((word >> 28) & 0xF) as u8,
        opcode: ((word >> 21) & 0xF) as u8,
        set_flags: word & (1 << 20) != 0,
        rn: ((word >> 16) & 0xF) as u8,
        rd: ((word >> 12) & 0xF) as u8,
        operand2,
        operand2_carry,
    }))
}

#[cfg(test)]
mod tests {
    use super::{
        decode_arm, AddressOffset, BlockTransfer, Branch, BranchExchange, DataProcessing,
        HalfwordTransfer, HalfwordTransferKind, Instruction, LongMultiply, Multiply, Operand2,
        StatusRegisterSource, StatusRegisterTransfer,
    };
    use crate::asm::{self, HalfwordKind, HalfwordOffset as AsmHalfwordOffset, Opcode, ShiftType};
    use crate::condition::Condition;

    #[test]
    fn decodes_immediate_operand_and_data_processing_fields() {
        let word =
            asm::data_processing_immediate(Condition::AL, Opcode::ADD, true, 3, 5, 0x400).unwrap();

        assert_eq!(
            decode_arm(word),
            Some(Instruction::DataProcessing(DataProcessing {
                condition: Condition::AL as u8,
                opcode: Opcode::ADD as u8,
                set_flags: true,
                rn: 3,
                rd: 5,
                operand2: Operand2::Immediate(0x400),
                operand2_carry: Some(false),
            }))
        );
    }

    #[test]
    fn decodes_register_operand_with_immediate_shift() {
        let word = asm::data_processing_register_immediate_shift(
            Condition::NE,
            Opcode::ADD,
            false,
            1,
            2,
            3,
            ShiftType::LSL,
            7,
        )
        .unwrap();

        assert_eq!(
            decode_arm(word),
            Some(Instruction::DataProcessing(DataProcessing {
                condition: Condition::NE as u8,
                opcode: Opcode::ADD as u8,
                set_flags: false,
                rn: 1,
                rd: 2,
                operand2: Operand2::RegisterImmediateShift {
                    rm: 3,
                    shift_type: ShiftType::LSL as u8,
                    amount: 7,
                },
                operand2_carry: None,
            }))
        );
    }

    #[test]
    fn decodes_register_operand_with_register_controlled_shift() {
        let word = asm::data_processing_register_register_shift(
            Condition::AL,
            Opcode::MOV,
            true,
            0,
            2,
            3,
            ShiftType::ROR,
            4,
        )
        .unwrap();

        assert_eq!(
            decode_arm(word),
            Some(Instruction::DataProcessing(DataProcessing {
                condition: Condition::AL as u8,
                opcode: Opcode::MOV as u8,
                set_flags: true,
                rn: 0,
                rd: 2,
                operand2: Operand2::RegisterShift {
                    rm: 3,
                    shift_type: ShiftType::ROR as u8,
                    rs: 4,
                },
                operand2_carry: None,
            }))
        );
    }

    #[test]
    fn rejects_non_data_processing_and_overlapping_special_encodings() {
        let branch = asm::branch(Condition::AL, false, 0).unwrap();
        let swi = asm::software_interrupt(Condition::AL, 1).unwrap();
        let multiply = asm::multiply(Condition::AL, false, false, 1, 0, 2, 3).unwrap();
        let halfword = asm::halfword_transfer(
            Condition::AL,
            HalfwordKind::Halfword,
            true,
            true,
            true,
            false,
            0,
            1,
            AsmHalfwordOffset::Immediate(0),
        )
        .unwrap();

        assert!(matches!(decode_arm(branch), Some(Instruction::Branch(_))));
        assert_eq!(decode_arm(swi), None);
        assert!(matches!(
            decode_arm(multiply),
            Some(Instruction::Multiply(_))
        ));
        assert!(matches!(
            decode_arm(halfword),
            Some(Instruction::HalfwordTransfer(_))
        ));
        assert_eq!(decode_arm(0xE102_0091), None); // SWP r0, r1, [r2]
        assert!(matches!(
            decode_arm(0xE10F_0000),
            Some(Instruction::StatusRegisterTransfer(
                StatusRegisterTransfer::MoveFrom { .. }
            ))
        )); // MRS r0, CPSR
        assert!(matches!(
            decode_arm(0xE121_F000),
            Some(Instruction::StatusRegisterTransfer(
                StatusRegisterTransfer::MoveTo { .. }
            ))
        )); // MSR CPSR, r0
        assert!(matches!(
            decode_arm(0xE321_F0D3),
            Some(Instruction::StatusRegisterTransfer(
                StatusRegisterTransfer::MoveTo { .. }
            ))
        )); // MSR CPSR, #imm
    }

    #[test]
    fn decodes_arm_branch_and_branch_with_link() {
        let branch = asm::branch(Condition::AL, false, 0xB8).unwrap();
        let branch_link = asm::branch(Condition::NE, true, -0x20).unwrap();

        assert_eq!(
            decode_arm(branch),
            Some(Instruction::Branch(Branch {
                condition: Some(Condition::AL as u8),
                link: false,
                offset: 0xB8,
                pc_bias: 8,
            }))
        );
        assert_eq!(
            decode_arm(branch_link),
            Some(Instruction::Branch(Branch {
                condition: Some(Condition::NE as u8),
                link: true,
                offset: -0x20,
                pc_bias: 8,
            }))
        );
    }

    #[test]
    fn decodes_arm_branch_exchange() {
        assert_eq!(
            decode_arm(0xE12F_FF10),
            Some(Instruction::BranchExchange(BranchExchange {
                condition: Condition::AL as u8,
                rm: 0,
            }))
        );
    }

    #[test]
    fn decodes_mul_and_mla_fields() {
        let word = asm::multiply(Condition::NE, true, true, 1, 2, 3, 4).unwrap();
        assert_eq!(
            decode_arm(word),
            Some(Instruction::Multiply(Multiply {
                condition: Condition::NE as u8,
                accumulate: true,
                set_flags: true,
                rd: 1,
                rn: 2,
                rs: 3,
                rm: 4,
            }))
        );
    }

    #[test]
    fn decodes_signed_and_unsigned_long_multiply_fields() {
        let word = (u32::from(Condition::AL as u8) << 28)
            | (1 << 23)
            | (1 << 22)
            | (1 << 21)
            | (1 << 20)
            | (1 << 16)
            | (2 << 12)
            | (3 << 8)
            | 0x90
            | 4;

        assert_eq!(
            decode_arm(word),
            Some(Instruction::LongMultiply(LongMultiply {
                condition: Condition::AL as u8,
                signed: true,
                accumulate: true,
                set_flags: true,
                rd_hi: 1,
                rd_lo: 2,
                rs: 3,
                rm: 4,
            }))
        );

        let unsigned = word & !(1 << 22);
        assert!(matches!(
            decode_arm(unsigned),
            Some(Instruction::LongMultiply(LongMultiply {
                signed: false,
                ..
            }))
        ));
    }

    #[test]
    fn decodes_halfword_transfer_fields_and_register_offset() {
        let word = asm::halfword_transfer(
            Condition::AL,
            HalfwordKind::Halfword,
            false,
            false,
            true,
            false,
            1,
            3,
            AsmHalfwordOffset::Immediate(0xA5),
        )
        .unwrap();
        assert_eq!(
            decode_arm(word),
            Some(Instruction::HalfwordTransfer(HalfwordTransfer {
                condition: Condition::AL as u8,
                kind: HalfwordTransferKind::Halfword,
                load: false,
                pre_index: false,
                add_offset: true,
                writeback: false,
                rn: 1,
                rd: 3,
                offset: super::HalfwordOffset::Immediate(0xA5),
            }))
        );

        let register_offset = asm::halfword_transfer(
            Condition::NE,
            HalfwordKind::SignedHalfword,
            true,
            true,
            false,
            true,
            4,
            2,
            AsmHalfwordOffset::Register(5),
        )
        .unwrap();
        assert!(matches!(
            decode_arm(register_offset),
            Some(Instruction::HalfwordTransfer(HalfwordTransfer {
                kind: HalfwordTransferKind::SignedHalfword,
                load: true,
                pre_index: true,
                add_offset: false,
                writeback: true,
                offset: super::HalfwordOffset::Register(5),
                ..
            }))
        ));

        assert_eq!(
            decode_arm(0xE0C1_30B2),
            Some(Instruction::HalfwordTransfer(HalfwordTransfer {
                condition: Condition::AL as u8,
                kind: HalfwordTransferKind::Halfword,
                load: false,
                pre_index: false,
                add_offset: true,
                writeback: false,
                rn: 1,
                rd: 3,
                offset: super::HalfwordOffset::Immediate(2),
            }))
        );
    }

    #[test]
    fn decodes_block_load_and_store_fields() {
        assert_eq!(
            decode_arm(0xE8BD_8003),
            Some(Instruction::BlockTransfer(BlockTransfer {
                condition: Condition::AL as u8,
                pre_index: false,
                increment: true,
                user_registers: false,
                writeback: true,
                load: true,
                rn: 13,
                register_list: 0x8003,
            }))
        );
        assert_eq!(
            decode_arm(0xE92D_4003),
            Some(Instruction::BlockTransfer(BlockTransfer {
                condition: Condition::AL as u8,
                pre_index: true,
                increment: false,
                user_registers: false,
                writeback: true,
                load: false,
                rn: 13,
                register_list: 0x4003,
            }))
        );
        assert_eq!(decode_arm(0xE890_0000), None);
    }

    #[test]
    fn decodes_single_transfer_immediate_and_shifted_register_offsets() {
        let word =
            asm::memory_immediate(Condition::AL, true, false, true, true, false, 0, 1, 4).unwrap();
        assert_eq!(
            decode_arm(word),
            Some(Instruction::SingleTransfer(super::SingleTransfer {
                condition: Condition::AL as u8,
                pre_index: true,
                add_offset: true,
                byte: false,
                writeback: false,
                load: true,
                rn: 0,
                rd: 1,
                offset: AddressOffset::Immediate(4),
            }))
        );

        let register_offset = (u32::from(Condition::NE as u8) << 28)
            | (1 << 26)
            | (1 << 25)
            | (1 << 24)
            | (1 << 23)
            | (1 << 20)
            | (2 << 16)
            | (3 << 12)
            | (2 << 7)
            | (1 << 5)
            | 4;
        assert!(matches!(
            decode_arm(register_offset),
            Some(Instruction::SingleTransfer(super::SingleTransfer {
                load: true,
                offset: AddressOffset::Register {
                    rm: 4,
                    shift_type: 1,
                    amount: 2,
                },
                ..
            }))
        ));
    }

    #[test]
    fn decodes_msr_immediate_rotated_operand_and_field_mask() {
        assert_eq!(
            decode_arm(0xE328_F101),
            Some(Instruction::StatusRegisterTransfer(
                StatusRegisterTransfer::MoveTo {
                    condition: Condition::AL as u8,
                    saved: false,
                    field_mask: 0b1000,
                    source: StatusRegisterSource::Immediate(0x4000_0000),
                }
            ))
        );
    }

    #[test]
    fn rejects_register_shift_encoding_with_bit_7_set() {
        let valid = asm::data_processing_register_register_shift(
            Condition::AL,
            Opcode::MOV,
            false,
            0,
            1,
            2,
            ShiftType::LSL,
            3,
        )
        .unwrap();

        assert_eq!(decode_arm(valid | (1 << 7)), None);
    }
}
