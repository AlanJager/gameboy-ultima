//! 耀西万有引力卡带上的两轴加速度。
//!
//! 寄存器在 SRAM 窗口，不在 GPIO。只有 `KYGJ`、`KYGE`、`KYGP` 启用。
//! 先向 `0x0E008000` 写 `0x55`，再向 `0x0E008100` 写 `0xAA`，才会锁存。
//! 静止是 `0x3A0`。按住一侧减或加 `0xE0`。X 的高半字节 bit 7 一直是 1，表示转换已经完成。

const UNLOCK: u32 = 0x8000;
const SAMPLE: u32 = 0x8100;
const X_LO: u32 = 0x8200;
const X_HI: u32 = 0x8300;
const Y_LO: u32 = 0x8400;
const Y_HI: u32 = 0x8500;

const REST: u16 = 0x3A0;
const STEP: i16 = 0xE0;
const IDLE: u16 = 0xFFF;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct Tilt {
    present: bool,
    armed: bool,
    x: u16,
    y: u16,
    x_rate: i8,
    y_rate: i8,
}

impl Default for Tilt {
    fn default() -> Self {
        Self {
            present: false,
            armed: false,
            x: IDLE,
            y: IDLE,
            x_rate: 0,
            y_rate: 0,
        }
    }
}

impl Tilt {
    pub(crate) fn set_present(&mut self, present: bool) {
        let x_rate = self.x_rate;
        let y_rate = self.y_rate;
        *self = Self {
            present,
            x_rate,
            y_rate,
            ..Self::default()
        };
    }

    pub(crate) fn set_axes(&mut self, x: i8, y: i8) {
        self.x_rate = x.clamp(-1, 1);
        self.y_rate = y.clamp(-1, 1);
    }

    pub(crate) fn read(&self, addr: u32) -> Option<u8> {
        if !self.present {
            return None;
        }
        let value = match addr & 0xFFFF {
            X_LO => self.x as u8,
            X_HI => ((self.x >> 8) as u8 & 0x0F) | 0x80,
            Y_LO => self.y as u8,
            Y_HI => (self.y >> 8) as u8 & 0x0F,
            _ => return None,
        };
        Some(value)
    }

    /// 这两个地址被传感器吃掉，不再折进 SRAM。
    pub(crate) fn write(&mut self, addr: u32, value: u8) -> bool {
        if !self.present {
            return false;
        }
        match addr & 0xFFFF {
            UNLOCK => {
                if value == 0x55 {
                    self.armed = true;
                }
                true
            }
            SAMPLE => {
                if value == 0xAA && self.armed {
                    self.armed = false;
                    self.x = axis(self.x_rate);
                    self.y = axis(self.y_rate);
                }
                true
            }
            _ => false,
        }
    }
}

fn axis(rate: i8) -> u16 {
    let delta = i16::from(rate) * STEP;
    REST.wrapping_add(delta as u16)
}

/// bit 0 左，bit 1 右，bit 2 上，bit 3 下。同一轴两边都按着是静止。
pub(crate) fn tilt_axes(bits: i8) -> (i8, i8) {
    (
        axis_rate(bits & 1 != 0, bits & 2 != 0),
        axis_rate(bits & 4 != 0, bits & 8 != 0),
    )
}

fn axis_rate(negative: bool, positive: bool) -> i8 {
    match (negative, positive) {
        (true, false) => -1,
        (false, true) => 1,
        _ => 0,
    }
}

pub(crate) fn tilt_cart(rom: &[u8]) -> bool {
    matches!(rom.get(0xAC..0xB0), Some(b"KYGJ" | b"KYGE" | b"KYGP"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Bus;

    fn sample(bus: &Bus) -> (u16, u16) {
        let x = u16::from(bus.read_u8(0x0E00_8200))
            | (u16::from(bus.read_u8(0x0E00_8300) & 0x0F) << 8);
        let y = u16::from(bus.read_u8(0x0E00_8400))
            | (u16::from(bus.read_u8(0x0E00_8500) & 0x0F) << 8);
        (x, y)
    }

    fn latch(bus: &mut Bus) {
        bus.write_u8(0x0E00_8000, 0x55);
        bus.write_u8(0x0E00_8100, 0xAA);
    }

    #[test]
    fn both_directions_on_one_axis_stay_at_rest() {
        assert_eq!(tilt_axes(0), (0, 0));
        assert_eq!(tilt_axes(0b0001), (-1, 0));
        assert_eq!(tilt_axes(0b0010), (1, 0));
        assert_eq!(tilt_axes(0b0100), (0, -1));
        assert_eq!(tilt_axes(0b1000), (0, 1));
        assert_eq!(tilt_axes(0b1111), (0, 0));
    }

    #[test]
    fn a_yoshi_cart_latches_both_axes_without_touching_sram() {
        let mut bus = Bus::new();
        let mut rom = vec![0; 0xB0];
        rom[0xAC..0xB0].copy_from_slice(b"KYGE");
        bus.load_rom(&rom);
        assert_eq!(sample(&bus), (IDLE, IDLE));
        assert_eq!(bus.read_u8(0x0E00_8300) & 0x80, 0x80);

        bus.write_u8(0x0E00_8100, 0xAA);
        assert_eq!(sample(&bus), (IDLE, IDLE));

        latch(&mut bus);
        assert_eq!(sample(&bus), (REST, REST));
        assert_eq!(bus.read_u8(0x0E00_0000), 0xFF);

        bus.set_tilt(-1, 1);
        latch(&mut bus);
        assert_eq!(sample(&bus), (REST - STEP as u16, REST + STEP as u16));
        assert_eq!(bus.read_u8(0x0E00_0123), 0xFF);
        bus.write_u8(0x0E00_8123, 0x3C);
        assert_eq!(bus.read_u8(0x0E00_0123), 0x3C);
    }

    #[test]
    fn another_cart_still_mirrors_that_address_into_sram() {
        let mut bus = Bus::new();
        let mut rom = vec![0; 0xB0];
        rom[0xAC..0xB0].copy_from_slice(b"TEST");
        bus.load_rom(&rom);
        bus.write_u8(0x0E00_8000, 0x55);
        assert_eq!(bus.read_u8(0x0E00_0000), 0x55);
        assert!(!tilt_cart(&rom));
        rom[0xAC..0xB0].copy_from_slice(b"KYGJ");
        assert!(tilt_cart(&rom));
    }
}
