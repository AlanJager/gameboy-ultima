//! 卡带震动马达。瓦里奥制造 Twisted 和钻头娃娃把 GPIO bit 3 当作输出。
//!
//! 控制位打开、方向是输出、数据位为 1 时马达转。键盘没有振子，转的时候由画面晃一下来表示。

const DATA: u32 = 0xC4;
const DIRECTION: u32 = 0xC6;
const CONTROL: u32 = 0xC8;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct Rumble {
    present: bool,
    control: u8,
    direction: u8,
    data: u8,
    /// 这一帧里曾经转过。采样之后清掉，避免短脉冲被帧末的低电平吃掉。
    pulsed: bool,
}

impl Default for Rumble {
    fn default() -> Self {
        Self {
            present: false,
            control: 0,
            direction: 0,
            data: 0,
            pulsed: false,
        }
    }
}

impl Rumble {
    pub(crate) fn set_present(&mut self, present: bool) {
        *self = Self {
            present,
            ..Self::default()
        };
    }

    pub(crate) fn present(&self) -> bool {
        self.present
    }

    pub(crate) fn read(&self, addr: u32) -> Option<u8> {
        if !self.present || self.control & 1 == 0 {
            return None;
        }
        match addr & 0x01FF_FFFF {
            DATA => Some(self.data & self.direction),
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
            DATA => self.data = value & 0x0F,
            DIRECTION => self.direction = value & 0x0F,
            CONTROL => self.control = value & 1,
            _ => return,
        }
        if self.spinning() {
            self.pulsed = true;
        }
    }

    pub(crate) fn take(&mut self) -> bool {
        let active = self.spinning() || self.pulsed;
        self.pulsed = false;
        active
    }

    fn spinning(&self) -> bool {
        self.present && self.control & 1 != 0 && self.direction & 8 != 0 && self.data & 8 != 0
    }
}

pub(crate) fn rumble_cart(rom: &[u8]) -> bool {
    matches!(
        rom.get(0xAC..0xB0),
        Some(b"RZWJ" | b"RZWE" | b"RZWP" | b"V49J" | b"V49E" | b"V49P")
    )
}

#[cfg(test)]
mod tests {
    use crate::Bus;

    fn cart(code: &[u8]) -> Bus {
        let mut rom = vec![0u8; 0xB0];
        rom[0xAC..0xB0].copy_from_slice(code);
        let mut bus = Bus::new();
        bus.load_rom(&rom);
        bus
    }

    fn motor(bus: &mut Bus, on: bool) {
        if on {
            bus.write_u8(0x0800_00C8, 1);
            bus.write_u8(0x0800_00C6, 8);
            bus.write_u8(0x0800_00C4, 8);
        } else {
            bus.write_u8(0x0800_00C4, 0);
        }
    }

    #[test]
    fn only_twisted_and_drill_dozer_grow_a_motor() {
        assert!(cart(b"V49E").rumble_present());
        assert!(cart(b"RZWJ").rumble_present());
        assert!(!cart(b"AGBJ").rumble_present());
        assert!(!cart(b"ULTS").rumble_present());
    }

    #[test]
    fn the_motor_runs_only_while_bit_3_is_driven_high() {
        let mut bus = cart(b"V49J");
        assert!(!bus.take_rumble());
        motor(&mut bus, true);
        assert!(bus.take_rumble());
        assert!(bus.take_rumble());
        motor(&mut bus, false);
        assert!(!bus.take_rumble());

        bus.write_u8(0x0800_00C8, 1);
        bus.write_u8(0x0800_00C6, 0);
        bus.write_u8(0x0800_00C4, 8);
        assert!(!bus.take_rumble());

        bus.write_u8(0x0800_00C6, 8);
        bus.write_u8(0x0800_00C4, 8);
        bus.write_u8(0x0800_00C8, 0);
        assert!(bus.take_rumble());
        assert!(!bus.take_rumble());
    }

    #[test]
    fn a_pulse_inside_the_frame_is_still_visible_once() {
        let mut bus = cart(b"V49P");
        motor(&mut bus, true);
        motor(&mut bus, false);
        assert!(bus.take_rumble());
        assert!(!bus.take_rumble());
    }

    #[test]
    fn twisted_can_rumble_without_losing_the_gyro_sample() {
        let mut bus = cart(b"RZWE");
        bus.set_gyro_rate(0);
        bus.write_u8(0x0800_00C8, 1);
        bus.write_u8(0x0800_00C6, 0b1011);
        bus.write_u8(0x0800_00C4, 0b1010);
        bus.write_u8(0x0800_00C4, 0b1011);
        let mut sample = u16::from((bus.read_u8(0x0800_00C4) >> 2) & 1);
        bus.write_u8(0x0800_00C4, 0b1010);
        for _ in 0..15 {
            bus.write_u8(0x0800_00C4, 0b1000);
            sample = (sample << 1) | u16::from((bus.read_u8(0x0800_00C4) >> 2) & 1);
            bus.write_u8(0x0800_00C4, 0b1010);
        }
        assert_eq!(sample, 0x700);
        assert!(bus.take_rumble());
    }
}
