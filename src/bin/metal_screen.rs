//! Presents the GBA frame with a nearest-neighbor Metal sampler.
//!
//! The view sits under GPUI's window view. GPUI leaves the screen rectangle
//! unpainted, so this layer shows through, and GPUI overlays stay in front.

use cocoa::appkit::NSView;
use cocoa::base::{id, nil};
use cocoa::foundation::{NSPoint, NSRect, NSSize, NSString};
use gameboy_ultima::{HEIGHT, WIDTH};
use gpui::{Bounds, Pixels, Window, px};
use metal::foreign_types::ForeignType;
use metal::{
    Buffer, CompileOptions, Device, MTLClearColor, MTLLoadAction, MTLPixelFormat,
    MTLPrimitiveType, MTLResourceOptions, MTLStorageMode, MTLStoreAction,
    MTLTextureType, MTLTextureUsage, MetalLayer, RenderPassDescriptor, RenderPipelineDescriptor,
    RenderPipelineState, Texture, TextureDescriptor,
};
use objc::runtime::YES;
use objc::{class, msg_send, sel, sel_impl};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

const SHADER: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct ScreenVertex {
    float4 position [[position]];
    float2 uv;
};

vertex ScreenVertex screen_vertex(uint vid [[vertex_id]]) {
    // Metal clip space points up. The texture origin is the first row of the frame.
    const float2 position[6] = {
        float2(-1.0, -1.0), float2(1.0, -1.0), float2(-1.0, 1.0),
        float2(-1.0, 1.0), float2(1.0, -1.0), float2(1.0, 1.0)
    };
    const float2 uv[6] = {
        float2(0.0, 1.0), float2(1.0, 1.0), float2(0.0, 0.0),
        float2(0.0, 0.0), float2(1.0, 1.0), float2(1.0, 0.0)
    };
    ScreenVertex out;
    out.position = float4(position[vid], 0.0, 1.0);
    out.uv = uv[vid];
    return out;
}

fragment float4 screen_fragment(ScreenVertex in [[stage_in]], texture2d<float> tex [[texture(0)]]) {
    constexpr sampler nearest(mag_filter::nearest, min_filter::nearest, address::clamp_to_edge);
    return tex.sample(nearest, in.uv);
}

// RetroArch cheap-sharpen at its default 0.4, applied to source texels.
// A flat area stays unchanged. A texel next to a different color gains contrast.
fragment float4 screen_fragment_sharpen(ScreenVertex in [[stage_in]], texture2d<float> tex [[texture(0)]]) {
    constexpr sampler nearest(mag_filter::nearest, min_filter::nearest, address::clamp_to_edge);
    float2 texel = 1.0 / float2(tex.get_width(), tex.get_height());
    float3 center = tex.sample(nearest, in.uv).rgb;
    float3 up = tex.sample(nearest, in.uv + float2(0.0, -texel.y)).rgb;
    float3 down = tex.sample(nearest, in.uv + float2(0.0, texel.y)).rgb;
    float3 left = tex.sample(nearest, in.uv + float2(-texel.x, 0.0)).rgb;
    float3 right = tex.sample(nearest, in.uv + float2(texel.x, 0.0)).rgb;
    float sharpness = 0.4;
    float3 color = (1.0 + sharpness) * center - 0.25 * sharpness * (up + down + left + right);
    return float4(saturate(color), 1.0);
}
"#;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ScreenFilter {
    Nearest,
    Sharpen,
}

impl ScreenFilter {
    fn fragment(self) -> &'static str {
        match self {
            ScreenFilter::Nearest => "screen_fragment",
            ScreenFilter::Sharpen => "screen_fragment_sharpen",
        }
    }
}

pub struct MetalScreen {
    view: id,
    layer: MetalLayer,
    queue: metal::CommandQueue,
    pipeline: RenderPipelineState,
    filter: ScreenFilter,
    // The previous present may still be reading one buffer.
    frames: [LinearFrame; 2],
    shown: usize,
    has_frame: bool,
    frame_dirty: bool,
    attached: bool,
    placement: Placement,
}

