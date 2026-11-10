//! 金手指。CodeBreaker 按帧写进内存；9 开头的那行会给后面的码解密。
//! GameShark v1 和 Action Replay v3 先按各自的钥匙解开。v3 还能做间接写、加减、条件块，以及盖住卡带上的半字。

use crate::bus::Bus;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheatFamily {
    CodeBreaker,
    GameShark,
    ActionReplay,
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
    /// 同一数值写 `count` 次。每次把 `value_step` 加进数值，地址再走 `addr_step`。
    Repeat {
        addr: u32,
        value: u32,
        width: u8,
        count: u32,
        addr_step: u32,
        value_step: u32,
    },
    /// 从 `addr` 读出指针，加上偏移再写。
    Indirect {
        addr: u32,
        value: u32,
        width: u8,
        offset: u32,
    },
    /// 把数值加进原来的字节。
    Add {
        addr: u32,
        value: u32,
        width: u8,
    },
    /// 条件成立才执行后面 `then_len` 条；否则跳过它们，改走 `else_len` 条。
    When {
        kind: WhenKind,
        addr: u32,
        value: u32,
        width: u8,
        then_len: u16,
        else_len: u16,
        saw_else: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WhenKind {
    Eq,
    Ne,
    Lt,
    Gt,
    Ult,
    Ugt,
    And,
    Never,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheatProgram {
    ops: Vec<Op>,
    rom: Vec<(u32, u16)>,
}

struct Effect {
    pass: bool,
    then_len: usize,
    else_len: usize,
}

fn goes_through() -> Effect {
    Effect {
        pass: true,
        then_len: 0,
        else_len: 0,
    }
}

fn skip_next(pass: bool) -> Effect {
    Effect {
        pass,
        then_len: 1,
        else_len: 0,
    }
}

impl CheatProgram {
    pub fn apply(&self, bus: &mut Bus) {
        for (addr, value) in &self.rom {
            bus.patch_cheat_rom(*addr, *value);
        }
        let mut index = 0;
        // 条件成立时，走到 then 的最后一条再跳过 else。
        let mut jump_after: Option<(usize, usize)> = None;
        while index < self.ops.len() {
            let effect = apply_op(bus, &self.ops[index]);
            if let Some((last_then, resume)) = jump_after {
                if index == last_then {
                    jump_after = None;
                    index = resume;
                    continue;
                }
            }
            if !effect.pass && effect.then_len > 0 {
                index = index.saturating_add(effect.then_len).saturating_add(1);
                continue;
            }
            if effect.pass && effect.else_len > 0 {
                if effect.then_len == 0 {
                    index = index.saturating_add(effect.else_len).saturating_add(1);
                    continue;
                }
                jump_after = Some((
                    index + effect.then_len,
                    index + effect.then_len + effect.else_len + 1,
                ));
            }
            index += 1;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheatReading {
    Empty,
    Short { missing: usize },
    Lines(Vec<String>),
    Invalid(String),
}

const CHEAT_DIGIT_CAP: usize = 480;

pub fn format_cheat(family: CheatFamily, text: &str) -> String {
    let mut digits = String::new();
    for ch in text.chars() {
        if ch.is_ascii_hexdigit() {
            digits.push(ch.to_ascii_uppercase());
            if digits.len() == CHEAT_DIGIT_CAP {
                break;
            }
        }
    }
    let value_len = match family {
        CheatFamily::CodeBreaker => 4,
        CheatFamily::GameShark | CheatFamily::ActionReplay => 8,
    };
    let pair = 8 + value_len;
    let mut lines = Vec::new();
    let mut offset = 0;
    while offset < digits.len() {
        let end = (offset + pair).min(digits.len());
        let chunk = &digits[offset..end];
        if chunk.len() <= 8 {
            lines.push(chunk.to_string());
        } else {
            lines.push(format!("{} {}", &chunk[..8], &chunk[8..]));
        }
        offset = end;
    }
    lines.join("\n")
}

pub fn read_cheat(family: CheatFamily, text: &str) -> CheatReading {
    let formatted = format_cheat(family, text);
    let digits = formatted.chars().filter(|ch| ch.is_ascii_hexdigit()).count();
    if digits == 0 {
        return CheatReading::Empty;
    }
    let pair = match family {
        CheatFamily::CodeBreaker => 12,
        CheatFamily::GameShark | CheatFamily::ActionReplay => 16,
    };
    let missing = pair - (digits % pair);
    if missing != pair {
        return CheatReading::Short { missing };
    }
    match explain_cheat(family, &formatted) {
        Ok(lines) => CheatReading::Lines(lines),
        Err(error) => CheatReading::Invalid(error),
    }
}

fn explain_cheat(family: CheatFamily, text: &str) -> Result<Vec<String>, String> {
    let program = parse_cheat(family, text)?;
    let mut lines = Vec::new();
    for op in &program.ops {
        lines.push(explain_op(op));
    }
    for (addr, value) in &program.rom {
        lines.push(format!("把卡带 {addr:08X} 的半字改成 {value:04X}"));
    }
    if lines.is_empty() {
        return Err("这些行没有要写入的码".into());
    }
    Ok(lines)
}

fn explain_op(op: &Op) -> String {
    match op {
        Op::Write8 { addr, value } => write_phrase(*addr, u32::from(*value), 1),
        Op::Write16 { addr, value } => write_phrase(*addr, u32::from(*value), 2),
        Op::Write32 { addr, value } => write_phrase(*addr, *value, 4),
        Op::Or16 { addr, value } => {
            format!("把 {addr:08X} 的半字按位或上 {value:04X}")
        }
        Op::And16 { addr, value } => {
            format!("把 {addr:08X} 的半字按位与上 {value:04X}")
        }
        Op::Add16 { addr, value } => format!("把 {addr:08X} 的半字加上 {value:04X}"),
        Op::Slide16 {
            addr,
            value,
            count,
            value_step,
            addr_step,
        } => format!(
            "从 {addr:08X} 起写 {count} 次半字，开头 {value:04X}，每次地址加 {addr_step}、数值加 {value_step}"
        ),
        Op::Block16 { addr, values } => {
            if values.len() <= 4 {
                let listed = values
                    .iter()
                    .map(|value| format!("{value:04X}"))
                    .collect::<Vec<_>>()
                    .join("、");
                format!("从 {addr:08X} 起写入半字 {listed}")
            } else {
                format!("从 {addr:08X} 起写入 {} 个半字", values.len())
            }
        }
        Op::If { cond, addr, value } => {
            format!("若 {}，才做下一条", half_cond(*addr, *cond, *value))
        }
        Op::IfButtons { mask, mode } => button_phrase(*mask, mode),
        Op::Unless { cond, addr, value } => {
            format!("若 {}，就跳过下一条", half_cond(*addr, *cond, *value))
        }
        Op::Repeat {
            addr,
            value,
            width,
            count,
            addr_step,
            value_step,
        } => {
            if *count <= 1 && *value_step == 0 {
                write_phrase(*addr, *value, *width)
            } else if *value_step == 0 && *addr_step == u32::from(*width) {
                format!(
                    "从 {addr:08X} 起，把{} {} 连写 {count} 次",
                    width_name(*width),
                    hex_value(*value, *width)
                )
            } else {
                format!(
                    "从 {addr:08X} 起写 {count} 次{}，开头 {}，每次地址加 {addr_step}、数值加 {value_step}",
                    width_name(*width),
                    hex_value(*value, *width)
                )
            }
        }
        Op::Indirect {
            addr,
            value,
            width,
            offset,
        } => {
            if *offset == 0 {
                format!(
                    "读出 {addr:08X} 里的指针，把那里的{}写成 {}",
                    width_name(*width),
                    hex_value(*value, *width)
                )
            } else {
                format!(
                    "读出 {addr:08X} 里的指针，加上 {offset}，把那里的{}写成 {}",
                    width_name(*width),
                    hex_value(*value, *width)
                )
            }
        }
        Op::Add { addr, value, width } => format!(
            "把 {addr:08X} 的{}加上 {}",
            width_name(*width),
            hex_value(*value, *width)
        ),
        Op::When {
            kind,
            addr,
            value,
            width,
            then_len,
            else_len,
            ..
        } => {
            let phrase = when_phrase(*kind, *addr, *value, *width);
            if *else_len == 0 && *then_len <= 1 {
                format!("若 {phrase}，才做下一条")
            } else if *else_len == 0 {
                format!("若 {phrase}，做后面 {then_len} 条")
            } else if *then_len == 0 {
                format!("若 {phrase}，跳过后面 {else_len} 条")
            } else {
                format!("若 {phrase}，做后面 {then_len} 条，否则做后面 {else_len} 条")
            }
        }
    }
}

fn write_phrase(addr: u32, value: u32, width: u8) -> String {
    format!(
        "把 {addr:08X} 的{}写成 {}",
        width_name(width),
        hex_value(value, width)
    )
}

fn width_name(width: u8) -> &'static str {
    match width {
        1 => "字节",
        2 => "半字",
        _ => "字",
    }
}

fn hex_value(value: u32, width: u8) -> String {
    match width {
        1 => format!("{value:02X}"),
        2 => format!("{value:04X}"),
        _ => format!("{value:08X}"),
    }
}

fn half_cond(addr: u32, cond: Cond, value: u16) -> String {
    let place = format!("{addr:08X} 的半字");
    match cond {
        Cond::Eq => format!("{place}等于 {value:04X}"),
        Cond::Ne => format!("{place}不等于 {value:04X}"),
        Cond::Gt => format!("{place}大于 {value:04X}"),
        Cond::Lt => format!("{place}小于 {value:04X}"),
        Cond::And => format!("{place}按位与上 {value:04X} 之后不为 0"),
    }
}

fn when_phrase(kind: WhenKind, addr: u32, value: u32, width: u8) -> String {
    if kind == WhenKind::Never {
        return "这个条件永远不成立".into();
    }
    let place = format!("{addr:08X} 的{}", width_name(width));
    let shown = hex_value(value, width);
    match kind {
        WhenKind::Eq => format!("{place}等于 {shown}"),
        WhenKind::Ne => format!("{place}不等于 {shown}"),
        WhenKind::Lt => format!("{place}有符号小于 {shown}"),
        WhenKind::Gt => format!("{place}有符号大于 {shown}"),
        WhenKind::Ult => format!("{place}小于 {shown}"),
        WhenKind::Ugt => format!("{place}大于 {shown}"),
        WhenKind::And => format!("{place}按位与上 {shown} 之后不为 0"),
        WhenKind::Never => "这个条件永远不成立".into(),
    }
}

fn button_phrase(mask: u16, mode: &ButtonMode) -> String {
    const NAMES: [&str; 10] = ["A", "B", "SELECT", "START", "右", "左", "上", "下", "R", "L"];
    let names = NAMES
        .iter()
        .enumerate()
        .filter(|(bit, _)| mask & (1 << bit) != 0)
        .map(|(_, name)| *name)
        .collect::<Vec<_>>()
        .join("、");
    if names.is_empty() {
        return match mode {
            ButtonMode::Exact => "什么键都没按时才做下一条".into(),
            _ => "不看按键，直接做下一条".into(),
        };
    }
    match mode {
        ButtonMode::All => format!("按着 {names} 时才做下一条"),
        ButtonMode::None => format!("没按 {names} 时才做下一条"),
        ButtonMode::Exact => format!("只按着 {names} 时才做下一条"),
    }
}

pub fn parse_cheat(family: CheatFamily, text: &str) -> Result<CheatProgram, String> {
    let pairs = parse_pairs(text)?;
    if pairs.is_empty() {
        return Err("金手指是空的".into());
    }
    let (ops, rom) = match family {
        CheatFamily::CodeBreaker => (parse_codebreaker(&pairs)?, Vec::new()),
        CheatFamily::GameShark => (parse_gameshark(&pairs)?, Vec::new()),
        CheatFamily::ActionReplay => parse_action_replay(&pairs)?,
    };
    if ops.is_empty() && rom.is_empty() {
        return Err("这些行没有要写入的码".into());
    }
    Ok(CheatProgram { ops, rom })
}

fn apply_op(bus: &mut Bus, op: &Op) -> Effect {
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
            return skip_next(cond_holds(*cond, bus.read_u16(*addr), *value));
        }
        Op::IfButtons { mask, mode } => {
            let pressed = !bus.read_u16(0x0400_0130) & 0x03FF;
            let ok = match mode {
                ButtonMode::All => pressed & mask == *mask,
                ButtonMode::None => pressed & mask == 0,
                ButtonMode::Exact => pressed == *mask,
            };
            return skip_next(ok);
        }
        Op::Unless { cond, addr, value } => {
            return skip_next(!cond_holds(*cond, bus.read_u16(*addr), *value));
        }
        Op::Repeat {
            addr,
            value,
            width,
            count,
            addr_step,
            value_step,
        } => {
            let mut at = *addr;
            let mut current = *value;
            for _ in 0..*count {
                write_width(bus, at, *width, current);
                at = at.wrapping_add(*addr_step);
                current = current.wrapping_add(*value_step);
            }
        }
        Op::Indirect {
            addr,
            value,
            width,
            offset,
        } => {
            let dest = bus.read_u32(*addr).wrapping_add(*offset);
            write_width(bus, dest, *width, *value);
        }
        Op::Add { addr, value, width } => {
            let next = read_width(bus, *addr, *width).wrapping_add(*value);
            write_width(bus, *addr, *width, next);
        }
        Op::When {
            kind,
            addr,
            value,
            width,
            then_len,
            else_len,
            ..
        } => {
            return Effect {
                pass: when_holds(*kind, bus, *addr, *value, *width),
                then_len: usize::from(*then_len),
                else_len: usize::from(*else_len),
            };
        }
    }
    goes_through()
}

fn write_width(bus: &mut Bus, addr: u32, width: u8, value: u32) {
    match width {
        1 => bus.write_u8(addr, value as u8),
        2 => bus.write_u16(addr, value as u16),
        _ => bus.write_u32(addr, value),
    }
}

fn read_width(bus: &Bus, addr: u32, width: u8) -> u32 {
    match width {
        1 => u32::from(bus.read_u8(addr)),
        2 => u32::from(bus.read_u16(addr)),
        _ => bus.read_u32(addr),
    }
}

fn when_holds(kind: WhenKind, bus: &Bus, addr: u32, value: u32, width: u8) -> bool {
    if kind == WhenKind::Never {
        return false;
    }
    let have = read_width(bus, addr, width);
    match kind {
        WhenKind::Eq => have == value,
        WhenKind::Ne => have != value,
        WhenKind::Lt => signed_width(have, width) < signed_width(value, width),
        WhenKind::Gt => signed_width(have, width) > signed_width(value, width),
        WhenKind::Ult => have < value,
        WhenKind::Ugt => have > value,
        WhenKind::And => have & value != 0,
        WhenKind::Never => false,
    }
}

fn signed_width(value: u32, width: u8) -> i32 {
    match width {
        1 => i32::from(value as u8),
        2 => i32::from(value as u16),
        _ => value as i32,
    }
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
        let (op1, op2) = gameshark_decrypt(*op1, *op2, &GS_SEEDS);
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

fn gameshark_decrypt(mut op1: u32, mut op2: u32, seeds: &[u32; 4]) -> (u32, u32) {
    let mut sum = 0xC6EF_3720u32;
    for _ in 0..32 {
        op2 = op2.wrapping_sub(
            op1.wrapping_shl(4)
                .wrapping_add(seeds[2])
                ^ op1.wrapping_add(sum)
                ^ op1.wrapping_shr(5).wrapping_add(seeds[3]),
        );
        op1 = op1.wrapping_sub(
            op2.wrapping_shl(4)
                .wrapping_add(seeds[0])
                ^ op2.wrapping_add(sum)
                ^ op2.wrapping_shr(5).wrapping_add(seeds[1]),
        );
        sum = sum.wrapping_sub(0x9E37_79B9);
    }
    (op1, op2)
}

const PAR_SEEDS: [u32; 4] = [0x7AA9_648F, 0x7FAE_6994, 0xC0EF_AAD5, 0x4271_2C57];

const PAR_T1: [u8; 256] = [
    0xD0, 0xFF, 0xBA, 0xE5, 0xC1, 0xC7, 0xDB, 0x5B, 0x16, 0xE3, 0x6E, 0x26, 0x62, 0x31, 0x2E, 0x2A,
    0xD1, 0xBB, 0x4A, 0xE6, 0xAE, 0x2F, 0x0A, 0x90, 0x29, 0x90, 0xB6, 0x67, 0x58, 0x2A, 0xB4, 0x45,
    0x7B, 0xCB, 0xF0, 0x73, 0x84, 0x30, 0x81, 0xC2, 0xD7, 0xBE, 0x89, 0xD7, 0x4E, 0x73, 0x5C, 0xC7,
    0x80, 0x1B, 0xE5, 0xE4, 0x43, 0xC7, 0x46, 0xD6, 0x6F, 0x7B, 0xBF, 0xED, 0xE5, 0x27, 0xD1, 0xB5,
    0xD0, 0xD8, 0xA3, 0xCB, 0x2B, 0x30, 0xA4, 0xF0, 0x84, 0x14, 0x72, 0x5C, 0xFF, 0xA4, 0xFB, 0x54,
    0x9D, 0x70, 0xE2, 0xFF, 0xBE, 0xE8, 0x24, 0x76, 0xE5, 0x15, 0xFB, 0x1A, 0xBC, 0x87, 0x02, 0x2A,
    0x58, 0x8F, 0x9A, 0x95, 0xBD, 0xAE, 0x8D, 0x0C, 0xA5, 0x4C, 0xF2, 0x5C, 0x7D, 0xAD, 0x51, 0xFB,
    0xB1, 0x22, 0x07, 0xE0, 0x29, 0x7C, 0xEB, 0x98, 0x14, 0xC6, 0x31, 0x97, 0xE4, 0x34, 0x8F, 0xCC,
    0x99, 0x56, 0x9F, 0x78, 0x43, 0x91, 0x85, 0x3F, 0xC2, 0xD0, 0xD1, 0x80, 0xD1, 0x77, 0xA7, 0xE2,
    0x43, 0x99, 0x1D, 0x2F, 0x8B, 0x6A, 0xE4, 0x66, 0x82, 0xF7, 0x2B, 0x0B, 0x65, 0x14, 0xC0, 0xC2,
    0x1D, 0x96, 0x78, 0x1C, 0xC4, 0xC3, 0xD2, 0xB1, 0x64, 0x07, 0xD7, 0x6F, 0x02, 0xE9, 0x44, 0x31,
    0xDB, 0x3C, 0xEB, 0x93, 0xED, 0x9A, 0x57, 0x05, 0xB9, 0x0E, 0xAF, 0x1F, 0x48, 0x11, 0xDC, 0x35,
    0x6C, 0xB8, 0xEE, 0x2A, 0x48, 0x2B, 0xBC, 0x89, 0x12, 0x59, 0xCB, 0xD1, 0x18, 0xEA, 0x72, 0x11,
    0x01, 0x75, 0x3B, 0xB5, 0x56, 0xF4, 0x8B, 0xA0, 0x41, 0x75, 0x86, 0x7B, 0x94, 0x12, 0x2D, 0x4C,
    0x0C, 0x22, 0xC9, 0x4A, 0xD8, 0xB1, 0x8D, 0xF0, 0x55, 0x2E, 0x77, 0x50, 0x1C, 0x64, 0x77, 0xAA,
    0x3E, 0xAC, 0xD3, 0x3D, 0xCE, 0x60, 0xCA, 0x5D, 0xA0, 0x92, 0x78, 0xC6, 0x51, 0xFE, 0xF9, 0x30,
];

const PAR_T2: [u8; 256] = [
    0xAA, 0xAF, 0xF0, 0x72, 0x90, 0xF7, 0x71, 0x27, 0x06, 0x11, 0xEB, 0x9C, 0x37, 0x12, 0x72, 0xAA,
    0x65, 0xBC, 0x0D, 0x4A, 0x76, 0xF6, 0x5C, 0xAA, 0xB0, 0x7A, 0x7D, 0x81, 0xC1, 0xCE, 0x2F, 0x9F,
    0x02, 0x75, 0x38, 0xC8, 0xFC, 0x66, 0x05, 0xC2, 0x2C, 0xBD, 0x91, 0xAD, 0x03, 0xB1, 0x88, 0x93,
    0x31, 0xC6, 0xAB, 0x40, 0x23, 0x43, 0x76, 0x54, 0xCA, 0xE7, 0x00, 0x96, 0x9F, 0xD8, 0x24, 0x8B,
    0xE4, 0xDC, 0xDE, 0x48, 0x2C, 0xCB, 0xF7, 0x84, 0x1D, 0x45, 0xE5, 0xF1, 0x75, 0xA0, 0xED, 0xCD,
    0x4B, 0x24, 0x8A, 0xB3, 0x98, 0x7B, 0x12, 0xB8, 0xF5, 0x63, 0x97, 0xB3, 0xA6, 0xA6, 0x0B, 0xDC,
    0xD8, 0x4C, 0xA8, 0x99, 0x27, 0x0F, 0x8F, 0x94, 0x63, 0x0F, 0xB0, 0x11, 0x94, 0xC7, 0xE9, 0x7F,
    0x3B, 0x40, 0x72, 0x4C, 0xDB, 0x84, 0x78, 0xFE, 0xB8, 0x56, 0x08, 0x80, 0xDF, 0x20, 0x2F, 0xB9,
    0x66, 0x2D, 0x60, 0x63, 0xF5, 0x18, 0x15, 0x1B, 0x86, 0x85, 0xB9, 0xB4, 0x68, 0x0E, 0xC6, 0xD1,
    0x8A, 0x81, 0x2B, 0xB3, 0xF6, 0x48, 0xF0, 0x4F, 0x9C, 0x28, 0x1C, 0xA4, 0x51, 0x2F, 0xD7, 0x4B,
    0x17, 0xE7, 0xCC, 0x50, 0x9F, 0xD0, 0xD1, 0x40, 0x0C, 0x0D, 0xCA, 0x83, 0xFA, 0x5E, 0xCA, 0xEC,
    0xBF, 0x4E, 0x7C, 0x8F, 0xF0, 0xAE, 0xC2, 0xD3, 0x28, 0x41, 0x9B, 0xC8, 0x04, 0xB9, 0x4A, 0xBA,
    0x72, 0xE2, 0xB5, 0x06, 0x2C, 0x1E, 0x0B, 0x2C, 0x7F, 0x11, 0xA9, 0x26, 0x51, 0x9D, 0x3F, 0xF8,
    0x62, 0x11, 0x2E, 0x89, 0xD2, 0x9D, 0x35, 0xB1, 0xE4, 0x0A, 0x4D, 0x93, 0x01, 0xA7, 0xD1, 0x2D,
    0x00, 0x87, 0xE2, 0x2D, 0xA4, 0xE9, 0x0A, 0x06, 0x66, 0xF8, 0x1F, 0x44, 0x75, 0xB5, 0x6B, 0x1C,
    0xFC, 0x31, 0x09, 0x48, 0xA3, 0xFF, 0x92, 0x12, 0x58, 0xE9, 0xFA, 0xAE, 0x4F, 0xE2, 0xB4, 0xCC,
];

fn reseed(seeds: &mut [u32; 4], params: u16) {
    let s0 = usize::from(params >> 8);
    let s1 = usize::from(params & 0xFF);
    for y in 0..4 {
        for x in 0..4 {
            let mixed = PAR_T1[(s0 + x) & 0xFF].wrapping_add(PAR_T2[(s1 + y) & 0xFF]);
            seeds[y] = (seeds[y] << 8) | u32::from(mixed);
        }
    }
}

fn par_addr(word: u32) -> u32 {
    (word & 0x000F_FFFF) | ((word << 4) & 0x0F00_0000)
}

fn mask_width(width: u32) -> u32 {
    if width >= 4 {
        u32::MAX
    } else {
        (1u32 << (width * 8)) - 1
    }
}

fn fit_block(len: usize) -> Result<u16, String> {
    u16::try_from(len).map_err(|_| "条件块太长".into())
}

enum Pending {
    Assign { index: usize, button: bool },
    Rom { addr: u32 },
}

struct ActionReplay {
    seeds: [u32; 4],
    ops: Vec<Op>,
    rom: Vec<(u32, u16)>,
    pending: Option<Pending>,
    blocks: Vec<usize>,
}

fn parse_action_replay(pairs: &[(u32, u32)]) -> Result<(Vec<Op>, Vec<(u32, u16)>), String> {
    let mut parser = ActionReplay {
        seeds: PAR_SEEDS,
        ops: Vec::new(),
        rom: Vec::new(),
        pending: None,
        blocks: Vec::new(),
    };
    for (op1, op2) in pairs {
        let (op1, op2) = gameshark_decrypt(*op1, *op2, &parser.seeds);
        parser.line(op1, op2)?;
    }
    parser.finish()
}

impl ActionReplay {
    fn line(&mut self, op1: u32, op2: u32) -> Result<(), String> {
        if self.consume_pending(op1, op2)? {
            return Ok(());
        }
        if op2 == 0x001D_C0DE {
            return Ok(());
        }
        if op1 == 0 {
            return self.special(op2);
        }
        if op1 == 0xDEAD_FACE {
            reseed(&mut self.seeds, op2 as u16);
            return Ok(());
        }
        // 钩子是让游戏自己去跑这条码。这里本来就每帧写，所以只收下这一行。
        if op1 >> 24 == 0xC4 {
            return Ok(());
        }
        if op1 & 0x3800_0000 != 0 {
            return self.condition(op1, op2);
        }
        self.write(op1, op2)
    }

    fn consume_pending(&mut self, op1: u32, op2: u32) -> Result<bool, String> {
        let Some(pending) = self.pending.take() else {
            return Ok(false);
        };
        match pending {
            Pending::Rom { addr } => self.rom.push((addr, op1 as u16)),
            Pending::Assign { index, button } => {
                let width = match self.ops.get(index) {
                    Some(Op::Repeat { width, .. }) => *width,
                    _ => return Err("码的下一行对不上".into()),
                };
                let value = op1 & mask_width(u32::from(width));
                match &mut self.ops[index] {
                    Op::Repeat {
                        value: slot,
                        count,
                        addr_step,
                        value_step,
                        ..
                    } => {
                        *slot = value;
                        if !button {
                            *count = (op2 >> 16) & 0xFF;
                            *addr_step = (op2 & 0xFFFF).wrapping_mul(u32::from(width));
                            *value_step = op2 >> 24;
                        }
                    }
                    _ => return Err("码的下一行对不上".into()),
                }
            }
        }
        Ok(true)
    }

    fn special(&mut self, op2: u32) -> Result<(), String> {
        match op2 & 0xFF00_0000 {
            0x0800_0000 => Err("这版不做减速码".into()),
            0x1000_0000 => {
                self.begin_assign(par_addr(op2), 1, true);
                Ok(())
            }
            0x1200_0000 => {
                self.begin_assign(par_addr(op2), 2, true);
                Ok(())
            }
            0x1400_0000 => {
                self.begin_assign(par_addr(op2), 4, true);
                Ok(())
            }
            0x1800_0000 | 0x1A00_0000 | 0x1C00_0000 | 0x1E00_0000 => {
                let addr = 0x0800_0000 | ((op2 & 0x00FF_FFFF) << 1);
                self.pending = Some(Pending::Rom { addr });
                Ok(())
            }
            0x4000_0000 => self.end_block(),
            0x6000_0000 => self.else_block(),
            0x8000_0000 => {
                self.begin_assign(par_addr(op2), 1, false);
                Ok(())
            }
            0x8200_0000 => {
                self.begin_assign(par_addr(op2), 2, false);
                Ok(())
            }
            0x8400_0000 => {
                self.begin_assign(par_addr(op2), 4, false);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn begin_assign(&mut self, addr: u32, width: u8, button: bool) {
        self.ops.push(Op::Repeat {
            addr,
            value: 0,
            width,
            count: u32::from(button),
            addr_step: 0,
            value_step: 0,
        });
        self.pending = Some(Pending::Assign {
            index: self.ops.len() - 1,
            button,
        });
    }

    fn condition(&mut self, op1: u32, op2: u32) -> Result<(), String> {
        if op1 & 0xC000_0000 == 0xC000_0000 {
            return Err("这版不会按一条码关掉别的码".into());
        }
        let width = 1u32 << ((op1 & 0x0600_0000) >> 25);
        let (kind, width) = if width > 4 {
            (WhenKind::Never, 1)
        } else {
            let kind = match op1 & 0x3800_0000 {
                0x0800_0000 => WhenKind::Eq,
                0x1000_0000 => WhenKind::Ne,
                0x1800_0000 => WhenKind::Lt,
                0x2000_0000 => WhenKind::Gt,
                0x2800_0000 => WhenKind::Ult,
                0x3000_0000 => WhenKind::Ugt,
                0x3800_0000 => WhenKind::And,
                _ => WhenKind::Never,
            };
            (kind, width as u8)
        };
        let then_len = match op1 & 0xC000_0000 {
            0x4000_0000 => 2,
            0x8000_0000 => 0,
            _ => 1,
        };
        self.ops.push(Op::When {
            kind,
            addr: par_addr(op1),
            value: op2 & mask_width(u32::from(width)),
            width,
            then_len,
            else_len: 0,
            saw_else: false,
        });
        if then_len == 0 {
            self.blocks.push(self.ops.len() - 1);
        }
        Ok(())
    }

    fn write(&mut self, op1: u32, op2: u32) -> Result<(), String> {
        if op1 & 0x0100_0000 != 0 && (op1 & 0xFE00_0000) != 0xC600_0000 {
            return Err("这行 v3 码带了不认识的标志".into());
        }
        let base = op1 & 0xC000_0000;
        let width = if base == 0xC000_0000 {
            ((op1 >> 24) & 1) + 1
        } else {
            1u32 << ((op1 & 0x0600_0000) >> 25)
        };
        if width > 4 {
            return Err("这行 v3 码的宽度不认识".into());
        }
        let width = width as u8;
        let value = op2 & mask_width(u32::from(width));
        match base {
            0x0000_0000 => {
                let count = if width < 4 {
                    (op2 >> (u32::from(width) * 8)).wrapping_add(1)
                } else {
                    1
                };
                if count > 4096 {
                    return Err("这行要连续写的次数太多".into());
                }
                self.ops.push(Op::Repeat {
                    addr: par_addr(op1),
                    value,
                    width,
                    count,
                    addr_step: u32::from(width),
                    value_step: 0,
                });
            }
            0x4000_0000 => {
                let offset = if width < 4 {
                    (op2 >> (u32::from(width) * 8)).wrapping_mul(u32::from(width))
                } else {
                    0
                };
                self.ops.push(Op::Indirect {
                    addr: par_addr(op1),
                    value,
                    width,
                    offset,
                });
            }
            0x8000_0000 => self.ops.push(Op::Add {
                addr: par_addr(op1),
                value,
                width,
            }),
            _ => self.ops.push(Op::Repeat {
                addr: 0x0400_0000 | (op1 & 0x00FF_FFFF),
                value,
                width,
                count: 1,
                addr_step: 0,
                value_step: 0,
            }),
        }
        Ok(())
    }

    fn else_block(&mut self) -> Result<(), String> {
        let Some(&at) = self.blocks.last() else {
            return Err("条件块没有开头".into());
        };
        let size = fit_block(self.ops.len() - at - 1)?;
        match &mut self.ops[at] {
            Op::When {
                then_len, saw_else, ..
            } => {
                *then_len = size;
                *saw_else = true;
                Ok(())
            }
            _ => Err("条件块对不上".into()),
        }
    }

    fn end_block(&mut self) -> Result<(), String> {
        let Some(at) = self.blocks.pop() else {
            return Err("条件块没有开头".into());
        };
        close_block(&mut self.ops, at)
    }

    fn finish(mut self) -> Result<(Vec<Op>, Vec<(u32, u16)>), String> {
        if self.pending.is_some() {
            return Err("这组码少了下一行".into());
        }
        while let Some(at) = self.blocks.pop() {
            close_block(&mut self.ops, at)?;
        }
        Ok((self.ops, self.rom))
    }
}

fn close_block(ops: &mut [Op], at: usize) -> Result<(), String> {
    let size = fit_block(ops.len() - at - 1)?;
    match &mut ops[at] {
        Op::When {
            then_len,
            else_len,
            saw_else,
            ..
        } => {
            if *saw_else {
                *else_len = size.saturating_sub(*then_len);
            } else {
                *then_len = size;
            }
            Ok(())
        }
        _ => Err("条件块对不上".into()),
    }
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
        let (hi, lo) = encrypt_gameshark(GS_SEEDS, 0x1200_0000, 0x0000_BEEF);
        let program = parse_cheat(CheatFamily::GameShark, &format!("{hi:08X} {lo:08X}")).unwrap();
        let mut bus = Bus::new();
        program.apply(&mut bus);
        assert_eq!(bus.read_u16(0x0200_0000), 0xBEEF);
    }

    #[test]
    fn action_replay_v3_writes_adds_and_follows_a_pointer() {
        let program = ar(&[
            (0x0020_0010, 0x0000_00AB),
            (0x0420_0000, 0x89AB_CDEF),
            (0x8020_0020, 0x0000_0003),
            (0x4020_0030, 0x0000_005A),
            (0xC700_0008, 0x0000_1234),
        ]);
        let mut bus = Bus::new();
        bus.write_u8(0x0200_0020, 0x10);
        bus.write_u32(0x0200_0030, 0x0200_0040);
        program.apply(&mut bus);
        assert_eq!(bus.read_u8(0x0200_0010), 0xAB);
        assert_eq!(bus.read_u32(0x0200_0000), 0x89AB_CDEF);
        assert_eq!(bus.read_u8(0x0200_0020), 0x13);
        assert_eq!(bus.read_u8(0x0200_0040), 0x5A);
        assert_eq!(bus.read_u16(0x0400_0008), 0x1234);
    }

    #[test]
    fn action_replay_v3_repeats_a_byte_and_fills_from_the_next_line() {
        let program = ar(&[
            (0x0020_0050, 0x0000_02AB),
            (0x0000_0000, 0x8020_0060),
            (0x0000_0011, 0x0004_0002),
            (0x0000_0000, 0x1020_0070),
            (0x0000_0077, 0x0000_0000),
        ]);
        let mut bus = Bus::new();
        program.apply(&mut bus);
        assert_eq!(bus.read_u8(0x0200_0050), 0xAB);
        assert_eq!(bus.read_u8(0x0200_0051), 0xAB);
        assert_eq!(bus.read_u8(0x0200_0052), 0xAB);
        assert_eq!(bus.read_u8(0x0200_0053), 0);
        assert_eq!(bus.read_u8(0x0200_0060), 0x11);
        assert_eq!(bus.read_u8(0x0200_0062), 0x11);
        assert_eq!(bus.read_u8(0x0200_0066), 0x11);
        assert_eq!(bus.read_u8(0x0200_0068), 0);
        assert_eq!(bus.read_u8(0x0200_0070), 0x77);
    }

    #[test]
    fn action_replay_v3_skips_one_line_or_takes_the_else() {
        let skip = ar(&[
            (0x0820_0000, 0x0000_0001),
            (0x0020_0002, 0x0000_0011),
            (0x4820_0000, 0x0000_0001),
            (0x0020_0004, 0x0000_0022),
            (0x0020_0005, 0x0000_0033),
            (0x0020_0006, 0x0000_0044),
        ]);
        let mut bus = Bus::new();
        skip.apply(&mut bus);
        assert_eq!(bus.read_u8(0x0200_0002), 0);
        assert_eq!(bus.read_u8(0x0200_0004), 0);
        assert_eq!(bus.read_u8(0x0200_0005), 0);
        assert_eq!(bus.read_u8(0x0200_0006), 0x44);
        bus.write_u8(0x0200_0000, 1);
        skip.apply(&mut bus);
        assert_eq!(bus.read_u8(0x0200_0002), 0x11);

        let block = ar(&[
            (0x8A20_0000, 0x0000_0001),
            (0x0220_0002, 0x0000_1111),
            (0x0000_0000, 0x6000_0000),
            (0x0220_0004, 0x0000_2222),
            (0x0000_0000, 0x4000_0000),
        ]);
        let mut bus = Bus::new();
        block.apply(&mut bus);
        assert_eq!(bus.read_u16(0x0200_0002), 0);
        assert_eq!(bus.read_u16(0x0200_0004), 0x2222);
        bus.write_u16(0x0200_0004, 0);
        bus.write_u16(0x0200_0000, 1);
        block.apply(&mut bus);
        assert_eq!(bus.read_u16(0x0200_0002), 0x1111);
        assert_eq!(bus.read_u16(0x0200_0004), 0);
    }

    #[test]
    fn action_replay_v3_patches_a_rom_half_and_reseed_changes_the_next_line() {
        let program = ar(&[
            (0x0000_0000, 0x1A00_0080),
            (0x0000_BEEF, 0x0000_0000),
            (0xC400_0100, 0x0000_0000),
            (0x0020_0080, 0x001D_C0DE),
            (0x0020_0080, 0x0000_003C),
        ]);
        let mut bus = Bus::new();
        program.apply(&mut bus);
        assert_eq!(bus.read_u16(0x0800_0100), 0xBEEF);
        assert_eq!(bus.read_u8(0x0200_0080), 0x3C);
        bus.clear_cheat_rom();
        assert_eq!(bus.read_u16(0x0800_0100), 0);

        let mut seeds = PAR_SEEDS;
        reseed(&mut seeds, 0x1234);
        assert_eq!(
            seeds,
            [0x6D09_D152, 0x8D29_F172, 0xC05C_24A5, 0x9E3A_0283]
        );
        let (face_hi, face_lo) = encrypt_gameshark(PAR_SEEDS, 0xDEAD_FACE, 0x0000_1234);
        let (write_hi, write_lo) = encrypt_gameshark(seeds, 0x0020_0090, 0x0000_004D);
        let text = format!("{face_hi:08X} {face_lo:08X} {write_hi:08X} {write_lo:08X}");
        let program = parse_cheat(CheatFamily::ActionReplay, &text).unwrap();
        program.apply(&mut bus);
        assert_eq!(bus.read_u8(0x0200_0090), 0x4D);
    }

    #[test]
    fn action_replay_v3_rejects_a_huge_repeat_and_a_slowdown() {
        let huge = encrypt_gameshark(PAR_SEEDS, 0x0020_0000, 0x0010_0000);
        let text = format!("{:08X} {:08X}", huge.0, huge.1);
        let error = parse_cheat(CheatFamily::ActionReplay, &text).unwrap_err();
        assert!(error.contains("太多"), "{error}");
        let slow = encrypt_gameshark(PAR_SEEDS, 0, 0x0800_0000);
        let text = format!("{:08X} {:08X}", slow.0, slow.1);
        let error = parse_cheat(CheatFamily::ActionReplay, &text).unwrap_err();
        assert!(error.contains("减速"), "{error}");
        let disable = encrypt_gameshark(PAR_SEEDS, 0xC820_0000, 1);
        let text = format!("{:08X} {:08X}", disable.0, disable.1);
        let error = parse_cheat(CheatFamily::ActionReplay, &text).unwrap_err();
        assert!(error.contains("关掉"), "{error}");
        let open = encrypt_gameshark(PAR_SEEDS, 0, 0x4000_0000);
        let text = format!("{:08X} {:08X}", open.0, open.1);
        let error = parse_cheat(CheatFamily::ActionReplay, &text).unwrap_err();
        assert!(error.contains("没有开头"), "{error}");
        let dangling = encrypt_gameshark(PAR_SEEDS, 0, 0x8020_0000);
        let text = format!("{:08X} {:08X}", dangling.0, dangling.1);
        let error = parse_cheat(CheatFamily::ActionReplay, &text).unwrap_err();
        assert!(error.contains("少了下一行"), "{error}");
    }

    #[test]
    fn action_replay_v3_signed_compare_differs_from_unsigned_and_blocks_nest() {
        let program = ar(&[
            (0x1C20_0000, 0),
            (0x0020_0010, 0x0000_0011),
            (0x2C20_0000, 0),
            (0x0020_0011, 0x0000_0022),
            (0x2420_0000, 0),
            (0x0020_0012, 0x0000_0033),
            (0x3820_0000, 0x0000_0001),
            (0x0020_0013, 0x0000_0044),
            (0x1020_0001, 0),
            (0x0020_0014, 0x0000_0055),
        ]);
        let mut bus = Bus::new();
        bus.write_u32(0x0200_0000, 0xFFFF_FFFF);
        bus.write_u8(0x0200_0001, 0x0F);
        program.apply(&mut bus);
        assert_eq!(bus.read_u8(0x0200_0010), 0x11);
        assert_eq!(bus.read_u8(0x0200_0011), 0);
        assert_eq!(bus.read_u8(0x0200_0012), 0);
        assert_eq!(bus.read_u8(0x0200_0013), 0x44);
        assert_eq!(bus.read_u8(0x0200_0014), 0x55);

        let nested = ar(&[
            (0x8820_0000, 1),
            (0x0020_0020, 0x0000_00AA),
            (0x8820_0001, 2),
            (0x0020_0021, 0x0000_00BB),
            (0, 0x4000_0000),
            (0x0020_0022, 0x0000_00CC),
            (0, 0x4000_0000),
            (0x0020_0023, 0x0000_00DD),
        ]);
        let mut bus = Bus::new();
        nested.apply(&mut bus);
        assert_eq!(bus.read_u8(0x0200_0020), 0);
        assert_eq!(bus.read_u8(0x0200_0021), 0);
        assert_eq!(bus.read_u8(0x0200_0022), 0);
        assert_eq!(bus.read_u8(0x0200_0023), 0xDD);
        bus.write_u8(0x0200_0000, 1);
        nested.apply(&mut bus);
        assert_eq!(bus.read_u8(0x0200_0020), 0xAA);
        assert_eq!(bus.read_u8(0x0200_0021), 0);
        assert_eq!(bus.read_u8(0x0200_0022), 0xCC);
        bus.write_u8(0x0200_0001, 2);
        bus.write_u8(0x0200_0021, 0);
        nested.apply(&mut bus);
        assert_eq!(bus.read_u8(0x0200_0021), 0xBB);

        let empty_then = ar(&[
            (0x8820_0000, 1),
            (0, 0x6000_0000),
            (0x0020_0030, 0x0000_0066),
            (0, 0x4000_0000),
        ]);
        let mut bus = Bus::new();
        empty_then.apply(&mut bus);
        assert_eq!(bus.read_u8(0x0200_0030), 0x66);
        bus.write_u8(0x0200_0030, 0);
        bus.write_u8(0x0200_0000, 1);
        empty_then.apply(&mut bus);
        assert_eq!(bus.read_u8(0x0200_0030), 0);

        let open_block = ar(&[(0x8820_0000, 1), (0x0020_0031, 0x0000_0055)]);
        let mut bus = Bus::new();
        open_block.apply(&mut bus);
        assert_eq!(bus.read_u8(0x0200_0031), 0);
        bus.write_u8(0x0200_0000, 1);
        open_block.apply(&mut bus);
        assert_eq!(bus.read_u8(0x0200_0031), 0x55);
    }

    #[test]
    fn action_replay_v3_offsets_a_pointer_and_steps_a_fill() {
        let program = ar(&[
            (0x4020_00A0, 0x0000_04AB),
            (0, 0x8220_0080),
            (0x0000_1234, 0x0002_0001),
            (0, 0x8020_0090),
            (0x0000_0010, 0x0103_0001),
        ]);
        let mut bus = Bus::new();
        bus.write_u32(0x0200_00A0, 0x0200_0100);
        program.apply(&mut bus);
        assert_eq!(bus.read_u8(0x0200_0104), 0xAB);
        assert_eq!(bus.read_u8(0x0200_0100), 0);
        assert_eq!(bus.read_u16(0x0200_0080), 0x1234);
        assert_eq!(bus.read_u16(0x0200_0082), 0x1234);
        assert_eq!(bus.read_u16(0x0200_0084), 0);
        assert_eq!(bus.read_u8(0x0200_0090), 0x10);
        assert_eq!(bus.read_u8(0x0200_0091), 0x11);
        assert_eq!(bus.read_u8(0x0200_0092), 0x12);
        assert_eq!(bus.read_u8(0x0200_0093), 0);
    }

    #[test]
    fn action_replay_v3_patch_is_visible_through_the_rom_mirror() {
        let program = ar(&[(0, 0x1A00_0080), (0x0000_BEEF, 0)]);
        let mut bus = Bus::new();
        program.apply(&mut bus);
        assert_eq!(bus.read_u16(0x0A00_0100), 0xBEEF);
        let encrypted = {
            let (op1, op2) = encrypt_gameshark(PAR_SEEDS, 0x0020_0010, 0xAB);
            format!("{op1:08X} {op2:08X}")
        };
        let as_v1 = parse_cheat(CheatFamily::GameShark, &encrypted).ok();
        let mut bus = Bus::new();
        if let Some(program) = as_v1 {
            program.apply(&mut bus);
        }
        assert_eq!(bus.read_u8(0x0200_0010), 0);
    }

    #[test]
    fn formatting_groups_each_line_and_a_short_line_says_what_is_missing() {
        assert_eq!(
            format_cheat(CheatFamily::CodeBreaker, "8200000003e7 32000000 00ab"),
            "82000000 03E7\n32000000 00AB"
        );
        assert_eq!(
            format_cheat(CheatFamily::CodeBreaker, "8200000003"),
            "82000000 03"
        );
        assert_eq!(
            format_cheat(CheatFamily::GameShark, "12 000000 beefabcd"),
            "12000000 BEEFABCD"
        );
        assert_eq!(
            read_cheat(CheatFamily::CodeBreaker, "8200000003"),
            CheatReading::Short { missing: 2 }
        );
        assert_eq!(read_cheat(CheatFamily::CodeBreaker, "  "), CheatReading::Empty);
    }

    #[test]
    fn a_code_reads_as_the_memory_it_changes() {
        assert_eq!(
            read_cheat(CheatFamily::CodeBreaker, "32000000 00AB\n82000002 1234"),
            CheatReading::Lines(vec![
                "把 02000000 的字节写成 AB".into(),
                "把 02000002 的半字写成 1234".into(),
            ])
        );
        assert_eq!(
            read_cheat(CheatFamily::CodeBreaker, "72000000 0001\nD0000020 0001"),
            CheatReading::Lines(vec![
                "若 02000000 的半字等于 0001，才做下一条".into(),
                "按着 A 时才做下一条".into(),
            ])
        );
        let (hi, lo) = encrypt_gameshark(GS_SEEDS, 0x1200_0000, 0x0000_BEEF);
        assert_eq!(
            read_cheat(CheatFamily::GameShark, &format!("{hi:08X}{lo:08X}")),
            CheatReading::Lines(vec!["把 02000000 的半字写成 BEEF".into()])
        );
    }

    fn ar(pairs: &[(u32, u32)]) -> CheatProgram {
        let text = pairs
            .iter()
            .map(|(op1, op2)| {
                let (op1, op2) = encrypt_gameshark(PAR_SEEDS, *op1, *op2);
                format!("{op1:08X} {op2:08X}")
            })
            .collect::<Vec<_>>()
            .join(" ");
        parse_cheat(CheatFamily::ActionReplay, &text).unwrap()
    }

    fn encrypt_gameshark(seeds: [u32; 4], mut op1: u32, mut op2: u32) -> (u32, u32) {
        let mut sum = 0u32;
        for _ in 0..32 {
            sum = sum.wrapping_add(0x9E37_79B9);
            op1 = op1.wrapping_add(
                op2.wrapping_shl(4)
                    .wrapping_add(seeds[0])
                    ^ op2.wrapping_add(sum)
                    ^ op2.wrapping_shr(5).wrapping_add(seeds[1]),
            );
            op2 = op2.wrapping_add(
                op1.wrapping_shl(4)
                    .wrapping_add(seeds[2])
                    ^ op1.wrapping_add(sum)
                    ^ op1.wrapping_shr(5).wrapping_add(seeds[3]),
            );
        }
        (op1, op2)
    }
}
