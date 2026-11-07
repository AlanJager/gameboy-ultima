//! 一条 ARM 指令的初态和终态。
//!
//! `data/arm` 里每个操作一个文件。整组用 `cargo test --test arm`。
//! 只看一种操作：`cargo test --test arm op_sub`。

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use gameboy_ultima::{step, Bus, Registers};
use serde::Deserialize;

const SAMPLE_FILES: &[&str] = &[
    "adc.json",
    "add.json",
    "and.json",
    "b.json",
    "bic.json",
    "bx.json",
    "cmn.json",
    "cmp.json",
    "cond.json",
    "eor.json",
    "ldm.json",
    "ldr.json",
    "ldrh.json",
    "mov.json",
    "mrs.json",
    "msr.json",
    "mul.json",
    "mvn.json",
    "orr.json",
    "rsb.json",
    "rsc.json",
    "sbc.json",
    "shift.json",
    "stm.json",
    "str.json",
    "strh.json",
    "sub.json",
    "swi.json",
    "swp.json",
    "teq.json",
    "thumb.json",
    "tst.json",
    "unknown.json",
];

#[derive(Deserialize)]
struct Case {
    name: String,
    word: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    cycles: Option<u32>,
    #[serde(default)]
    error: Option<String>,
    initial: State,
    #[serde(rename = "final")]
    end: State,
}

#[derive(Deserialize)]
struct State {
    pc: String,
    cpsr: String,
    #[serde(default)]
    gpr: BTreeMap<String, String>,
    #[serde(default)]
    ram: Vec<[String; 2]>,
}

#[test]
fn every_sample_file_has_a_test() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/arm");
    let mut found = Vec::new();
    for entry in fs::read_dir(&dir).unwrap_or_else(|err| panic!("读不到 {}：{err}", dir.display()))
    {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "json") {
            found.push(path.file_name().unwrap().to_string_lossy().into_owned());
        }
    }
    found.sort();
    let listed: Vec<String> = SAMPLE_FILES
        .iter()
        .map(|file| (*file).to_string())
        .collect();
    assert_eq!(found, listed, "data/arm 里的文件和下面的测试不一致");
}

#[test]
fn op_adc() {
    run_sample("adc.json");
}
#[test]
fn op_add() {
    run_sample("add.json");
}
#[test]
fn op_and() {
    run_sample("and.json");
}
#[test]
fn op_b() {
    run_sample("b.json");
}
#[test]
fn op_bic() {
    run_sample("bic.json");
}
#[test]
fn op_bx() {
    run_sample("bx.json");
}
#[test]
fn op_cmp() {
    run_sample("cmp.json");
}
#[test]
fn op_cmn() {
    run_sample("cmn.json");
}
#[test]
fn op_cond() {
    run_sample("cond.json");
}
#[test]
fn op_eor() {
    run_sample("eor.json");
}
#[test]
fn op_ldr() {
    run_sample("ldr.json");
}
#[test]
fn op_ldm() {
    run_sample("ldm.json");
}
#[test]
fn op_ldrh() {
    run_sample("ldrh.json");
}
#[test]
fn op_mrs() {
    run_sample("mrs.json");
}
#[test]
fn op_msr() {
    run_sample("msr.json");
}
#[test]
fn op_mov() {
    run_sample("mov.json");
}
#[test]
fn op_mul() {
    run_sample("mul.json");
}
#[test]
fn op_mvn() {
    run_sample("mvn.json");
}
#[test]
fn op_orr() {
    run_sample("orr.json");
}
#[test]
fn op_rsb() {
    run_sample("rsb.json");
}
#[test]
fn op_rsc() {
    run_sample("rsc.json");
}
#[test]
fn op_sbc() {
    run_sample("sbc.json");
}
#[test]
fn op_shift() {
    run_sample("shift.json");
}
#[test]
fn op_str() {
    run_sample("str.json");
}
#[test]
fn op_stm() {
    run_sample("stm.json");
}
#[test]
fn op_strh() {
    run_sample("strh.json");
}
#[test]
fn op_swi() {
    run_sample("swi.json");
}
#[test]
fn op_swp() {
    run_sample("swp.json");
}
#[test]
fn op_sub() {
    run_sample("sub.json");
}
#[test]
fn op_teq() {
    run_sample("teq.json");
}
#[test]
fn op_thumb() {
    run_sample("thumb.json");
}
#[test]
fn op_tst() {
    run_sample("tst.json");
}
#[test]
fn op_unknown() {
    run_sample("unknown.json");
}

