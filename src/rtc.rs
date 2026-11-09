//! 卡带上的 S-3511 实时时钟。红宝石、蓝宝石用它记日期。
//!
//! 引脚挂在卡带 `0x080000C4` 起的三个半字上。控制位关掉时，读回去的仍是卡带里的字节。
//! 游戏把 `SCK`、`SIO`、`CS` 按位拨进来。命令低位先送，高半字节是 `0x6`。
//! 写过的时刻记成相对墙上时钟的偏移，所以关机之后时间还在走。

use serde::{Deserialize, Serialize};

const DATA: u32 = 0xC4;
const DIRECTION: u32 = 0xC6;
const CONTROL: u32 = 0xC8;

const PIN_SCK: u8 = 1;
const PIN_SIO: u8 = 2;
const PIN_CS: u8 = 4;

const STATUS_24H: u8 = 0x40;
const STATUS_POWER: u8 = 0x80;

const CMD_RESET: u8 = 0;
const CMD_STATUS: u8 = 1;
const CMD_DATETIME: u8 = 2;
const CMD_TIME: u8 = 3;
const CMD_ALARM: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
enum Phase {
    #[default]
    Idle,
    Command,
    Read,
    Write,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Rtc {
    /// 状态字节。最高位是掉电，游戏靠它判断要不要重新对时。
    status: u8,
    /// `芯片时刻 - 墙上时钟`，单位秒。
    delta: i64,
    alarm: [u8; 3],
    dirty: bool,
    control: u8,
    direction: u8,
    pins: u8,
    chip_sio: bool,
    phase: Phase,
    command: u8,
    bit_count: u8,
    byte_index: u8,
    payload: [u8; 7],
    length: u8,
    /// 命令的最后一拍落下时，还不能把读出的第一位移走。
    skip_fall: bool,
    #[serde(skip)]
    unix_override: Option<i64>,
}

impl Default for Rtc {
    fn default() -> Self {
        Self {
            status: STATUS_POWER,
            delta: 0,
            alarm: [0; 3],
            dirty: false,
            control: 0,
            direction: 0,
            pins: 0,
            chip_sio: false,
            phase: Phase::Idle,
            command: 0,
            bit_count: 0,
            byte_index: 0,
            payload: [0; 7],
            length: 0,
            skip_fall: false,
            unix_override: None,
        }
    }
}

impl Rtc {
    pub(crate) fn read(&self, addr: u32) -> Option<u8> {
        let off = addr & 0x01FF_FFFF;
        if self.control & 1 == 0 {
            return None;
        }
        match off {
            DATA => Some(self.pin_levels()),
            DIRECTION => Some(self.direction),
            CONTROL => Some(self.control),
            _ => None,
        }
    }

    pub(crate) fn write(&mut self, addr: u32, value: u8) {
        match addr & 0x01FF_FFFF {
            DATA => self.write_pins(value),
            DIRECTION => self.direction = value & 0x0F,
            CONTROL => self.control = value & 1,
            _ => {}
        }
    }

    pub(crate) fn dirty(&self) -> bool {
        self.dirty
    }

    pub(crate) fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(16);
        out.extend_from_slice(b"RTC1");
        out.push(self.status);
        out.extend_from_slice(&self.delta.to_le_bytes());
        out.extend_from_slice(&self.alarm);
        out
    }

    pub(crate) fn load(&mut self, bytes: &[u8]) -> bool {
        if bytes.len() != 16 || &bytes[..4] != b"RTC1" {
            return false;
        }
        let mut delta = [0u8; 8];
        delta.copy_from_slice(&bytes[5..13]);
        self.status = bytes[4];
        self.delta = i64::from_le_bytes(delta);
        self.alarm.copy_from_slice(&bytes[13..16]);
        self.dirty = false;
        true
    }

    #[cfg(test)]
    pub(crate) fn set_unix(&mut self, unix: i64) {
        self.unix_override = Some(unix);
    }

    fn pin_levels(&self) -> u8 {
        let mut pins = self.pins & 0x0F;
        if self.direction & PIN_SIO == 0 {
            pins &= !PIN_SIO;
            if self.chip_sio {
                pins |= PIN_SIO;
            }
        }
        pins
    }

    fn write_pins(&mut self, value: u8) {
        let dir = self.direction & 0x0F;
        let prev = self.pins;
        let next = (prev & !dir) | (value & dir);
        if prev & PIN_CS != 0 && next & PIN_CS == 0 {
            self.phase = Phase::Idle;
            self.pins = next;
            return;
        }
        if prev & PIN_CS == 0 && next & PIN_CS != 0 {
            self.begin_command();
        }
        self.pins = next;
        if next & PIN_CS == 0 {
            return;
        }
        let rise = prev & PIN_SCK == 0 && next & PIN_SCK != 0;
        let fall = prev & PIN_SCK != 0 && next & PIN_SCK == 0;
        if rise {
            self.on_rise();
        } else if fall {
            self.on_fall();
        }
    }

