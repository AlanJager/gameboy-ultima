//! 键盘和手柄怎样合成 GBA 的 10 个按键位。
//! 低 10 位从低到高是 A、B、Select、Start、右、左、上、下、R、L。1 表示按下。

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use gilrs::{Axis, Button, Gilrs};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PadButton {
    A,
    B,
    Select,
    Start,
    Right,
    Left,
    Up,
    Down,
    R,
    L,
    Reset,
}

impl PadButton {
    pub(crate) const ALL: [Self; 11] = [
        Self::A,
        Self::B,
        Self::Select,
        Self::Start,
        Self::Right,
        Self::Left,
        Self::Up,
        Self::Down,
        Self::R,
        Self::L,
        Self::Reset,
    ];

    pub(crate) fn index(self) -> usize {
        match self {
            Self::A => 0,
            Self::B => 1,
            Self::Select => 2,
            Self::Start => 3,
            Self::Right => 4,
            Self::Left => 5,
            Self::Up => 6,
            Self::Down => 7,
            Self::R => 8,
            Self::L => 9,
            Self::Reset => 10,
        }
    }

    pub(crate) fn bit(self) -> Option<u16> {
        match self {
            Self::A => Some(1 << 0),
            Self::B => Some(1 << 1),
            Self::Select => Some(1 << 2),
            Self::Start => Some(1 << 3),
            Self::Right => Some(1 << 4),
            Self::Left => Some(1 << 5),
            Self::Up => Some(1 << 6),
            Self::Down => Some(1 << 7),
            Self::R => Some(1 << 8),
            Self::L => Some(1 << 9),
            Self::Reset => None,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
            Self::Select => "SELECT",
            Self::Start => "START",
            Self::Right => "右",
            Self::Left => "左",
            Self::Up => "上",
            Self::Down => "下",
            Self::R => "R",
            Self::L => "L",
            Self::Reset => "复位",
        }
    }

    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::A => "a",
            Self::B => "b",
            Self::Select => "select",
            Self::Start => "start",
            Self::Right => "right",
            Self::Left => "left",
            Self::Up => "up",
            Self::Down => "down",
            Self::R => "r",
            Self::L => "l",
            Self::Reset => "reset",
        }
    }

    pub(crate) fn element_id(self) -> &'static str {
        match self {
            Self::A => "pad-a",
            Self::B => "pad-b",
            Self::Select => "pad-select",
            Self::Start => "pad-start",
            Self::Right => "pad-right",
            Self::Left => "pad-left",
            Self::Up => "pad-up",
            Self::Down => "pad-down",
            Self::R => "pad-r",
            Self::L => "pad-l",
            Self::Reset => "pad-reset",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hotkey {
    QuickSave,
    QuickLoad,
    FastForward,
    Rewind,
}

impl Hotkey {
    pub(crate) const ALL: [Self; 4] = [
        Self::QuickSave,
        Self::QuickLoad,
        Self::FastForward,
        Self::Rewind,
    ];

    pub(crate) fn index(self) -> usize {
        match self {
            Self::QuickSave => 0,
            Self::QuickLoad => 1,
            Self::FastForward => 2,
            Self::Rewind => 3,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::QuickSave => "快速存档",
            Self::QuickLoad => "快速读档",
            Self::FastForward => "按住加速",
            Self::Rewind => "按住回退",
        }
    }

    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::QuickSave => "quick_save",
            Self::QuickLoad => "quick_load",
            Self::FastForward => "fast_forward",
            Self::Rewind => "rewind",
        }
    }

    pub(crate) fn element_id(self) -> &'static str {
        match self {
            Self::QuickSave => "hotkey-quick-save",
            Self::QuickLoad => "hotkey-quick-load",
            Self::FastForward => "hotkey-fast-forward",
            Self::Rewind => "hotkey-rewind",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BindingTarget {
    Pad(PadButton),
    Hotkey(Hotkey),
}

impl BindingTarget {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Pad(button) => button.name(),
            Self::Hotkey(hotkey) => hotkey.name(),
        }
    }

    pub(crate) fn element_id(self) -> &'static str {
        match self {
            Self::Pad(button) => button.element_id(),
            Self::Hotkey(hotkey) => hotkey.element_id(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KeyMap {
    keys: [String; 11],
    hotkeys: [String; 4],
}

impl Default for KeyMap {
    fn default() -> Self {
        Self {
            keys: [
                "x".into(),
                "z".into(),
                "backspace".into(),
                "enter".into(),
                "right".into(),
                "left".into(),
                "up".into(),
                "down".into(),
                "s".into(),
                "a".into(),
                "r".into(),
            ],
            hotkeys: ["f5".into(), "f8".into(), "tab".into(), "f6".into()],
        }
    }
}

impl KeyMap {
    pub(crate) fn key(&self, button: PadButton) -> &str {
        &self.keys[button.index()]
    }

    pub(crate) fn hotkey(&self, hotkey: Hotkey) -> &str {
        &self.hotkeys[hotkey.index()]
    }

    pub(crate) fn button_for_key(&self, key: &str) -> Option<PadButton> {
        let key = normalize_key(key);
        if key.is_empty() {
            return None;
        }
        PadButton::ALL
            .into_iter()
            .find(|button| self.key(*button) == key)
    }

    pub(crate) fn hotkey_for_key(&self, key: &str) -> Option<Hotkey> {
        let key = normalize_key(key);
        if key.is_empty() {
            return None;
        }
        Hotkey::ALL
            .into_iter()
            .find(|hotkey| self.hotkey(*hotkey) == key)
    }

    pub(crate) fn bind_target(&mut self, target: BindingTarget, key: &str) {
        let key = normalize_key(key);
        if !bindable_key(&key) {
            return;
        }
        for button in PadButton::ALL {
            if BindingTarget::Pad(button) != target && self.key(button) == key {
                self.keys[button.index()].clear();
            }
        }
        for hotkey in Hotkey::ALL {
            if BindingTarget::Hotkey(hotkey) != target && self.hotkey(hotkey) == key {
                self.hotkeys[hotkey.index()].clear();
            }
        }
        match target {
            BindingTarget::Pad(button) => self.keys[button.index()] = key,
            BindingTarget::Hotkey(hotkey) => self.hotkeys[hotkey.index()] = key,
        }
    }
}

pub(crate) const SPEEDS: [u8; 6] = [1, 2, 3, 4, 5, 8];
pub(crate) const DEFAULT_SPEED: u8 = 1;
pub(crate) const DEFAULT_TURBO: u8 = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SavedControls {
    pub(crate) map: KeyMap,
    pub(crate) speed: u8,
    pub(crate) turbo: u8,
}

impl Default for SavedControls {
    fn default() -> Self {
        Self {
            map: KeyMap::default(),
            speed: DEFAULT_SPEED,
            turbo: DEFAULT_TURBO,
        }
    }
}

pub(crate) fn normalize_speed(speed: u8) -> u8 {
    if SPEEDS.contains(&speed) {
        speed
    } else {
        DEFAULT_SPEED
    }
}

pub(crate) fn normalize_turbo(speed: u8) -> u8 {
    if SPEEDS.contains(&speed) {
        speed
    } else {
        DEFAULT_TURBO
    }
}

/// 锁定速度一直生效。按住加速键时，用锁定速度和加速档里更高的一档。
pub(crate) fn effective_speed(locked: u8, turbo: u8, holding: bool) -> u8 {
    let locked = normalize_speed(locked);
    let turbo = normalize_turbo(turbo);
    if holding {
        locked.max(turbo)
    } else {
        locked
    }
}

pub(crate) fn frame_pace(base: Duration, speed: u8) -> Duration {
    base / u32::from(speed.max(1))
}

#[derive(Serialize, Deserialize)]
struct ControlsFile {
    version: u8,
    bindings: HashMap<String, String>,
    #[serde(default)]
    speed: u8,
    #[serde(default)]
    turbo: u8,
}

pub(crate) fn normalize_key(key: &str) -> String {
    match key.to_ascii_lowercase().as_str() {
        "arrowright" => "right".into(),
        "arrowleft" => "left".into(),
        "arrowup" => "up".into(),
        "arrowdown" => "down".into(),
        other => other.to_string(),
    }
}

pub(crate) fn bindable_key(key: &str) -> bool {
    !matches!(
        key,
        "" | "escape"
            | "space"
            | "shift"
            | "control"
            | "alt"
            | "cmd"
            | "command"
            | "fn"
            | "function"
            | "super"
            | "win"
            | "meta"
    )
}

pub(crate) fn game_key_mask(key: &str, map: &KeyMap) -> Option<u16> {
    map.button_for_key(key).and_then(PadButton::bit)
}

pub(crate) fn keyboard_bits(held: u16, key: &str, map: &KeyMap, pressed: bool) -> u16 {
    let Some(mask) = game_key_mask(key, map) else {
        return held;
    };
    if pressed {
        held | mask
    } else {
        held & !mask
    }
}

/// 键盘是边沿，手柄是每帧的电平。同一位两边都按下时保持按下，松开一边不会清掉另一边。
pub(crate) fn held_keys(keyboard: u16, gamepad: u16) -> u16 {
    keyboard | gamepad
}

const STICK_DEADZONE: f32 = 0.4;

struct GamepadSample {
    east: bool,
    south: bool,
    select: bool,
    start: bool,
    dpad_right: bool,
    dpad_left: bool,
    dpad_up: bool,
    dpad_down: bool,
    right_bumper: bool,
    left_bumper: bool,
    left_x: f32,
    left_y: f32,
    dpad_x: f32,
    dpad_y: f32,
}

impl GamepadSample {
    fn idle() -> Self {
        Self {
            east: false,
            south: false,
            select: false,
            start: false,
            dpad_right: false,
            dpad_left: false,
            dpad_up: false,
            dpad_down: false,
            right_bumper: false,
            left_bumper: false,
            left_x: 0.0,
            left_y: 0.0,
            dpad_x: 0.0,
            dpad_y: 0.0,
        }
    }
}

/// 东面键是 A，南面键是 B，和 GBA 上 A 在右边一致。
fn gamepad_mask(sample: &GamepadSample) -> u16 {
    let mut mask = 0u16;
    if sample.east {
        mask |= 1 << 0;
    }
    if sample.south {
        mask |= 1 << 1;
    }
    if sample.select {
        mask |= 1 << 2;
    }
    if sample.start {
        mask |= 1 << 3;
    }
    if sample.dpad_right || sample.left_x >= STICK_DEADZONE || sample.dpad_x >= STICK_DEADZONE {
        mask |= 1 << 4;
    }
    if sample.dpad_left || sample.left_x <= -STICK_DEADZONE || sample.dpad_x <= -STICK_DEADZONE {
        mask |= 1 << 5;
    }
    if sample.dpad_up || sample.left_y >= STICK_DEADZONE || sample.dpad_y >= STICK_DEADZONE {
        mask |= 1 << 6;
    }
    if sample.dpad_down || sample.left_y <= -STICK_DEADZONE || sample.dpad_y <= -STICK_DEADZONE {
        mask |= 1 << 7;
    }
    if sample.right_bumper {
        mask |= 1 << 8;
    }
    if sample.left_bumper {
        mask |= 1 << 9;
    }
    mask
}

fn sample_gamepad(pad: &gilrs::Gamepad) -> GamepadSample {
    GamepadSample {
        east: pad.is_pressed(Button::East),
        south: pad.is_pressed(Button::South),
        select: pad.is_pressed(Button::Select),
        start: pad.is_pressed(Button::Start),
        dpad_right: pad.is_pressed(Button::DPadRight),
        dpad_left: pad.is_pressed(Button::DPadLeft),
        dpad_up: pad.is_pressed(Button::DPadUp),
        dpad_down: pad.is_pressed(Button::DPadDown),
        right_bumper: pad.is_pressed(Button::RightTrigger),
        left_bumper: pad.is_pressed(Button::LeftTrigger),
        left_x: pad.value(Axis::LeftStickX),
        left_y: pad.value(Axis::LeftStickY),
        dpad_x: pad.value(Axis::DPadX),
        dpad_y: pad.value(Axis::DPadY),
    }
}

pub(crate) fn open_gamepads() -> Option<Gilrs> {
    match Gilrs::new() {
        Ok(pads) => Some(pads),
        Err(error) => {
            eprintln!("手柄不可用：{error}");
            None
        }
    }
}

pub(crate) fn poll_gamepad(pads: &mut Option<Gilrs>) -> u16 {
    let Some(pads) = pads else {
        return 0;
    };
    while pads.next_event().is_some() {}
    pads.gamepads()
        .next()
        .map(|(_, pad)| gamepad_mask(&sample_gamepad(&pad)))
        .unwrap_or(0)
}

pub(crate) fn is_reset_key(key: &str, map: &KeyMap) -> bool {
    map.button_for_key(key) == Some(PadButton::Reset)
}

fn is_function_key(key: &str) -> bool {
    let Some(number) = key.strip_prefix('f') else {
        return false;
    };
    !number.is_empty() && number.chars().all(|ch| ch.is_ascii_digit())
}

pub(crate) fn key_label(key: &str) -> String {
    if key.is_empty() {
        return "未设置".into();
    }
    match key {
        "left" => "←".into(),
        "right" => "→".into(),
        "up" => "↑".into(),
        "down" => "↓".into(),
        "enter" => "Enter".into(),
        "backspace" => "Bksp".into(),
        "delete" => "Del".into(),
        "tab" => "Tab".into(),
        other if is_function_key(other) => format!("F{}", &other[1..]),
        other if other.chars().count() == 1 => other.to_ascii_uppercase(),
        other => other.to_string(),
    }
}

pub(crate) fn read_controls(path: &Path) -> Result<SavedControls, String> {
    if !path.exists() {
        return Ok(SavedControls::default());
    }
    let bytes = std::fs::read(path)
        .map_err(|error| format!("读取键位 {} 失败：{error}", path.display()))?;
    let file: ControlsFile = serde_json::from_slice(&bytes)
        .map_err(|error| format!("解析键位 {} 失败：{error}", path.display()))?;
    if file.version != 1 {
        return Err(format!("不支持的键位版本：{}", file.version));
    }
    let mut map = KeyMap::default();
    for button in PadButton::ALL {
        let Some(key) = file.bindings.get(button.id()) else {
            continue;
        };
        apply_saved_key(&mut map.keys[button.index()], key);
    }
    for hotkey in Hotkey::ALL {
        let Some(key) = file.bindings.get(hotkey.id()) else {
            continue;
        };
        apply_saved_key(&mut map.hotkeys[hotkey.index()], key);
    }
    let mut seen = Vec::new();
    for button in PadButton::ALL {
        claim_saved_key(&mut map.keys[button.index()], &mut seen);
    }
    for hotkey in Hotkey::ALL {
        claim_saved_key(&mut map.hotkeys[hotkey.index()], &mut seen);
    }
    Ok(SavedControls {
        map,
        speed: normalize_speed(file.speed),
        turbo: normalize_turbo(file.turbo),
    })
}

fn apply_saved_key(slot: &mut String, key: &str) {
    let key = normalize_key(key);
    if key.is_empty() {
        slot.clear();
    } else if bindable_key(&key) {
        *slot = key;
    }
}

fn claim_saved_key(slot: &mut String, seen: &mut Vec<String>) {
    if slot.is_empty() {
        return;
    }
    if seen.iter().any(|used| used == slot) {
        slot.clear();
    } else {
        seen.push(slot.clone());
    }
}

pub(crate) fn write_controls(path: &Path, saved: &SavedControls) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建键位目录 {} 失败：{error}", parent.display()))?;
    }
    let mut bindings: HashMap<String, String> = PadButton::ALL
        .into_iter()
        .map(|button| (button.id().to_string(), saved.map.key(button).to_string()))
        .collect();
    for hotkey in Hotkey::ALL {
        bindings.insert(
            hotkey.id().to_string(),
            saved.map.hotkey(hotkey).to_string(),
        );
    }
    let bytes = serde_json::to_vec_pretty(&ControlsFile {
        version: 1,
        bindings,
        speed: normalize_speed(saved.speed),
        turbo: normalize_turbo(saved.turbo),
    })
    .map_err(|error| format!("写入键位失败：{error}"))?;
    std::fs::write(path, bytes)
        .map_err(|error| format!("写入键位 {} 失败：{error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_gba_controls_to_the_existing_key_bits() {
        let map = KeyMap::default();
        assert_eq!(game_key_mask("x", &map), Some(1 << 0));
        assert_eq!(game_key_mask("z", &map), Some(1 << 1));
        assert_eq!(game_key_mask("backspace", &map), Some(1 << 2));
        assert_eq!(game_key_mask("enter", &map), Some(1 << 3));
        assert_eq!(game_key_mask("right", &map), Some(1 << 4));
        assert_eq!(game_key_mask("arrowright", &map), Some(1 << 4));
        assert_eq!(game_key_mask("arrowleft", &map), Some(1 << 5));
        assert_eq!(game_key_mask("arrowup", &map), Some(1 << 6));
        assert_eq!(game_key_mask("arrowdown", &map), Some(1 << 7));
        assert_eq!(game_key_mask("a", &map), Some(1 << 9));
        assert_eq!(game_key_mask("unknown", &map), None);
        assert!(is_reset_key("r", &map));
        assert_eq!(game_key_mask("r", &map), None);
    }

    #[test]
    fn keyboard_press_and_release_change_only_that_bit() {
        let map = KeyMap::default();
        let held = keyboard_bits(0, "x", &map, true);
        assert_eq!(held, 1 << 0);
        let held = keyboard_bits(held, "arrowright", &map, true);
        assert_eq!(held, (1 << 0) | (1 << 4));
        let held = keyboard_bits(held, "x", &map, true);
        assert_eq!(held, (1 << 0) | (1 << 4));
        let held = keyboard_bits(held, "x", &map, false);
        assert_eq!(held, 1 << 4);
        let held = keyboard_bits(held, "arrowright", &map, false);
        assert_eq!(held, 0);
        assert_eq!(keyboard_bits(0, "unknown", &map, true), 0);
        assert_eq!(keyboard_bits(1 << 0, "r", &map, true), 1 << 0);
        assert_eq!(keyboard_bits(0, "right", &map, false), 0);
    }

    #[test]
    fn maps_a_gamepad_onto_the_same_gba_bits() {
        assert_eq!(gamepad_mask(&GamepadSample::idle()), 0);

        let mut sample = GamepadSample::idle();
        sample.east = true;
        assert_eq!(gamepad_mask(&sample), 1 << 0);
        sample = GamepadSample::idle();
        sample.south = true;
        assert_eq!(gamepad_mask(&sample), 1 << 1);
        sample = GamepadSample::idle();
        sample.select = true;
        assert_eq!(gamepad_mask(&sample), 1 << 2);
        sample = GamepadSample::idle();
        sample.start = true;
        assert_eq!(gamepad_mask(&sample), 1 << 3);
        sample = GamepadSample::idle();
        sample.dpad_right = true;
        assert_eq!(gamepad_mask(&sample), 1 << 4);
        sample = GamepadSample::idle();
        sample.dpad_left = true;
        assert_eq!(gamepad_mask(&sample), 1 << 5);
        sample = GamepadSample::idle();
        sample.dpad_up = true;
        assert_eq!(gamepad_mask(&sample), 1 << 6);
        sample = GamepadSample::idle();
        sample.dpad_down = true;
        assert_eq!(gamepad_mask(&sample), 1 << 7);
        sample = GamepadSample::idle();
        sample.right_bumper = true;
        assert_eq!(gamepad_mask(&sample), 1 << 8);
        sample = GamepadSample::idle();
        sample.left_bumper = true;
        assert_eq!(gamepad_mask(&sample), 1 << 9);

        sample = GamepadSample::idle();
        sample.left_x = 0.39;
        sample.left_y = -0.39;
        assert_eq!(gamepad_mask(&sample), 0);
        sample.left_x = 0.4;
        assert_eq!(gamepad_mask(&sample), 1 << 4);
        sample.left_x = -0.4;
        assert_eq!(gamepad_mask(&sample), 1 << 5);
        sample.left_x = 0.0;
        sample.left_y = 0.4;
        assert_eq!(gamepad_mask(&sample), 1 << 6);
        sample.left_y = -0.4;
        assert_eq!(gamepad_mask(&sample), 1 << 7);

        sample = GamepadSample::idle();
        sample.dpad_x = 1.0;
        sample.dpad_y = -1.0;
        assert_eq!(gamepad_mask(&sample), (1 << 4) | (1 << 7));

        sample = GamepadSample::idle();
        sample.east = true;
        sample.dpad_left = true;
        sample.left_x = 0.4;
        assert_eq!(gamepad_mask(&sample), (1 << 0) | (1 << 4) | (1 << 5));
    }

    #[test]
    fn keyboard_and_gamepad_keep_their_own_bits() {
        let map = KeyMap::default();
        let keyboard = keyboard_bits(keyboard_bits(0, "x", &map, true), "right", &map, true);
        let mut sample = GamepadSample::idle();
        sample.south = true;
        sample.left_x = 0.4;
        let combined = held_keys(keyboard, gamepad_mask(&sample));
        assert_eq!(combined, (1 << 0) | (1 << 1) | (1 << 4));

        let keyboard = keyboard_bits(
            keyboard_bits(keyboard, "x", &map, false),
            "right",
            &map,
            false,
        );
        assert_eq!(
            held_keys(keyboard, gamepad_mask(&sample)),
            (1 << 1) | (1 << 4)
        );
        assert_eq!(held_keys(keyboard, gamepad_mask(&GamepadSample::idle())), 0);
    }

    #[test]
    fn locked_speed_stays_on_and_the_held_key_uses_the_faster_ratio() {
        assert_eq!(effective_speed(1, 4, false), 1);
        assert_eq!(effective_speed(1, 4, true), 4);
        assert_eq!(effective_speed(2, 8, true), 8);
        assert_eq!(effective_speed(8, 4, true), 8);
        assert_eq!(effective_speed(2, 2, true), 2);
        assert_eq!(effective_speed(0, 7, false), DEFAULT_SPEED);
        assert_eq!(effective_speed(0, 7, true), DEFAULT_TURBO);
        let base = Duration::from_millis(16);
        assert_eq!(frame_pace(base, 1), base);
        assert_eq!(frame_pace(base, 4), Duration::from_millis(4));
        assert_eq!(frame_pace(base, 0), base);
    }

    #[test]
    fn a_missing_gamepad_context_holds_nothing() {
        assert_eq!(poll_gamepad(&mut None), 0);
    }

    #[test]
    fn an_open_gamepad_context_only_uses_the_gba_bits() {
        let mut pads = open_gamepads();
        let mask = poll_gamepad(&mut pads);
        assert_eq!(mask & !0x03FF, 0);
    }

    #[test]
    fn rebinding_a_key_moves_it_and_roundtrips() {
        let mut map = KeyMap::default();
        map.bind_target(BindingTarget::Pad(PadButton::A), "k");
        assert_eq!(game_key_mask("k", &map), Some(1 << 0));
        assert_eq!(game_key_mask("x", &map), None);
        map.bind_target(BindingTarget::Pad(PadButton::B), "K");
        assert_eq!(game_key_mask("k", &map), Some(1 << 1));
        assert_eq!(map.key(PadButton::A), "");
        map.bind_target(BindingTarget::Pad(PadButton::Reset), "escape");
        assert!(is_reset_key("r", &map));
        map.bind_target(BindingTarget::Pad(PadButton::A), "space");
        assert_eq!(map.key(PadButton::A), "");
        map.bind_target(BindingTarget::Pad(PadButton::A), "shift");
        assert_eq!(map.key(PadButton::A), "");
        let dir = std::env::temp_dir().join(format!(
            "ultima-keys-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("controls.json");
        write_controls(
            &path,
            &SavedControls {
                map: map.clone(),
                speed: 2,
                turbo: 8,
            },
        )
        .unwrap();
        let loaded = read_controls(&path).unwrap();
        assert_eq!(loaded.speed, 2);
        assert_eq!(loaded.turbo, 8);
        assert_eq!(loaded.map.key(PadButton::B), "k");
        assert_eq!(loaded.map.key(PadButton::A), "");
        assert_eq!(loaded.map.key(PadButton::Reset), "r");
        assert_eq!(loaded.map.hotkey(Hotkey::QuickSave), "f5");
        assert_eq!(loaded.map.hotkey(Hotkey::QuickLoad), "f8");
        assert_eq!(loaded.map.hotkey(Hotkey::FastForward), "tab");
        assert_eq!(loaded.map.hotkey(Hotkey::Rewind), "f6");
        let legacy = dir.join("legacy.json");
        std::fs::write(&legacy, r#"{"version":1,"bindings":{"a":"k"}}"#).unwrap();
        let legacy = read_controls(&legacy).unwrap();
        assert_eq!(legacy.speed, DEFAULT_SPEED);
        assert_eq!(legacy.turbo, DEFAULT_TURBO);
        assert_eq!(legacy.map.key(PadButton::A), "k");
        assert_eq!(legacy.map.hotkey(Hotkey::QuickSave), "f5");
        assert_eq!(legacy.map.hotkey(Hotkey::QuickLoad), "f8");
        assert_eq!(legacy.map.hotkey(Hotkey::FastForward), "tab");
        assert_eq!(legacy.map.hotkey(Hotkey::Rewind), "f6");
        assert_eq!(key_label("left"), "←");
        assert_eq!(key_label("f5"), "F5");
        assert_eq!(key_label(""), "未设置");
        assert_eq!(map.hotkey(Hotkey::QuickSave), "f5");
        assert_eq!(map.hotkey(Hotkey::QuickLoad), "f8");
        map.bind_target(BindingTarget::Hotkey(Hotkey::QuickSave), "k");
        assert_eq!(map.hotkey(Hotkey::QuickSave), "k");
        assert_eq!(map.key(PadButton::B), "");
        assert_eq!(map.hotkey_for_key("k"), Some(Hotkey::QuickSave));
        assert_eq!(game_key_mask("k", &map), None);
        map.bind_target(BindingTarget::Pad(PadButton::Reset), "f8");
        assert_eq!(map.hotkey(Hotkey::QuickLoad), "");
        assert!(is_reset_key("f8", &map));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saved_controls_drop_duplicates_and_reject_unknown_versions() {
        let dir = std::env::temp_dir().join(format!(
            "ultima-keys-file-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("missing.json");
        assert_eq!(read_controls(&missing).unwrap(), SavedControls::default());

        let duplicate = dir.join("duplicate.json");
        std::fs::write(
            &duplicate,
            r#"{"version":1,"bindings":{"a":"k","b":"k","quick_save":"k"}}"#,
        )
        .unwrap();
        let loaded = read_controls(&duplicate).unwrap();
        assert_eq!(loaded.map.key(PadButton::A), "k");
        assert_eq!(loaded.map.key(PadButton::B), "");
        assert_eq!(loaded.map.hotkey(Hotkey::QuickSave), "");

        let cleared = dir.join("cleared.json");
        std::fs::write(
            &cleared,
            r#"{"version":1,"bindings":{"a":"","b":"escape","right":"ArrowLeft"}}"#,
        )
        .unwrap();
        let loaded = read_controls(&cleared).unwrap();
        assert_eq!(loaded.map.key(PadButton::A), "");
        assert_eq!(loaded.map.key(PadButton::B), "z");
        assert_eq!(loaded.map.key(PadButton::Right), "left");
        assert_eq!(loaded.map.key(PadButton::Left), "");
        assert_eq!(game_key_mask("left", &loaded.map), Some(1 << 4));

        let broken = dir.join("broken.json");
        std::fs::write(&broken, "{").unwrap();
        let error = read_controls(&broken).unwrap_err();
        assert!(error.contains("解析键位"), "{error}");

        let future = dir.join("future.json");
        std::fs::write(&future, r#"{"version":2,"bindings":{}}"#).unwrap();
        let error = read_controls(&future).unwrap_err();
        assert!(error.contains("不支持的键位版本：2"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
