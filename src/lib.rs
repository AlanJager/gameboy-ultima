//! ARM7TDMI，ARM 和 Thumb 两套指令。模式跟着 CPSR 走，分组寄存器留在原处。
//! 加上 GBA 模式 3 的一帧画面、四路音源、两路直接声、四路 DMA、四路定时器、按键和 PWM 采样。

mod apu;
pub mod asm;
mod backup;
mod bios;
mod bus;
pub mod condition;
mod cpu;
pub mod decode;
mod disasm;
mod dma;
mod gfxcache;
mod icache;
mod keypad;
mod music;
mod lesson;
mod ppu;
mod registers;
mod rtc;
mod ring;
mod sched;
mod serde_array;
pub mod link;
#[cfg(test)]
mod link_rom;
mod sio;
mod wireless;
mod sound;
mod thumb;
mod timer;
mod wait;

pub use bus::Bus;
pub use cpu::{run_until_self_branch, step, step_quiet, Stepped};
pub use disasm::{disassemble, Disassembly};
pub use lesson::{
    demo_rom, mosaic_sprite_rom, rotate_sprite_rom, run_demo, semi_sprite_rom, window_rom,
};
pub use ppu::{
    finished_lines, front_color, render_frame, rgb555_to_888, scan_at, Frame, Layer, LayerKind,
    Ppu, ScanPos, CYCLES_PER_FRAME, CYCLES_PER_LINE, HEIGHT, WIDTH,
};
pub use registers::Registers;
pub use sound::{audio_pace, sample_hz, wav_bytes, AudioPlay, Sample};
pub use thumb::disassemble_thumb;