fn run_sample(file: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("data/arm")
        .join(file);
    let text =
        fs::read_to_string(&path).unwrap_or_else(|err| panic!("读不到 {}：{err}", path.display()));
    let cases: Vec<Case> = serde_json::from_str(&text)
        .unwrap_or_else(|err| panic!("{} 不是用例：{err}", path.display()));
    assert!(!cases.is_empty(), "{} 里面没有用例", path.display());
    for case in &cases {
        if let Err(diff) = run_case(case) {
            panic!("{} {}：{diff}", path.display(), case.name);
        }
    }
}

fn run_case(case: &Case) -> Result<(), String> {
    let word = parse_u32(&case.word)?;
    let start_pc = parse_u32(&case.initial.pc)?;
    let start_cpsr = parse_u32(&case.initial.cpsr)?;
    let start_gpr = gprs(&case.initial.gpr)?;
    let start_ram = ram(&case.initial.ram)?;

    let mut bus = Bus::new();
    let mut rom = Vec::new();
    for (&addr, &value) in &start_ram {
        write_mem(&mut bus, &mut rom, addr, value);
    }
    for (index, byte) in word.to_le_bytes().into_iter().enumerate() {
        write_mem(&mut bus, &mut rom, start_pc + index as u32, byte);
    }

    let mut regs = Registers::arm_start(start_pc);
    regs.set_cpsr(start_cpsr);
    let mut initial = [0u32; 15];
    for index in 0..15 {
        initial[index] = start_gpr[index].unwrap_or(0);
        regs.set(index as u8, initial[index]);
    }

    let stepped = step(&mut regs, &mut bus);
    match (&case.error, stepped) {
        (Some(needle), Err(err)) => {
            if !err.contains(needle) {
                return Err(format!("错误信息是「{err}」，里面没有「{needle}」"));
            }
        }
        (Some(_), Ok(_)) => return Err("这条用例应该停住，step 却成功了".to_string()),
        (None, Err(err)) => return Err(format!("step 停住了：{err}")),
        (None, Ok(stepped)) => {
            let text = case.text.as_deref().ok_or("成功的用例要写 text")?;
            if stepped.text != text {
                return Err(format!("译成了「{}」，用例写的是「{text}」", stepped.text));
            }
            let cycles = case.cycles.ok_or("成功的用例要写 cycles")?;
            if stepped.cycles != cycles {
                return Err(format!("周期是 {}，用例写的是 {cycles}", stepped.cycles));
            }
        }
    }

    let end_pc = parse_u32(&case.end.pc)?;
    let end_cpsr = parse_u32(&case.end.cpsr)?;
    let end_gpr = gprs(&case.end.gpr)?;
    if regs.pc() != end_pc {
        return Err(format!(
            "PC 是 0x{:08X}，用例写的是 0x{end_pc:08X}",
            regs.pc()
        ));
    }
    if regs.cpsr() != end_cpsr {
        return Err(format!(
            "CPSR 是 0x{:08X}（NZCV {}），用例写的是 0x{end_cpsr:08X}",
            regs.cpsr(),
            nzcv(regs.cpsr())
        ));
    }
    for index in 0..15 {
        let actual = regs.get(index as u8);
        let expected = end_gpr[index].unwrap_or(initial[index]);
        if actual != expected {
            return Err(format!(
                "r{index} 是 0x{actual:08X}，用例写的是 0x{expected:08X}"
            ));
        }
    }

    let mut expect_mem = start_ram;
    for (index, byte) in word.to_le_bytes().into_iter().enumerate() {
        expect_mem.entry(start_pc + index as u32).or_insert(byte);
    }
    for (addr, value) in ram(&case.end.ram)? {
        expect_mem.insert(addr, value);
    }
    check_memory(&bus, &rom, &expect_mem)
}

