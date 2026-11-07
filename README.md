# gameboy-ultima

用 Rust 写的 Game Boy Advance 模拟器。CPU 按 ARM7TDMI 逐条解释执行，画面、声音、DMA 和定时器跟它共用 16.777216 MHz 的周期。窗口在另一条线程里取已经画好的帧和采样，不把模拟停下来等喇叭。

现在可以载入卡带、玩到自己的进度，并把进度存下来。它还不是一台能保证跑完商业游戏的机器：没有放入任天堂的 16KB BIOS，联机和一部分边角硬件也还没有。

## 已经有的

- ARM 与 Thumb，含条件码、异常和 IRQ
- 背景模式 0–5，精灵、旋转缩放、窗口、混合和马赛克
- 四路 Game Boy 音源，以及两路 Direct Sound
- 四路 DMA、四路定时器、卡带等待周期和预取
- SRAM、Flash 和 EEPROM 存档
- 常用软件中断的内置实现，也可以关掉，改走异常向量
- 即时存档。游戏自己的存档仍写在卡带旁边

周期、地址和每个寄存器的行为记在 [doc/hardware.md](doc/hardware.md)。源文件各管什么见 [CODE_MAP.md](CODE_MAP.md)。

## 跑起来

安装 Rust 之后：

```sh
cargo run --release
cargo run --release -- 卡带.gba
```

不带路径时打开卡带图库。载入过的卡带记在应用数据目录的 `library.json` 里。macOS 上这个目录是 `~/Library/Application Support/gameboy-ultima`。图库显示卡带头里的标题和游戏代码。没有自选封面时，运行大约半秒后会用当时的画面做封面。

macOS 可以打成独立应用。构建需要 Xcode Command Line Tools：

```sh
./scripts/package-macos.sh
```

产物是 `target/ULTIMA.app`。

| 按键 | 作用 |
| --- | --- |
| 方向键 | 十字键 |
| X、Z | A、B |
| A、S | L、R |
| Enter | Start |
| Backspace | Select |
| Space | 暂停或继续 |

即时存档写在卡带旁边的 `.state`，只接受同一张卡带。游戏存档写在旁边的 `.sav`，换卡或退出时如果改过就写回。

## 开发

```sh
cargo test
cargo test --test arm
cargo test --test thumb
cargo test --test arm_gba --test thumb_gba --test memory_gba
```

`data/arm` 和 `data/thumb` 是单条指令的初态和终态。`roms/` 里的 `arm.gba`、`thumb.gba`、`memory.gba` 来自 [jsmolka/gba-tests](https://github.com/jsmolka/gba-tests)，许可证是 MIT，用来检查 CPU 和内存，不代表商业游戏能运行。

## 边界

指令仍是解释执行，没有编译成宿主机器码。中止模式暂时和用户模式共用寄存器组。测试 ROM 通过，只说明被测的那一部分符合预期。

## 许可

本仓库还没有选定许可证。`roms/` 中的测试卡带另按 MIT 提供。`assets/keymap.png` 是 Joonas Javanainen 拍摄的 Game Boy Advance 照片，裁切后用于图库，许可是 CC BY 4.0，出处写在 `assets/keymap.license`。