    fn begin_command(&mut self) {
        self.phase = Phase::Command;
        self.command = 0;
        self.bit_count = 0;
        self.byte_index = 0;
        self.skip_fall = false;
    }

    fn on_rise(&mut self) {
        let bit = u8::from(self.pins & PIN_SIO != 0);
        match self.phase {
            Phase::Command => {
                self.command |= bit << self.bit_count;
                self.bit_count += 1;
                if self.bit_count == 8 {
                    self.finish_command();
                }
            }
            Phase::Write => {
                let index = self.byte_index as usize;
                if index < self.payload.len() {
                    self.payload[index] |= bit << self.bit_count;
                }
                self.bit_count += 1;
                if self.bit_count == 8 {
                    self.bit_count = 0;
                    self.byte_index += 1;
                    if self.byte_index >= self.length {
                        self.apply_write();
                        self.phase = Phase::Idle;
                    }
                }
            }
            Phase::Read | Phase::Idle => {}
        }
    }

    fn on_fall(&mut self) {
        if self.phase != Phase::Read {
            return;
        }
        if self.skip_fall {
            self.skip_fall = false;
            return;
        }
        self.bit_count += 1;
        if self.bit_count == 8 {
            self.bit_count = 0;
            self.byte_index += 1;
            if self.byte_index >= self.length {
                self.phase = Phase::Idle;
                self.chip_sio = false;
                return;
            }
        }
        self.present_read_bit();
    }

    fn finish_command(&mut self) {
        if self.command & 0xF0 != 0x60 {
            self.phase = Phase::Idle;
            return;
        }
        let kind = (self.command >> 1) & 7;
        let read = self.command & 1 != 0;
        self.bit_count = 0;
        self.byte_index = 0;
        self.payload = [0; 7];
        if read {
            self.length = self.fill_read(kind);
            if self.length == 0 {
                self.phase = Phase::Idle;
                return;
            }
            self.phase = Phase::Read;
            self.skip_fall = true;
            self.present_read_bit();
            return;
        }
        if kind == CMD_RESET {
            self.status |= STATUS_POWER;
            self.dirty = true;
            self.phase = Phase::Idle;
            return;
        }
        self.length = match kind {
            CMD_STATUS => 1,
            CMD_DATETIME => 7,
            CMD_TIME => 3,
            CMD_ALARM => 3,
            _ => 0,
        };
        if self.length == 0 {
            self.phase = Phase::Idle;
            return;
        }
        self.phase = Phase::Write;
    }

    fn fill_read(&mut self, kind: u8) -> u8 {
        match kind {
            CMD_STATUS => {
                self.payload[0] = self.status;
                1
            }
            CMD_DATETIME => {
                self.payload = self.datetime_bytes();
                7
            }
            CMD_TIME => {
                let all = self.datetime_bytes();
                self.payload[..3].copy_from_slice(&all[4..7]);
                3
            }
            CMD_ALARM => {
                self.payload[..3].copy_from_slice(&self.alarm);
                3
            }
            _ => 0,
        }
    }

    fn present_read_bit(&mut self) {
        let index = self.byte_index as usize;
        let byte = self.payload.get(index).copied().unwrap_or(0);
        self.chip_sio = (byte >> self.bit_count) & 1 != 0;
    }

    fn apply_write(&mut self) {
        match (self.command >> 1) & 7 {
            CMD_STATUS => {
                self.status = self.payload[0];
                self.dirty = true;
            }
            CMD_DATETIME => {
                if let Some(unix) = datetime_to_unix(self.payload, self.status) {
                    self.delta = unix - self.now();
                    self.status &= !STATUS_POWER;
                    self.dirty = true;
                }
            }
            CMD_TIME => {
                let mut parts = self.datetime_bytes();
                let hour = decode_hour(self.payload[0], self.status);
                parts[4] = to_bcd(hour);
                parts[5] = self.payload[1];
                parts[6] = self.payload[2];
                if let Some(unix) = datetime_to_unix(parts, self.status | STATUS_24H) {
                    self.delta = unix - self.now();
                    self.dirty = true;
                }
            }
            CMD_ALARM => {
                self.alarm.copy_from_slice(&self.payload[..3]);
                self.dirty = true;
            }
            _ => {}
        }
    }

