//! Headless (no window/surface) render-and-compare tests for `GpuPainter` —
//! renders a scene via `vello`/`wgpu` to an offscreen texture, reads it back,
//! and samples known-safe interior points against a `SkiaPainter`-rendered
//! reference of the identical scene. Not pixel-identical (different AA/
//! color-space pipelines — see `nowui-render-gpu/src/lib.rs`'s module doc)
//! — a coarse tolerance is deliberate, not a placeholder to tighten later.

use std::sync::Mutex;

use nowui_core::{Color, Edges, Painter, Rect};
use nowui_render_gpu::{GpuFontCache, GpuPainter};
use vello::wgpu;

const WHITE: Color = Color { r: 255, g: 255, b: 255, a: 255 };
const RED: Color = Color { r: 220, g: 30, b: 30, a: 255 };
const BLUE: Color = Color { r: 30, g: 30, b: 220, a: 255 };

/// Rust's test harness runs every `#[test]` fn in this binary concurrently
/// by default, and each `render_gpu` call spins up its own `wgpu::Instance`/
/// `Device` — several alive at once (especially the text tests, whose glyph
/// atlas allocations are larger) can exhaust a driver's resource limits and
/// fail with a spurious `wgpu error: Out of Memory` that has nothing to do
/// with the scene being rendered. Serialize GPU device creation/use across
/// tests in this binary to avoid that; it's the tests fighting each other
/// for GPU resources, not a real per-scene resource problem.
static GPU_TEST_LOCK: Mutex<()> = Mutex::new(());

async fn create_device() -> (wgpu::Device, wgpu::Queue) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions::default())
        .await
        .expect("no adapter available for headless rendering");
    adapter.request_device(&wgpu::DeviceDescriptor::default()).await.expect("device request failed")
}

fn render_gpu(width: u32, height: u32, draw: impl FnOnce(&mut GpuPainter)) -> Vec<u8> {
    let _guard = GPU_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (device, queue) = pollster::block_on(create_device());
    let mut renderer = vello::Renderer::new(
        &device,
        vello::RendererOptions {
            use_cpu: false,
            antialiasing_support: vello::AaSupport::area_only(),
            num_init_threads: std::num::NonZeroUsize::new(1),
            pipeline_cache: None,
        },
    )
    .expect("renderer init");

    let mut scene = vello::Scene::new();
    let mut text = nowui_text::TextContext::new();
    let mut font_cache = GpuFontCache::new();
    {
        let mut painter = GpuPainter::new(&mut scene, &mut text, &mut font_cache);
        draw(&mut painter);
    }

    let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("offscreen"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());

    renderer
        .render_to_texture(
            &device,
            &queue,
            &scene,
            &view,
            &vello::RenderParams {
                base_color: vello::peniko::Color::from_rgba8(WHITE.r, WHITE.g, WHITE.b, WHITE.a),
                width,
                height,
                antialiasing_method: vello::AaConfig::Area,
            },
        )
        .expect("render_to_texture");

    let padded_byte_width = (width * 4).next_multiple_of(256);
    let buffer_size = padded_byte_width as u64 * height as u64;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: buffer_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("copy") });
    encoder.copy_texture_to_buffer(
        target.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded_byte_width), rows_per_image: None },
        },
        size,
    );
    queue.submit([encoder.finish()]);

    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None }).expect("device poll");
    rx.recv().expect("map_async channel closed").expect("buffer map failed");

    let data = slice.get_mapped_range();
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height {
        let start = (row * padded_byte_width) as usize;
        out.extend_from_slice(&data[start..start + (width * 4) as usize]);
    }
    out
}

fn render_cpu(width: u32, height: u32, draw: impl FnOnce(&mut nowui_render::SkiaPainter)) -> Vec<u8> {
    let mut pixmap = tiny_skia::Pixmap::new(width, height).expect("pixmap alloc");
    pixmap.fill(tiny_skia::Color::from_rgba8(WHITE.r, WHITE.g, WHITE.b, WHITE.a));
    let mut text = nowui_render::TextContext::new();
    {
        let mut painter = nowui_render::SkiaPainter::new(&mut pixmap, &mut text);
        draw(&mut painter);
    }
    pixmap.data().to_vec()
}

fn pixel_at(rgba: &[u8], width: u32, x: u32, y: u32) -> (u8, u8, u8) {
    let idx = ((y * width + x) * 4) as usize;
    (rgba[idx], rgba[idx + 1], rgba[idx + 2])
}

