//! GBA 的一帧画面。每层先交出颜色，窗口决定能不能画，混合再处理最上面两层。
//!
//! 显存从 `0x06000000` 起，每个像素是 16 位 RGB555，一行 240 个，共 160 行。
//! 累计周期往前走时，光束经过的可见像素用当时的显存锁进画面。
//! 一行的后 272 个周期和垂直消隐不取样。一条指令的周期用它做完之后的显存。

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

use crate::bus::Bus;
use crate::gfxcache::Gfx;

pub const WIDTH: usize = 240;
pub const HEIGHT: usize = 160;

/// 一行 1232 个周期：前 960 个画 240 个像素，后 272 个是行消隐。
pub const CYCLES_PER_LINE: u64 = 1232;
pub const VISIBLE_CYCLES: u64 = 960;
/// 一帧 228 行：0–159 可见，160–227 垂直消隐。1232 × 228 = 280896。
pub const LINES_PER_FRAME: u64 = 228;
pub const VISIBLE_LINES: u64 = 160;
pub const CYCLES_PER_FRAME: u64 = CYCLES_PER_LINE * LINES_PER_FRAME;

/// 累计周期在一帧里的位置。行号就是 `VCOUNT`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanPos {
    pub line: u16,
    pub cycle: u16,
}

impl ScanPos {
    pub fn visible(self) -> bool {
        u64::from(self.line) < VISIBLE_LINES && u64::from(self.cycle) < VISIBLE_CYCLES
    }

    pub fn hblank(self) -> bool {
        u64::from(self.line) < VISIBLE_LINES && u64::from(self.cycle) >= VISIBLE_CYCLES
    }

    pub fn vblank(self) -> bool {
        u64::from(self.line) >= VISIBLE_LINES
    }

    /// `DISPSTAT` 的三个只读位。垂直消隐是第 160–226 行，第 227 行不算。
    /// 行消隐是每一行的后 272 个周期，垂直消隐里也算。`lyc` 和当前行号相等时第 2 位置上。
    pub fn dispstat_flags(self, lyc: u8) -> u8 {
        let mut flags = 0;
        if (VISIBLE_LINES..LINES_PER_FRAME - 1).contains(&u64::from(self.line)) {
            flags |= 1;
        }
        if u64::from(self.cycle) >= VISIBLE_CYCLES {
            flags |= 2;
        }
        if self.line as u8 == lyc {
            flags |= 4;
        }
        flags
    }
}

pub fn scan_at(cycles: u64) -> ScanPos {
    let in_frame = cycles % CYCLES_PER_FRAME;
    ScanPos {
        line: (in_frame / CYCLES_PER_LINE) as u16,
        cycle: (in_frame % CYCLES_PER_LINE) as u16,
    }
}

