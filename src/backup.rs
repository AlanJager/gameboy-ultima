// 存档芯片
use serde::{Deserialize, Serialize};

// `data` 就是芯片里的字节，`.sav` 整段写的就是它。
// 总线地址不另存一份。`cmd`、`mode`、`bank`、`dirty`、`locked` 和 `eeprom_out` 只是访问时的状态。
//
// SRAM，32,768 字节，挂在 `0x0E000000`。只取低 15 位，写到 `0x0E008000` 会折回开头：
//   data[addr & 0x7FFF]
//
// Flash，65,536 或 131,072 字节，也挂在 `0x0E000000`。窗口一次只看见 65,536 字节：
//   64KB：  data[addr & 0xFFFF]
//   128KB： data[bank * 0x10000 + (addr & 0xFFFF)]，bank 为 0 或 1
//   擦除按 4,096 字节一块。64KB 有 16 块，128KB 有 32 块。
//
// EEPROM，512 或 8,192 字节。不按总线字节存放，DMA3 每次读写连续的 8 字节：
//   data[address * 8 .. address * 8 + 8]
//   512 字节有 64 块，命令里的地址是 6 位。8,192 字节有 1,024 块，地址是 14 位。
const SRAM_SIZE: usize = 32 * 1024;
const FLASH_64: usize = 64 * 1024;
const FLASH_128: usize = 128 * 1024;
const EEPROM_SMALL: usize = 512;
const EEPROM_LARGE: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Kind {
    Guess,
    Sram,
    Flash,
    Eeprom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Cmd {
    Ready,
    Aa,
    Aa55,
    Program,
    Erase,
    EraseAa,
    EraseAa55,
    Bank,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum ReadMode {
    Data,
    Id,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Backup {
    kind: Kind,
    data: Vec<u8>,
    cmd: Cmd,
    mode: ReadMode,
    bank: u8,
    dirty: bool,
    /// 从文件读入之后，探测序列不再把芯片换成另一种。
    locked: bool,
    eeprom_out: Vec<u8>,
    eeprom_pos: usize,
}

impl Backup {
    pub(crate) fn new() -> Self {
        Self {
            kind: Kind::Guess,
            data: vec![0xFF; SRAM_SIZE],
            cmd: Cmd::Ready,
            mode: ReadMode::Data,
            bank: 0,
            dirty: false,
            locked: false,
            eeprom_out: Vec::new(),
            eeprom_pos: 0,
        }
    }

    pub(crate) fn load(&mut self, bytes: &[u8]) {
        self.cmd = Cmd::Ready;
        self.mode = ReadMode::Data;
        self.bank = 0;
        self.dirty = false;
        self.locked = true;
        self.eeprom_out.clear();
        self.eeprom_pos = 0;
        match bytes.len() {
            EEPROM_SMALL | EEPROM_LARGE => {
                self.kind = Kind::Eeprom;
                self.data = bytes.to_vec();
            }
            FLASH_64 | FLASH_128 => {
                self.kind = Kind::Flash;
                self.data = bytes.to_vec();
            }
            _ => {
                self.kind = Kind::Sram;
                self.data = vec![0xFF; SRAM_SIZE];
                let n = bytes.len().min(SRAM_SIZE);
                self.data[..n].copy_from_slice(&bytes[..n]);
            }
        }
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.data
    }

    pub(crate) fn dirty(&self) -> bool {
        self.dirty
    }

    pub(crate) fn read(&self, addr: u32) -> u8 {
        match self.kind {
            Kind::Eeprom => 0xFF,
            Kind::Flash => self.read_flash(addr),
            Kind::Guess | Kind::Sram => self.data[(addr as usize) & (SRAM_SIZE - 1)],
        }
    }

    pub(crate) fn write(&mut self, addr: u32, value: u8) {
        if self.kind == Kind::Eeprom {
            return;
        }
        let off = addr & 0xFFFF;
        match self.cmd {
            Cmd::Ready => {
                if self.kind == Kind::Flash && value == 0xF0 {
                    self.mode = ReadMode::Data;
                    return;
                }
                if off == 0x5555 && value == 0xAA {
                    self.cmd = Cmd::Aa;
                }
                self.store_sram(addr, value);
            }
            Cmd::Aa => {
                if off == 0x2AAA && value == 0x55 {
                    self.cmd = Cmd::Aa55;
                } else if off == 0x5555 && value == 0xAA {
                    self.cmd = Cmd::Aa;
                } else {
                    self.cmd = Cmd::Ready;
                }
                self.store_sram(addr, value);
            }
            Cmd::Aa55 => self.finish_unlock(addr, off, value),
            Cmd::Program => {
                self.cmd = Cmd::Ready;
                if self.kind == Kind::Flash {
                    self.program(addr, value);
                } else {
                    self.store_sram(addr, value);
                }
            }
            Cmd::Bank => {
                self.cmd = Cmd::Ready;
                if self.kind == Kind::Flash && self.data.len() > FLASH_64 {
                    self.bank = value & 1;
                } else {
                    self.store_sram(addr, value);
                }
            }
            Cmd::Erase => {
                if off == 0x5555 && value == 0xAA {
                    self.cmd = Cmd::EraseAa;
                } else {
                    self.cmd = Cmd::Ready;
                }
                self.store_sram(addr, value);
            }
            Cmd::EraseAa => {
                if off == 0x2AAA && value == 0x55 {
                    self.cmd = Cmd::EraseAa55;
                } else {
                    self.cmd = Cmd::Ready;
                }
                self.store_sram(addr, value);
            }
            Cmd::EraseAa55 => {
                self.cmd = Cmd::Ready;
                if self.kind == Kind::Flash && value == 0x10 && off == 0x5555 {
                    self.fill(0xFF);
                } else if self.kind == Kind::Flash && value == 0x30 {
                    self.erase_sector(addr);
                } else {
                    self.store_sram(addr, value);
                }
            }
        }
    }

    /// DMA3 的半字传输落在 EEPROM 窗口里。ROM 大于 16MB 时只认最后 256 字节。
    pub(crate) fn claims_eeprom(&self, src: u32, dst: u32, rom_len: usize) -> bool {
        if self.locked && self.kind != Kind::Eeprom {
            return false;
        }
        if !matches!(self.kind, Kind::Guess | Kind::Eeprom) {
            return false;
        }
        self.eeprom_addr(src, rom_len) || self.eeprom_addr(dst, rom_len)
    }

    pub(crate) fn eeprom_submit(&mut self, bits: &[u8]) {
        if bits.len() < 9 || bits[0] != 1 {
            return;
        }
        let addr_bits: usize = if matches!(bits.len(), 17 | 81) || bits.len() > 81 {
            14
        } else if matches!(bits.len(), 9 | 73) {
            6
        } else {
            return;
        };
        if self.kind != Kind::Eeprom {
            self.kind = Kind::Eeprom;
            self.data = vec![0xFF; EEPROM_SMALL];
            self.dirty = false;
            self.cmd = Cmd::Ready;
        }
        if addr_bits == 14 {
            self.grow_eeprom();
        }
        let mut address = 0usize;
        for bit in &bits[2..2 + addr_bits] {
            address = (address << 1) | usize::from(*bit);
        }
        let blocks = self.data.len() / 8;
        if address >= blocks {
            return;
        }
        if bits[1] == 1 {
            self.queue_read(address);
        } else if bits[1] == 0 && bits.len() >= 2 + addr_bits + 64 {
            self.write_block(address, &bits[2 + addr_bits..2 + addr_bits + 64]);
        }
    }

    pub(crate) fn eeprom_out(&mut self) -> u8 {
        if self.eeprom_pos < self.eeprom_out.len() {
            let bit = self.eeprom_out[self.eeprom_pos];
            self.eeprom_pos += 1;
            bit
        } else {
            1
        }
    }

    pub(crate) fn in_window(addr: u32, rom_len: usize) -> bool {
        addr >> 24 == 0x0D && (rom_len <= 16 * 1024 * 1024 || addr >= 0x0DFF_FF00)
    }

    fn eeprom_addr(&self, addr: u32, rom_len: usize) -> bool {
        Self::in_window(addr, rom_len)
    }

    fn finish_unlock(&mut self, addr: u32, off: u32, value: u8) {
        self.cmd = Cmd::Ready;
        if value == 0xF0 {
            self.mode = ReadMode::Data;
            self.store_sram(addr, value);
            return;
        }
        if off != 0x5555 {
            self.store_sram(addr, value);
            return;
        }
        if !matches!(value, 0x90 | 0xA0 | 0x80 | 0xB0) {
            self.store_sram(addr, value);
            return;
        }
        if self.kind != Kind::Flash {
            if self.locked {
                self.store_sram(addr, value);
                return;
            }
            self.promote_flash();
        }
        match value {
            0x90 => self.mode = ReadMode::Id,
            0xA0 => self.cmd = Cmd::Program,
            0x80 => self.cmd = Cmd::Erase,
            0xB0 => self.cmd = Cmd::Bank,
            _ => {}
        }
    }

    fn promote_flash(&mut self) {
        self.kind = Kind::Flash;
        self.data = vec![0xFF; FLASH_128];
        self.bank = 0;
        self.mode = ReadMode::Data;
        self.dirty = false;
    }

    fn store_sram(&mut self, addr: u32, value: u8) {
        if self.kind == Kind::Flash {
            return;
        }
        let index = (addr as usize) & (SRAM_SIZE - 1);
        if self.data.len() != SRAM_SIZE {
            return;
        }
        if self.data[index] != value {
            self.data[index] = value;
            self.dirty = true;
        }
    }

    fn read_flash(&self, addr: u32) -> u8 {
        let off = addr & 0xFFFF;
        if self.mode == ReadMode::Id {
            if off == 0 {
                return if self.data.len() > FLASH_64 {
                    0xC2
                } else {
                    0xBF
                };
            }
            if off == 1 {
                return if self.data.len() > FLASH_64 {
                    0x09
                } else {
                    0xD4
                };
            }
        }
        self.data.get(self.phys(addr)).copied().unwrap_or(0xFF)
    }

    fn program(&mut self, addr: u32, value: u8) {
        let phys = self.phys(addr);
        if let Some(cell) = self.data.get_mut(phys) {
            let next = *cell & value;
            if *cell != next {
                *cell = next;
                self.dirty = true;
            }
        }
    }

    fn erase_sector(&mut self, addr: u32) {
        let start = self.phys(addr) & !0xFFF;
        let end = (start + 0x1000).min(self.data.len());
        if start >= self.data.len() {
            return;
        }
        self.fill_range(start, end, 0xFF);
    }

    fn fill(&mut self, value: u8) {
        self.fill_range(0, self.data.len(), value);
    }

    fn fill_range(&mut self, start: usize, end: usize, value: u8) {
        let mut changed = false;
        for cell in &mut self.data[start..end] {
            if *cell != value {
                *cell = value;
                changed = true;
            }
        }
        if changed {
            self.dirty = true;
        }
    }

    fn phys(&self, addr: u32) -> usize {
        let off = (addr as usize) & 0xFFFF;
        let bank = if self.data.len() > FLASH_64 {
            usize::from(self.bank)
        } else {
            0
        };
        bank * 0x10000 + off
    }

    fn grow_eeprom(&mut self) {
        if self.data.len() >= EEPROM_LARGE {
            return;
        }
        if self.locked && self.data.len() == EEPROM_SMALL {
            return;
        }
        self.data.resize(EEPROM_LARGE, 0xFF);
    }

    fn queue_read(&mut self, address: usize) {
        self.eeprom_out.clear();
        self.eeprom_out.extend_from_slice(&[0, 0, 0, 0]);
        let start = address * 8;
        for byte in &self.data[start..start + 8] {
            for bit in (0..8).rev() {
                self.eeprom_out.push((byte >> bit) & 1);
            }
        }
        self.eeprom_pos = 0;
    }

    fn write_block(&mut self, address: usize, bits: &[u8]) {
        let start = address * 8;
        for (index, chunk) in bits.chunks(8).take(8).enumerate() {
            let mut byte = 0u8;
            for bit in chunk {
                byte = (byte << 1) | bit;
            }
            let cell = &mut self.data[start + index];
            if *cell != byte {
                *cell = byte;
                self.dirty = true;
            }
        }
        self.eeprom_out.clear();
        self.eeprom_pos = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Bus;

    fn flash_unlock(bus: &mut Bus, third: u8) {
        bus.write_u8(0x0E00_5555, 0xAA);
        bus.write_u8(0x0E00_2AAA, 0x55);
        bus.write_u8(0x0E00_5555, third);
    }

    #[test]
    fn sram_stores_a_byte_and_starts_blank() {
        let mut bus = Bus::new();
        assert_eq!(bus.read_u8(0x0E00_0000), 0xFF);
        bus.write_u8(0x0E00_0123, 0x3C);
        assert_eq!(bus.read_u8(0x0E00_0123), 0x3C);
        assert_eq!(bus.read_u8(0x0E00_8123), 0x3C);
        assert!(bus.backup_dirty());
        assert_eq!(bus.backup_bytes().len(), SRAM_SIZE);
    }

    #[test]
    fn flash_identifies_erases_and_programs() {
        let mut bus = Bus::new();
        flash_unlock(&mut bus, 0x90);
        assert_eq!(bus.read_u8(0x0E00_0000), 0xC2);
        assert_eq!(bus.read_u8(0x0E00_0001), 0x09);
        assert!(!bus.backup_dirty());
        bus.write_u8(0x0E00_0000, 0xF0);
        assert_eq!(bus.read_u8(0x0E00_0000), 0xFF);

        flash_unlock(&mut bus, 0xA0);
        bus.write_u8(0x0E00_0010, 0x12);
        assert_eq!(bus.read_u8(0x0E00_0010), 0x12);
        flash_unlock(&mut bus, 0xA0);
        bus.write_u8(0x0E00_0010, 0xFF);
        assert_eq!(bus.read_u8(0x0E00_0010), 0x12);

        flash_unlock(&mut bus, 0x80);
        flash_unlock(&mut bus, 0x10);
        assert_eq!(bus.read_u8(0x0E00_0010), 0xFF);
        assert_eq!(bus.backup_bytes().len(), FLASH_128);
    }

    #[test]
    fn flash_bank_and_sector_stay_apart() {
        let mut bus = Bus::new();
        flash_unlock(&mut bus, 0xB0);
        bus.write_u8(0x0E00_0000, 1);
        flash_unlock(&mut bus, 0xA0);
        bus.write_u8(0x0E00_0020, 0xAB);
        flash_unlock(&mut bus, 0xB0);
        bus.write_u8(0x0E00_0000, 0);
        assert_eq!(bus.read_u8(0x0E00_0020), 0xFF);
        flash_unlock(&mut bus, 0xB0);
        bus.write_u8(0x0E00_0000, 1);
        assert_eq!(bus.read_u8(0x0E00_0020), 0xAB);

        flash_unlock(&mut bus, 0xA0);
        bus.write_u8(0x0E00_1000, 0x77);
        flash_unlock(&mut bus, 0x80);
        bus.write_u8(0x0E00_5555, 0xAA);
        bus.write_u8(0x0E00_2AAA, 0x55);
        bus.write_u8(0x0E00_0020, 0x30);
        assert_eq!(bus.read_u8(0x0E00_0020), 0xFF);
        assert_eq!(bus.read_u8(0x0E00_1000), 0x77);
    }

    #[test]
    fn loaded_sram_ignores_the_flash_probe() {
        let mut bus = Bus::new();
        bus.load_backup(&vec![0xFF; SRAM_SIZE]);
        flash_unlock(&mut bus, 0x90);
        assert_eq!(bus.read_u8(0x0E00_0000), 0xFF);
        assert_eq!(bus.read_u8(0x0E00_5555), 0x90);
        assert_eq!(bus.backup_bytes().len(), SRAM_SIZE);
    }

    #[test]
    fn loaded_64k_flash_reports_sst() {
        let mut bus = Bus::new();
        bus.load_backup(&vec![0xFF; 64 * 1024]);
        flash_unlock(&mut bus, 0x90);
        assert_eq!(bus.read_u8(0x0E00_0000), 0xBF);
        assert_eq!(bus.read_u8(0x0E00_0001), 0xD4);
        assert_eq!(bus.backup_bytes().len(), 64 * 1024);
    }

    #[test]
    fn eeprom_round_trip_uses_dma3() {
        let mut bus = Bus::new();
        let mut bits = vec![1u8, 0];
        bits.extend([0, 0, 0, 0, 0, 1]);
        for byte in [0x01u8, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF] {
            for bit in (0..8).rev() {
                bits.push((byte >> bit) & 1);
            }
        }
        bits.push(0);
        dma_bits(&mut bus, 0x0200_0000, 0x0DFF_FF00, &bits);
        let mut request = vec![1u8, 1];
        request.extend([0, 0, 0, 0, 0, 1]);
        request.push(0);
        dma_bits(&mut bus, 0x0200_0000, 0x0DFF_FF00, &request);
        let read = dma_read(&mut bus, 0x0DFF_FF00, 0x0200_1000, 68);
        assert_eq!(&read[..4], &[0, 0, 0, 0]);
        assert_eq!(
            pack_bytes(&read[4..68]),
            [0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF]
        );
        assert_eq!(bus.backup_bytes().len(), EEPROM_SMALL);
        assert!(bus.backup_dirty());
    }

    #[test]
    fn wide_eeprom_address_grows_the_chip() {
        let mut bus = Bus::new();
        let mut bits = vec![1u8, 1];
        bits.extend(std::iter::repeat(0).take(14));
        bits.push(0);
        dma_bits(&mut bus, 0x0200_0000, 0x0DFF_FF00, &bits);
        assert_eq!(bus.backup_bytes().len(), EEPROM_LARGE);
    }

    fn dma_bits(bus: &mut Bus, source: u32, dest: u32, bits: &[u8]) {
        for (index, bit) in bits.iter().enumerate() {
            bus.write_u16(source + index as u32 * 2, u16::from(*bit));
        }
        arm_dma(bus, source, dest, bits.len() as u16);
    }

    fn dma_read(bus: &mut Bus, source: u32, dest: u32, count: u16) -> Vec<u8> {
        arm_dma(bus, source, dest, count);
        (0..count)
            .map(|index| bus.read_u16(dest + u32::from(index) * 2) as u8)
            .collect()
    }

    fn arm_dma(bus: &mut Bus, source: u32, dest: u32, count: u16) {
        bus.write_u32(0x0400_00D4, source);
        bus.write_u32(0x0400_00D8, dest);
        bus.write_u32(0x0400_00DC, u32::from(count) | (0x8000 << 16));
    }

    fn pack_bytes(bits: &[u8]) -> Vec<u8> {
        bits.chunks(8)
            .map(|chunk| chunk.iter().fold(0u8, |byte, bit| (byte << 1) | bit))
            .collect()
    }
}