#[track_caller]
fn assert_close(a: (u8, u8, u8), b: (u8, u8, u8), tol: i32, msg: &str) {
    let d = |x: u8, y: u8| (x as i32 - y as i32).abs();
    assert!(
        d(a.0, b.0) <= tol && d(a.1, b.1) <= tol && d(a.2, b.2) <= tol,
        "{msg}: gpu={a:?} vs cpu={b:?} (tolerance {tol})"
    );
}

const TOL: i32 = 24;
const W: u32 = 100;
const H: u32 = 100;

#[test]
fn fills_a_solid_rect_matching_the_cpu_reference() {
    let draw = |p: &mut dyn Painter| p.fill_rect(Rect::new(20.0, 20.0, 40.0, 40.0), RED, Edges::default());
    let gpu = render_gpu(W, H, |p| draw(p));
    let cpu = render_cpu(W, H, |p| draw(p));

    assert_close(pixel_at(&gpu, W, 40, 40), pixel_at(&cpu, W, 40, 40), TOL, "inside the filled rect");
    assert_close(pixel_at(&gpu, W, 5, 5), pixel_at(&cpu, W, 5, 5), TOL, "outside the filled rect");
    assert_close(pixel_at(&gpu, W, 40, 40), (RED.r, RED.g, RED.b), TOL, "gpu fill matches the intended color");
}

#[test]
fn strokes_a_rect_leaving_the_interior_untouched() {
    let draw = |p: &mut dyn Painter| p.stroke_rect(Rect::new(20.0, 20.0, 40.0, 40.0), BLUE, 6.0, Edges::default());
    let gpu = render_gpu(W, H, |p| draw(p));
    let cpu = render_cpu(W, H, |p| draw(p));

    // Well inside the stroked border, on all sides.
    assert_close(pixel_at(&gpu, W, 40, 21), pixel_at(&cpu, W, 40, 21), TOL, "top edge of the stroke");
    // Center of the rect (interior) — untouched, stays background on both.
    assert_close(pixel_at(&gpu, W, 40, 40), pixel_at(&cpu, W, 40, 40), TOL, "interior (unstroked)");
    assert_close(pixel_at(&gpu, W, 40, 40), (WHITE.r, WHITE.g, WHITE.b), TOL, "interior stays background");
}

#[test]
fn clips_content_to_the_intersection_of_nested_clips() {
    let draw = |p: &mut dyn Painter| {
        p.push_clip(Rect::new(10.0, 10.0, 50.0, 50.0));
        p.push_clip(Rect::new(30.0, 30.0, 50.0, 50.0)); // intersection: (30,30)..(60,60)
        p.fill_rect(Rect::new(0.0, 0.0, 100.0, 100.0), RED, Edges::default());
        p.pop_clip();
        p.pop_clip();
    };
    let gpu = render_gpu(W, H, |p| draw(p));
    let cpu = render_cpu(W, H, |p| draw(p));

    assert_close(pixel_at(&gpu, W, 45, 45), pixel_at(&cpu, W, 45, 45), TOL, "inside the intersected clip");
    assert_close(pixel_at(&gpu, W, 45, 45), (RED.r, RED.g, RED.b), TOL, "gpu clip lets fill through inside bounds");
    assert_close(pixel_at(&gpu, W, 15, 15), pixel_at(&cpu, W, 15, 15), TOL, "inside the outer clip only, outside the inner one");
    assert_close(pixel_at(&gpu, W, 15, 15), (WHITE.r, WHITE.g, WHITE.b), TOL, "gpu clip excludes this point");
    assert_close(pixel_at(&gpu, W, 90, 90), pixel_at(&cpu, W, 90, 90), TOL, "outside both clips");
}

