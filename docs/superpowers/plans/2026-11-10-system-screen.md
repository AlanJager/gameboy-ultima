# System Screen Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 按 F7 把游戏停在当前这一批指令上，画面换成一页系统，能恢复，也能改联机、速度、金手指和画面。

**Architecture:** 暂停仍用工作线程里的 `paused`，因此不再调用 `step_quiet`。系统页开或关由纯函数 `apply_system` 决定，工作线程只套用结果。页面在 `src/bin/system.rs`，联机和金手指复用现在的面板内容，不往 GBA 显存画，也不新做网络。

**Tech Stack:** Rust，GPUI，现有的 `Command` 通道和键位 JSON。测试用 `cargo test --offline --bin gameboy-ultima-gpui`。不要 `cargo build --bin gameboy-ultima`，不要对整个仓库跑 rustfmt。

**Spec:** `docs/superpowers/specs/2026-11-10-system-screen-design.md`

## Global Constraints

- `paused` 为真时不再调用 `step_quiet`。停在一批指令的边界上。
- 系统页开着就一定是暂停。离开系统页就继续游戏。
- 没有卡带时系统键不起作用。
- 默认热键是 `f7`。`escape` 不能存成键位。
- 旧键位文件没有「系统」时，读出来仍是 `f7`，其它热键不变。
- 页标题是「系统」。载入了卡带时副标题用卡带名。
- 页上只有恢复、联机、速度（1、2、3、4、5、8）、金手指、画面（液晶余辉、色彩校正、画面震动）。
- 放大方式留在「视图」菜单。音量、BIOS、即时存档留在菜单栏。
- 系统页打开时关掉联机面板、金手指面板和键位面板。菜单栏入口还在。
- 输入框有焦点时，按键交给输入框。`f7` 只有在没有焦点时离开系统页。
- 换卡、复位、读档关掉系统页，暂停与否沿用这些操作现在的规则。
- 打开系统不清回退记录，也不拆联机房间。
- 不做 GameCube 联机，不新做网卡，不改 BIOS。
- README 桌面 App 表加上系统这一行，「还没有」里拿掉 GameCube 联机。

## Review Focus

- 系统页如果像联机面板那样吃掉全部按键，F7 就无法离开。没有输入焦点时，F7 必须恢复游戏。
- 空格或菜单「暂停 / 继续」如果只翻转 `paused`，游戏会在系统页底下跑起来。系统页开着时，这两者必须关掉页面并继续。
- 换卡、复位、读档如果留下系统页，新的一局会被这页盖住。这些操作必须先关掉页面，再使用它们自己的暂停规则。
- 打开系统如果调用 `link.stop()`，房间会散。系统键不得停止联机。
- 名字或金手指输入框有焦点时，F7 不得被当成文字，也不得退出。其它可输入字符仍进输入框。

---

### Task 1: 系统热键

**Files:**
- Modify: `src/bin/controls.rs`（`Hotkey`、`KeyMap::hotkeys`）
- Modify: `src/bin/gpui.rs`（`Hotkey::command`、键位设置里的一行）
- Test: `src/bin/controls.rs` 的 `mod tests`

**Interfaces:**
- Consumes: 现有 `KeyMap`、`map_from_bindings`、`bindable_key`（`escape` 已经拒绝）
- Produces: `Hotkey::System`，`name()` 为 `"系统"`，`id()` 为 `"system"`，`element_id()` 为 `"hotkey-system"`，`index()` 为 `10`。默认键 `"f7"`。`Hotkey::ALL` 长度为 11。

- [ ] **Step 1: Write the failing test**

在 `src/bin/controls.rs` 的 `mod tests` 末尾加上：

```rust
#[test]
fn an_old_keymap_without_system_still_uses_f7() {
    let map = map_from_bindings(&HashMap::from([("rewind".to_string(), "f6".to_string())]));
    assert_eq!(map.hotkey(Hotkey::System), "f7");
    assert_eq!(map.hotkey(Hotkey::Rewind), "f6");
    assert_eq!(map.hotkey(Hotkey::FastForward), "tab");
    assert_eq!(KeyMap::default().hotkey(Hotkey::System), "f7");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --offline --bin gameboy-ultima-gpui -- an_old_keymap_without_system_still_uses_f7`

Expected: FAIL，编译错误，没有 `Hotkey::System`。

- [ ] **Step 3: Write minimal implementation**