/// 一层画面。`color` 为空表示这个点是透明的，要看后面的层。
///
/// 优先级只用低两位：0 在最前，3 在最后。同一档里精灵盖过背景；
/// 精灵之间序号小的在前；背景之间编号小的在前。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerKind {
    Background(u8),
    Sprite(u16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layer {
    pub kind: LayerKind,
    pub priority: u8,
    pub color: Option<u16>,
    /// 精灵属性里的半透明。不改变谁排在前面，只参与排完之后的混合。
    pub semi: bool,
}

/// 按四档优先级选出这个点要显示的颜色。一层都没盖住时用背景色。
pub fn front_color(layers: &[Layer], backdrop: u16) -> u16 {
    let mut winner: Option<(u8, u8, u16, u16)> = None;
    for layer in layers {
        let Some(color) = layer.color else {
            continue;
        };
        let key = layer_rank(layer);
        let closer = match winner {
            None => true,
            Some((priority, class, index, _)) => key < (priority, class, index),
        };
        if closer {
            winner = Some((key.0, key.1, key.2, color));
        }
    }
    winner.map(|(_, _, _, color)| color).unwrap_or(backdrop)
}

fn layer_rank(layer: &Layer) -> (u8, u8, u16) {
    let priority = layer.priority & 3;
    match layer.kind {
        LayerKind::Sprite(index) => (priority, 0, index),
        LayerKind::Background(index) => (priority, 1, u16::from(index)),
    }
}

/// 背景 2 或背景 3 的旋转缩放。系数和原点都是 8 位小数。
#[derive(Clone, Copy, Debug)]
pub(crate) struct Affine {
    pub pa: i16,
    pub pb: i16,
    pub pc: i16,
    pub pd: i16,
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Sample {
    pub disp: u16,
    pub bg_priority: [u8; 4],
    pub bg_cnt: [u16; 4],
    pub bg_hofs: [u16; 4],
    pub bg_vofs: [u16; 4],
    pub affine: [Affine; 2],
    pub mosaic: u16,
    pub win_h: [u16; 2],
    pub win_v: [u16; 2],
    pub win_in: u16,
    pub win_out: u16,
    pub bldcnt: u16,
    pub bldalpha: u16,
    pub bldy: u16,
    pub backdrop: u16,
}

struct PixelGate {
    bg: [bool; 4],
    obj: bool,
    blend: bool,
}

enum BgFormula {
    Off,
    Text,
    Bitmap,
    Affine,
}

fn bg_formula(mode: u16, index: usize) -> BgFormula {
    match (mode, index) {
        (0, 0..=3) => BgFormula::Text,
        (1, 0 | 1) => BgFormula::Text,
        (1, 2) | (2, 2 | 3) => BgFormula::Affine,
        (3..=5, 2) => BgFormula::Bitmap,
        _ => BgFormula::Off,
    }
}

const OBJ_COUNT: usize = 128;

fn screen_color(
    sample: &Sample,
    vram: &[u8],
    oam: &[u8],
    palette: &[u8],
    x: usize,
    y: usize,
    sprites: &[u8],
    gfx: &mut Gfx,
) -> u16 {
    if sample.disp & (1 << 7) != 0 {
        return 0x7FFF;
    }
    let gate = pixel_gate(sample, vram, oam, palette, x, y, sprites, gfx);
    let empty = Layer {
        kind: LayerKind::Background(0),
        priority: 0,
        color: None,
        semi: false,
    };
    let mut layers = [empty; 4 + OBJ_COUNT];
    let mut count = 0;
    for index in 0..4 {
        if sample.disp & (1 << (8 + index)) == 0 || !gate.bg[index] {
            continue;
        }
        let (sx, sy) = if sample.bg_cnt[index] & (1 << 6) != 0 {
            mosaic_anchor(x, y, sample.mosaic, false)
        } else {
            (x, y)
        };
        let Some(color) = background_color(sample, vram, palette, index, sx, sy, gfx) else {
            continue;
        };
        layers[count] = Layer {
            kind: LayerKind::Background(index as u8),
            priority: sample.bg_priority[index],
            color: Some(color),
            semi: false,
        };
        count += 1;
    }
    if sample.disp & (1 << 12) != 0 && gate.obj {
        for &index in sprites {
            let Some(hit) = obj_pixel(sample, vram, oam, palette, index as usize, x, y, gfx) else {
                continue;
            };
            if hit.mode == 2 {
                continue;
            }
            layers[count] = Layer {
                kind: LayerKind::Sprite(index as u16),
                priority: hit.priority,
                color: Some(hit.color),
                semi: hit.mode == 1,
            };
            count += 1;
        }
    }
    blend_color(&layers[..count], sample, gate.blend)
}

struct ObjHit {
    priority: u8,
    color: u16,
    mode: u16,
}

fn pixel_gate(
    sample: &Sample,
    vram: &[u8],
    oam: &[u8],
    palette: &[u8],
    x: usize,
    y: usize,
    sprites: &[u8],
    gfx: &mut Gfx,
) -> PixelGate {
    let win0 = sample.disp & (1 << 13) != 0;
    let win1 = sample.disp & (1 << 14) != 0;
    let obj_win = sample.disp & (1 << 15) != 0;
    if !win0 && !win1 && !obj_win {
        return PixelGate {
            bg: [true; 4],
            obj: true,
            blend: true,
        };
    }
    let bits = if win0 && in_window(x, y, sample.win_h[0], sample.win_v[0]) {
        sample.win_in & 0x3F
    } else if win1 && in_window(x, y, sample.win_h[1], sample.win_v[1]) {
        (sample.win_in >> 8) & 0x3F
    } else if obj_win && obj_window_hit(sample, vram, oam, palette, sprites, x, y, gfx) {
        (sample.win_out >> 8) & 0x3F
    } else {
        sample.win_out & 0x3F
    };
    PixelGate {
        bg: [bits & 1 != 0, bits & 2 != 0, bits & 4 != 0, bits & 8 != 0],
        obj: bits & 16 != 0,
        blend: bits & 32 != 0,
    }
}

fn in_window(x: usize, y: usize, horizontal: u16, vertical: u16) -> bool {
    span_contains(x, horizontal >> 8, horizontal & 0xFF)
        && span_contains(y, vertical >> 8, vertical & 0xFF)
}

fn span_contains(pos: usize, start: u16, end: u16) -> bool {
    let pos = pos as u16;
    match start.cmp(&end) {
        Ordering::Less => pos >= start && pos < end,
        Ordering::Greater => pos >= start || pos < end,
        Ordering::Equal => false,
    }
}

fn obj_window_hit(
    sample: &Sample,
    vram: &[u8],
    oam: &[u8],
    palette: &[u8],
    sprites: &[u8],
    x: usize,
    y: usize,
    gfx: &mut Gfx,
) -> bool {
    if sample.disp & (1 << 12) == 0 {
        return false;
    }
    sprites.iter().any(|&index| {
        obj_pixel(sample, vram, oam, palette, index as usize, x, y, gfx)
            .is_some_and(|hit| hit.mode == 2)
    })
}

fn mosaic_anchor(x: usize, y: usize, mosaic: u16, obj: bool) -> (usize, usize) {
    let shift = if obj { 8 } else { 0 };
    let wide = ((mosaic >> shift) & 0xF) as usize + 1;
    let high = ((mosaic >> (shift + 4)) & 0xF) as usize + 1;
    (x / wide * wide, y / high * high)
}

fn blend_color(layers: &[Layer], sample: &Sample, blend_on: bool) -> u16 {
    let backdrop = sample.backdrop;
    let top = top_two(layers);
    let Some((first, second)) = top else {
        if !blend_on {
            return backdrop;
        }
        let mode = (sample.bldcnt >> 6) & 3;
        if (mode == 2 || mode == 3) && sample.bldcnt & (1 << 5) != 0 {
            return brightness(backdrop, sample.bldy, mode == 2);
        }
        return backdrop;
    };
    if !blend_on {
        return first.color;
    }
    let mode = (sample.bldcnt >> 6) & 3;
    let selected = sample.bldcnt & (1 << first.bit) != 0;
    let alpha = match mode {
        0 => first.semi,
        1 => selected || first.semi,
        _ => false,
    };
    if mode == 2 || mode == 3 {
        if selected {
            return brightness(first.color, sample.bldy, mode == 2);
        }
        return first.color;
    }
    if !alpha {
        return first.color;
    }
    let (behind, bit) = second.unwrap_or((backdrop, 5));
    if sample.bldcnt & (1 << (8 + bit)) == 0 {
        return first.color;
    }
    let eva = u32::from(sample.bldalpha & 0x1F).min(16);
    let evb = u32::from((sample.bldalpha >> 8) & 0x1F).min(16);
    mix(first.color, behind, eva, evb)
}

struct Top {
    color: u16,
    bit: u16,
    semi: bool,
    rank: (u8, u8, u16),
}

fn top_two(layers: &[Layer]) -> Option<(Top, Option<(u16, u16)>)> {
    let mut first: Option<Top> = None;
    let mut second: Option<(u16, u16, (u8, u8, u16))> = None;
    for layer in layers {
        let Some(color) = layer.color else {
            continue;
        };
        let rank = layer_rank(layer);
        let bit = match layer.kind {
            LayerKind::Sprite(_) => 4,
            LayerKind::Background(index) => u16::from(index),
        };
        let closer_than_first = first.as_ref().is_none_or(|top| rank < top.rank);
        if closer_than_first {
            if let Some(top) = first.take() {
                second = Some((top.color, top.bit, top.rank));
            }
            first = Some(Top {
                color,
                bit,
                semi: layer.semi,
                rank,
            });
        } else if second.as_ref().is_none_or(|(_, _, prev)| rank < *prev) {
            second = Some((color, bit, rank));
        }
    }
    let first = first?;
    Some((first, second.map(|(color, bit, _)| (color, bit))))
}

fn mix(top: u16, behind: u16, eva: u32, evb: u32) -> u16 {
    let channel = |shift: u8| {
        let a = u32::from((top >> shift) & 0x1F);
        let b = u32::from((behind >> shift) & 0x1F);
        ((a * eva + b * evb) / 16).min(31) as u16
    };
    channel(0) | (channel(5) << 5) | (channel(10) << 10)
}

fn brightness(color: u16, evy: u16, brighter: bool) -> u16 {
    let evy = u32::from(evy & 0x1F).min(16);
    let channel = |shift: u8| {
        let value = u32::from((color >> shift) & 0x1F);
        let next = if brighter {
            value + (31 - value) * evy / 16
        } else {
            value - value * evy / 16
        };
        next as u16
    };
    channel(0) | (channel(5) << 5) | (channel(10) << 10)
}

fn affine_coord(origin: i32, pa: i16, pb: i16, x: i32, y: i32) -> i32 {
    let scaled = i32::from(pa)
        .wrapping_mul(x)
        .wrapping_add(i32::from(pb).wrapping_mul(y));
    origin.wrapping_add(scaled) >> 8
}

/// 精灵盖住取样点时返回优先级、颜色和模式。模式 2 是窗口，不作为颜色层。
fn obj_pixel(
    sample: &Sample,
    vram: &[u8],
    oam: &[u8],
    palette: &[u8],
    index: usize,
    x: usize,
    y: usize,
    gfx: &mut Gfx,
) -> Option<ObjHit> {
    let base = index * 8;
    let attr0 = obj_attr(oam, base)?;
    let attr1 = obj_attr(oam, base + 2)?;
    let attr2 = obj_attr(oam, base + 4)?;
    let affine = attr0 & (1 << 8) != 0;
    if !affine && attr0 & (1 << 9) != 0 {
        return None;
    }
    let mode = (attr0 >> 10) & 3;
    if mode == 3 {
        return None;
    }
    let (_, _, width, height) = sprite_delta(attr0, attr1, x as i32, y as i32)?;
    let (sx, sy) = if attr0 & (1 << 12) != 0 {
        mosaic_anchor(x, y, sample.mosaic, true)
    } else {
        (x, y)
    };
    let (dx, dy, _, _) = sprite_delta(attr0, attr1, sx as i32, sy as i32)?;
    let (px, py) = if affine {
        let slot = ((attr1 >> 9) & 0x1F) as usize;
        let pa = obj_param(oam, slot * 32 + 0x06)?;
        let pb = obj_param(oam, slot * 32 + 0x0E)?;
        let pc = obj_param(oam, slot * 32 + 0x16)?;
        let pd = obj_param(oam, slot * 32 + 0x1E)?;
        let area_w = sprite_area(attr0, width);
        let area_h = sprite_area(attr0, height);
        let tex_x = affine_coord(0, pa, pb, dx - area_w / 2, dy as i32 - area_h / 2);
        let tex_y = affine_coord(0, pc, pd, dx - area_w / 2, dy as i32 - area_h / 2);
        let tex_x = tex_x + width as i32 / 2;
        let tex_y = tex_y + height as i32 / 2;
        if tex_x < 0 || tex_y < 0 || tex_x >= width as i32 || tex_y >= height as i32 {
            return None;
        }
        (tex_x as u32, tex_y as u32)
    } else {
        let mut px = dx as u32;
        let mut py = dy;
        if attr1 & (1 << 12) != 0 {
            px = width - 1 - px;
        }
        if attr1 & (1 << 13) != 0 {
            py = height - 1 - py;
        }
        (px, py)
    };
    let index = obj_texel(
        vram,
        sample.disp,
        u32::from(attr2 & 0x3FF),
        px,
        py,
        width,
        attr0 & (1 << 13) != 0,
        gfx,
    )?;
    let color = obj_color(palette, (attr2 >> 12) as u8, index, attr0 & (1 << 13) != 0)?;
    Some(ObjHit {
        priority: ((attr2 >> 10) & 3) as u8,
        color,
        mode,
    })
}

fn sprite_delta(attr0: u16, attr1: u16, x: i32, y: i32) -> Option<(i32, u32, u32, u32)> {
    let (width, height) = sprite_size((attr0 >> 14) as u8, (attr1 >> 14) as u8)?;
    let area_w = sprite_area(attr0, width);
    let area_h = sprite_area(attr0, height);
    let raw = i32::from(attr1 & 0x1FF);
    let obj_x = if raw >= 256 { raw - 512 } else { raw };
    let dx = x - obj_x;
    if dx < 0 || dx >= area_w {
        return None;
    }
    let dy = u32::from((y as u8).wrapping_sub((attr0 & 0xFF) as u8));
    if dy >= area_h as u32 {
        return None;
    }
    Some((dx, dy, width, height))
}

fn sprite_area(attr0: u16, size: u32) -> i32 {
    if attr0 & (3 << 8) == 3 << 8 {
        (size * 2) as i32
    } else {
        size as i32
    }
}

fn obj_param(oam: &[u8], offset: usize) -> Option<i16> {
    Some(obj_attr(oam, offset)? as i16)
}

fn obj_attr(oam: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from(*oam.get(offset)?) | (u16::from(*oam.get(offset + 1)?) << 8))
}

fn sprite_size(shape: u8, size: u8) -> Option<(u32, u32)> {
    const SIZES: [[Option<(u32, u32)>; 4]; 4] = [
        [Some((8, 8)), Some((16, 16)), Some((32, 32)), Some((64, 64))],
        [Some((16, 8)), Some((32, 8)), Some((32, 16)), Some((64, 32))],
        [Some((8, 16)), Some((8, 32)), Some((16, 32)), Some((32, 64))],
        [None, None, None, None],
    ];
    SIZES.get(shape as usize)?.get(size as usize).copied()?
}

fn obj_texel(
    vram: &[u8],
    disp: u16,
    tile: u32,
    px: u32,
    py: u32,
    width: u32,
    bpp8: bool,
    gfx: &mut Gfx,
) -> Option<u8> {
    let tx = px / 8;
    let ty = py / 8;
    let sub_x = (px % 8) as usize;
    let sub_y = (py % 8) as usize;
    let wide = width / 8;
    let one_d = disp & (1 << 6) != 0;
    let tile_no = if one_d {
        let step = if bpp8 { 2 } else { 1 };
        tile + (ty * wide + tx) * step
    } else if bpp8 {
        tile + ty * 32 + tx * 2
    } else {
        tile + ty * 32 + tx
    };
    let base = if disp & 7 >= 3 { 0x1_4000 } else { 0x1_0000 };
    let addr = base + tile_no as usize * 32;
    Some(gfx.index(vram, addr, bpp8, sub_x, sub_y))
}

fn background_color(
    sample: &Sample,
    vram: &[u8],
    palette: &[u8],
    index: usize,
    x: usize,
    y: usize,
    gfx: &mut Gfx,
) -> Option<u16> {
    match bg_formula(sample.disp & 7, index) {
        BgFormula::Off => None,
        BgFormula::Text => text_bg_color(sample, vram, palette, index, x, y, gfx),
        BgFormula::Bitmap => bitmap_color(sample, vram, palette, x, y),
        BgFormula::Affine => affine_bg_color(sample, vram, palette, index, x, y, gfx),
    }
}

fn affine_bg_color(
    sample: &Sample,
    vram: &[u8],
    palette: &[u8],
    index: usize,
    x: usize,
    y: usize,
    gfx: &mut Gfx,
) -> Option<u16> {
    let aff = sample.affine[index - 2];
    let cnt = sample.bg_cnt[index];
    let tiles = 16usize << ((cnt >> 14) & 3);
    let pixels = (tiles * 8) as i32;
    let mut tex_x = affine_coord(aff.x, aff.pa, aff.pb, x as i32, y as i32);
    let mut tex_y = affine_coord(aff.y, aff.pc, aff.pd, x as i32, y as i32);
    if cnt & (1 << 13) != 0 {
        tex_x = tex_x.rem_euclid(pixels);
        tex_y = tex_y.rem_euclid(pixels);
    } else if tex_x < 0 || tex_y < 0 || tex_x >= pixels || tex_y >= pixels {
        return None;
    }
    let tile_x = tex_x as usize / 8;
    let tile_y = tex_y as usize / 8;
    let screen = ((cnt >> 8) & 0x1F) as usize * 0x800;
    let tile_no = *vram.get(screen + tile_y * tiles + tile_x)? as usize;
    let char_base = ((cnt >> 2) & 3) as usize * 0x4000;
    let tile_addr = char_base + tile_no * 64;
    let sub_x = (tex_x as usize) % 8;
    let sub_y = (tex_y as usize) % 8;
    let pixel = gfx.index(vram, tile_addr, true, sub_x, sub_y);
    palette_color(palette, 0, 0, pixel)
}

/// 文字背景。16 色或 256 色。地图按 32×32 一块来拼。
fn text_bg_color(
    sample: &Sample,
    vram: &[u8],
    palette: &[u8],
    index: usize,
    x: usize,
    y: usize,
    gfx: &mut Gfx,
) -> Option<u16> {
    let cnt = sample.bg_cnt[index];
    let bpp8 = cnt & (1 << 7) != 0;
    let size = (cnt >> 14) & 3;
    let (width, height) = match size {
        1 => (512, 256),
        2 => (256, 512),
        3 => (512, 512),
        _ => (256, 256),
    };
    let map_x = (x as u32 + u32::from(sample.bg_hofs[index] & 0x1FF)) & (width - 1);
    let map_y = (y as u32 + u32::from(sample.bg_vofs[index] & 0x1FF)) & (height - 1);
    let block = match size {
        1 => map_x / 256,
        2 => map_y / 256,
        3 => (map_y / 256) * 2 + map_x / 256,
        _ => 0,
    };
    let screen = ((cnt >> 8) & 0x1F) as usize * 0x800 + block as usize * 0x800;
    let tile_x = (map_x & 255) / 8;
    let tile_y = (map_y & 255) / 8;
    let entry_at = screen + (tile_y as usize * 32 + tile_x as usize) * 2;
    let entry = vram_u16(vram, entry_at)?;
    let mut sub_x = map_x & 7;
    let mut sub_y = map_y & 7;
    if entry & (1 << 10) != 0 {
        sub_x = 7 - sub_x;
    }
    if entry & (1 << 11) != 0 {
        sub_y = 7 - sub_y;
    }
    let char_base = ((cnt >> 2) & 3) as usize * 0x4000;
    let tile_no = usize::from(entry & 0x3FF);
    let pixel = if bpp8 {
        let tile_addr = char_base + tile_no * 64;
        gfx.index(vram, tile_addr, true, sub_x as usize, sub_y as usize)
    } else {
        let tile_addr = char_base + tile_no * 32;
        gfx.index(vram, tile_addr, false, sub_x as usize, sub_y as usize)
    };
    let bank = if bpp8 { 0 } else { (entry >> 12) as u8 };
    palette_color(palette, 0, bank, pixel)
}

fn vram_u16(vram: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from(*vram.get(offset)?) | (u16::from(*vram.get(offset + 1)?) << 8))
}