#[test]
fn rotated_fill_matches_the_cpu_reference() {
    let draw = |p: &mut dyn Painter| {
        p.push_transform(
            nowui_core::Transform2D { rotate_deg: 45.0, ..Default::default() },
            nowui_core::Point::new(50.0, 50.0),
        );
        p.fill_rect(Rect::new(30.0, 30.0, 40.0, 40.0), RED, Edges::default());
        p.pop_transform();
    };
    let gpu = render_gpu(W, H, |p| draw(p));
    let cpu = render_cpu(W, H, |p| draw(p));

    // Center of rotation stays inside the rotated rect regardless of angle.
    assert_close(pixel_at(&gpu, W, 50, 50), pixel_at(&cpu, W, 50, 50), TOL, "center of a 45deg-rotated fill");
    assert_close(pixel_at(&gpu, W, 50, 50), (RED.r, RED.g, RED.b), TOL, "gpu rotated fill matches the intended color");
    // A corner that a 45deg rotation should have swung clear of the rect.
    assert_close(pixel_at(&gpu, W, 5, 5), pixel_at(&cpu, W, 5, 5), TOL, "far corner, outside the rotated rect");
}

#[test]
fn nested_opacity_composes_multiplicatively_matching_the_cpu_reference() {
    let draw = |p: &mut dyn Painter| {
        p.push_opacity(0.5);
        p.push_opacity(0.5); // cumulative 0.25
        p.fill_rect(Rect::new(20.0, 20.0, 40.0, 40.0), RED, Edges::default());
        p.pop_opacity();
        p.pop_opacity();
    };
    let gpu = render_gpu(W, H, |p| draw(p));
    let cpu = render_cpu(W, H, |p| draw(p));

    assert_close(pixel_at(&gpu, W, 40, 40), pixel_at(&cpu, W, 40, 40), TOL, "25% opacity fill over white background");
    // A quarter-opacity red over white should read closer to white than to
    // full red — a coarse sanity check independent of the CPU reference.
    let (r, g, b) = pixel_at(&gpu, W, 40, 40);
    assert!(r > 180 && g > 150 && b > 150, "expected a light, mostly-background tint, got ({r}, {g}, {b})");
}

#[test]
fn draws_text_matching_the_cpu_reference_in_size_and_position() {
    let style = nowui_core::TextStyle {
        color: Color { r: 0, g: 0, b: 0, a: 255 },
        size: 24.0,
        align: nowui_core::TextAlign::Left,
        weight: 400,
        letter_spacing: 0.0,
    };
    let draw = |p: &mut dyn Painter| p.draw_text("W", Rect::new(10.0, 10.0, 80.0, 40.0), &style);
    let gpu = render_gpu(W, H, |p| draw(p));
    let cpu = render_cpu(W, H, |p| draw(p));

    // A capital "W" at (10,10) with a 24px box should darken pixels somewhere
    // in its glyph box on both backends (exact glyph shape/AA will differ,
    // so this only checks "something dark got drawn roughly here", not a
    // pixel-perfect glyph match).
    let darkened = |rgba: &[u8]| {
        (10..40).flat_map(|y| (10..80).map(move |x| (x, y))).any(|(x, y)| {
            let (r, g, b) = pixel_at(rgba, W, x, y);
            r < 200 && g < 200 && b < 200
        })
    };
    assert!(darkened(&gpu), "gpu backend drew nothing dark in the glyph's bounding box");
    assert!(darkened(&cpu), "cpu backend drew nothing dark in the glyph's bounding box (sanity check on the test itself)");
}

#[test]
fn rotated_text_visibly_rotates_on_gpu_unlike_cpu() {
    // Documented, intentional fidelity difference (see nowui-render-gpu's
    // module doc): SkiaPainter blits glyphs as pixels, never affected by the
    // active transform; GpuPainter draws them as real transformable
    // primitives via `vello::Scene::draw_glyphs`, so they *do* rotate.
    let style = nowui_core::TextStyle {
        color: Color { r: 0, g: 0, b: 0, a: 255 },
        size: 40.0,
        align: nowui_core::TextAlign::Left,
        weight: 400,
        letter_spacing: 0.0,
    };
    let draw = |p: &mut dyn Painter| {
        p.push_transform(
            nowui_core::Transform2D { rotate_deg: 90.0, ..Default::default() },
            nowui_core::Point::new(50.0, 50.0),
        );
        p.draw_text("I", Rect::new(45.0, 5.0, 10.0, 60.0), &style);
        p.pop_transform();
    };
    let gpu_a = render_gpu(W, H, |p| draw(p));

    let draw_unrotated = |p: &mut dyn Painter| p.draw_text("I", Rect::new(45.0, 5.0, 10.0, 60.0), &style);
    let gpu_b = render_gpu(W, H, |p| draw_unrotated(p));

    assert_ne!(gpu_a, gpu_b, "a 90deg rotation should visibly move the glyph on the gpu backend");
}
