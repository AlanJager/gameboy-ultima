//! 单独测：跑 N 帧模拟要多久（不含 sleep / 窗口）。
use gameboy_ultima::{step, Bus, Registers, CYCLES_PER_FRAME};
use std::env;
use std::fs;
use std::time::Instant;

fn main() {
    let path = env::args().nth(1).expect("rom");
    let frames: u64 = env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(120);
    let rom = fs::read(&path).expect("rom");
    let mut bus = Bus::new();
    bus.load_rom(&rom);
    let mut regs = Registers::arm_start(0x0800_0000);
    regs.set_boot_stacks();
    // warmup a bit
    for _ in 0..30 {
        let end = bus.cycles().saturating_add(CYCLES_PER_FRAME as u64);
        while bus.cycles() < end {
            let _ = step(&mut regs, &mut bus);
        }
    }
    let t0 = Instant::now();
    for _ in 0..frames {
        let end = bus.cycles().saturating_add(CYCLES_PER_FRAME as u64);
        while bus.cycles() < end {
            step(&mut regs, &mut bus).expect("step");
        }
    }
    let span = t0.elapsed();
    let secs = span.as_secs_f64();
    println!(
        "纯模拟 {frames} 帧，用时 {secs:.3}s，约 {:.1} FPS（不含 sleep/窗口）",
        frames as f64 / secs
    );
    println!("公式自检：若 span=1.0s、frames=60 → {:.1}", 60.0f64 / 1.0);
}