fn palette_color(palette: &[u8], base: usize, bank: u8, index: u8) -> Option<u16> {
    if index == 0 {
        return None;
    }
    let offset = base + usize::from(bank) * 32 + usize::from(index) * 2;
    let lo = *palette.get(offset)?;
    let hi = *palette.get(offset + 1)?;
    Some((u16::from(lo) | (u16::from(hi) << 8)) & 0x7FFF)
}

fn obj_color(palette: &[u8], bank: u8, index: u8, bpp8: bool) -> Option<u16> {
    if bpp8 {
        if index == 0 {
            return None;
        }
        let offset = 0x200 + usize::from(index) * 2;
        let lo = *palette.get(offset)?;
        let hi = *palette.get(offset + 1)?;
        Some((u16::from(lo) | (u16::from(hi) << 8)) & 0x7FFF)
    } else {
        palette_color(palette, 0x200, bank, index)
    }
}

fn bitmap_color(sample: &Sample, vram: &[u8], palette: &[u8], x: usize, y: usize) -> Option<u16> {
    let mode = sample.disp & 7;
    let page = if mode != 3 && sample.disp & (1 << 4) != 0 {
        0xA000
    } else {
        0
    };
    match mode {
        4 => {
            let index = *vram.get(page + y * WIDTH + x)?;
            Some(palette_opaque(palette, index))
        }
        5 if x < 160 && y < 128 => Some(direct_color(vram, page + (y * 160 + x) * 2)),
        5 => None,
        _ => Some(vram_color(vram, x, y)),
    }
}

fn palette_opaque(palette: &[u8], index: u8) -> u16 {
    let offset = usize::from(index) * 2;
    direct_color(palette, offset) & 0x7FFF
}

fn vram_color(vram: &[u8], x: usize, y: usize) -> u16 {
    direct_color(vram, (y * WIDTH + x) * 2)
}

fn direct_color(vram: &[u8], offset: usize) -> u16 {
    if offset + 1 >= vram.len() {
        return 0;
    }
    u16::from(vram[offset]) | (u16::from(vram[offset + 1]) << 8)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Frame {
    #[serde(with = "crate::serde_array")]
    pixels: [u16; WIDTH * HEIGHT],
}

impl Frame {
    pub(crate) fn blank() -> Self {
        Self {
            pixels: [0; WIDTH * HEIGHT],
        }
    }

    pub fn pixel(&self, x: usize, y: usize) -> u16 {
        self.pixels[y * WIDTH + x]
    }

    fn put(&mut self, x: usize, y: usize, color: u16) {
        self.pixels[y * WIDTH + x] = color;
    }
}

/// 这一帧里已经完整扫完的可见行数。当前行还没走完时不算进去。
pub fn finished_lines(cycles: u64) -> usize {
    let pos = scan_at(cycles);
    if pos.vblank() {
        HEIGHT
    } else if u64::from(pos.cycle) >= VISIBLE_CYCLES {
        pos.line as usize + 1
    } else {
        pos.line as usize
    }
}

/// 下一次会合成像素的周期。已经站在像素第一拍上就返回 `cycle` 本身。
pub(crate) fn next_paint(cycle: u64) -> u64 {
    let frame_base = cycle - cycle % CYCLES_PER_FRAME;
    let in_frame = cycle % CYCLES_PER_FRAME;
    let line = in_frame / CYCLES_PER_LINE;
    let dot = in_frame % CYCLES_PER_LINE;
    if line < VISIBLE_LINES && dot < VISIBLE_CYCLES {
        let aligned = dot + (4 - (dot % 4)) % 4;
        if aligned < VISIBLE_CYCLES {
            return frame_base + line * CYCLES_PER_LINE + aligned;
        }
    }
    if line + 1 < VISIBLE_LINES {
        return frame_base + (line + 1) * CYCLES_PER_LINE;
    }
    frame_base + CYCLES_PER_FRAME
}

/// 160 行各 1 位，一共 20 字节。合成看 `compose`，贴窗口看 `present`。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct DirtyLines {
    compose: [u8; DIRTY_BYTES],
    present: [u8; DIRTY_BYTES],
    /// 光束正扫到这一行时又写入了。扫完不能把合成位清掉，下一帧还要画。
    again: [u8; DIRTY_BYTES],
    /// 这一帧这行的消隐已经处理过，避免在消隐里清两次。
    closed: Option<usize>,
    beam: Option<usize>,
    composed: u64,
}

const DIRTY_BYTES: usize = HEIGHT / 8;

impl DirtyLines {
    pub(crate) fn new() -> Self {
        Self {
            compose: [0xFF; DIRTY_BYTES],
            present: [0; DIRTY_BYTES],
            again: [0; DIRTY_BYTES],
            closed: None,
            beam: None,
            composed: 0,
        }
    }

    pub(crate) fn composed(&self) -> u64 {
        self.composed
    }

    pub(crate) fn line_dirty(&self, y: usize) -> bool {
        bit_on(&self.compose, y)
    }

    /// 用当前光束位置决定：写在正在扫的行上时，这一行下一帧还要再画。
    pub(crate) fn set_beam(&mut self, line: u16, cycle: u16) {
        self.beam =
            if u64::from(line) < VISIBLE_LINES && cycle > 0 && u64::from(cycle) < VISIBLE_CYCLES {
                Some(line as usize)
            } else {
                None
            };
    }

    pub(crate) fn mark_all(&mut self) {
        self.compose = [0xFF; DIRTY_BYTES];
        if let Some(y) = self.beam {
            set_bit(&mut self.again, y);
        }
    }

    pub(crate) fn mark_line(&mut self, y: usize) {
        if y >= HEIGHT {
            return;
        }
        set_bit(&mut self.compose, y);
        if self.beam == Some(y) {
            set_bit(&mut self.again, y);
        }
    }