`Hotkey` 增加 `System`。`ALL` 改成 `[Self; 11]`，最后一项是 `Self::System`。`index`、`name`、`id`、`element_id` 按接口补上。`KeyMap.hotkeys` 改成 `[String; 11]`，默认数组末尾加 `"f7".into()`。

`src/bin/gpui.rs` 的 `Hotkey::command` 加上 `Self::System => None`。`function_bindings` 在回退那一行后面加：

```rust
.child(named_hotkey(cx, map, capturing, Hotkey::System))
```

不要在这一步处理按下 F7。

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --offline --bin gameboy-ultima-gpui -- an_old_keymap_without_system_still_uses_f7 rebinding_a_key`

Expected: PASS。`rebinding_a_key` 里旧 JSON 的其它热键仍然是原来的默认值。

- [ ] **Step 5: Commit**

```bash
git add src/bin/controls.rs src/bin/gpui.rs
git commit -m "$(cat <<'EOF'
feat: add a rebindable system hotkey

Old key files omit it, so the default stays F7 and the keys already saved do not move.

EOF
)"
```

### Task 2: 开、关和按键规则

**Files:**
- Create: `src/bin/system.rs`
- Modify: `src/bin/gpui.rs`（`mod system;`）
- Test: `src/bin/system.rs` 的 `mod tests`

**Interfaces:**
- Consumes: 无
- Produces:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SystemRun {
    pub open: bool,
    pub paused: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SystemInput {
    Open,
    Resume,
    Hide,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SystemField {
    None,
    LinkName,
    LinkAddress,
    CheatName,
    CheatCode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SystemKey {
    Resume,
    Edit,
    Ignore,
}

pub(crate) fn apply_system(state: SystemRun, has_cart: bool, input: SystemInput) -> SystemRun
pub(crate) fn system_key(field: SystemField, key: &str) -> SystemKey
pub(crate) fn system_stops_link(input: SystemInput) -> bool
pub(crate) fn pause_key_while_open(open: bool) -> Option<SystemInput>
```

`pause_key_while_open(true)` 返回 `Some(SystemInput::Resume)`。`pause_key_while_open(false)` 返回 `None`，空格仍走原来的暂停翻转。

`system_stops_link` 对三个输入都返回 `false`。

- [ ] **Step 1: Write the failing test**

`src/bin/gpui.rs` 在 `mod controls;` 旁边加 `mod system;`。`src/bin/system.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_pauses_and_resume_continues() {
        let running = SystemRun { open: false, paused: false };
        let open = apply_system(running, true, SystemInput::Open);
        assert_eq!(open, SystemRun { open: true, paused: true });
        let again = apply_system(
            SystemRun { open: false, paused: true },
            true,
            SystemInput::Open,
        );
        assert_eq!(again, SystemRun { open: true, paused: true });
        assert_eq!(
            apply_system(open, true, SystemInput::Resume),
            SystemRun { open: false, paused: false }
        );
    }

    #[test]
    fn no_cart_and_hide_leave_the_pause_bit_alone() {
        let running = SystemRun { open: false, paused: false };
        assert_eq!(apply_system(running, false, SystemInput::Open), running);
        let suspended = SystemRun { open: true, paused: true };
        assert_eq!(
            apply_system(suspended, true, SystemInput::Hide),
            SystemRun { open: false, paused: true }
        );
    }

    #[test]
    fn f7_resumes_only_when_no_field_is_focused() {
        assert_eq!(system_key(SystemField::None, "f7"), SystemKey::Resume);
        assert_eq!(system_key(SystemField::LinkName, "f7"), SystemKey::Ignore);
        assert_eq!(system_key(SystemField::CheatCode, "a"), SystemKey::Edit);
        assert_eq!(system_key(SystemField::None, "a"), SystemKey::Ignore);
        assert!(!system_stops_link(SystemInput::Open));
        assert!(!system_stops_link(SystemInput::Resume));
        assert!(!system_stops_link(SystemInput::Hide));
        assert_eq!(pause_key_while_open(true), Some(SystemInput::Resume));
        assert_eq!(pause_key_while_open(false), None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --offline --bin gameboy-ultima-gpui -- opening_pauses_and_resume_continues`

Expected: FAIL，`apply_system` 未定义。

- [ ] **Step 3: Write minimal implementation**