    fn datetime_bytes(&self) -> [u8; 7] {
        let (year, month, day, dow, hour, minute, second) = parts_from_unix(self.now() + self.delta);
        [
            to_bcd(year % 100),
            to_bcd(month),
            to_bcd(day),
            to_bcd(dow),
            encode_hour(hour, self.status),
            to_bcd(minute),
            to_bcd(second),
        ]
    }

    fn now(&self) -> i64 {
        if let Some(unix) = self.unix_override {
            return unix;
        }
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs() as i64)
            .unwrap_or(0)
    }
}

fn to_bcd(value: u32) -> u8 {
    ((value / 10) << 4 | (value % 10)) as u8
}

fn from_bcd(value: u8) -> u32 {
    u32::from(value >> 4) * 10 + u32::from(value & 0x0F)
}

fn encode_hour(hour: u32, status: u8) -> u8 {
    if status & STATUS_24H != 0 {
        return to_bcd(hour);
    }
    let pm = hour >= 12;
    let mut twelve = hour % 12;
    if twelve == 0 {
        twelve = 12;
    }
    let mut byte = to_bcd(twelve);
    if pm {
        byte |= 0x80;
    }
    byte
}

fn decode_hour(byte: u8, status: u8) -> u32 {
    if status & STATUS_24H != 0 {
        return from_bcd(byte);
    }
    let pm = byte & 0x80 != 0;
    let mut hour = from_bcd(byte & 0x3F);
    if hour == 12 {
        hour = 0;
    }
    if pm {
        hour += 12;
    }
    hour
}

fn datetime_to_unix(bytes: [u8; 7], status: u8) -> Option<i64> {
    let year = 2000 + from_bcd(bytes[0]) as i32;
    let month = from_bcd(bytes[1]);
    let day = from_bcd(bytes[2]);
    let hour = decode_hour(bytes[4], status);
    let minute = from_bcd(bytes[5]);
    let second = from_bcd(bytes[6]);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 59
    {
        return None;
    }
    let days = days_from_civil(year, month, day)?;
    Some(days * 86400 + i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second))
}

fn parts_from_unix(unix: i64) -> (u32, u32, u32, u32, u32, u32, u32) {
    let days = unix.div_euclid(86400);
    let tod = unix.rem_euclid(86400) as u32;
    let (year, month, day) = civil_from_days(days);
    let dow = (days + 4).rem_euclid(7) as u32;
    let hour = tod / 3600;
    let minute = (tod % 3600) / 60;
    let second = tod % 60;
    (year, month, day, dow, hour, minute, second)
}

/// 公历换算，Howard Hinnant 的 `civil_from_days` / `days_from_civil`。
fn civil_from_days(days_unix: i64) -> (u32, u32, u32) {
    let z = days_unix + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    (year as u32, month as u32, day as u32)
}

