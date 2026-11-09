//! 金手指。CodeBreaker 按帧写进内存；9 开头的那行会给后面的码解密。
//! GameShark / Action Replay v1 先按默认钥匙解开，再做 8、16、32 位写入。

use crate::bus::Bus;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheatFamily {
    CodeBreaker,
    GameShark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cond {
    Eq,
    Ne,
    Gt,
    Lt,
    And,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ButtonMode {
    /// 列出的键都按着。
    All,
    /// 列出的键都没按。
    None,
    /// 按下的组合正好是这些键。
    Exact,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Op {
    Write8 { addr: u32, value: u8 },
    Write16 { addr: u32, value: u16 },
    Write32 { addr: u32, value: u32 },
    Or16 { addr: u32, value: u16 },
    And16 { addr: u32, value: u16 },
    Add16 { addr: u32, value: u16 },
    Slide16 {
        addr: u32,
        value: u16,
        count: u16,
        value_step: u16,
        addr_step: u16,
    },
    Block16 { addr: u32, values: Vec<u16> },
    If { cond: Cond, addr: u32, value: u16 },
    IfButtons { mask: u16, mode: ButtonMode },
    /// GameShark：条件成立就跳过下一条。
    Unless { cond: Cond, addr: u32, value: u16 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheatProgram {
    ops: Vec<Op>,
}

impl CheatProgram {
    pub fn apply(&self, bus: &mut Bus) {
        let mut skip = false;
        for op in &self.ops {
            if skip {
                skip = false;
                continue;
            }
            skip = apply_op(bus, op);
        }
    }
}

pub fn parse_cheat(family: CheatFamily, text: &str) -> Result<CheatProgram, String> {
    let pairs = parse_pairs(text)?;
    if pairs.is_empty() {
        return Err("金手指是空的".into());
    }
    let ops = match family {
        CheatFamily::CodeBreaker => parse_codebreaker(&pairs)?,
        CheatFamily::GameShark => parse_gameshark(&pairs)?,
    };
    if ops.is_empty() {
        return Err("这些行没有要写入的码".into());
    }
    Ok(CheatProgram { ops })
}

fn apply_op(bus: &mut Bus, op: &Op) -> bool {
    match op {
        Op::Write8 { addr, value } => bus.write_u8(*addr, *value),
        Op::Write16 { addr, value } => bus.write_u16(*addr, *value),
        Op::Write32 { addr, value } => bus.write_u32(*addr, *value),
        Op::Or16 { addr, value } => {
            let next = bus.read_u16(*addr) | value;
            bus.write_u16(*addr, next);
        }
        Op::And16 { addr, value } => {
            let next = bus.read_u16(*addr) & value;
            bus.write_u16(*addr, next);
        }
        Op::Add16 { addr, value } => {
            let next = bus.read_u16(*addr).wrapping_add(*value);
            bus.write_u16(*addr, next);
        }
        Op::Slide16 {
            addr,
            value,
            count,
            value_step,
            addr_step,
        } => {
            let mut at = *addr;
            let mut current = *value;
            for _ in 0..*count {
                bus.write_u16(at, current);
                at = at.wrapping_add(u32::from(*addr_step));
                current = current.wrapping_add(*value_step);
            }
        }
        Op::Block16 { addr, values } => {
            let mut at = *addr;
            for value in values {
                bus.write_u16(at, *value);
                at = at.wrapping_add(2);
            }
        }
        Op::If { cond, addr, value } => {
            return !cond_holds(*cond, bus.read_u16(*addr), *value);
        }
        Op::IfButtons { mask, mode } => {
            let pressed = !bus.read_u16(0x0400_0130) & 0x03FF;
            let ok = match mode {
                ButtonMode::All => pressed & mask == *mask,
                ButtonMode::None => pressed & mask == 0,
                ButtonMode::Exact => pressed == *mask,
            };
            return !ok;
        }
        Op::Unless { cond, addr, value } => {
            return cond_holds(*cond, bus.read_u16(*addr), *value);
        }
    }
    false
}

fn cond_holds(cond: Cond, have: u16, want: u16) -> bool {
    match cond {
        Cond::Eq => have == want,
        Cond::Ne => have != want,
        Cond::Gt => have > want,
        Cond::Lt => have < want,
        Cond::And => have & want != 0,
    }
}

fn parse_pairs(text: &str) -> Result<Vec<(u32, u32)>, String> {
    let mut values = Vec::new();
    for raw in text.split(|ch: char| ch.is_whitespace() || ch == ';' || ch == ',') {
        if raw.is_empty() {
            continue;
        }
        if raw.len() > 8 || !raw.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return Err(format!("无法识别的金手指：{raw}"));
        }
        let value = u32::from_str_radix(raw, 16).expect("hex 已经检查过");
        values.push(value);
    }
    if values.len() % 2 != 0 {
        return Err("金手指要成对，一行地址、一行数值".into());
    }
    Ok(values
        .chunks_exact(2)
        .map(|pair| (pair[0], pair[1]))
        .collect())
}

fn parse_codebreaker(pairs: &[(u32, u32)]) -> Result<Vec<Op>, String> {
    for (_, value) in pairs {
        if *value > 0xFFFF {
            return Err("CodeBreaker 的数值是 4 位十六进制".into());
        }
    }
    let mut cipher = CodeBreakerCipher::new();
    let mut words = Vec::with_capacity(pairs.len());
    for (op1, op2) in pairs {
        let mut op1 = *op1;
        let mut op2 = *op2 as u16;
        if cipher.armed() {
            cipher.decrypt(&mut op1, &mut op2);
        }
        if (op1 >> 28) == 9 {
            cipher.reseed(op1, op2);
            continue;
        }
        words.push((op1, op2));
    }

    let mut ops = Vec::new();
    let mut index = 0;
    while index < words.len() {
        let (op1, op2) = words[index];
        let addr = op1 & 0x0FFF_FFFF;
        match op1 >> 28 {
            0 | 1 => {}
            2 => ops.push(Op::Or16 { addr, value: op2 }),
            3 => ops.push(Op::Write8 {
                addr,
                value: op2 as u8,
            }),
            4 => {
                let Some((next1, next2)) = words.get(index + 1).copied() else {
                    return Err("滑动码还缺第二行".into());
                };
                index += 1;
                ops.push(Op::Slide16 {
                    addr,
                    value: op2,
                    value_step: (next1 >> 16) as u16,
                    count: next1 as u16,
                    addr_step: next2,
                });
            }
            5 => {
                let mut left = op2 as usize;
                let mut values = Vec::with_capacity(left);
                let base = index;
                while left > 0 {
                    index += 1;
                    let Some((data1, data2)) = words.get(index).copied() else {
                        return Err("批量写入的数据不够".into());
                    };
                    for half in [data1 >> 16, data1 & 0xFFFF, u32::from(data2)] {
                        if left == 0 {
                            break;
                        }
                        values.push((half as u16).rotate_left(8));
                        left -= 1;
                    }
                }
                if index == base {
                    return Err("批量写入的数据不够".into());
                }
                ops.push(Op::Block16 { addr, values });
            }
            6 => ops.push(Op::And16 { addr, value: op2 }),
            7 => ops.push(Op::If {
                cond: Cond::Eq,
                addr,
                value: op2,
            }),
            8 => ops.push(Op::Write16 { addr, value: op2 }),
            0xA => ops.push(Op::If {
                cond: Cond::Ne,
                addr,
                value: op2,
            }),
            0xB => ops.push(Op::If {
                cond: Cond::Gt,
                addr,
                value: op2,
            }),
            0xC => ops.push(Op::If {
                cond: Cond::Lt,
                addr,
                value: op2,
            }),
            0xD => {
                let mode = match addr {
                    0x20 => ButtonMode::All,
                    0x10 => ButtonMode::None,
                    0x00 => ButtonMode::Exact,
                    _ => return Err(format!("不认识的按键码 {op1:08X} {op2:04X}")),
                };
                ops.push(Op::IfButtons { mask: op2, mode });
            }
            0xE => ops.push(Op::Add16 { addr, value: op2 }),
            0xF => ops.push(Op::If {
                cond: Cond::And,
                addr,
                value: op2,
            }),
            _ => return Err(format!("不认识的 CodeBreaker 码 {op1:08X} {op2:04X}")),
        }
        index += 1;
    }
    Ok(ops)
}

const GS_SEEDS: [u32; 4] = [0x09F4_FBBD, 0x9681_884A, 0x3520_27E9, 0xF3DE_E5A7];

fn parse_gameshark(pairs: &[(u32, u32)]) -> Result<Vec<Op>, String> {
    let mut ops = Vec::new();
    for (op1, op2) in pairs {
        let (op1, op2) = gameshark_decrypt(*op1, *op2);
        if op2 == 0x001D_C0DE {
            continue;
        }
        let addr = op1 & 0x0FFF_FFFF;
        match op1 >> 28 {
            0 => ops.push(Op::Write8 {
                addr,
                value: op2 as u8,
            }),
            1 => ops.push(Op::Write16 {
                addr,
                value: op2 as u16,
            }),
            2 => ops.push(Op::Write32 { addr, value: op2 }),
            0xD => {
                let cond = match (op2 >> 20) & 0xF {
                    0 => Cond::Ne,
                    1 => Cond::Eq,
                    2 => Cond::Gt,
                    3 => Cond::Lt,
                    _ => {
                        return Err(format!("不认识的 GameShark 条件 {op1:08X} {op2:08X}"));
                    }
                };
                ops.push(Op::Unless {
                    cond,
                    addr,
                    value: op2 as u16,
                });
            }
            0xF => {}
            _ => {
                return Err(format!(
                    "这版只做 GameShark v1 的写入和条件，解不开 {op1:08X} {op2:08X}"
                ));
            }
        }
    }
    Ok(ops)
}

fn gameshark_decrypt(mut op1: u32, mut op2: u32) -> (u32, u32) {
    let mut sum = 0xC6EF_3720u32;
    for _ in 0..32 {
        op2 = op2.wrapping_sub(
            op1.wrapping_shl(4)
                .wrapping_add(GS_SEEDS[2])
                ^ op1.wrapping_add(sum)
                ^ op1.wrapping_shr(5).wrapping_add(GS_SEEDS[3]),
        );
        op1 = op1.wrapping_sub(
            op2.wrapping_shl(4)
                .wrapping_add(GS_SEEDS[0])
                ^ op2.wrapping_add(sum)
                ^ op2.wrapping_shr(5).wrapping_add(GS_SEEDS[1]),
        );
        sum = sum.wrapping_sub(0x9E37_79B9);
    }
    (op1, op2)
}

struct CodeBreakerCipher {
    master: u32,
    seeds: [u32; 4],
    table: [u8; 0x30],
    rng: u32,
}

impl CodeBreakerCipher {
    fn new() -> Self {
        Self {
            master: 0,
            seeds: [0; 4],
            table: [0; 0x30],
            rng: 0,
        }
    }

    fn armed(&self) -> bool {
        self.master != 0
    }

    fn reseed(&mut self, op1: u32, op2: u16) {
        self.rng = u32::from(op2 & 0xFF) ^ 0x1111;
        for (index, slot) in self.table.iter_mut().enumerate() {
            *slot = index as u8;
        }
        for _ in 0..0x50 {
            let x = self.swap_index();
            let y = self.swap_index();
            self.table.swap(x, y);
        }
        self.rng = 0x4EFA_D1C3;
        for _ in 0..(op1 >> 24) & 0xF {
            self.rand();
        }
        self.seeds[2] = self.rand();
        self.seeds[3] = self.rand();
        self.rng = u32::from(op2 >> 8) ^ 0xF254;
        for _ in 0..u32::from(op2 >> 8) {
            self.rand();
        }
        self.seeds[0] = self.rand();
        self.seeds[1] = self.rand();
        self.master = op1;
    }

    fn decrypt(&self, op1: &mut u32, op2: &mut u16) {
        let mut buffer = byteswap(*op1, *op2);
        for i in (0..self.table.len()).rev() {
            let offset_x = i >> 3;
            let offset_y = (self.table[i] >> 3) as usize;
            let bit_x = i & 7;
            let bit_y = (self.table[i] & 7) as usize;
            let x = (buffer[offset_x] >> bit_x) & 1;
            let y = (buffer[offset_y] >> bit_y) & 1;
            let mut x2 = buffer[offset_x] & !(1 << bit_x);
            if y != 0 {
                x2 |= 1 << bit_x;
            }
            buffer[offset_x] = x2;
            let mut y2 = buffer[offset_y] & !(1 << bit_y);
            if x != 0 {
                y2 |= 1 << bit_y;
            }
            buffer[offset_y] = y2;
        }
        (*op1, *op2) = unbyteswap(buffer);
        *op1 ^= self.seeds[0];
        *op2 ^= self.seeds[1] as u16;

        buffer = byteswap(*op1, *op2);
        let master = self.master;
        for i in 0..5 {
            buffer[i] ^= ((master >> 8) as u8) ^ buffer[i + 1];
        }
        buffer[5] ^= (master >> 8) as u8;
        for i in (1..=5).rev() {
            buffer[i] ^= (master as u8) ^ buffer[i - 1];
        }
        buffer[0] ^= master as u8;
        (*op1, *op2) = unbyteswap(buffer);
        *op1 ^= self.seeds[2];
        *op2 ^= self.seeds[3] as u16;
    }

    fn rand(&mut self) -> u32 {
        let roll = self.rng.wrapping_mul(0x41C6_4E6D).wrapping_add(0x3039);
        let roll2 = roll.wrapping_mul(0x41C6_4E6D).wrapping_add(0x3039);
        let roll3 = roll2.wrapping_mul(0x41C6_4E6D).wrapping_add(0x3039);
        let mix = (roll << 14) & 0xC000_0000
            | (roll2 >> 1) & 0x3FFF_8000
            | (roll3 >> 16) & 0x7FFF;
        self.rng = roll3;
        mix
    }

    fn swap_index(&mut self) -> usize {
        let mut roll = self.rand();
        let mut count: u32 = self.table.len() as u32;
        if roll == count {
            roll = 0;
        }
        if roll < count {
            return roll as usize;
        }
        let mut bit = 1u32;
        while count < 0x1000_0000 && count < roll {
            count <<= 4;
            bit <<= 4;
        }
        while count < 0x8000_0000 && count < roll {
            count <<= 1;
            bit <<= 1;
        }
        let mask = loop {
            let mut mask = 0u32;
            if roll >= count {
                roll -= count;
            }
            if roll >= count >> 1 {
                roll -= count >> 1;
                mask |= bit.rotate_right(1);
            }
            if roll >= count >> 2 {
                roll -= count >> 2;
                mask |= bit.rotate_right(2);
            }
            if roll >= count >> 3 {
                roll -= count >> 3;
                mask |= bit.rotate_right(3);
            }
            if roll == 0 || bit >> 4 == 0 {
                break mask;
            }
            bit >>= 4;
            count >>= 4;
        };
        let mask = mask & 0xE000_0000;
        if mask == 0 || bit & 7 == 0 {
            return roll as usize;
        }
        if mask & bit.rotate_right(3) != 0 {
            roll += count >> 3;
        }
        if mask & bit.rotate_right(2) != 0 {
            roll += count >> 2;
        }
        if mask & bit.rotate_right(1) != 0 {
            roll += count >> 1;
        }
        roll as usize
    }
}

fn byteswap(op1: u32, op2: u16) -> [u8; 6] {
    [
        (op1 >> 24) as u8,
        (op1 >> 16) as u8,
        (op1 >> 8) as u8,
        op1 as u8,
        (op2 >> 8) as u8,
        op2 as u8,
    ]
}

fn unbyteswap(buffer: [u8; 6]) -> (u32, u16) {
    let op1 = u32::from_be_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]);
    let op2 = u16::from_be_bytes([buffer[4], buffer[5]]);
    (op1, op2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codebreaker_writes_a_byte_and_a_halfword() {
        let program = parse_cheat(CheatFamily::CodeBreaker, "32000000 00AB\n82000002 1234").unwrap();
        let mut bus = Bus::new();
        program.apply(&mut bus);
        assert_eq!(bus.read_u8(0x0200_0000), 0xAB);
        assert_eq!(bus.read_u16(0x0200_0002), 0x1234);
    }

    #[test]
    fn codebreaker_skips_the_write_when_the_halfword_differs() {
        let program = parse_cheat(
            CheatFamily::CodeBreaker,
            "72000000 0001\n82000002 00FF",
        )
        .unwrap();
        let mut bus = Bus::new();
        bus.write_u16(0x0200_0000, 0x0002);
        program.apply(&mut bus);
        assert_eq!(bus.read_u16(0x0200_0002), 0);
        bus.write_u16(0x0200_0000, 0x0001);
        program.apply(&mut bus);
        assert_eq!(bus.read_u16(0x0200_0002), 0x00FF);
    }

    #[test]
    fn codebreaker_slide_or_and_add_and_buttons() {
        let program = parse_cheat(
            CheatFamily::CodeBreaker,
            "42000000 0010\n00020003 0002\n22000010 000F\n62000010 00F0\nE2000012 0001\nD0000020 0001\n82000014 0007",
        )
        .unwrap();
        let mut bus = Bus::new();
        bus.write_u16(0x0200_0010, 0x00F0);
        bus.set_keys(0x0001);
        program.apply(&mut bus);
        assert_eq!(bus.read_u16(0x0200_0000), 0x0010);
        assert_eq!(bus.read_u16(0x0200_0002), 0x0012);
        assert_eq!(bus.read_u16(0x0200_0004), 0x0014);
        assert_eq!(bus.read_u16(0x0200_0010), 0x00F0);
        assert_eq!(bus.read_u16(0x0200_0012), 0x0001);
        assert_eq!(bus.read_u16(0x0200_0014), 0x0007);
    }

    #[test]
    fn codebreaker_block_byteswaps_each_halfword() {
        let program =
            parse_cheat(CheatFamily::CodeBreaker, "52000000 0002\n11223344 5566").unwrap();
        let mut bus = Bus::new();
        program.apply(&mut bus);
        assert_eq!(bus.read_u16(0x0200_0000), 0x2211);
        assert_eq!(bus.read_u16(0x0200_0002), 0x4433);
    }

    #[test]
    fn codebreaker_seed_matches_the_reference_decrypt() {
        let mut cipher = CodeBreakerCipher::new();
        cipher.reseed(0x9ABC_DEF0, 0x1234);
        let mut op1 = 0x1111_1111;
        let mut op2 = 0x2222;
        cipher.decrypt(&mut op1, &mut op2);
        assert_eq!((op1, op2), (0x5ADE_2CA5, 0x00E6));
    }

    #[test]
    fn gameshark_v1_decrypts_into_a_halfword_write() {
        let (hi, lo) = encrypt_gameshark(0x1200_0000, 0x0000_BEEF);
        let program = parse_cheat(CheatFamily::GameShark, &format!("{hi:08X} {lo:08X}")).unwrap();
        let mut bus = Bus::new();
        program.apply(&mut bus);
        assert_eq!(bus.read_u16(0x0200_0000), 0xBEEF);
    }

    fn encrypt_gameshark(mut op1: u32, mut op2: u32) -> (u32, u32) {
        let mut sum = 0u32;
        for _ in 0..32 {
            sum = sum.wrapping_add(0x9E37_79B9);
            op1 = op1.wrapping_add(
                op2.wrapping_shl(4)
                    .wrapping_add(GS_SEEDS[0])
                    ^ op2.wrapping_add(sum)
                    ^ op2.wrapping_shr(5).wrapping_add(GS_SEEDS[1]),
            );
            op2 = op2.wrapping_add(
                op1.wrapping_shl(4)
                    .wrapping_add(GS_SEEDS[2])
                    ^ op1.wrapping_add(sum)
                    ^ op1.wrapping_shr(5).wrapping_add(GS_SEEDS[3]),
            );
        }
        (op1, op2)
    }
}