struct LinearFrame {
    texture: Texture,
    buffer: Buffer,
    row_bytes: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Placement {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

impl MetalScreen {
    pub fn new(filter: ScreenFilter) -> Self {
        let device = Device::system_default().expect("Metal device");
        let queue = device.new_command_queue();
        let pipeline = screen_pipeline(&device, filter);
        let frames = [
            linear_frame(&device, WIDTH as u64, HEIGHT as u64),
            linear_frame(&device, WIDTH as u64, HEIGHT as u64),
        ];
        let layer = MetalLayer::new();
        layer.set_device(&device);
        layer.set_pixel_format(MTLPixelFormat::RGBA8Unorm);
        layer.set_framebuffer_only(true);
        layer.set_opaque(true);
        layer.set_edge_antialiasing_mask(0);
        layer.set_masks_to_bounds(true);
        unsafe {
            let _: () = msg_send![layer.as_ref(), setAllowsNextDrawableTimeout: YES];
            let filter = NSString::alloc(nil).init_str("nearest");
            let _: () = msg_send![layer.as_ref(), setMagnificationFilter: filter];
            let _: () = msg_send![layer.as_ref(), setMinificationFilter: filter];
            let _: () = msg_send![filter, release];
        }

        let view: id = unsafe { NSView::alloc(nil).initWithFrame_(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0))) };
        unsafe {
            view.setWantsLayer(YES);
            view.setLayer(layer.as_ptr() as id);
        }

        Self {
            view,
            layer,
            queue,
            pipeline,
            filter,
            frames,
            shown: 0,
            has_frame: false,
            frame_dirty: false,
            attached: false,
            placement: Placement {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            },
        }
    }

    pub fn set_filter(&mut self, filter: ScreenFilter) {
        if self.filter == filter {
            return;
        }
        self.pipeline = screen_pipeline(self.layer.device(), filter);
        self.filter = filter;
        self.frame_dirty = true;
    }

    pub fn upload(&mut self, rgba: &[u8]) {
        if rgba.len() != WIDTH * HEIGHT * 4 {
            return;
        }
        let index = self.shown ^ 1;
        write_rgba(&self.frames[index], rgba, WIDTH, HEIGHT);
        self.shown = index;
        self.has_frame = true;
        self.frame_dirty = true;
    }

    pub fn place(&mut self, window: &Window, bounds: Bounds<Pixels>, factor: u32) {
        let Some(host) = gpui_view(window) else {
            return;
        };
        if !self.attached {
            self.attach(host);
        }
        let scale = sane_scale(window.scale_factor());
        let frame = view_frame(host, bounds, factor, scale);
        let placement = Placement {
            x: (frame.origin.x * scale).round() as i32,
            y: (frame.origin.y * scale).round() as i32,
            width: (WIDTH as u32).saturating_mul(factor.max(1)),
            height: (HEIGHT as u32).saturating_mul(factor.max(1)),
        };
        let moved = placement != self.placement;
        if moved {
            unsafe {
                let _: () = msg_send![self.view, setFrame: frame];
            }
            self.layer.set_contents_scale(scale);
            unsafe {
                let size = CgSize {
                    width: placement.width as f64,
                    height: placement.height as f64,
                };
                let _: () = msg_send![self.layer.as_ref(), setDrawableSize: size];
            }
            self.placement = placement;
        }
        if self.has_frame && placement.width > 0 && (moved || self.frame_dirty) && self.present() {
            self.frame_dirty = false;
        }
    }