```rust
pub(crate) fn apply_system(state: SystemRun, has_cart: bool, input: SystemInput) -> SystemRun {
    if !has_cart {
        return state;
    }
    match input {
        SystemInput::Open => SystemRun { open: true, paused: true },
        SystemInput::Resume => SystemRun { open: false, paused: false },
        SystemInput::Hide => SystemRun { open: false, paused: state.paused },
    }
}

pub(crate) fn system_key(field: SystemField, key: &str) -> SystemKey {
    if key == "f7" {
        return if field == SystemField::None {
            SystemKey::Resume
        } else {
            SystemKey::Ignore
        };
    }
    if field == SystemField::None {
        SystemKey::Ignore
    } else {
        SystemKey::Edit
    }
}

pub(crate) fn system_stops_link(_input: SystemInput) -> bool {
    false
}

pub(crate) fn pause_key_while_open(open: bool) -> Option<SystemInput> {
    if open {
        Some(SystemInput::Resume)
    } else {
        None
    }
}
```

把 Step 1 的类型定义放在这些函数上面。`system_key` 这里先认字面量 `"f7"`。Task 3 改成认当前绑定的系统键，测试仍传入 `"f7"`，因为默认绑定就是它。

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --offline --bin gameboy-ultima-gpui -- opening_pauses_and_resume_continues no_cart_and_hide_leave_the_pause_bit_alone f7_resumes_only_when_no_field_is_focused`

Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/bin/system.rs src/bin/gpui.rs
git commit -m "$(cat <<'EOF'
feat: decide when the system page pauses and resumes

Opening always pauses, resume always continues, and hiding the page leaves an in-progress pause alone.

EOF
)"
```

### Task 3: 接到暂停和工作线程

**Files:**
- Modify: `src/bin/gpui.rs`（`Command`、`Snapshot`、`handle_key_down`、`TogglePause`、会换卡的命令）
- Modify: `src/bin/system.rs`（`system_key` 改为接受绑定后的键名）
- Test: `src/bin/system.rs`

**Interfaces:**
- Consumes: `apply_system`、`system_key`、`system_stops_link`、`Hotkey::System`
- Produces: `Command::SystemKey`。`Snapshot.system_open: bool`。工作线程本地 `system_open`。按下系统键时清掉按住的加速、回退和方向，不清 `Rewind` 的记录。

- [ ] **Step 1: Write the failing test**

把 `system_key` 改成 `system_key(field, key, system_binding: &str)`。更新 Task 2 的调用，第三个参数传 `"f7"`。再加：

```rust
#[test]
fn a_rebound_system_key_is_the_one_that_resumes() {
    assert_eq!(
        system_key(SystemField::None, "f9", "f9"),
        SystemKey::Resume
    );
    assert_eq!(
        system_key(SystemField::None, "f7", "f9"),
        SystemKey::Ignore
    );
    assert_eq!(
        system_key(SystemField::LinkName, "f9", "f9"),
        SystemKey::Ignore
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --offline --bin gameboy-ultima-gpui -- a_rebound_system_key_is_the_one_that_resumes`

Expected: FAIL，参数个数不对。

- [ ] **Step 3: Write minimal implementation**

`system_key` 的 `f7` 比较改成 `key == system_binding`。

`Command` 增加 `SystemKey`。`Snapshot` 增加 `system_open: bool`（`Default` 会是 `false`）。`Hotkey::command` 里 `System` 仍返回 `None`，避免没卡带时发出去。

工作线程增加 `let mut system_open = false;`。辅助函数：

```rust
fn publish_system(snapshot: &Mutex<Snapshot>, open: bool, paused: bool) {
    let mut state = snapshot.lock().expect("emulator snapshot poisoned");
    state.system_open = open;
    state.running = !paused && state.rom_path.is_some();
    if state.error.is_none() {
        state.status = if paused { "PAUSED" } else { "RUNNING" }.into();
    }
}

fn take_system(open: bool, paused: bool, has_cart: bool, input: SystemInput) -> SystemRun {
    debug_assert!(!system_stops_link(input));
    apply_system(SystemRun { open, paused }, has_cart, input)
}
```

`Command::SystemKey` 在 `take_link_command` 里处理并返回 `None`：