    pub(crate) fn mark_vram(&mut self, off: usize, mode: u8) {
        match mode & 7 {
            3 => {
                if off < WIDTH * HEIGHT * 2 {
                    self.mark_line(off / (WIDTH * 2));
                } else {
                    self.mark_all();
                }
            }
            4 => {
                // 精灵图块从 `0x14000` 起，不能再按点阵页长取模，否则只弄脏第 0 行。
                if off >= 0x14000 {
                    self.mark_all();
                } else {
                    let page = if off >= 0xA000 { off - 0xA000 } else { off };
                    if page < WIDTH * HEIGHT {
                        self.mark_line(page / WIDTH);
                    } else {
                        self.mark_all();
                    }
                }
            }
            5 => {
                if off >= 0x14000 {
                    self.mark_all();
                } else {
                    let page = if off >= 0xA000 { off - 0xA000 } else { off };
                    if page < 160 * 128 * 2 {
                        self.mark_line(page / (160 * 2));
                    } else {
                        self.mark_all();
                    }
                }
            }
            _ => self.mark_all(),
        }
    }

    /// 按这一个 OAM 字节所属的精灵，标它盖住的行。参数字节标全部行。
    pub(crate) fn mark_oam_byte(&mut self, oam: &[u8], off: usize) {
        if off >= oam.len() {
            return;
        }
        if off & 7 >= 6 {
            self.mark_all();
            return;
        }
        let base = off & !7;
        let Some(attr0) = obj_attr(oam, base) else {
            return;
        };
        let Some(attr1) = obj_attr(oam, base + 2) else {
            return;
        };
        let affine = attr0 & (1 << 8) != 0;
        if !affine && attr0 & (1 << 9) != 0 {
            return;
        }
        let Some((_, height)) = sprite_size((attr0 >> 14) as u8, (attr1 >> 14) as u8) else {
            return;
        };
        let area = sprite_area(attr0, height) as u32;
        let obj_y = (attr0 & 0xFF) as u8;
        for y in 0..HEIGHT {
            let dy = u32::from((y as u8).wrapping_sub(obj_y));
            if dy < area {
                self.mark_line(y);
            }
        }
    }

    fn begin_line(&mut self, y: usize) {
        if self.closed == Some(y) {
            self.closed = None;
        }
    }

    fn finish_line(&mut self, y: usize) {
        if self.closed == Some(y) {
            return;
        }
        self.closed = Some(y);
        if bit_on(&self.again, y) {
            clear_bit(&mut self.again, y);
        } else {
            clear_bit(&mut self.compose, y);
        }
    }

    fn paint_pixel(&mut self, y: usize) -> bool {
        if !bit_on(&self.compose, y) {
            return false;
        }
        set_bit(&mut self.present, y);
        self.composed += 1;
        true
    }

    pub(crate) fn take_present(&mut self) -> [u8; DIRTY_BYTES] {
        let present = self.present;
        self.present = [0; DIRTY_BYTES];
        present
    }
}

fn set_bit(bits: &mut [u8; DIRTY_BYTES], y: usize) {
    bits[y / 8] |= 1 << (y % 8);
}

fn clear_bit(bits: &mut [u8; DIRTY_BYTES], y: usize) {
    bits[y / 8] &= !(1 << (y % 8));
}

fn bit_on(bits: &[u8; DIRTY_BYTES], y: usize) -> bool {
    y < HEIGHT && bits[y / 8] & (1 << (y % 8)) != 0
}

/// 把 `[from, to)` 这段周期里光束经过的可见像素锁进 `frame`。
/// 干净的行不再合成。一行扫完就清掉它的脏位。
pub(crate) fn latch_span(
    frame: &mut Frame,
    sample: &Sample,
    vram: &[u8],
    oam: &[u8],
    palette: &[u8],
    dirty: &mut DirtyLines,
    gfx: &mut Gfx,
    mut from: u64,
    to: u64,
) {
    let mut sprites = [0u8; OBJ_COUNT];
    let mut sprite_count = 0;
    let mut cached_line = usize::MAX;
    let mut sprites_ready = false;
    while from < to {
        let in_frame = from % CYCLES_PER_FRAME;
        let line = in_frame / CYCLES_PER_LINE;
        let dot = in_frame % CYCLES_PER_LINE;
        if line >= VISIBLE_LINES {
            from = (from - in_frame + CYCLES_PER_FRAME).min(to);
            continue;
        }
        if dot >= VISIBLE_CYCLES {
            dirty.finish_line(line as usize);
            from = (from - dot + CYCLES_PER_LINE).min(to);
            continue;
        }
        let pixel_end = from - (dot % 4) + 4;
        // 一个像素占 4 拍。只在进入这个像素的第一拍合成一次。
        // 同一像素里后面的指令不再重画，否则每条 1 拍的指令都会把 128 个精灵扫一遍。
        if dot % 4 == 0 {
            let y = line as usize;
            if y != cached_line {
                dirty.begin_line(y);
                cached_line = y;
                sprites_ready = false;
            }
            if dirty.paint_pixel(y) {
                if !sprites_ready {
                    sprite_count = sprites_on_line(sample, oam, y, &mut sprites, gfx);
                    sprites_ready = true;
                }
                let x = (dot / 4) as usize;
                frame.put(
                    x,
                    y,
                    screen_color(
                        sample,
                        vram,
                        oam,
                        palette,
                        x,
                        y,
                        &sprites[..sprite_count],
                        gfx,
                    ),
                );
            }
        }
        let visible_end = from - dot + VISIBLE_CYCLES;
        from = pixel_end.min(to);
        // `to` 往往正好停在第 960 拍，循环条件看不到消隐。走到这里也算这行扫完。
        if from >= visible_end {
            dirty.finish_line(line as usize);
        }
    }
}

fn sprites_on_line(
    sample: &Sample,
    oam: &[u8],
    y: usize,
    out: &mut [u8; OBJ_COUNT],
    gfx: &mut Gfx,
) -> usize {
    gfx.sprites_of_line(y, out, |buf| scan_sprites(sample, oam, y, buf))
}

fn scan_sprites(sample: &Sample, oam: &[u8], y: usize, out: &mut [u8; OBJ_COUNT]) -> usize {
    if sample.disp & (1 << 12) == 0 {
        return 0;
    }
    let mut count = 0;
    for index in 0..OBJ_COUNT {
        let base = index * 8;
        let Some(attr0) = obj_attr(oam, base) else {
            continue;
        };
        let Some(attr1) = obj_attr(oam, base + 2) else {
            continue;
        };
        let affine = attr0 & (1 << 8) != 0;
        if !affine && attr0 & (1 << 9) != 0 {
            continue;
        }
        if (attr0 >> 10) & 3 == 3 {
            continue;
        }
        let Some((_, height)) = sprite_size((attr0 >> 14) as u8, (attr1 >> 14) as u8) else {
            continue;
        };
        let area_h = sprite_area(attr0, height) as u32;
        let dy = u32::from((y as u8).wrapping_sub((attr0 & 0xFF) as u8));
        if dy >= area_h {
            continue;
        }
        out[count] = index as u8;
        count += 1;
    }
    count
}

pub struct Ppu {
    frame: Frame,
    line: usize,
}

impl Ppu {
    pub fn new() -> Self {
        Self {
            frame: Frame::blank(),
            line: 0,
        }
    }

    pub fn frame(&self) -> &Frame {
        &self.frame
    }

    /// 扫一行。扫完可见的 160 行之后返回 `false`。
    pub fn step_line(&mut self, bus: &mut Bus) -> bool {
        if self.line >= HEIGHT {
            return false;
        }
        self.paint_line(bus, self.line);
        self.line += 1;
        self.line < HEIGHT
    }
}

impl Default for Ppu {
    fn default() -> Self {
        Self::new()
    }
}

impl Ppu {
    fn paint_line(&mut self, bus: &mut Bus, y: usize) {
        let sample = bus.sample();
        let mut sprites = [0u8; OBJ_COUNT];
        let Bus {
            vram,
            oam,
            palette,
            gfx,
            ..
        } = bus;
        let sprite_count = sprites_on_line(&sample, oam, y, &mut sprites, gfx);
        for x in 0..WIDTH {
            self.frame.put(
                x,
                y,
                screen_color(
                    &sample,
                    vram,
                    oam,
                    palette,
                    x,
                    y,
                    &sprites[..sprite_count],
                    gfx,
                ),
            );
        }
    }
}

pub fn render_frame(bus: &mut Bus) -> Frame {
    let mut ppu = Ppu::new();
    while ppu.step_line(bus) {}
    ppu.frame
}

