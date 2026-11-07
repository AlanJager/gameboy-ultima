//! jsmolka/gba-tests 的 `arm.gba`。MIT。全部通过时 `r12` 是 0，然后停在原地跳转。

use gameboy_ultima::{run_until_self_branch, Bus, Registers};

const ROM: &[u8] = include_bytes!("../roms/arm.gba");

#[test]
fn arm_gba_passes() {
    let mut bus = Bus::new();
    bus.load_rom(ROM);
    let mut regs = Registers::arm_start(0x0800_0000);
    regs.set_boot_stacks();
    let steps = run_until_self_branch(&mut regs, &mut bus, 500_000).unwrap();
    assert!(steps < 500_000);
    assert_eq!(regs.get(12), 0, "第一个失败的测试编号");
    let ink = (0..240 * 160)
        .filter(|index| bus.read_u8(0x0600_0000 + *index) != 0)
        .count();
    assert!(ink > 100, "模式 4 上应该写下通过的字，非零字节只有 {ink}");
}