```rust
Command::SystemKey => {
    let has_cart = cart.is_some();
    let next = take_system(
        *system_open,
        *paused,
        has_cart,
        if *system_open { SystemInput::Resume } else { SystemInput::Open },
    );
    *system_open = next.open;
    *paused = next.paused;
    publish_system(snapshot, next.open, *paused);
    None
}
```

`take_link_command` 增加参数 `system_open: &mut bool` 和 `paused: &mut bool`。三个调用点都传进去。`apply_command` 的 `Command::SystemKey` 分支是 `CommandResult::Continue`。

`Command::TogglePause`：先看 `pause_key_while_open(*system_open)`。是 `Some(input)` 时走 `take_system`，不要先翻转 `paused`。是 `None` 时保持现在的翻转。

`replaces_cart` 为真时，在现有 `link.stop()` 之前：

```rust
let hidden = take_system(*system_open, *paused, true, SystemInput::Hide);
*system_open = hidden.open;
publish_system(snapshot, hidden.open, *paused);
```

这里故意不把 `hidden.paused` 写回。后面的换卡、复位、读档继续设置它们自己的 `paused`。不要在 `SystemKey` 或 `Hide` 上调用 `link.stop()` 或 `rewind.clear()`。

`handle_key_down`：在联机面板和金手指面板的提前返回之后、复位键判断之前，如果 `snapshot.system_open`：

```rust
let binding = map.hotkey(Hotkey::System).to_string();
let field = self.system_field;
match system_key(field, &key, &binding) {
    SystemKey::Resume if !event.is_held => {
        self.system_field = SystemField::None;
        self.link_ui.lock().expect("link ui poisoned").open = false;
        self.cheat_ui.lock().expect("cheat ui poisoned").open = false;
        self.controls.lock().expect("control settings poisoned").open = false;
        self.keys.store(0, Ordering::Relaxed);
        self.turbo_held.store(false, Ordering::Relaxed);
        self.rewind_held.store(false, Ordering::Relaxed);
        let _ = self.commands.send(Command::SystemKey);
        cx.notify();
        return;
    }
    SystemKey::Edit => {
        self.edit_system_field(event, cx);
        return;
    }
    SystemKey::Resume | SystemKey::Ignore => return,
}
```

`AppView` 增加 `system_field: SystemField`，初始 `SystemField::None`。`edit_system_field` 这一步可以先是空函数，Task 4 再把字符送进现有输入逻辑。没有 `snapshot.system_open` 时，原有的热键路径里如果 `hotkey == Hotkey::System` 且不是按住重复：

```rust
let has_cart = self
    .snapshot
    .lock()
    .expect("emulator snapshot poisoned")
    .rom_path
    .is_some();
if has_cart {
    self.link_ui.lock().expect("link ui poisoned").open = false;
    self.cheat_ui.lock().expect("cheat ui poisoned").open = false;
    self.controls.lock().expect("control settings poisoned").open = false;
    self.keys.store(0, Ordering::Relaxed);
    self.turbo_held.store(false, Ordering::Relaxed);
    self.rewind_held.store(false, Ordering::Relaxed);
    let _ = self.commands.send(Command::SystemKey);
}
return;
```

没有卡带时直接 `return`，不关面板，不发命令。

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --offline --bin gameboy-ultima-gpui -- a_rebound_system_key_is_the_one_that_resumes opening_pauses_and_resume_continues`

Expected: PASS。`cargo check --offline --bin gameboy-ultima-gpui` 只有原来的 `GamepadSample::idle` 警告。

- [ ] **Step 5: Commit**

```bash
git add src/bin/gpui.rs src/bin/system.rs
git commit -m "$(cat <<'EOF'
feat: pause on the system key without dropping the link room

The worker applies the same open and resume rules, and swapping carts only hides the page.