fn gprs(raw: &BTreeMap<String, String>) -> Result<[Option<u32>; 15], String> {
    let mut out = [None; 15];
    for (name, value) in raw {
        let index = reg_index(name)?;
        if index == 15 {
            return Err("PC 写在 pc 字段里".to_string());
        }
        out[index as usize] = Some(parse_u32(value)?);
    }
    Ok(out)
}

fn ram(raw: &[[String; 2]]) -> Result<BTreeMap<u32, u8>, String> {
    let mut out = BTreeMap::new();
    for pair in raw {
        let addr = parse_u32(&pair[0])?;
        let value = parse_u32(&pair[1])?;
        if value > 0xFF {
            return Err(format!("内存 0x{addr:08X} 的值 0x{value:X} 超过一个字节"));
        }
        out.insert(addr, value as u8);
    }
    Ok(out)
}

fn reg_index(name: &str) -> Result<u8, String> {
    match name {
        "sp" => Ok(13),
        "lr" => Ok(14),
        "pc" => Ok(15),
        other => {
            let number = other
                .strip_prefix('r')
                .and_then(|rest| rest.parse::<u8>().ok())
                .filter(|index| *index < 16);
            number.ok_or_else(|| format!("不认识寄存器 {name}"))
        }
    }
}

fn parse_u32(text: &str) -> Result<u32, String> {
    let hex = text
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    u32::from_str_radix(hex, 16).map_err(|_| format!("「{text}」不是十六进制数"))
}

fn write_mem(bus: &mut Bus, rom: &mut Vec<u8>, addr: u32, value: u8) {
    if (0x0800_0000..0x0E00_0000).contains(&addr) {
        let off = (addr & 0x01FF_FFFF) as usize;
        if rom.len() <= off {
            rom.resize(off + 1, 0);
        }
        rom[off] = value;
        bus.load_rom(rom);
    } else {
        bus.write_u8(addr, value);
    }
}

fn check_memory(bus: &Bus, rom: &[u8], expect: &BTreeMap<u32, u8>) -> Result<(), String> {
    for (&addr, &value) in expect {
        let actual = bus.read_u8(addr);
        if actual != value {
            return Err(format!(
                "内存 0x{addr:08X} 是 0x{actual:02X}，用例写的是 0x{value:02X}"
            ));
        }
    }
    for (base, len) in [
        (0x0200_0000, 256 * 1024),
        (0x0300_0000, 32 * 1024),
        (0x0400_0000, 0x400),
        (0x0500_0000, 0x400),
        (0x0600_0000, 0x18000),
    ] {
        for offset in 0..len {
            let addr = base + offset as u32;
            let actual = bus.read_u8(addr);
            let expected = expect.get(&addr).copied().unwrap_or(0);
            // DISPSTAT 的低 3 位、VCOUNT 随周期变。按键松开时低 10 位是 1。
            // 用例没写它们时，不拿读数和 0 比。
            if !expect.contains_key(&addr)
                && matches!(addr, 0x0400_0004 | 0x0400_0006 | 0x0400_0130 | 0x0400_0131)
            {
                if addr == 0x0400_0004 && actual & 0x38 != 0 {
                    return Err(format!(
                        "DISPSTAT 的中断允许位是 0x{actual:02X}，用例没有写过"
                    ));
                }
                continue;
            }
            if actual != expected {
                return Err(format!(
                    "内存 0x{addr:08X} 是 0x{actual:02X}，不在用例里，应该保持 0x{expected:02X}"
                ));
            }
        }
    }
    for (offset, original) in rom.iter().enumerate() {
        let addr = 0x0800_0000 + offset as u32;
        let actual = bus.read_u8(addr);
        let expected = expect.get(&addr).copied().unwrap_or(*original);
        if actual != expected {
            return Err(format!(
                "卡带 0x{addr:08X} 是 0x{actual:02X}，用例写的是 0x{expected:02X}"
            ));
        }
    }
    Ok(())
}

fn nzcv(cpsr: u32) -> String {
    format!(
        "{}{}{}{}",
        u8::from(cpsr & 0x8000_0000 != 0),
        u8::from(cpsr & 0x4000_0000 != 0),
        u8::from(cpsr & 0x2000_0000 != 0),
        u8::from(cpsr & 0x1000_0000 != 0)
    )
}