    fn attach(&mut self, host: id) {
        unsafe {
            let superview: id = msg_send![host, superview];
            if superview.is_null() {
                return;
            }
            let _: () = msg_send![
                superview,
                addSubview: self.view
                positioned: -1i64
                relativeTo: host
            ];
            let window: id = msg_send![host, window];
            if !window.is_null() {
                let color: id = msg_send![class!(NSColor), colorWithSRGBRed:0.0f64 green:0.0 blue:0.0 alpha:1.0];
                let _: () = msg_send![window, setBackgroundColor: color];
            }
        }
        self.attached = true;
    }

    fn present(&mut self) -> bool {
        let Some(drawable) = self.layer.next_drawable() else {
            return false;
        };
        let command = encode_nearest(
            &self.queue,
            &self.pipeline,
            &self.frames[self.shown].texture,
            drawable.texture(),
        );
        command.present_drawable(drawable);
        command.commit();
        true
    }
}

impl Drop for MetalScreen {
    fn drop(&mut self) {
        unsafe {
            if !self.view.is_null() {
                self.view.removeFromSuperview();
                let _: () = msg_send![self.view, release];
                self.view = nil;
            }
        }
    }
}

unsafe impl Send for MetalScreen {}

fn screen_pipeline(device: &metal::DeviceRef, filter: ScreenFilter) -> RenderPipelineState {
    let library = device
        .new_library_with_source(SHADER, &CompileOptions::new())
        .expect("compile the GBA screen shader");
    let vertex = library
        .get_function("screen_vertex", None)
        .expect("screen_vertex");
    let fragment = library
        .get_function(filter.fragment(), None)
        .expect(filter.fragment());
    let descriptor = RenderPipelineDescriptor::new();
    descriptor.set_vertex_function(Some(&vertex));
    descriptor.set_fragment_function(Some(&fragment));
    descriptor
        .color_attachments()
        .object_at(0)
        .unwrap()
        .set_pixel_format(MTLPixelFormat::RGBA8Unorm);
    device
        .new_render_pipeline_state(&descriptor)
        .expect("GBA screen pipeline")
}

fn linear_frame(device: &Device, width: u64, height: u64) -> LinearFrame {
    let align = device
        .minimum_linear_texture_alignment_for_pixel_format(MTLPixelFormat::RGBA8Unorm)
        .max(64) as usize;
    let tight = (width as usize).saturating_mul(4);
    let row_bytes = tight.div_ceil(align).saturating_mul(align).max(align);
    let length = row_bytes.saturating_mul(height as usize);
    let buffer = device.new_buffer(length as u64, MTLResourceOptions::StorageModeShared);
    unsafe {
        std::ptr::write_bytes(buffer.contents(), 0, length);
    }
    let descriptor = TextureDescriptor::new();
    descriptor.set_texture_type(MTLTextureType::D2);
    descriptor.set_pixel_format(MTLPixelFormat::RGBA8Unorm);
    descriptor.set_width(width);
    descriptor.set_height(height);
    descriptor.set_storage_mode(MTLStorageMode::Shared);
    descriptor.set_usage(MTLTextureUsage::ShaderRead);
    let texture = buffer.new_texture_with_descriptor(&descriptor, 0, row_bytes as u64);
    LinearFrame {
        texture,
        buffer,
        row_bytes,
    }
}

fn write_rgba(frame: &LinearFrame, rgba: &[u8], width: usize, height: usize) {
    let tight = width.saturating_mul(4);
    if rgba.len() != tight.saturating_mul(height) {
        return;
    }
    let destination = frame.buffer.contents().cast::<u8>();
    for y in 0..height {
        unsafe {
            std::ptr::copy_nonoverlapping(
                rgba.as_ptr().add(y * tight),
                destination.add(y * frame.row_bytes),
                tight,
            );
        }
    }
}

#[cfg(test)]
fn rgba_texture(device: &Device, width: u64, height: u64, render_target: bool) -> Texture {
    let descriptor = TextureDescriptor::new();
    descriptor.set_texture_type(MTLTextureType::D2);
    descriptor.set_pixel_format(MTLPixelFormat::RGBA8Unorm);
    descriptor.set_width(width);
    descriptor.set_height(height);
    descriptor.set_storage_mode(MTLStorageMode::Shared);
    let mut usage = MTLTextureUsage::ShaderRead;
    if render_target {
        usage |= MTLTextureUsage::RenderTarget;
    }
    descriptor.set_usage(usage);
    device.new_texture(&descriptor)
}

