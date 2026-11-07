//! 按键。`0x04000130` 低 10 位是 0 表示按下，松开是 1。
//! `0x04000132` 选出参与判断的键。第 14 位打开中断，第 15 位要求选中的键全都按下。
//! 窗口每帧把键盘送进来。开机时全部松开。

use serde::{Deserialize, Serialize};

const RELEASED: u16 = 0x03FF;
const CONTROL_USED: u16 = 0xC3FF;
const IRQ_ENABLE: u16 = 1 << 14;
const IRQ_AND: u16 = 1 << 15;
const IF_KEY: u16 = 1 << 12;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Keypad {
    /// 1 表示这颗键正按着。
    pressed: u16,
    control: u16,
}

impl Keypad {
    pub(crate) fn new() -> Self {
        Self {
            pressed: 0,
            control: 0,
        }
    }

    pub(crate) fn set_pressed(&mut self, pressed: u16) {
        self.pressed = pressed & RELEASED;
    }

    pub(crate) fn read_byte(&self, offset: usize) -> u8 {
        let input = self.pressed ^ RELEASED;
        match offset {
            0 => input as u8,
            1 => (input >> 8) as u8,
            2 => self.control as u8,
            _ => (self.control >> 8) as u8,
        }
    }

    /// 写入 `KEYINPUT` 留不下。控制字改完后，调用方再看要不要置 `IF`。
    pub(crate) fn write_byte(&mut self, offset: usize, value: u8) {
        match offset {
            2 => self.control = (self.control & 0xFF00) | u16::from(value),
            3 => {
                self.control = (self.control & 0x00FF) | (u16::from(value) << 8) & CONTROL_USED;
            }
            _ => {}
        }
        self.control &= CONTROL_USED;
    }

    pub(crate) fn irq_mask(&self) -> u16 {
        if self.control & IRQ_ENABLE == 0 {
            return 0;
        }
        let selected = self.control & RELEASED;
        let pressed = self.pressed & selected;
        let hit = if self.control & IRQ_AND != 0 {
            selected != 0 && pressed == selected
        } else {
            pressed != 0
        };
        if hit {
            IF_KEY
        } else {
            0
        }
    }
}
