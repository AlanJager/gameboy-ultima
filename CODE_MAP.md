# 代码地图

## 执行核心

| 路径 | 职责 |
| --- | --- |
| `src/lib.rs` | 声明模拟器模块并导出 CPU、总线、寄存器、PPU 和音频 API |
| `src/cpu.rs` | ARM/Thumb 单步执行、异常处理和指令周期核算 |
| `src/registers.rs` | ARM 模式寄存器组、CPSR/SPSR 和 PC 状态 |
| `src/disasm.rs` | ARM 解码及文本反汇编；CPU 执行和调试共用解码结构 |
| `src/thumb.rs` | Thumb 解码、反汇编和执行 |
| `src/asm.rs` | ARM 机器码编码辅助函数 |
| `src/condition.rs` | ARM 条件码 |
| `src/decode.rs` | 独立的 ARM 指令字段解码 API |

## GBA 硬件

| 路径 | 职责 |
| --- | --- |
| `src/bus.rs` | 地址映射、ROM/RAM/I/O、周期推进和硬件协调 |
| `src/ppu.rs` | 扫描位置、逐行渲染、图层优先级和帧缓存 |
| `src/gfxcache.rs` | VRAM 图块和 OAM 精灵缓存 |
| `src/apu.rs` | GBA 音源和 Direct Sound FIFO |
| `src/sound.rs`, `src/ring.rs` | 音频采样、播放、WAV 输出和有界队列 |
| `src/dma.rs`, `src/timer.rs`, `src/keypad.rs` | DMA、定时器和按键输入 |
| `src/wait.rs`, `src/sched.rs` | 总线等待周期及按周期调度硬件事件 |
| `src/backup.rs` | SRAM、Flash、EEPROM 存档识别和读写 |
| `src/bios.rs` | BIOS SWI 与异常服务 |
| `src/icache.rs` | ARM/Thumb 解码缓存 |

## 前端、示例与测试

| 路径 | 职责 |
| --- | --- |
| `src/main.rs`, `src/bin/gpui.rs` | GPUI 默认前端；模拟器后台线程、菜单、ROM 和即时存档 |
| `src/lesson.rs` | 程序化生成演示 ROM |
| `tests/arm.rs`, `tests/thumb.rs` | 使用 JSON 测试 ARM/Thumb 指令状态 |
| `tests/arm_gba.rs`, `tests/thumb_gba.rs`, `tests/memory_gba.rs` | ARM、Thumb 和内存集成 ROM 测试 |
| `data/` | 单步指令测试输入和预期状态 |
| `roms/` | MIT 许可的模拟器测试 ROM |
| `examples/` | 帧率基准和卡顿探测工具 |

运行方式、当前覆盖范围和硬件行为见 [README.md](./README.md) 与 [doc/hardware.md](./doc/hardware.md)。
