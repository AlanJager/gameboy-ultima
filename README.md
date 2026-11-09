# gameboy-ultima

这个仓库是一台 Game Boy Advance 模拟器，用来学习怎么写模拟器。

## 功能

### 核心

| 功能 | 说明 |
| --- | --- |
| ARM 与 Thumb 解释执行 | 含条件码、IRQ 和未定义指令 |
| 用户与系统共用一组寄存器 | 管理、IRQ、中止、未定义各有 SP、LR、SPSR；FIQ 另有 `r8`–`r14` |
| 板载内存、卡带等待和预取；四路 DMA、四路定时器、按键 | 跟画面共用周期 |
| 背景模式 0–5，精灵、旋转缩放、窗口、混合、马赛克 | |
| SRAM、Flash（64 KiB / 128 KiB）和 EEPROM 存档 | |
| 卡带 RTC | S-3511。写入日期后按真实时间走，记在卡带旁边的 `.rtc` |

### 声音

| 功能 | 说明 |
| --- | --- |
| 两路方波、一路波形、一路噪声，以及两路 Direct Sound | |
| 按 `SOUNDBIAS` 做 PWM 采样 | 样本进环形缓冲，播放在另一条线程 |
| 1× 时按环里的积压拉长或缩短这一帧的等待 | （`audio_pace`，大约在 0.92–1.08）；环满了仍丢掉新样本 |
| 速度高于 1× 时静音 | 并清掉已经进环的样本 |

### BIOS HLE

没有放进任天堂的 BIOS。认得出的软件中断在调用处做完，模式不变；认不出的进入向量 `0x08`。这条快路径默认开着，菜单里可以关掉，关掉后已实现的调用也走向量。向量上是一小段 `movs pc, lr`，用来返回。

| 功能 | 说明 |
| --- | --- |
| 软复位、寄存器复位、硬复位、Halt、Stop、IntrWait、VBlankIntrWait、CustomHalt | |
| 除法、开方、反正切；CpuSet / CpuFastSet | |
| 位展开、LZ77、哈夫曼、游程、差分；背景和精灵的仿射参数 | |
| `0x0D` | 把 `r0` 设成 `0xBAAE187F` |
| SoundBias | 把偏置收到 0 或 `0x200` |
| MidiKey2Freq（`0x1F`） | 按波形频率、音高和微调算出定时器频率 |
| 音乐序列器 | `0x1A` 初始化工作区并写到 `0x03007FF0`，`0x1B` 改混音模式。每帧 `0x1D` 重开 FIFO DMA，`0x1C` 按 MP2k 轨道推进一步并混进 PCM。`0x1E` 清声部。`0x20`–`0x24` 是打开、开播、暂停、继续、淡出。`0x28` / `0x29` 开关垂直同步。`0x2A` 写下 36 个指针，都指向 `0x188` 的 `bx lr` |
| MultiBoot（`0x25`） | 没有另一台机器，`r0` 返回 1 |

### 联机

| 功能 | 说明 |
| --- | --- |
| 多人线缆模式 | 最多 4 台，一次交换四路 16 位。没插线时到点自己结束，空位是 `0xFFFF` |
| 局域网发现 | 主机听 TCP `5576`，并往局域网 UDP 广播；可以点搜到的主机，也可以手填地址。卡带内容不一致会拒绝 |
| 名字和四个座位 | 主机是 1 号机 |
| 无线适配器走 32 位普通模式 | 登录口令做高低半字对调，再用 `0x9966` 命令做广播、目录、连接和数据，电波经同一个房间转发。没进房间时适配器还在，只是看不到别人 |
| 同一台 Mac 上再开一台 | `open -n target/ULTIMA.app`，用加入连到已经在听 `5576` 的那台 |

### 桌面 App

要跑的程序是 `gameboy-ultima-gpui`。

| 功能 | 说明 |
| --- | --- |
| GPUI 窗口 | 模拟在另一条线程里出帧和采样 |
| 图库 | 记下载入过的 `.gba`（不复制卡带文件）。没有自选封面时，大约 30 帧后用当时的画面做封面，也可以另选 PNG 或 JPEG |
| 速度 | 1、2、3、4、5、8 倍；按住加速默认 4 倍，按住时取锁定速度和加速档里更高的一档 |
| 键盘可以改键 | 手柄用 gilrs：东面键 A，南面键 B，Select / Start，十字键和左摇杆，左右扳机是 R / L |
| 即时存档 | 三个槽加一份快速档，按卡带内容分开，写在应用数据目录的 `save_states/`。游戏自己的 `.sav` 仍写在卡带旁边，换卡或退出时如果改过就写回 |
| 暂停、复位、音量、静音、联机面板 | |
| 画面 | 各平台有 CPU 整数最近邻和线性放大；macOS 另有 Metal 最近邻和锐化，也可以并排对比 |

### 还没有

| 功能 | 说明 |
| --- | --- |
| 更完整的无线登录和 Union Room | 未做 |
| 回退 | 未做 |
| 金手指 | 未做 |
| 按卡带分开的速度、BIOS 或键位 | 未做 |
| 跨网中继 | 未做 |
| 陀螺仪、太阳能、震动 | 未做 |
| 动态重编译 | 未做 |
| e-Reader | 未做 |
| GameCube 联机 | 未做 |

## 跑起来

安装 Rust 之后：

```sh
cargo run --release --bin gameboy-ultima-gpui
cargo run --release --bin gameboy-ultima-gpui -- 卡带.gba
```

不带路径时打开图库。macOS 上应用数据在 `~/Library/Application Support/gameboy-ultima`（`library.json`、封面、即时存档、键位）。

macOS 可以打成独立应用，构建需要 Xcode Command Line Tools：

```sh
./scripts/package-macos.sh
```

产物是 `target/ULTIMA.app`。

默认按键：方向键是十字键，X / Z 是 A / B，A / S 是 L / R，Enter 是 Start，Backspace 是 Select，R 复位，Space 暂停。F5 快速存档，F8 快速读档，Tab 按住加速。这些都可以在键位设置里改。

## 测试

```sh
cargo test
```

`data/arm` 和 `data/thumb` 是单条指令的初态和终态。`roms/` 里的 `arm.gba`、`thumb.gba`、`memory.gba` 来自 [jsmolka/gba-tests](https://github.com/jsmolka/gba-tests)，许可证是 MIT，用来检查 CPU 和内存。

## 许可

本仓库的代码采用 MIT 许可证，见 `LICENSE`。`roms/` 中的测试卡带另按 MIT 提供。`assets/keymap.png` 是 Joonas Javanainen 拍摄的 Game Boy Advance 照片，裁切后用在键位设置里，许可是 CC BY 4.0，出处写在 `assets/keymap.license`。