fn encode_nearest<'a>(
    queue: &'a metal::CommandQueueRef,
    pipeline: &metal::RenderPipelineStateRef,
    source: &metal::TextureRef,
    destination: &metal::TextureRef,
) -> &'a metal::CommandBufferRef {
    let pass = RenderPassDescriptor::new();
    let color = pass.color_attachments().object_at(0).unwrap();
    color.set_texture(Some(destination));
    color.set_load_action(MTLLoadAction::Clear);
    color.set_store_action(MTLStoreAction::Store);
    color.set_clear_color(MTLClearColor::new(0.0, 0.0, 0.0, 1.0));
    let command = queue.new_command_buffer();
    let encoder = command.new_render_command_encoder(pass);
    encoder.set_render_pipeline_state(pipeline);
    encoder.set_fragment_texture(0, Some(source));
    encoder.draw_primitives(MTLPrimitiveType::Triangle, 0, 6);
    encoder.end_encoding();
    command
}

fn gpui_view(window: &Window) -> Option<id> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    match handle.as_raw() {
        RawWindowHandle::AppKit(appkit) => Some(appkit.ns_view.as_ptr().cast()),
        _ => None,
    }
}

fn sane_scale(scale: f32) -> f64 {
    if scale.is_finite() && scale > 0.0 {
        scale as f64
    } else {
        1.0
    }
}

fn view_frame(host: id, bounds: Bounds<Pixels>, factor: u32, scale: f64) -> NSRect {
    let host_frame = unsafe { NSView::frame(host) };
    let width = (WIDTH as u32).saturating_mul(factor.max(1)) as f64 / scale;
    let height = (HEIGHT as u32).saturating_mul(factor.max(1)) as f64 / scale;
    let origin_x = host_frame.origin.x + snap_points(bounds.origin.x / px(1.0), scale);
    let origin_y = host_frame.origin.y + host_frame.size.height
        - snap_points(bounds.origin.y / px(1.0), scale)
        - height;
    NSRect::new(NSPoint::new(origin_x, origin_y), NSSize::new(width, height))
}

fn snap_points(points: f32, scale: f64) -> f64 {
    ((points as f64) * scale).round() / scale
}

#[repr(C)]
struct CgSize {
    width: f64,
    height: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_sample_keeps_each_source_pixel_a_solid_block() {
        let device = Device::system_default().expect("Metal device");
        let queue = device.new_command_queue();
        let pipeline = screen_pipeline(&device, ScreenFilter::Nearest);
        let source = linear_frame(&device, 2, 2);
        let pixels = [
            0x11, 0x00, 0x00, 0xFF, 0x00, 0x22, 0x00, 0xFF, 0x00, 0x00, 0x33, 0xFF, 0x44, 0x44,
            0x44, 0xFF,
        ];
        write_rgba(&source, &pixels, 2, 2);
        let destination = rgba_texture(&device, 4, 4, true);
        let command = encode_nearest(&queue, &pipeline, &source.texture, &destination);
        let row = 256u64;
        let readback = device.new_buffer(row * 4, metal::MTLResourceOptions::StorageModeShared);
        let blit = command.new_blit_command_encoder();
        blit.copy_from_texture_to_buffer(
            &destination,
            0,
            0,
            metal::MTLOrigin { x: 0, y: 0, z: 0 },
            metal::MTLSize {
                width: 4,
                height: 4,
                depth: 1,
            },
            &readback,
            0,
            row,
            row * 4,
            metal::MTLBlitOption::None,
        );
        blit.end_encoding();
        command.commit();
        command.wait_until_completed();

        let bytes = unsafe {
            std::slice::from_raw_parts(readback.contents().cast::<u8>(), (row * 4) as usize)
        };
        let pixel = |x: usize, y: usize| {
            let offset = y * row as usize + x * 4;
            [
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
            ]
        };
        assert_eq!(pixel(0, 0), [0x11, 0x00, 0x00, 0xFF]);
        assert_eq!(pixel(1, 1), [0x11, 0x00, 0x00, 0xFF]);
        assert_eq!(pixel(2, 0), [0x00, 0x22, 0x00, 0xFF]);
        assert_eq!(pixel(3, 1), [0x00, 0x22, 0x00, 0xFF]);
        assert_eq!(pixel(0, 2), [0x00, 0x00, 0x33, 0xFF]);
        assert_eq!(pixel(1, 3), [0x00, 0x00, 0x33, 0xFF]);
        assert_eq!(pixel(2, 2), [0x44, 0x44, 0x44, 0xFF]);
        assert_eq!(pixel(3, 3), [0x44, 0x44, 0x44, 0xFF]);
    }