EOF
)"
```

### Task 4: 系统页、说明和可运行的 App

**Files:**
- Modify: `src/bin/system.rs`（页面）
- Modify: `src/bin/gpui.rs`（渲染、输入、从现有面板拆出内容）
- Modify: `README.md`
- Test: 这一步没有新的纯函数。用 Task 2 和 Task 3 的测试守住规则，再用打包后的 App 看页面。

**Interfaces:**
- Consumes: `link_overlay` 的内层面板、`cheat_overlay` 的内层面板、`Snapshot` 里的速度和画面开关、`Command::SystemKey`
- Produces: `system_page(...) -> impl IntoElement`。全屏，可滚动，标题「系统」。

- [ ] **Step 1: 把现有面板的内容拆出来**

`link_overlay` 保留外面的全屏遮罩。里面从 `.id("link-panel")` 开始的那一块移到：

```rust
fn link_panel(
    cx: &mut Context<AppView>,
    address: &str,
    name: &str,
    editing_name: bool,
    line: &str,
    card_line: &str,
    error: Option<&str>,
    lan_ip: &str,
    hosts: &[(String, String)],
    in_room: bool,
    seats: &[LinkSeat],
) -> impl IntoElement
```

`link_overlay` 的遮罩 `.child(link_panel(...))`。`cheat_overlay` 同样拆出 `cheat_panel(...)`，参数和现在的 `cheat_overlay` 一样。遮罩行为不变。

- [ ] **Step 2: 写出系统页**

`system.rs` 里 `system_page` 用 GPUI 的 `div`。背景用 `gpui.rs` 已有的 `BG`。页面：

- 标题文本「系统」。`title` 非空时下面一行是卡带名。
- 「恢复」按钮，`id("system-resume")`，点击发送 `Command::SystemKey`，并把 `system_field` 设为 `None`。
- 速度六个按钮，文字用 `SPEEDS` 里的数字。当前档位来自 `locked_speed`。点击时做和 `apply_speed_choice(launch, Some(speed), None)` 相同的事：写入 `controls.speed`、`locked_speed`，再 `persist_open_cart`。不要改加速档。
- 三个画面按钮，标签用 `picture_option_label`。点击时对 `ScreenToggle::Ghost`、`Color`、`Rumble` 做和 `toggle_screen_option` 相同的翻转、原子变量和 `persist_runtime`。不放放大方式。
- `.child(link_panel(...))` 和 `.child(cheat_panel(...))`。不要再套它们的全屏遮罩。

`render` 里当 `snapshot.system_open` 时，用这一页替换游戏内容，并且不要再挂联机、金手指、键位三块遮罩。`system_open` 为假时，这三块保持现在的条件。

- [ ] **Step 3: 输入焦点**

`AppView.system_field` 已在 Task 3 添加。点击联机名字把 `system_field` 设为 `LinkName`，点击地址设为 `LinkAddress`，金手指名字和代码同理。点击「恢复」、速度和画面按钮时设回 `None`。

`edit_system_field`：`LinkName` 和 `LinkAddress` 走现在 `edit_link_field` 的字符、退格和粘贴逻辑，但 `escape` 只把 `system_field` 设为 `None`，不调用 `close_link`。金手指同样，不调用 `close_cheats`。`f7` 不会进到这里，Task 3 已经把它标成 `Ignore`。

- [ ] **Step 4: README**

桌面 App 表「暂停、复位、音量、静音、联机面板」后面加一行：

```markdown
| 系统 | 默认 F7，可在键位里改。游戏停在当前这一步，画面换成系统页。可以恢复，也可以改联机、速度、金手指，以及液晶余辉、色彩校正和画面震动。寄存器、内存、回退记录和联机房间都留着 |
```

「还没有」表删掉 GameCube 那一行。如果表因此变空，连「还没有」这一节一起删掉。许可那一句保持「本仓库的代码采用 MIT 许可证，见 LICENSE。」

- [ ] **Step 5: 验证并打包**

Run: `cargo test --offline --bin gameboy-ultima-gpui`

Expected: PASS。允许原来的 `GamepadSample::idle` 警告。

然后：

```bash
cargo build --offline --release --bin gameboy-ultima-gpui
mkdir -p target/ULTIMA.app/Contents/MacOS
cp target/release/gameboy-ultima-gpui target/ULTIMA.app/Contents/MacOS/gameboy-ultima-gpui
cp macos/Info.plist target/ULTIMA.app/Contents/Info.plist
codesign --force --deep --sign - target/ULTIMA.app
```

打开 App，载入一张卡，按 F7：游戏停住，这一页盖住画面，联机房间还在。再按 F7 或「恢复」或空格，从原地继续。没有卡带时 F7 没有反应。

- [ ] **Step 6: Commit**

```bash
git add src/bin/system.rs src/bin/gpui.rs README.md
git commit -m "$(cat <<'EOF'
feat: show link, speed, cheats, and picture on the system page

The page replaces the game while the machine is paused, and the same commands still change those settings.

EOF
)"
```

不要把 `target/ULTIMA.app` 加进提交。不要推送。