/// RGB555 扩成窗口用的 `0x00RRGGBB`。低 5 位再补到高位，避免纯色发灰。
pub fn rgb555_to_888(color: u16) -> u32 {
    let channel = |bits: u16| {
        let bits = u32::from(bits);
        (bits << 3) | (bits >> 2)
    };
    let red = channel(color & 0x1F);
    let green = channel((color >> 5) & 0x1F);
    let blue = channel((color >> 10) & 0x1F);
    (red << 16) | (green << 8) | blue
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISPCNT: u32 = 0x0400_0000;
    const VRAM: u32 = 0x0600_0000;
    const BACKDROP: u32 = 0x0500_0000;

    #[test]
    fn rgb555_red_fills_the_low_bits() {
        assert_eq!(rgb555_to_888(0x001F), 0x00FF_0000);
        assert_eq!(rgb555_to_888(0x03E0), 0x0000_FF00);
        assert_eq!(rgb555_to_888(0x7C00), 0x0000_00FF);
    }

    #[test]
    fn one_line_reads_vram_and_leaves_the_next_black() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x0403);
        bus.write_u16(VRAM, 0x001F);
        let mut ppu = Ppu::new();
        assert!(ppu.step_line(&mut bus));
        assert_eq!(ppu.frame().pixel(0, 0), 0x001F);
        assert_eq!(ppu.frame().pixel(0, 1), 0);
    }

    #[test]
    fn forced_blank_is_white() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x0483);
        bus.write_u16(VRAM, 0x001F);
        let frame = render_frame(&mut bus);
        assert_eq!(frame.pixel(0, 0), 0x7FFF);
    }

    #[test]
    fn scan_position_follows_the_cycle_count() {
        let start = scan_at(0);
        assert_eq!((start.line, start.cycle), (0, 0));
        assert!(start.visible());

        let last_pixel = scan_at(VISIBLE_CYCLES - 1);
        assert_eq!((last_pixel.line, last_pixel.cycle), (0, 959));
        assert!(last_pixel.visible());

        let hblank = scan_at(VISIBLE_CYCLES);
        assert_eq!((hblank.line, hblank.cycle), (0, 960));
        assert!(hblank.hblank());

        let end_of_line = scan_at(CYCLES_PER_LINE - 1);
        assert_eq!((end_of_line.line, end_of_line.cycle), (0, 1231));
        assert!(end_of_line.hblank());

        let next_line = scan_at(CYCLES_PER_LINE);
        assert_eq!((next_line.line, next_line.cycle), (1, 0));
        assert!(next_line.visible());

        let vblank = scan_at(VISIBLE_LINES * CYCLES_PER_LINE);
        assert_eq!((vblank.line, vblank.cycle), (160, 0));
        assert!(vblank.vblank());

        let last = scan_at(CYCLES_PER_FRAME - 1);
        assert_eq!((last.line, last.cycle), (227, 1231));
        assert!(last.vblank());

        let wrapped = scan_at(CYCLES_PER_FRAME);
        assert_eq!((wrapped.line, wrapped.cycle), (0, 0));
        assert!(wrapped.visible());
    }

    #[test]
    fn the_beam_latches_one_pixel_every_four_cycles() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x0403);
        bus.write_u16(VRAM, 0x001F);
        bus.write_u16(VRAM + 2, 0x03E0);
        assert_eq!(bus.frame().pixel(0, 0), 0);

        bus.advance(4);
        assert_eq!(bus.frame().pixel(0, 0), 0x001F);
        assert_eq!(bus.frame().pixel(1, 0), 0);

        bus.advance(2);
        assert_eq!(bus.frame().pixel(1, 0), 0x03E0);
        assert_eq!(bus.frame().pixel(2, 0), 0);
    }

    #[test]
    fn later_dots_of_a_pixel_do_not_recompose() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x0403);
        bus.write_u16(VRAM, 0x001F);
        bus.advance(1);
        assert_eq!(bus.frame().pixel(0, 0), 0x001F);
        bus.write_u16(VRAM, 0x03E0);
        bus.advance(3);
        assert_eq!(bus.frame().pixel(0, 0), 0x001F);
        bus.write_u16(VRAM + 2, 0x7C00);
        bus.advance(1);
        assert_eq!(bus.frame().pixel(1, 0), 0x7C00);
    }

    #[test]
    fn sprites_on_a_line_skip_objects_on_other_rows() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x1403);
        hide_objs(&mut bus);
        bus.write_u16(VRAM, 0x7C00);
        bus.write_u16(VRAM + 2, 0x7C00);
        bus.write_u16(VRAM + 16, 0x7C00);
        bus.write_u16(OBJ_PALETTE + 2, 0x001F);
        bus.write_u16(OBJ_PALETTE + 4, 0x03E0);
        bus.store_u8(OBJ_TILES, 0x01);
        bus.store_u8(OBJ_TILES + 32, 0x02);
        put_obj(&mut bus, 3, 0, 0, 0);
        put_obj(&mut bus, 90, 20, 0, 1);
        put_obj(&mut bus, 127, 0, 8, 1);

        let sample = bus.sample();
        let oam = bus.oam().to_vec();
        let mut found = [0u8; OBJ_COUNT];
        let on_top = sprites_on_line(&sample, &oam, 0, &mut found, &mut bus.gfx);
        assert_eq!(&found[..on_top], &[3, 127]);
        let on_20 = sprites_on_line(&sample, &oam, 20, &mut found, &mut bus.gfx);
        assert_eq!(&found[..on_20], &[90]);
        assert_eq!(
            sprites_on_line(&sample, &oam, 8, &mut found, &mut bus.gfx),
            0
        );

        bus.advance(33);
        assert_eq!(bus.frame().pixel(0, 0), 0x001F);
        assert_eq!(bus.frame().pixel(1, 0), 0x7C00);
        assert_eq!(bus.frame().pixel(8, 0), 0x03E0);
    }

    #[test]
    fn hblank_does_not_sample_and_old_pixels_stay_latched() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x0403);
        for x in 0..WIDTH {
            bus.write_u16(VRAM + (x as u32) * 2, 0x001F);
        }
        bus.advance(956);
        assert_eq!(bus.frame().pixel(238, 0), 0x001F);
        assert_eq!(bus.frame().pixel(239, 0), 0);
        bus.advance(4);
        assert_eq!(bus.frame().pixel(239, 0), 0x001F);
        assert_eq!(bus.frame().pixel(0, 1), 0);
        assert_eq!(finished_lines(bus.cycles()), 1);

        bus.write_u16(VRAM, 0x7C00);
        bus.advance((CYCLES_PER_LINE - VISIBLE_CYCLES) as u32);
        assert_eq!(bus.cycles(), CYCLES_PER_LINE);
        assert_eq!(bus.frame().pixel(0, 0), 0x001F);
        assert_eq!(bus.frame().pixel(0, 1), 0);
    }

    #[test]
    fn the_next_frame_overwrites_a_latched_pixel() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x0403);
        bus.write_u16(VRAM, 0x001F);
        bus.advance(4);
        bus.write_u16(VRAM, 0x03E0);
        bus.advance((CYCLES_PER_FRAME - 4) as u32);
        assert_eq!(bus.frame().pixel(0, 0), 0x001F);
        bus.advance(4);
        assert_eq!(bus.frame().pixel(0, 0), 0x03E0);
    }

    #[test]
    fn a_bitmap_write_dirties_only_that_line() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x0403);
        bus.advance(CYCLES_PER_FRAME as u32);
        assert!(!bus.line_dirty(0));
        assert!(!bus.line_dirty(5));
        bus.write_u16(VRAM + 5 * 240 * 2, 0x001F);
        assert!(bus.line_dirty(5));
        assert!(!bus.line_dirty(4));
        assert!(!bus.line_dirty(6));
        bus.write_u16(0x0400_0010, 0x0003);
        assert!(bus.line_dirty(0) && bus.line_dirty(159));
    }

    #[test]
    fn palette_write_dirties_every_line() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x0403);
        bus.advance(CYCLES_PER_FRAME as u32);
        assert!(!bus.line_dirty(0));
        bus.write_u16(0x0500_0000, 0x7C00);
        assert!(bus.line_dirty(0) && bus.line_dirty(80) && bus.line_dirty(159));
    }

    #[test]
    fn an_object_dirties_only_its_rows() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x1403);
        bus.advance(CYCLES_PER_FRAME as u32);
        assert!(!bus.line_dirty(19) && !bus.line_dirty(20) && !bus.line_dirty(28));
        put_obj(&mut bus, 0, 20, 0, 0);
        assert!(!bus.line_dirty(19));
        assert!(bus.line_dirty(20) && bus.line_dirty(27));
        assert!(!bus.line_dirty(28));
    }

    #[test]
    fn a_clean_line_is_not_composed_again() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x0403);
        bus.write_u16(VRAM, 0x001F);
        bus.advance(CYCLES_PER_FRAME as u32);
        let composed = bus.composed_pixels();
        assert!(composed >= 160 * 240);
        assert!(!bus.line_dirty(0));
        bus.poke_vram_quietly(0, 0xE0);
        bus.poke_vram_quietly(1, 0x03);
        bus.advance(4);
        assert_eq!(bus.composed_pixels(), composed);
        assert_eq!(bus.frame().pixel(0, 0), 0x001F);
        bus.write_u16(VRAM, 0x03E0);
        assert!(bus.line_dirty(0));
        bus.advance(CYCLES_PER_FRAME as u32);
        assert_eq!(bus.frame().pixel(0, 0), 0x03E0);
        let present = bus.take_present();
        assert!(present[0] & 1 != 0);
        assert_eq!(bus.take_present(), [0; 20]);
    }

    #[test]
    fn forced_blank_latches_white() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x0483);
        bus.write_u16(VRAM, 0x001F);
        bus.advance(4);
        assert_eq!(bus.frame().pixel(0, 0), 0x7FFF);
    }

    fn layer(kind: LayerKind, priority: u8, color: Option<u16>) -> Layer {
        Layer {
            kind,
            priority,
            color,
            semi: false,
        }
    }

    #[test]
    fn smaller_priority_number_is_in_front() {
        let layers = [
            layer(LayerKind::Background(1), 3, Some(0x001F)),
            layer(LayerKind::Background(0), 0, Some(0x03E0)),
        ];
        assert_eq!(front_color(&layers, 0), 0x03E0);
    }

    #[test]
    fn a_sprite_covers_a_background_on_the_same_priority() {
        let layers = [
            layer(LayerKind::Background(0), 1, Some(0x001F)),
            layer(LayerKind::Sprite(4), 1, Some(0x03E0)),
        ];
        assert_eq!(front_color(&layers, 0), 0x03E0);
    }

    #[test]
    fn lower_background_number_wins_the_same_priority() {
        let layers = [
            layer(LayerKind::Background(3), 2, Some(0x001F)),
            layer(LayerKind::Background(1), 2, Some(0x03E0)),
        ];
        assert_eq!(front_color(&layers, 0), 0x03E0);
    }

    #[test]
    fn lower_sprite_index_wins_the_same_priority() {
        let layers = [
            layer(LayerKind::Sprite(9), 0, Some(0x001F)),
            layer(LayerKind::Sprite(2), 0, Some(0x03E0)),
        ];
        assert_eq!(front_color(&layers, 0), 0x03E0);
    }

    #[test]
    fn a_transparent_pixel_shows_the_layer_behind_it() {
        let layers = [
            layer(LayerKind::Sprite(0), 0, None),
            layer(LayerKind::Background(2), 1, Some(0x7C00)),
        ];
        assert_eq!(front_color(&layers, 0x001F), 0x7C00);
        assert_eq!(front_color(&[], 0x001F), 0x001F);
    }

    #[test]
    fn mode3_black_covers_the_backdrop() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x0403);
        bus.write_u16(BACKDROP, 0x001F);
        bus.advance(4);
        assert_eq!(bus.frame().pixel(0, 0), 0);

        bus.write_u16(VRAM + 0xA000, 0x03E0);
        bus.write_u16(DISPCNT, 0x0413);
        bus.advance(4);
        assert_eq!(bus.frame().pixel(0, 0), 0);
    }

    #[test]
    fn mode4_uses_palette_indexes_and_two_pages() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x1404);
        bus.write_u16(BACKDROP, 0x001F);
        bus.write_u16(0x0500_0004, 0x7C00);
        bus.write_u16(0x0500_0006, 0x03E0);
        hide_objs(&mut bus);
        bus.write_u16(OBJ_PALETTE + 2, 0x03E0);
        bus.store_u8(OBJ_TILES, 0x01);
        put_obj(&mut bus, 0, 0, 0, 3 << 10);
        bus.store_u8(VRAM, 0);
        bus.store_u8(VRAM + 1, 2);
        bus.store_u8(VRAM + 160, 3);
        bus.store_u8(VRAM + 240, 2);
        let frame = render_frame(&mut bus);
        assert_eq!(frame.pixel(0, 0), 0x001F);
        assert_eq!(frame.pixel(1, 0), 0x7C00);
        assert_eq!(frame.pixel(0, 1), 0x7C00);

        bus.store_u8(VRAM + 0xA000, 2);
        bus.write_u16(DISPCNT, 0x0414);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x7C00);
        bus.write_u16(DISPCNT, 0x0404);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x001F);
    }

    #[test]
    fn mode5_is_a_small_direct_color_page() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x1405);
        bus.write_u16(BACKDROP, 0x7C00);
        bus.write_u16(VRAM, 0x001F);
        bus.write_u16(VRAM + 2, 0);
        bus.write_u16(VRAM + (127 * 160 + 159) * 2, 0x03E0);
        hide_objs(&mut bus);
        bus.write_u16(OBJ_PALETTE + 2, 0x001F);
        bus.write_u16(OBJ_PALETTE + 4, 0x03E0);
        bus.store_u8(OBJ_TILES, 0x01);
        bus.store_u8(OBJ_TILES + 32, 0x02);
        put_obj(&mut bus, 0, 0, 0, 3 << 10);
        put_obj(&mut bus, 1, 0, 160, 1);
        let frame = render_frame(&mut bus);
        assert_eq!(frame.pixel(0, 0), 0x001F);
        assert_eq!(frame.pixel(1, 0), 0);
        assert_eq!(frame.pixel(159, 127), 0x03E0);
        assert_eq!(frame.pixel(160, 0), 0x03E0);
        assert_eq!(frame.pixel(0, 128), 0x7C00);

        bus.write_u16(VRAM + 0xA000, 0x03E0);
        bus.write_u16(DISPCNT, 0x0415);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x03E0);
        bus.write_u16(DISPCNT, 0x0405);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x001F);
    }

    #[test]
    fn a_disabled_background_shows_the_backdrop() {
        let mut bus = Bus::new();
        bus.write_u16(BACKDROP, 0x001F);
        bus.write_u16(VRAM, 0x03E0);
        bus.advance(4);
        assert_eq!(bus.frame().pixel(0, 0), 0x001F);
    }

    const OAM: u32 = 0x0700_0000;
    const OBJ_TILES: u32 = 0x0601_4000;
    const OBJ_PALETTE: u32 = 0x0500_0200;

    fn hide_objs(bus: &mut Bus) {
        for index in 0..128 {
            bus.write_u16(OAM + index * 8, 1 << 9);
        }
    }

    fn put_obj(bus: &mut Bus, index: u32, attr0: u16, attr1: u16, attr2: u16) {
        let addr = OAM + index * 8;
        bus.write_u16(addr, attr0);
        bus.write_u16(addr + 2, attr1);
        bus.write_u16(addr + 4, attr2);
    }

    fn put_matrix(bus: &mut Bus, slot: u32, pa: u16, pb: u16, pc: u16, pd: u16) {
        let addr = OAM + slot * 32;
        bus.write_u16(addr + 6, pa);
        bus.write_u16(addr + 0x0E, pb);
        bus.write_u16(addr + 0x16, pc);
        bus.write_u16(addr + 0x1E, pd);
    }

    #[test]
    fn a_sprite_covers_the_bitmap_pixel() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x1403);
        hide_objs(&mut bus);
        bus.write_u16(VRAM, 0x7C00);
        bus.write_u16(VRAM + 2, 0x7C00);
        bus.write_u16(OBJ_PALETTE + 2, 0x001F);
        bus.store_u8(OBJ_TILES, 0x01);
        put_obj(&mut bus, 0, 0, 0, 0);

        let frame = render_frame(&mut bus);
        assert_eq!(frame.pixel(0, 0), 0x001F);
        assert_eq!(frame.pixel(1, 0), 0x7C00);
        bus.advance(4);
        assert_eq!(bus.frame().pixel(0, 0), 0x001F);
    }

    #[test]
    fn a_later_priority_sprite_stays_behind_the_background() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x1403);
        hide_objs(&mut bus);
        bus.write_u16(VRAM, 0x7C00);
        bus.write_u16(OBJ_PALETTE + 2, 0x001F);
        bus.store_u8(OBJ_TILES, 0x01);
        put_obj(&mut bus, 0, 0, 0, 1 << 10);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x7C00);
    }

    #[test]
    fn the_lower_sprite_index_wins() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x1403);
        hide_objs(&mut bus);
        bus.write_u16(OBJ_PALETTE + 2, 0x001F);
        bus.write_u16(OBJ_PALETTE + 4, 0x03E0);
        bus.store_u8(OBJ_TILES, 0x01);
        bus.store_u8(OBJ_TILES + 32, 0x02);
        put_obj(&mut bus, 0, 0, 0, 0);
        put_obj(&mut bus, 1, 0, 0, 1);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x001F);

        put_obj(&mut bus, 0, 0, 0, 1 << 10);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x03E0);
    }

    #[test]
    fn hidden_objects_stay_off_and_affine_uses_its_matrix() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x1403);
        hide_objs(&mut bus);
        bus.write_u16(VRAM, 0x7C00);
        bus.write_u16(OBJ_PALETTE + 2, 0x001F);
        bus.store_u8(OBJ_TILES, 0x01);
        put_obj(&mut bus, 0, 1 << 9, 0, 0);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x7C00);

        put_obj(&mut bus, 0, 1 << 8, 0, 0);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x7C00);
        put_matrix(&mut bus, 0, 0x0100, 0, 0, 0x0100);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x001F);

        bus.write_u16(DISPCNT, 0x0403);
        put_obj(&mut bus, 0, 0, 0, 0);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x7C00);
    }

    #[test]
    fn flips_and_the_second_tile_are_sampled() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x1443);
        hide_objs(&mut bus);
        bus.write_u16(OBJ_PALETTE + 2, 0x001F);
        bus.store_u8(OBJ_TILES + 3, 0x10);
        put_obj(&mut bus, 0, 0, 1 << 12, 0);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x001F);
        assert_eq!(render_frame(&mut bus).pixel(7, 0), 0);

        bus.store_u8(OBJ_TILES + 3, 0);
        bus.store_u8(OBJ_TILES, 0x01);
        put_obj(&mut bus, 0, 0, 1 << 13, 0);
        assert_eq!(render_frame(&mut bus).pixel(0, 7), 0x001F);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0);

        bus.store_u8(OBJ_TILES, 0);
        bus.store_u8(OBJ_TILES + 32, 0x01);
        put_obj(&mut bus, 0, 1 << 14, 0, 0);
        assert_eq!(render_frame(&mut bus).pixel(8, 0), 0x001F);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0);
    }

    const BG0CNT: u32 = 0x0400_0008;
    const BG0HOFS: u32 = 0x0400_0010;
    const BG0VOFS: u32 = 0x0400_0012;

    fn enable_bg0(bus: &mut Bus, cnt: u16) {
        bus.write_u16(DISPCNT, 0x0100);
        bus.write_u16(BG0CNT, cnt);
    }

    #[test]
    fn bg0_reads_the_scrolled_tile() {
        let mut bus = Bus::new();
        enable_bg0(&mut bus, 0x0100);
        bus.write_u16(0x0500_0002, 0x001F);
        bus.write_u16(0x0500_0004, 0x03E0);
        bus.store_u8(VRAM, 0x01);
        bus.store_u8(VRAM + 32, 0x02);
        bus.write_u16(VRAM + 0x800, 0);
        bus.write_u16(VRAM + 0x800 + 2, 1);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x001F);
        assert_eq!(render_frame(&mut bus).pixel(1, 0), 0);

        bus.write_u16(BG0HOFS, 8);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x03E0);
        bus.write_u16(BG0HOFS, 248);
        assert_eq!(render_frame(&mut bus).pixel(8, 0), 0x001F);
    }

    #[test]
    fn bg0_flip_and_palette_come_from_the_map_entry() {
        let mut bus = Bus::new();
        enable_bg0(&mut bus, 0x0100);
        bus.write_u16(BACKDROP, 0x7C00);
        bus.write_u16(0x0500_0002, 0x001F);
        bus.store_u8(VRAM + 3, 0x10);
        bus.write_u16(VRAM + 0x800, 1 << 10);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x001F);
        assert_eq!(render_frame(&mut bus).pixel(7, 0), 0x7C00);

        bus.write_u16(0x0500_0022, 0x03E0);
        bus.store_u8(VRAM, 0x01);
        bus.write_u16(VRAM + 0x800, 1 << 12);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x03E0);
    }

    #[test]
    fn a_wide_map_uses_the_next_screen_block() {
        let mut bus = Bus::new();
        enable_bg0(&mut bus, 0x4100);
        bus.write_u16(0x0500_0002, 0x001F);
        bus.write_u16(0x0500_0004, 0x03E0);
        bus.store_u8(VRAM, 0x01);
        bus.store_u8(VRAM + 32, 0x02);
        bus.write_u16(VRAM + 0x800, 0);
        bus.write_u16(VRAM + 0x1000, 1);
        bus.write_u16(BG0HOFS, 256);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x03E0);
    }

    #[test]
    fn a_sprite_covers_bg0_on_the_same_priority() {
        let mut bus = Bus::new();
        enable_bg0(&mut bus, 0x0100);
        bus.write_u16(DISPCNT, 0x1100);
        hide_objs(&mut bus);
        bus.write_u16(0x0500_0002, 0x7C00);
        bus.store_u8(VRAM, 0x01);
        bus.write_u16(VRAM + 0x800, 0);
        bus.write_u16(OBJ_PALETTE + 2, 0x001F);
        bus.store_u8(0x0601_0000, 0x01);
        put_obj(&mut bus, 0, 0, 0, 0);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x001F);
        bus.write_u16(BG0VOFS, 8);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x001F);
    }

    #[test]
    fn a_256_color_tile_uses_the_full_background_palette() {
        let mut bus = Bus::new();
        enable_bg0(&mut bus, 0x0180);
        bus.write_u16(0x0500_0002, 0x001F);
        bus.write_u16(0x0500_000A, 0x03E0);
        bus.store_u8(VRAM + 32, 0x01);
        bus.store_u8(VRAM + 64, 0x05);
        bus.store_u8(VRAM + 65, 0x00);
        bus.write_u16(VRAM + 0x800, 1 | (1 << 12));
        let frame = render_frame(&mut bus);
        assert_eq!(frame.pixel(0, 0), 0x03E0);
        assert_eq!(frame.pixel(1, 0), 0);
    }

    #[test]
    fn backgrounds_1_to_3_scroll_on_their_own() {
        let cases = [
            (0x0200u16, 0x0400_000Au32, 0x0400_0014u32),
            (0x0400, 0x0400_000C, 0x0400_0018),
            (0x0800, 0x0400_000E, 0x0400_001C),
        ];
        for (disp, cnt, hofs) in cases {
            let mut bus = Bus::new();
            bus.write_u16(DISPCNT, disp);
            bus.write_u16(cnt, 0x0100);
            bus.write_u16(0x0500_0002, 0x001F);
            bus.store_u8(VRAM + 32, 0x01);
            bus.write_u16(VRAM + 0x800 + 2, 1);
            bus.write_u16(hofs, 8);
            assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x001F);
        }
    }

    #[test]
    fn the_lower_background_number_wins_when_priorities_match() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x0300);
        bus.write_u16(BG0CNT, 0x0101);
        bus.write_u16(0x0400_000A, 0x0205);
        bus.write_u16(0x0500_0002, 0x001F);
        bus.write_u16(0x0500_0004, 0x03E0);
        bus.store_u8(VRAM, 0x01);
        bus.store_u8(VRAM + 0x4000, 0x02);
        bus.write_u16(VRAM + 0x800, 0);
        bus.write_u16(VRAM + 0x1000, 0);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x001F);

        bus.write_u16(0x0400_000A, 0x0204);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x03E0);
    }

    #[test]
    fn mode1_does_not_treat_bg2_as_a_text_map() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x0201);
        bus.write_u16(0x0400_000A, 0x0100);
        bus.write_u16(BACKDROP, 0x7C00);
        bus.write_u16(0x0500_0002, 0x001F);
        bus.store_u8(VRAM, 0x01);
        bus.write_u16(VRAM + 0x800, 0);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x001F);

        bus.write_u16(DISPCNT, 0x0401);
        bus.write_u16(0x0400_000C, 0x0100);
        bus.write_u16(0x0400_0020, 0x0100);
        bus.write_u16(0x0400_0026, 0x0100);
        bus.store_u8(VRAM + 64, 0x02);
        bus.store_u8(VRAM + 0x800 + 1, 1);
        bus.write_u16(0x0500_0004, 0x03E0);
        let frame = render_frame(&mut bus);
        assert_eq!(frame.pixel(0, 0), 0x001F);
        assert_eq!(frame.pixel(8, 0), 0x03E0);
    }

    #[test]
    fn affine_reference_and_wrap_belong_to_that_layer() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x0401);
        bus.write_u16(0x0400_000C, 0x0100);
        bus.write_u16(BACKDROP, 0x7C00);
        bus.write_u16(0x0500_0002, 0x001F);
        bus.store_u8(VRAM, 0x01);
        bus.store_u8(VRAM + 0x800, 0);
        bus.write_u16(0x0400_0020, 0x0100);
        bus.write_u16(0x0400_0026, 0x0100);
        bus.write_u32(0x0400_0028, 128 << 8);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x7C00);

        bus.write_u16(0x0400_000C, 0x2100);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x001F);

        bus.write_u16(DISPCNT, 0x0802);
        bus.write_u16(0x0400_000E, 0x2100);
        bus.write_u16(0x0400_0030, 0x0100);
        bus.write_u16(0x0400_0036, 0x0100);
        bus.write_u32(0x0400_0038, 128 << 8);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x001F);
    }

    #[test]
    fn an_affine_sprite_scales_around_its_center() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x1403);
        hide_objs(&mut bus);
        bus.write_u16(VRAM, 0x7C00);
        bus.write_u16(OBJ_PALETTE + 2, 0x001F);
        bus.store_u8(OBJ_TILES, 0x01);
        put_obj(&mut bus, 0, 1 << 8, 0, 0);
        put_matrix(&mut bus, 0, 0x0200, 0, 0, 0x0100);
        let frame = render_frame(&mut bus);
        assert_eq!(frame.pixel(0, 0), 0x7C00);
        assert_eq!(frame.pixel(2, 0), 0x001F);

        put_obj(&mut bus, 0, (1 << 8) | (1 << 9), 0, 0);
        put_matrix(&mut bus, 0, 0x0100, 0, 0, 0x0100);
        let frame = render_frame(&mut bus);
        assert_eq!(frame.pixel(0, 0), 0x7C00);
        assert_eq!(frame.pixel(4, 4), 0x001F);
    }

    #[test]
    fn mosaic_repeats_the_anchor_and_stops_at_the_sprite_box() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x0403);
        bus.write_u16(0x0400_000C, 1 << 6);
        bus.write_u16(0x0400_004C, 0x0011);
        bus.write_u16(VRAM, 0x001F);
        bus.write_u16(VRAM + 2, 0x7C00);
        bus.write_u16(VRAM + 480, 0x03E0);
        let frame = render_frame(&mut bus);
        assert_eq!(frame.pixel(0, 0), 0x001F);
        assert_eq!(frame.pixel(1, 0), 0x001F);
        assert_eq!(frame.pixel(0, 1), 0x001F);

        bus.write_u16(0x0400_000C, 0);
        assert_eq!(render_frame(&mut bus).pixel(1, 0), 0x7C00);

        bus.write_u16(DISPCNT, 0x1403);
        bus.write_u16(BACKDROP, 0x7C00);
        hide_objs(&mut bus);
        bus.write_u16(0x0400_004C, 0x0100);
        bus.write_u16(OBJ_PALETTE + 2, 0x001F);
        bus.write_u16(OBJ_PALETTE + 4, 0x03E0);
        bus.store_u8(OBJ_TILES, 0x21);
        put_obj(&mut bus, 0, 1 << 12, 0, 0);
        let frame = render_frame(&mut bus);
        assert_eq!(frame.pixel(0, 0), 0x001F);
        assert_eq!(frame.pixel(1, 0), 0x001F);

        put_obj(&mut bus, 0, 1 << 12, 1, 0);
        assert_eq!(render_frame(&mut bus).pixel(1, 0), 0x7C00);
    }

    #[test]
    fn alpha_blend_mixes_only_the_top_two_targets() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x1403);
        hide_objs(&mut bus);
        bus.write_u16(VRAM, 0x03E0);
        bus.write_u16(OBJ_PALETTE + 2, 0x0010);
        bus.store_u8(OBJ_TILES, 0x01);
        put_obj(&mut bus, 0, 0, 0, 0);
        bus.write_u16(0x0400_0050, (1 << 6) | (1 << 4) | (1 << 10));
        bus.write_u16(0x0400_0052, 0x001F);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x0010);

        bus.write_u16(0x0400_0052, 0x1000);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x03E0);

        bus.write_u16(0x0400_0050, 1 << 6);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x0010);
    }

    #[test]
    fn semi_transparent_sprites_blend_unless_brightness_is_selected() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x1403);
        hide_objs(&mut bus);
        bus.write_u16(VRAM, 0x001F);
        bus.write_u16(OBJ_PALETTE + 2, 0x03E0);
        bus.store_u8(OBJ_TILES, 0x01);
        put_obj(&mut bus, 0, 1 << 10, 0, 0);
        bus.write_u16(0x0400_0050, 1 << 10);
        bus.write_u16(0x0400_0052, 0x1000);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x001F);

        bus.write_u16(0x0400_0050, 2 << 6);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x03E0);

        bus.write_u16(OBJ_PALETTE + 2, 0);
        bus.write_u16(0x0400_0050, (2 << 6) | (1 << 4));
        bus.write_u16(0x0400_0054, 16);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x7FFF);

        bus.write_u16(DISPCNT, 0);
        bus.write_u16(BACKDROP, 0);
        bus.write_u16(0x0400_0050, (2 << 6) | (1 << 5));
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x7FFF);
    }

    #[test]
    fn windows_choose_which_layers_and_whether_blending_runs() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x2403);
        bus.write_u16(BACKDROP, 0x03E0);
        bus.write_u16(VRAM, 0x001F);
        bus.write_u16(VRAM + 2, 0x001F);
        bus.write_u16(0x0400_0040, 0x0001);
        bus.write_u16(0x0400_0044, 0x0001);
        bus.write_u16(0x0400_0048, 1 << 2);
        let frame = render_frame(&mut bus);
        assert_eq!(frame.pixel(0, 0), 0x001F);
        assert_eq!(frame.pixel(1, 0), 0x03E0);

        bus.write_u16(DISPCNT, 0x3403);
        hide_objs(&mut bus);
        bus.write_u16(OBJ_PALETTE + 2, 0x7C00);
        bus.store_u8(OBJ_TILES, 0x01);
        put_obj(&mut bus, 0, 1 << 10, 0, 0);
        bus.write_u16(0x0400_0040, 0x00F0);
        bus.write_u16(0x0400_0044, 0x00A0);
        bus.write_u16(0x0400_0048, (1 << 2) | (1 << 4));
        bus.write_u16(0x0400_0050, 1 << 10);
        bus.write_u16(0x0400_0052, 0x1000);
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x7C00);

        bus.write_u16(0x0400_0048, (1 << 2) | (1 << 4) | (1 << 5));
        assert_eq!(render_frame(&mut bus).pixel(0, 0), 0x001F);

        bus.write_u16(DISPCNT, 0x9403);
        bus.write_u16(0x0400_004A, (1 << 10) | (1 << 12));
        put_obj(&mut bus, 0, 2 << 10, 0, 0);
        let frame = render_frame(&mut bus);
        assert_eq!(frame.pixel(0, 0), 0x001F);
        assert_eq!(frame.pixel(1, 0), 0x03E0);
    }

    fn assert_same_as_raw(bus: &mut Bus) {
        bus.set_gfx_raw(true);
        let raw = render_frame(bus);
        bus.set_gfx_raw(false);
        bus.gfx.invalidate_tiles();
        bus.gfx.invalidate_sprite_lines();
        let cached = render_frame(bus);
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                assert_eq!(
                    cached.pixel(x, y),
                    raw.pixel(x, y),
                    "({x},{y}) 缓存 {:04X} 原始 {:04X}",
                    cached.pixel(x, y),
                    raw.pixel(x, y)
                );
            }
        }
    }

    #[test]
    fn cached_pixels_match_the_uncached_path() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x1401);
        hide_objs(&mut bus);
        bus.write_u16(BACKDROP, 0x7C00);
        bus.write_u16(0x0400_000C, 0x0080);
        bus.write_u16(0x0500_0002, 0x001F);
        bus.write_u16(0x0500_0004, 0x03E0);
        bus.store_u8(VRAM, 0x21);
        bus.store_u8(VRAM + 3, 0x10);
        bus.write_u16(VRAM + 0x800, 1 << 10);
        bus.write_u16(0x0400_0020, 0x0100);
        bus.write_u16(0x0400_0026, 0x0100);
        bus.write_u16(OBJ_PALETTE + 2, 0x001F);
        bus.store_u8(0x0601_0000, 0x01);
        bus.store_u8(0x0601_0000 + 3, 0x10);
        put_obj(&mut bus, 0, 1 << 8, 1 << 12, 0);
        put_matrix(&mut bus, 0, 0x0080, 0, 0, 0x0100);
        assert_same_as_raw(&mut bus);
        let misses = bus.tile_misses();
        let scans = bus.sprite_scans();
        bus.write_u16(0x0400_0020, 0x0200);
        let again = render_frame(&mut bus);
        bus.set_gfx_raw(true);
        let raw = render_frame(&mut bus);
        bus.set_gfx_raw(false);
        assert_eq!(again.pixel(0, 0), raw.pixel(0, 0));
        assert_eq!(again.pixel(8, 0), raw.pixel(8, 0));
        assert_eq!(bus.tile_misses(), misses);
        assert_eq!(bus.sprite_scans(), scans);
    }

    #[test]
    fn the_next_latch_reuses_the_tile_and_the_sprite_list() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x1403);
        hide_objs(&mut bus);
        bus.write_u16(OBJ_PALETTE + 2, 0x001F);
        bus.write_u16(OBJ_PALETTE + 4, 0x03E0);
        bus.store_u8(OBJ_TILES, 0x21);
        put_obj(&mut bus, 0, 0, 0, 0);
        bus.advance(4);
        let misses = bus.tile_misses();
        let scans = bus.sprite_scans();
        assert!(misses >= 1);
        assert_eq!(scans, 1);
        bus.advance(4);
        assert_eq!(bus.tile_misses(), misses);
        assert!(bus.tile_hits() >= 1);
        assert_eq!(bus.sprite_scans(), scans);
        assert!(bus.sprite_hits() >= 1);
        assert_eq!(bus.frame().pixel(0, 0), 0x001F);
        assert_eq!(bus.frame().pixel(1, 0), 0x03E0);
    }

    #[test]
    fn palette_and_flip_do_not_decode_the_tile_again() {
        let mut bus = Bus::new();
        enable_bg0(&mut bus, 0x0100);
        bus.write_u16(BACKDROP, 0x7C00);
        bus.write_u16(0x0500_0002, 0x001F);
        bus.store_u8(VRAM + 3, 0x10);
        bus.write_u16(VRAM + 0x800, 1 << 10);
        bus.advance(4);
        assert_eq!(bus.frame().pixel(0, 0), 0x001F);
        let misses = bus.tile_misses();
        bus.write_u16(0x0500_0002, 0x03E0);
        bus.advance(CYCLES_PER_FRAME as u32);
        assert_eq!(bus.frame().pixel(0, 0), 0x03E0);
        assert_eq!(bus.tile_misses(), misses);

        bus.write_u16(VRAM + 0x800, 0);
        bus.advance(CYCLES_PER_FRAME as u32);
        assert_eq!(bus.frame().pixel(7, 0), 0x03E0);
        assert_eq!(bus.tile_misses(), misses);
    }

    #[test]
    fn moving_a_sprite_dirties_both_rows_and_rescans() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x1403);
        hide_objs(&mut bus);
        put_obj(&mut bus, 0, 0, 0, 0);
        bus.advance(CYCLES_PER_FRAME as u32);
        let scans = bus.sprite_scans();
        bus.advance(4);
        assert_eq!(bus.sprite_scans(), scans);
        assert!(bus.sprite_hits() >= 1);
        bus.write_u16(0x0700_0002, 30);
        bus.advance(4);
        assert_eq!(bus.sprite_scans(), scans);
        bus.write_u16(0x0700_0000, 20);
        assert!(bus.line_dirty(0));
        assert!(bus.line_dirty(20));
        assert!(bus.line_dirty(27));
        assert!(!bus.line_dirty(28));
        bus.advance(4);
        assert!(bus.sprite_scans() > scans);
    }

    #[test]
    fn a_tile_write_is_visible_on_the_next_sample() {
        let mut bus = Bus::new();
        enable_bg0(&mut bus, 0x0100);
        bus.write_u16(0x0500_0002, 0x001F);
        bus.write_u16(0x0500_0004, 0x03E0);
        bus.store_u8(VRAM, 0x21);
        bus.write_u16(VRAM + 0x800, 0);
        bus.advance(4);
        assert_eq!(bus.frame().pixel(0, 0), 0x001F);
        let misses = bus.tile_misses();
        bus.store_u8(VRAM, 0x22);
        bus.advance(4);
        assert_eq!(bus.frame().pixel(0, 0), 0x001F);
        assert_eq!(bus.frame().pixel(1, 0), 0x03E0);
        assert!(bus.tile_misses() > misses);
        let misses = bus.tile_misses();
        bus.store_u8(0x0602_0000, 0x12);
        bus.advance(CYCLES_PER_FRAME as u32);
        assert!(bus.tile_misses() > misses);
        assert_eq!(bus.frame().pixel(0, 0), 0x03E0);
        assert_eq!(bus.frame().pixel(1, 0), 0x001F);
    }

    #[test]
    fn mode4_object_tiles_dirty_more_than_line_zero() {
        let mut bus = Bus::new();
        bus.write_u16(DISPCNT, 0x0404);
        bus.advance(CYCLES_PER_FRAME as u32);
        assert!(!bus.line_dirty(40));
        bus.store_u8(0x0601_4000, 0x01);
        assert!(bus.line_dirty(40));
        assert!(bus.line_dirty(0));
    }

    #[test]
    #[ignore = "同一张文字背景画两遍，看第二次是否命中图块缓存"]
    fn second_text_frame_reuses_decoded_tiles() {
        use std::time::Instant;
        let mut bus = Bus::new();
        enable_bg0(&mut bus, 0x0100);
        bus.write_u16(0x0500_0002, 0x001F);
        for tile in 0..32u32 {
            bus.store_u8(VRAM + tile * 32, 0x01);
            bus.write_u16(VRAM + 0x800 + tile * 2, tile as u16);
        }
        let _warm = render_frame(&mut bus);
        let misses = bus.tile_misses();
        bus.set_gfx_raw(true);
        let raw_started = Instant::now();
        let raw = render_frame(&mut bus);
        let raw_elapsed = raw_started.elapsed();
        bus.set_gfx_raw(false);
        let started = Instant::now();
        let frame = render_frame(&mut bus);
        let elapsed = started.elapsed();
        assert_eq!(frame.pixel(0, 0), raw.pixel(0, 0));
        assert_eq!(bus.tile_misses(), misses);
        assert!(bus.tile_hits() > 1000);
        eprintln!(
            "文字背景整帧：不走缓存 {raw_elapsed:.3?}，命中缓存 {elapsed:.3?}，图块命中 {}",
            bus.tile_hits()
        );
    }
}
