//! 图块索引和精灵行表。
//!
//! 图块槽里是 8×8 的颜色索引，不是调色板查完的颜色。
//! 行表是这一行有哪些精灵。细节见 `doc/tile-sprite-cache.md`。

const SLOTS: usize = 512;
const OBJ_COUNT: usize = 128;
const LINES: usize = 160;

#[derive(Clone)]
struct TileSlot {
    live: bool,
    addr: u32,
    index: [u8; 64],
}

impl Default for TileSlot {
    fn default() -> Self {
        Self {
            live: false,
            addr: 0,
            index: [0; 64],
        }
    }
}

#[derive(Clone)]
pub(crate) struct Gfx {
    slots4: Box<[TileSlot]>,
    slots8: Box<[TileSlot]>,
    line_ok: [bool; LINES],
    line_len: [u8; LINES],
    line_ids: Box<[[u8; OBJ_COUNT]; LINES]>,
    /// 测试用来走没缓存的取样，好和正式路径对像素。
    raw: bool,
    #[cfg(test)]
    tile_hits: u64,
    #[cfg(test)]
    tile_misses: u64,
    #[cfg(test)]
    sprite_hits: u64,
    #[cfg(test)]
    sprite_scans: u64,
}

impl std::fmt::Debug for Gfx {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt.debug_struct("Gfx").field("raw", &self.raw).finish()
    }
}

