//! 我们的太阳卡带上的感光二极管。
//!
//! 和实时时钟共用 `0x080000C4`。只有 `U3IJ`、`U3IE`、`U3IP`、`U32J`、`U32E`、
//! `U32P`、`U33J` 启用。bit 2 拉高时这颗传感器让开，时钟继续用那三根线。
//! bit 1 拉高清零并锁存，bit 0 的上升沿加一。计数够到亮度之后，bit 3 拉高。
//! 亮度 0 是全黑，255 是最亮。游戏读到的是 `0xFF - 亮度`。

const DATA: u32 = 0xC4;
const DIRECTION: u32 = 0xC6;
const CONTROL: u32 = 0xC8;

const PIN_CLOCK: u8 = 1;
const PIN_RESET: u8 = 2;
const PIN_SELECT: u8 = 4;
const PIN_FLAG: u8 = 8;

pub const DEFAULT_SOLAR: u8 = 128;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct Solar {
    present: bool,
    control: u8,
    direction: u8,
    pins: u8,
    counter: u16,
    sample: u8,
    rise_ready: bool,
    flag: bool,
    brightness: u8,
}

impl Default for Solar {
    fn default() -> Self {
        Self {
            present: false,
            control: 0,
            direction: 0,
            pins: 0,
            counter: 0,
            sample: 0xFF,
            rise_ready: false,
            flag: false,
            brightness: DEFAULT_SOLAR,
        }
    }
}

impl Solar {
    pub(crate) fn set_present(&mut self, present: bool) {
        let brightness = self.brightness;
        *self = Self {
            present,
            brightness,
            ..Self::default()
        };
    }

    pub(crate) fn present(&self) -> bool {
        self.present
    }

    pub(crate) fn set_brightness(&mut self, brightness: u8) {
        self.brightness = brightness;
    }

    /// 传感器正在驱动 bit 3 时，返回 0 或 8。GBA 自己驱动这一位时不插手。
    pub(crate) fn driven_flag(&self) -> Option<u8> {
        if !self.present || self.control & 1 == 0 || self.direction & PIN_FLAG != 0 {
            return None;
        }
        Some(if self.flag { PIN_FLAG } else { 0 })
    }

    pub(crate) fn write(&mut self, addr: u32, value: u8) {
        if !self.present {
            return;
        }
        match addr & 0x01FF_FFFF {
            DATA => {
                let dir = self.direction & 0x0F;
                let next = (self.pins & !dir) | (value & dir);
                self.pins = next;
                self.scan(next);
            }
            DIRECTION => self.direction = value & 0x0F,
            CONTROL => self.control = value & 1,
            _ => {}
        }
    }

    fn scan(&mut self, pins: u8) {
        if pins & PIN_SELECT != 0 {
            return;
        }
        if pins & PIN_RESET != 0 {
            self.counter = 0;
            self.sample = 0xFF - self.brightness;
        }
        if pins & PIN_CLOCK != 0 && self.rise_ready {
            self.counter = self.counter.saturating_add(1);
        }
        self.rise_ready = pins & PIN_CLOCK == 0;
        self.flag = self.counter >= u16::from(self.sample);
    }
}

pub(crate) fn solar_cart(rom: &[u8]) -> bool {
    matches!(
        rom.get(0xAC..0xB0),
        Some(b"U3IJ" | b"U3IE" | b"U3IP" | b"U32J" | b"U32E" | b"U32P" | b"U33J")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Bus;

    fn arm(bus: &mut Bus) {
        bus.write_u8(0x0800_00C8, 1);
        bus.write_u8(0x0800_00C6, 0x7);
    }

    /// 和卡带上的采样循环一样：复位之后每个时钟看一次 bit 3，记下拉高之前的拍数。
    fn measure(bus: &mut Bus) -> u16 {
        arm(bus);
        bus.write_u8(0x0800_00C4, PIN_RESET);
        bus.write_u8(0x0800_00C4, 0);
        let mut count = 0u16;
        for _ in 0..0x100 {
            bus.write_u8(0x0800_00C4, PIN_CLOCK);
            bus.write_u8(0x0800_00C4, 0);
            if bus.read_u8(0x0800_00C4) & PIN_FLAG != 0 {
                break;
            }
            count += 1;
        }
        count
    }

    fn boktai() -> Bus {
        let mut bus = Bus::new();
        let mut rom = vec![0; 0xB0];
        rom[0xAC..0xB0].copy_from_slice(b"U3IE");
        bus.load_rom(&rom);
        bus
    }

    #[test]
    fn darkness_ramps_the_full_count_and_sunlight_trips_at_once() {
        let mut bus = boktai();
        bus.set_solar(0);
        assert_eq!(measure(&mut bus), 254);
        bus.set_solar(255);
        assert_eq!(measure(&mut bus), 0);
        bus.set_solar(0xFF - 0x50);
        assert_eq!(measure(&mut bus), 0x4F);
    }

    #[test]
    fn chip_select_lets_the_clock_keep_the_pins() {
        let mut bus = boktai();
        bus.set_solar(0);
        arm(&mut bus);
        bus.write_u8(0x0800_00C4, PIN_RESET);
        bus.write_u8(0x0800_00C4, 0);
        bus.write_u8(0x0800_00C4, PIN_CLOCK);
        bus.write_u8(0x0800_00C4, 0);
        for _ in 0..30 {
            bus.write_u8(0x0800_00C4, PIN_SELECT | PIN_CLOCK);
            bus.write_u8(0x0800_00C4, PIN_SELECT);
        }
        bus.write_u8(0x0800_00C4, 0);
        assert_eq!(bus.read_u8(0x0800_00C4) & PIN_FLAG, 0);
        assert_eq!(measure(&mut bus), 254);
    }

    #[test]
    fn another_cart_does_not_grow_a_light_sensor() {
        let mut bus = Bus::new();
        let mut rom = vec![0; 0xC9];
        rom[0xAC..0xB0].copy_from_slice(b"TEST");
        rom[0xC4] = 0xAB;
        bus.load_rom(&rom);
        assert!(!solar_cart(&rom));
        assert_eq!(bus.read_u8(0x0800_00C4), 0xAB);
        bus.write_u8(0x0800_00C8, 1);
        bus.write_u8(0x0800_00C6, 0x0F);
        bus.write_u8(0x0800_00C4, PIN_FLAG);
        assert_eq!(bus.read_u8(0x0800_00C4) & PIN_FLAG, PIN_FLAG);
        rom[0xAC..0xB0].copy_from_slice(b"U33J");
        assert!(solar_cart(&rom));
    }
}