    #[test]
    fn sharpen_raises_contrast_and_leaves_a_flat_area_unchanged() {
        let flat = present_filter(
            ScreenFilter::Sharpen,
            &[
                40, 80, 120, 255, 40, 80, 120, 255, 40, 80, 120, 255, 40, 80, 120, 255,
            ],
        );
        assert_eq!(flat(0, 0), [40, 80, 120, 255]);
        assert_eq!(flat(3, 3), [40, 80, 120, 255]);

        let edge = present_filter(
            ScreenFilter::Sharpen,
            &[
                0x11, 0x00, 0x00, 0xFF, 0x00, 0x22, 0x00, 0xFF, 0x00, 0x00, 0x33, 0xFF, 0x44,
                0x44, 0x44, 0xFF,
            ],
        );
        let nearest = present_filter(
            ScreenFilter::Nearest,
            &[
                0x11, 0x00, 0x00, 0xFF, 0x00, 0x22, 0x00, 0xFF, 0x00, 0x00, 0x33, 0xFF, 0x44,
                0x44, 0x44, 0xFF,
            ],
        );
        assert_eq!(edge(0, 0), edge(1, 1));
        assert!(edge(0, 0)[0] > nearest(0, 0)[0]);
        assert_eq!(edge(0, 0)[1], 0);
        assert_eq!(edge(0, 0)[2], 0);
        assert_eq!(edge(0, 0)[3], 255);
    }

    fn present_filter(filter: ScreenFilter, pixels: &[u8; 16]) -> impl Fn(usize, usize) -> [u8; 4] {
        let device = Device::system_default().expect("Metal device");
        let queue = device.new_command_queue();
        let pipeline = screen_pipeline(&device, filter);
        let source = linear_frame(&device, 2, 2);
        write_rgba(&source, pixels, 2, 2);
        let destination = rgba_texture(&device, 4, 4, true);
        let command = encode_nearest(&queue, &pipeline, &source.texture, &destination);
        let row = 256u64;
        let readback = device.new_buffer(row * 4, metal::MTLResourceOptions::StorageModeShared);
        let blit = command.new_blit_command_encoder();
        blit.copy_from_texture_to_buffer(
            &destination,
            0,
            0,
            metal::MTLOrigin { x: 0, y: 0, z: 0 },
            metal::MTLSize {
                width: 4,
                height: 4,
                depth: 1,
            },
            &readback,
            0,
            row,
            row * 4,
            metal::MTLBlitOption::None,
        );
        blit.end_encoding();
        command.commit();
        command.wait_until_completed();
        let bytes = unsafe {
            std::slice::from_raw_parts(readback.contents().cast::<u8>(), (row * 4) as usize)
        }
        .to_vec();
        move |x: usize, y: usize| {
            let offset = y * row as usize + x * 4;
            [
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
            ]
        }
    }
}
