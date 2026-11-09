//! 瓦里奥制造 Twisted 卡带上的角速度计。
//!
//! 引脚和 RTC 一样挂在 `0x080000C4`。只有 `RZWJ`、`RZWE`、`RZWP` 启用，
//! 其它卡带仍走时钟。bit 0 拉高立刻送出最高位，bit 1 的下降沿再送下一位，
//! 数据在 bit 2。静止是 `0x700`。按住左键减 `0x180`，按住右键加 `0x180`。

const DATA: u32 = 0xC4;
const DIRECTION: u32 = 0xC6;
const CONTROL: u32 = 0xC8;

const REST: u16 = 0x700;
const STEP: i16 = 0x180;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct Gyro {
    present: bool,
    control: u8,
    direction: u8,
    write_latch: u8,
    pins: u8,
    sample: u16,
    clock_high: bool,
    rate: i8,
}

impl Default for Gyro {
    fn default() -> Self {
        Self {
            present: false,
            control: 0,
            direction: 0,
            write_latch: 0,
            pins: 0,
            sample: 0,
            clock_high: false,
            rate: 0,
        }
    }
}

impl Gyro {
    pub(crate) fn set_present(&mut self, present: bool) {
        *self = Self {
            present,
            rate: self.rate,
            ..Self::default()
        };
    }

    pub(crate) fn present(&self) -> bool {
        self.present
    }

    pub(crate) fn set_rate(&mut self, rate: i8) {
        self.rate = rate.clamp(-1, 1);
    }

    pub(crate) fn read(&self, addr: u32) -> Option<u8> {
        if !self.present || self.control & 1 == 0 {
            return None;
        }
        match addr & 0x01FF_FFFF {
            DATA => Some(self.pins),
            DIRECTION => Some(self.direction),
            CONTROL => Some(self.control),
            0xC5 | 0xC7 | 0xC9 => Some(0),
            _ => None,
        }
    }

    pub(crate) fn write(&mut self, addr: u32, value: u8) {
        if !self.present {
            return;
        }
        match addr & 0x01FF_FFFF {
            DATA => {
                self.write_latch = value & 0x0F;
                self.apply_latch();
            }
            DIRECTION => {
                self.direction = value & 0x0F;
                self.apply_latch();
            }
            CONTROL => self.control = value & 1,
            _ => {}
        }
    }

    fn apply_latch(&mut self) {
        self.pins &= !self.direction;
        self.pins |= self.write_latch & self.direction;
        self.shift();
    }

    fn shift(&mut self) {
        let clock_high = self.pins & 2 != 0;
        let falling = self.clock_high && !clock_high;
        let mut output = falling;
        if self.pins & 1 != 0 {
            self.sample = self.latched();
            output = true;
        }
        if output {
            let bit = (self.sample >> 15) & 1;
            self.sample <<= 1;
            let driven = (bit as u8) << 2;
            self.pins &= self.direction;
            self.pins |= driven & !self.direction & 0x0F;
        }
        self.clock_high = clock_high;
    }

    fn latched(&self) -> u16 {
        let delta = i16::from(self.rate) * STEP;
        REST.wrapping_add(delta as u16)
    }
}

pub(crate) fn gyro_cart(rom: &[u8]) -> bool {
    matches!(
        rom.get(0xAC..0xB0),
        Some(b"RZWJ" | b"RZWE" | b"RZWP")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Bus;

    fn arm(gyro: &mut Gyro) {
        gyro.set_present(true);
        gyro.write(CONTROL, 1);
        gyro.write(DIRECTION, 0b0011);
        gyro.write(DATA, 0b0010);
    }

    fn read_sample(gyro: &mut Gyro) -> u16 {
        arm(gyro);
        gyro.write(DATA, 0b0011);
        let mut value = u16::from((gyro.read(DATA).unwrap() >> 2) & 1);
        gyro.write(DATA, 0b0010);
        for _ in 0..15 {
            gyro.write(DATA, 0b0000);
            value = (value << 1) | u16::from((gyro.read(DATA).unwrap() >> 2) & 1);
            gyro.write(DATA, 0b0010);
        }
        value
    }

    #[test]
    fn a_still_gyro_shifts_out_the_rest_value() {
        let mut gyro = Gyro::default();
        assert_eq!(read_sample(&mut gyro), REST);
    }

    #[test]
    fn holding_a_side_moves_the_rate_and_releasing_returns_to_rest() {
        let mut gyro = Gyro::default();
        gyro.set_present(true);
        gyro.set_rate(1);
        assert_eq!(read_sample(&mut gyro), REST + STEP as u16);
        gyro.set_rate(-1);
        assert_eq!(read_sample(&mut gyro), REST - STEP as u16);
        gyro.set_rate(0);
        assert_eq!(read_sample(&mut gyro), REST);
    }

    #[test]
    fn the_port_stays_rom_until_this_cart_enables_it() {
        let mut bus = Bus::new();
        let mut rom = vec![0; 0xC6];
        rom[0xC4] = 0xAB;
        rom[0xAC..0xB0].copy_from_slice(b"TEST");
        bus.load_rom(&rom);
        assert_eq!(bus.read_u8(0x0800_00C4), 0xAB);
        rom[0xAC..0xB0].copy_from_slice(b"RZWE");
        bus.load_rom(&rom);
        bus.write_u8(0x0800_00C8, 1);
        bus.write_u8(0x0800_00C6, 0b0011);
        bus.write_u8(0x0800_00C4, 0b0010);
        bus.write_u8(0x0800_00C4, 0b0011);
        let first = (bus.read_u16(0x0800_00C4) >> 2) & 1;
        assert_eq!(first, 0);
        assert_eq!(bus.read_u16(0x0800_00C4) >> 8, 0);
    }
}
