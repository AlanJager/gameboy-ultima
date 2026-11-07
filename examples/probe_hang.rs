//! 临时探头：跑 N 步，每段打印 PC/mode/T/IME/IE/IF。
use gameboy_ultima::{step, Bus, Registers, CYCLES_PER_FRAME};
use std::env;
use std::fs;

fn main() {
    let path = env::args().nth(1).expect("rom path");
    let steps: u64 = env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(200_000);
    let rom = fs::read(&path).expect("rom");
    let mut bus = Bus::new();
    bus.load_rom(&rom);
    let mut regs = Registers::arm_start(0x0800_0000);
    regs.set_boot_stacks();
    let mut n = 0u64;
    let mut last_frame = 0u64;
    let report_every = steps / 20;
    while n < steps {
        match step(&mut regs, &mut bus) {
            Ok(_) => {}
            Err(e) => {
                eprintln!("step#{n} ERR {e}");
                break;
            }
        }
        n += 1;
        let frames = bus.cycles() / CYCLES_PER_FRAME as u64;
        if n == 1 || n % report_every.max(1) == 0 || frames != last_frame && frames % 30 == 0 {
            last_frame = frames;
            let ie = bus.read_u16(0x0400_0200);
            let iff = bus.read_u16(0x0400_0202);
            let ime = bus.read_u16(0x0400_0208);
            println!(
                "n={n} frames={frames} pc=0x{:08X} cpsr=0x{:08X} mode=0x{:02X} T={} IME={ime:04X} IE={ie:04X} IF={iff:04X} lr=0x{:08X}",
                regs.pc(),
                regs.cpsr(),
                regs.mode(),
                regs.thumb() as u8,
                regs.get(14),
            );
        }
    }
    println!("done n={n} cycles={} pc=0x{:08X}", bus.cycles(), regs.pc());
}