fn days_from_civil(year: i32, month: u32, day: u32) -> Option<i64> {
    if month == 0 || month > 12 || day == 0 || day > 31 {
        return None;
    }
    let year = i64::from(year) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = (year - era * 400) as u64;
    let month_index = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * u64::from(month_index) + 2) / 5 + u64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe as i64 - 719_468)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Bus;

    const PORT: u32 = 0x0800_0000;
    const Y2K: i64 = 946_684_800;

    fn port(bus: &mut Bus) {
        bus.write_u16(PORT + CONTROL, 1);
        bus.write_u16(PORT + DIRECTION, u16::from(PIN_SCK | PIN_SIO | PIN_CS));
    }

    fn select(bus: &mut Bus) {
        bus.write_u16(PORT + DATA, 0);
        bus.write_u16(PORT + DATA, u16::from(PIN_CS));
    }

    fn deselect(bus: &mut Bus) {
        bus.write_u16(PORT + DATA, 0);
    }

    fn send_byte(bus: &mut Bus, byte: u8) {
        for bit in 0..8 {
            let sio = if (byte >> bit) & 1 != 0 { PIN_SIO } else { 0 };
            bus.write_u16(PORT + DATA, u16::from(PIN_CS | sio));
            bus.write_u16(PORT + DATA, u16::from(PIN_CS | PIN_SCK | sio));
            bus.write_u16(PORT + DATA, u16::from(PIN_CS | sio));
        }
    }

    fn read_byte(bus: &mut Bus) -> u8 {
        bus.write_u16(PORT + DIRECTION, u16::from(PIN_SCK | PIN_CS));
        let mut byte = 0;
        for bit in 0..8 {
            bus.write_u16(PORT + DATA, u16::from(PIN_CS | PIN_SCK));
            if bus.read_u16(PORT + DATA) & u16::from(PIN_SIO) != 0 {
                byte |= 1 << bit;
            }
            bus.write_u16(PORT + DATA, u16::from(PIN_CS));
        }
        bus.write_u16(PORT + DIRECTION, u16::from(PIN_SCK | PIN_SIO | PIN_CS));
        byte
    }

    fn command(bus: &mut Bus, byte: u8) {
        select(bus);
        send_byte(bus, byte);
    }

    #[test]
    fn gpio_stays_out_of_the_rom_until_the_port_is_enabled() {
        let mut bus = Bus::new();
        let mut rom = vec![0; 0xD0];
        rom[0xC4] = 0xAB;
        bus.load_rom(&rom);
        assert_eq!(bus.read_u8(PORT + DATA), 0xAB);
        bus.write_u16(PORT + CONTROL, 1);
        assert_eq!(bus.read_u8(PORT + DATA), 0);
        assert_eq!(bus.read_u8(PORT + 0x100), 0);
    }

    #[test]
    fn a_written_datetime_advances_with_the_wall_clock() {
        let mut bus = Bus::new();
        bus.set_rtc_unix(Y2K);
        port(&mut bus);
        command(&mut bus, 0x62);
        send_byte(&mut bus, STATUS_24H);
        deselect(&mut bus);

        command(&mut bus, 0x64);
        for byte in [0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00] {
            send_byte(&mut bus, byte);
        }
        deselect(&mut bus);

        command(&mut bus, 0x65);
        let mut got = [0u8; 7];
        for byte in &mut got {
            *byte = read_byte(&mut bus);
        }
        deselect(&mut bus);
        assert_eq!(got, [0x00, 0x01, 0x01, 0x06, 0x00, 0x00, 0x00]);

        bus.set_rtc_unix(Y2K + 3661);
        command(&mut bus, 0x65);
        for byte in &mut got {
            *byte = read_byte(&mut bus);
        }
        assert_eq!(got, [0x00, 0x01, 0x01, 0x06, 0x01, 0x01, 0x01]);
    }

    #[test]
    fn reset_marks_the_clock_unreliable_until_it_is_set_again() {
        let mut bus = Bus::new();
        bus.set_rtc_unix(Y2K);
        port(&mut bus);
        command(&mut bus, 0x63);
        assert_eq!(read_byte(&mut bus) & STATUS_POWER, STATUS_POWER);
        deselect(&mut bus);

        command(&mut bus, 0x62);
        send_byte(&mut bus, STATUS_24H);
        deselect(&mut bus);
        command(&mut bus, 0x63);
        assert_eq!(read_byte(&mut bus), STATUS_24H);
        deselect(&mut bus);

        command(&mut bus, 0x60);
        deselect(&mut bus);
        command(&mut bus, 0x63);
        assert_eq!(read_byte(&mut bus) & STATUS_POWER, STATUS_POWER);
    }

    #[test]
    fn the_saved_offset_comes_back_after_a_reload() {
        let mut bus = Bus::new();
        bus.set_rtc_unix(Y2K);
        port(&mut bus);
        command(&mut bus, 0x62);
        send_byte(&mut bus, STATUS_24H);
        deselect(&mut bus);
        command(&mut bus, 0x64);
        for byte in [0x26, 0x11, 0x09, 0x01, 0x16, 0x30, 0x00] {
            send_byte(&mut bus, byte);
        }
        deselect(&mut bus);
        assert!(bus.rtc_dirty());
        let saved = bus.rtc_bytes().expect("时钟被写过");

        let mut restored = Bus::new();
        restored.set_rtc_unix(Y2K + 86_400);
        assert!(restored.load_rtc(&saved));
        assert!(!restored.rtc_dirty());
        port(&mut restored);
        command(&mut restored, 0x65);
        let mut got = [0u8; 7];
        for byte in &mut got {
            *byte = read_byte(&mut restored);
        }
        assert_eq!(got[0], 0x26);
        assert_eq!(got[1], 0x11);
        assert_eq!(got[2], 0x10);
        assert_eq!(got[4], 0x16);
        assert_eq!(got[5], 0x30);
    }

    #[test]
    fn y2k_is_saturday_and_roundtrips_through_civil_days() {
        let (year, month, day, dow, hour, minute, second) = parts_from_unix(Y2K);
        assert_eq!((year, month, day, dow, hour, minute, second), (2000, 1, 1, 6, 0, 0, 0));
        assert_eq!(days_from_civil(2000, 1, 1), Some(Y2K / 86400));
        let later = parts_from_unix(Y2K + 86_400);
        assert_eq!((later.0, later.1, later.2), (2000, 1, 2));
    }
}
