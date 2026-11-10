//! 暂停时看的调试面。断点对齐到指令地址，命中后这一条先不执行。

use crate::bus::Bus;
use crate::disasm::disassemble;
use crate::thumb::disassemble_thumb;

pub const BREAK_CAP: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugLine {
    pub address: u32,
    pub text: String,
    pub here: bool,
    pub breakpoint: bool,
}

pub fn normalize_break(pc: u32) -> u32 {
    pc & !1
}

pub fn toggle_break(stops: &mut Vec<u32>, pc: u32) {
    let pc = normalize_break(pc);
    if let Some(index) = stops.iter().position(|slot| *slot == pc) {
        stops.remove(index);
        return;
    }
    if stops.len() == BREAK_CAP {
        stops.remove(0);
    }
    stops.push(pc);
}

/// 停在这条上之后，下一次从同一条继续时不再马上停下。
pub fn should_stop(stops: &[u32], pc: u32, skip: Option<u32>) -> bool {
    let pc = normalize_break(pc);
    stops.iter().any(|slot| *slot == pc) && skip != Some(pc)
}

pub fn format_memory(base: u32, bytes: &[u8]) -> Vec<String> {
    bytes
        .chunks(8)
        .enumerate()
        .map(|(row, chunk)| {
            let addr = base.wrapping_add((row * 8) as u32);
            let hex = chunk
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect::<Vec<_>>()
                .join(" ");
            format!("{addr:08X}  {hex}")
        })
        .collect()
}

pub fn level_text(levels: [u8; 4]) -> String {
    format!(
        "方波 1  {}    方波 2  {}    波形  {}    噪声  {}",
        levels[0], levels[1], levels[2], levels[3]
    )
}

pub fn list_around(bus: &Bus, pc: u32, thumb: bool, stops: &[u32]) -> Vec<DebugLine> {
    let pc = if thumb { pc & !1 } else { pc & !3 };
    let width = if thumb { 2u32 } else { 4 };
    let start = pc.wrapping_sub(width * 4);
    (0..12)
        .map(|index| {
            let address = start.wrapping_add(width * index);
            let text = if thumb {
                disassemble_thumb(address, bus.read_u16(address)).text
            } else {
                disassemble(address, bus.read_u32(address)).text
            };
            DebugLine {
                address,
                text,
                here: address == pc,
                breakpoint: stops.contains(&normalize_break(address)),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggling_a_breakpoint_adds_then_removes_the_aligned_address() {
        let mut stops = Vec::new();
        toggle_break(&mut stops, 0x0800_0001);
        assert_eq!(stops, vec![0x0800_0000]);
        assert!(should_stop(&stops, 0x0800_0000, None));
        assert!(!should_stop(&stops, 0x0800_0000, Some(0x0800_0000)));
        toggle_break(&mut stops, 0x0800_0000);
        assert!(stops.is_empty());
    }

    #[test]
    fn memory_window_is_eight_bytes_per_line() {
        let bytes: Vec<u8> = (0..16).collect();
        assert_eq!(
            format_memory(0x0200_0000, &bytes),
            vec![
                "02000000  00 01 02 03 04 05 06 07".to_string(),
                "02000008  08 09 0A 0B 0C 0D 0E 0F".to_string(),
            ]
        );
    }

    #[test]
    fn the_current_instruction_is_marked_and_a_square_reports_its_volume() {
        let mut bus = Bus::new();
        bus.write_u32(0x0200_0000, 0xE3A0_0000);
        let lines = list_around(&bus, 0x0200_0000, false, &[0x0200_0000]);
        let here = lines.iter().find(|line| line.here).expect("current line");
        assert_eq!(here.address, 0x0200_0000);
        assert!(here.breakpoint);
        assert!(here.text.contains("mov"));

        bus.write_u16(0x0400_0084, 0x0080);
        bus.write_u16(0x0400_0080, (1 << 9) | 0x0077);
        bus.write_u16(0x0400_0068, 0xF180);
        bus.write_u16(0x0400_006C, 0x8000 | 2047);
        assert_eq!(bus.channel_levels()[1], 15);
        assert_eq!(bus.channel_levels()[0], 0);
        assert!(level_text(bus.channel_levels()).contains("方波 2  15"));
    }
}