impl Gfx {
    pub(crate) fn new() -> Self {
        Self {
            slots4: vec![TileSlot::default(); SLOTS].into_boxed_slice(),
            slots8: vec![TileSlot::default(); SLOTS].into_boxed_slice(),
            line_ok: [false; LINES],
            line_len: [0; LINES],
            line_ids: vec![[0; OBJ_COUNT]; LINES]
                .into_boxed_slice()
                .try_into()
                .ok()
                .expect("160 行"),
            raw: false,
            #[cfg(test)]
            tile_hits: 0,
            #[cfg(test)]
            tile_misses: 0,
            #[cfg(test)]
            sprite_hits: 0,
            #[cfg(test)]
            sprite_scans: 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn set_raw(&mut self, raw: bool) {
        self.raw = raw;
    }

    #[cfg(test)]
    pub(crate) fn tile_hits(&self) -> u64 {
        self.tile_hits
    }

    #[cfg(test)]
    pub(crate) fn tile_misses(&self) -> u64 {
        self.tile_misses
    }

    #[cfg(test)]
    pub(crate) fn sprite_hits(&self) -> u64 {
        self.sprite_hits
    }

    #[cfg(test)]
    pub(crate) fn sprite_scans(&self) -> u64 {
        self.sprite_scans
    }

    /// 一个像素的颜色索引。没命中才把整块 8×8 拆进槽里。
    pub(crate) fn index(
        &mut self,
        vram: &[u8],
        addr: usize,
        bpp8: bool,
        sx: usize,
        sy: usize,
    ) -> u8 {
        if sx >= 8 || sy >= 8 {
            return 0;
        }
        if self.raw {
            return raw_index(vram, addr, bpp8, sx, sy);
        }
        let slots = if bpp8 {
            &mut self.slots8
        } else {
            &mut self.slots4
        };
        let slot_at = slot_of(addr);
        let slot = &slots[slot_at];
        if slot.live && slot.addr == addr as u32 {
            #[cfg(test)]
            {
                self.tile_hits += 1;
            }
            return slot.index[sy * 8 + sx];
        }
        #[cfg(test)]
        {
            self.tile_misses += 1;
        }
        let mut index = [0u8; 64];
        decode_tile(vram, addr, bpp8, &mut index);
        let pixel = index[sy * 8 + sx];
        slots[slot_at] = TileSlot {
            live: true,
            addr: addr as u32,
            index,
        };
        pixel
    }

    /// 这一字节盖住的 4bpp 槽和 8bpp 槽，地址对上才丢掉。
    pub(crate) fn invalidate_tile_byte(&mut self, off: usize) {
        let four = off & !31;
        let slot4 = slot_of(four);
        if self.slots4[slot4].live && self.slots4[slot4].addr == four as u32 {
            self.slots4[slot4].live = false;
        }
        let eight = off & !63;
        let slot8 = slot_of(eight);
        if self.slots8[slot8].live && self.slots8[slot8].addr == eight as u32 {
            self.slots8[slot8].live = false;
        }
    }

    pub(crate) fn invalidate_tiles(&mut self) {
        for slot in self.slots4.iter_mut().chain(self.slots8.iter_mut()) {
            slot.live = false;
        }
    }

    /// 行表还在就直接拷。否则调用 `scan` 再记下。
    pub(crate) fn sprites_of_line(
        &mut self,
        y: usize,
        out: &mut [u8; OBJ_COUNT],
        scan: impl FnOnce(&mut [u8; OBJ_COUNT]) -> usize,
    ) -> usize {
        if !self.raw && y < LINES && self.line_ok[y] {
            let count = usize::from(self.line_len[y]);
            out[..count].copy_from_slice(&self.line_ids[y][..count]);
            #[cfg(test)]
            {
                self.sprite_hits += 1;
            }
            return count;
        }
        let count = scan(out);
        if !self.raw && y < LINES {
            let count = count.min(OBJ_COUNT);
            self.line_ids[y][..count].copy_from_slice(&out[..count]);
            self.line_len[y] = count as u8;
            self.line_ok[y] = true;
            #[cfg(test)]
            {
                self.sprite_scans += 1;
            }
        }
        count
    }

    pub(crate) fn invalidate_sprite_lines(&mut self) {
        self.line_ok = [false; LINES];
    }
}

fn slot_of(addr: usize) -> usize {
    (addr >> 5) & (SLOTS - 1)
}

pub(crate) fn raw_index(vram: &[u8], addr: usize, bpp8: bool, sx: usize, sy: usize) -> u8 {
    if bpp8 {
        vram.get(addr + sy * 8 + sx).copied().unwrap_or(0)
    } else {
        let Some(byte) = vram.get(addr + sy * 4 + sx / 2).copied() else {
            return 0;
        };
        if sx & 1 == 0 {
            byte & 0x0F
        } else {
            byte >> 4
        }
    }
}

fn decode_tile(vram: &[u8], addr: usize, bpp8: bool, out: &mut [u8; 64]) {
    for sy in 0..8 {
        for sx in 0..8 {
            out[sy * 8 + sx] = raw_index(vram, addr, bpp8, sx, sy);
        }
    }
}

/// 行表要看的那些位。矩阵参数字节不参与，返回固定值。
pub(crate) fn sprite_coverage(oam: &[u8], off: usize) -> u32 {
    if off >= oam.len() || off & 7 >= 6 {
        return u32::MAX;
    }
    let base = off & !7;
    let attr0 = u16::from(oam[base]) | (u16::from(oam.get(base + 1).copied().unwrap_or(0)) << 8);
    let attr1 = u16::from(oam.get(base + 2).copied().unwrap_or(0))
        | (u16::from(oam.get(base + 3).copied().unwrap_or(0)) << 8);
    let y = u32::from(attr0 & 0xFF);
    let affine = u32::from((attr0 >> 8) & 1);
    let hidden = u32::from((attr0 >> 9) & 1);
    let mode = u32::from((attr0 >> 10) & 3);
    let shape = u32::from((attr0 >> 14) & 3);
    let size = u32::from((attr1 >> 14) & 3);
    y | (affine << 8) | (hidden << 9) | (mode << 10) | (shape << 14) | (size << 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_and_eight_bit_tiles_do_not_share_a_slot_entry() {
        let mut vram = vec![0u8; 128];
        vram[0] = 0x21;
        vram[1] = 0x43;
        let mut gfx = Gfx::new();
        assert_eq!(gfx.index(&vram, 0, false, 0, 0), 1);
        assert_eq!(gfx.index(&vram, 0, false, 1, 0), 2);
        assert_eq!(gfx.tile_misses(), 1);
        assert_eq!(gfx.index(&vram, 0, false, 0, 0), 1);
        assert_eq!(gfx.tile_hits(), 2);
        assert_eq!(gfx.index(&vram, 0, true, 0, 0), 0x21);
        assert_eq!(gfx.tile_misses(), 2);
        gfx.invalidate_tile_byte(1);
        assert_eq!(gfx.index(&vram, 0, false, 1, 0), 2);
        assert_eq!(gfx.index(&vram, 0, true, 0, 0), 0x21);
        assert_eq!(gfx.tile_misses(), 4);
    }

    #[test]
    fn horizontal_position_does_not_drop_the_line_list() {
        let mut oam = vec![0u8; 16];
        let before = sprite_coverage(&oam, 2);
        oam[2] = 40;
        assert_eq!(sprite_coverage(&oam, 2), before);
        let y_before = sprite_coverage(&oam, 0);
        oam[0] = 20;
        assert_ne!(sprite_coverage(&oam, 0), y_before);
        assert_eq!(sprite_coverage(&oam, 6), sprite_coverage(&oam, 7));
    }
}
