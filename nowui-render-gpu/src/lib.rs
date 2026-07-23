//! `vello`/`wgpu` implementation of `nowui_core::Painter` — the GPU sibling
//! of `nowui-render`'s CPU `SkiaPainter`. See `nowui-core/src/painter.rs` for
//! the render-boundary contract both must satisfy, and README.md's
//! architecture section for how a `Painter` backend is selected at runtime.
//!
//! Text shaping is shared with `SkiaPainter` via `nowui-text` (pure
//! cosmic-text, no rasterization) — only *what happens to shaped glyphs*
//! differs: `SkiaPainter` rasterizes them itself and blits pixels; this
//! backend hands cosmic-text's already-shaped glyph IDs/positions straight to
//! `vello::Scene::draw_glyphs`, which rasterizes them on the GPU. One
//! consequence: unlike `SkiaPainter` (where text is a pixel blit, never
//! affected by the active transform — see its own doc comment), text here
//! **is** drawn as a real transformable primitive, so it correctly
//! rotates/scales/skews with its node's transform. This is an intentional,
//! backend-specific fidelity difference, not a bug.
//!
//! Clip/transform/opacity stacks mirror `SkiaPainter`'s design: `transforms`/
//! `opacities` are plain cumulative data, applied fresh as parameters to each
//! individual draw call (not vello layers — pushing a real layer per node for
//! opacity would force costly off-screen compositing on every opacity-*
//! node, every frame). Only `clips` map onto real `vello::Scene::push_layer`/
//! `pop_layer` calls, because `push_clip`/`pop_clip` are the one stack that's
//! always properly nested in the paint walk (see `nowui-core/src/paint.rs`).
//! Clip rects are (like `SkiaPainter`'s) in the same untransformed coordinate
//! space `fill_rect`/`stroke_rect`/`draw_text`'s `rect`/`bounds` arguments
//! are — the active transform is *not* applied to the clip shape itself,
//! matching the CPU backend's existing behavior exactly.

use std::collections::HashMap;
use std::sync::Arc;

use cosmic_text::fontdb;
use nowui_core::{Color, Edges, Painter, Point, Rect, TextStyle, Transform2D};
use nowui_text::TextContext;
use vello::kurbo::{Affine, Rect as KurboRect, RoundedRect, RoundedRectRadii, Stroke as KurboStroke};
use vello::peniko::{Blob, BlendMode, Color as PenikoColor, Compose, Fill, FontData, Mix};
use vello::{Glyph, Scene};

/// Caches resolved `FontData` (font-file bytes + face index) per distinct
/// `fontdb::ID` — a one-time cost per unique font file, not per glyph/frame.
/// Persists across frames like `nowui_text::TextContext`'s `font_system`;
/// own it alongside that (see `GpuPainter::new`).
#[derive(Default)]
pub struct GpuFontCache {
    fonts: HashMap<fontdb::ID, FontData>,
}

impl GpuFontCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn get_or_insert(&mut self, id: fontdb::ID, font_system: &mut cosmic_text::FontSystem) -> Option<FontData> {
        if let Some(font) = self.fonts.get(&id) {
            return Some(font.clone());
        }
        let font = font_system
            .db()
            .with_face_data(id, |data, index| FontData::new(Blob::new(Arc::new(data.to_vec())), index))?;
        self.fonts.insert(id, font.clone());
        Some(font)
    }
}

pub struct GpuPainter<'a> {
    scene: &'a mut Scene,
    font_system: &'a mut cosmic_text::FontSystem,
    font_cache: &'a mut GpuFontCache,
    /// Stack of composed 2D transforms; the top is the active one.
    transforms: Vec<Affine>,
    /// Stack of multiplicative opacities; the top is the active (cumulative) one.
    opacities: Vec<f32>,
    /// Stack of cumulative-intersected clip rects — also mirrors the number
    /// of currently-open `vello` clip layers (one push/pop per entry), so
    /// `pop_clip` popping this stack and popping a vello layer always stay
    /// in lockstep.
    clips: Vec<KurboRect>,
}

impl<'a> GpuPainter<'a> {
    pub fn new(scene: &'a mut Scene, text: &'a mut TextContext, font_cache: &'a mut GpuFontCache) -> Self {
        GpuPainter {
            scene,
            font_system: &mut text.font_system,
            font_cache,
            transforms: Vec::new(),
            opacities: Vec::new(),
            clips: Vec::new(),
        }
    }

    fn active_transform(&self) -> Affine {
        self.transforms.last().copied().unwrap_or(Affine::IDENTITY)
    }

    fn active_opacity(&self) -> f32 {
        self.opacities.last().copied().unwrap_or(1.0)
    }

    /// Apply the active opacity to a straight-alpha color, then convert to
    /// `peniko`'s `Color` (an `AlphaColor<Srgb>`).
    fn with_opacity(&self, mut color: Color) -> PenikoColor {
        color.a = (color.a as f32 * self.active_opacity()).round().clamp(0.0, 255.0) as u8;
        PenikoColor::from_rgba8(color.r, color.g, color.b, color.a)
    }

    /// Compose a 2D affine transform around `origin`, in the same order
    /// `SkiaPainter::compose_transform` uses (translate is an absolute pixel
    /// shift, applied outside the origin-relative rotate/skew/scale).
    fn compose_transform(t: Transform2D, origin: Point) -> Affine {
        if t.is_identity() {
            return Affine::IDENTITY;
        }
        let kx = (t.skew_x_deg as f64).to_radians().tan();
        let ky = (t.skew_y_deg as f64).to_radians().tan();
        let skew = Affine::new([1.0, ky, kx, 1.0, 0.0, 0.0]);
        Affine::translate((origin.x as f64 + t.translate_x as f64, origin.y as f64 + t.translate_y as f64))
            * Affine::rotate((t.rotate_deg as f64).to_radians())
            * skew
            * Affine::scale_non_uniform(t.scale_x as f64, t.scale_y as f64)
            * Affine::translate((-origin.x as f64, -origin.y as f64))
    }

    /// `radius`: per-corner (`top`=top-left, `right`=top-right, `bottom`=
    /// bottom-right, `left`=bottom-left), matching `Style::radius`'s reuse of
    /// `Edges`'s CSS-shorthand ordering — the same ordering `kurbo`'s own
    /// `(f64, f64, f64, f64) -> RoundedRectRadii` tuple conversion documents
    /// ("clockwise from the top-left"), so this is a direct field mapping.
    fn rounded_rect(r: Rect, radius: Edges) -> Option<RoundedRect> {
        if r.w <= 0.0 || r.h <= 0.0 {
            return None;
        }
        let max_r = ((r.w / 2.0).min(r.h / 2.0)) as f64;
        let clamp = |v: f32| (v as f64).max(0.0).min(max_r);
        let radii: RoundedRectRadii = (clamp(radius.top), clamp(radius.right), clamp(radius.bottom), clamp(radius.left)).into();
        Some(RoundedRect::new(r.x as f64, r.y as f64, (r.x + r.w) as f64, (r.y + r.h) as f64, radii))
    }

    fn kurbo_rect(r: Rect) -> KurboRect {
        KurboRect::new(r.x as f64, r.y as f64, (r.x + r.w) as f64, (r.y + r.h) as f64)
    }

    fn clip_blend_mode() -> BlendMode {
        BlendMode::new(Mix::Normal, Compose::SrcOver)
    }
}

impl<'a> Painter for GpuPainter<'a> {
    fn fill_rect(&mut self, rect: Rect, color: Color, radius: Edges) {
        let Some(shape) = Self::rounded_rect(rect, radius) else { return };
        let brush = self.with_opacity(color);
        self.scene.fill(Fill::NonZero, self.active_transform(), brush, None, &shape);
    }

    fn stroke_rect(&mut self, rect: Rect, color: Color, width: f32, radius: Edges) {
        let Some(shape) = Self::rounded_rect(rect, radius) else { return };
        let brush = self.with_opacity(color);
        let stroke = KurboStroke::new(width as f64);
        self.scene.stroke(&stroke, self.active_transform(), brush, None, &shape);
    }

    fn draw_text(&mut self, text: &str, bounds: Rect, style: &TextStyle) {
        if text.is_empty() || bounds.w <= 0.0 || bounds.h <= 0.0 {
            return;
        }
        let buffer = nowui_text::shape_text(self.font_system, text, style.size, Some(bounds.w), style.align);
        let brush = self.with_opacity(style.color);
        let transform = self.active_transform();

        for run in buffer.layout_runs() {
            // Group by font — `draw_glyphs` takes one font per call — since a
            // single run can mix fonts (fallback for glyphs the primary font
            // doesn't cover).
            let mut by_font: HashMap<fontdb::ID, Vec<Glyph>> = HashMap::new();
            for g in run.glyphs {
                let x_offset = g.font_size * g.x_offset;
                let y_offset = g.font_size * g.y_offset;
                // Same formula `LayoutGlyph::physical` uses (offset =
                // `(0.0, run.line_y)`, per cosmic-text's own `Buffer::draw`),
                // minus its integer-pixel truncation — vello draws at
                // sub-pixel precision natively, so snapping to a pixel grid
                // here would only throw away accuracy the GPU path doesn't
                // need. An intentional, documented fidelity improvement over
                // `SkiaPainter`'s pixel-grid-snapped blit.
                let x = bounds.x + g.x + x_offset;
                let y = bounds.y + run.line_y + g.y - y_offset;
                by_font.entry(g.font_id).or_default().push(Glyph { id: g.glyph_id as u32, x, y });
            }
            for (font_id, glyphs) in by_font {
                let Some(font) = self.font_cache.get_or_insert(font_id, self.font_system) else { continue };
                self.scene
                    .draw_glyphs(&font)
                    .transform(transform)
                    .font_size(style.size)
                    .brush(brush)
                    .hint(true)
                    .draw(Fill::NonZero, glyphs.into_iter());
            }
        }
    }

    fn push_clip(&mut self, rect: Rect) {
        let new_rect = Self::kurbo_rect(rect);
        let intersected = match self.clips.last() {
            Some(prev) => prev.intersect(new_rect),
            None => new_rect,
        };
        self.scene.push_layer(Fill::NonZero, Self::clip_blend_mode(), 1.0, Affine::IDENTITY, &intersected);
        self.clips.push(intersected);
    }

    fn pop_clip(&mut self) {
        self.clips.pop();
        self.scene.pop_layer();
    }

    fn measure_text(&mut self, text: &str, size: f32) -> Point {
        nowui_text::measure(self.font_system, text, size)
    }

    fn push_transform(&mut self, transform: Transform2D, origin: Point) {
        let local = Self::compose_transform(transform, origin);
        self.transforms.push(self.active_transform() * local);
    }

    fn pop_transform(&mut self) {
        self.transforms.pop();
    }

    fn push_opacity(&mut self, opacity: f32) {
        self.opacities.push(self.active_opacity() * opacity);
    }

    fn pop_opacity(&mut self) {
        self.opacities.pop();
    }
}

/// Owns everything tied to an actual on-screen window: the `vello::util`
/// helper types that in turn own the `wgpu::Surface`/`Device`/`Queue` and an
/// intermediate storage-capable texture (`RenderSurface::target_view`) that
/// gets blitted onto the real swapchain image every frame — not every
/// surface format supports the `STORAGE_BINDING` usage `vello::Renderer`
/// needs to render into directly, so `vello::util::RenderContext` handles
/// that indirection for us rather than this crate reimplementing it.
/// Constructed once (`nowui-runtime`'s `App::resumed`, GPU backend only),
/// resized on `WindowEvent::Resized`, used every redraw via
/// `render_and_present`.
pub struct GpuSurfaceState {
    context: vello::util::RenderContext,
    surface: vello::util::RenderSurface<'static>,
    renderer: vello::Renderer,
}

impl GpuSurfaceState {
    /// `window` must resolve to a `Send + Sync` window handle (wgpu's
    /// `Surface` requirement) — a winit `Arc<Window>` works; a `Rc<Window>`
    /// does not.
    pub fn new<W>(window: W, width: u32, height: u32) -> Self
    where
        W: Into<vello::wgpu::SurfaceTarget<'static>>,
    {
        pollster::block_on(async {
            let mut context = vello::util::RenderContext::new();
            let surface = context
                .create_surface(window, width.max(1), height.max(1), vello::wgpu::PresentMode::AutoVsync)
                .await
                .expect("failed to create a GPU surface for the window");
            let device_handle = &context.devices[surface.dev_id];
            let renderer = vello::Renderer::new(
                &device_handle.device,
                vello::RendererOptions {
                    use_cpu: false,
                    antialiasing_support: vello::AaSupport::area_only(),
                    num_init_threads: std::num::NonZeroUsize::new(1),
                    pipeline_cache: None,
                },
            )
            .expect("vello renderer init failed");
            GpuSurfaceState { context, surface, renderer }
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.context.resize_surface(&mut self.surface, width.max(1), height.max(1));
    }

    /// Render `scene` and present it to the window. `base_color` is the
    /// frame's background, painted before anything in `scene` (matches
    /// `nowui-runtime`'s `CLEAR` constant for the CPU backend). A no-op for
    /// whatever frame a transient condition (window occluded/minimized, a
    /// resize race, a timeout) prevents from acquiring a surface texture —
    /// skip it and let the next fixed-rate frame try again, rather than
    /// treating a routine, recoverable condition as a hard error.
    pub fn render_and_present(&mut self, scene: &vello::Scene, base_color: nowui_core::Color) {
        use vello::wgpu::CurrentSurfaceTexture;

        let surface_texture = match self.surface.surface.get_current_texture() {
            CurrentSurfaceTexture::Success(t) | CurrentSurfaceTexture::Suboptimal(t) => t,
            _ => return,
        };

        let device_handle = &self.context.devices[self.surface.dev_id];
        let params = vello::RenderParams {
            base_color: vello::peniko::Color::from_rgba8(base_color.r, base_color.g, base_color.b, base_color.a),
            width: self.surface.config.width,
            height: self.surface.config.height,
            antialiasing_method: vello::AaConfig::Area,
        };
        self.renderer
            .render_to_texture(&device_handle.device, &device_handle.queue, scene, &self.surface.target_view, &params)
            .expect("render_to_texture failed");

        let mut encoder =
            device_handle.device.create_command_encoder(&vello::wgpu::CommandEncoderDescriptor { label: Some("surface blit") });
        self.surface.blitter.copy(
            &device_handle.device,
            &mut encoder,
            &self.surface.target_view,
            &surface_texture.texture.create_view(&vello::wgpu::TextureViewDescriptor::default()),
        );
        device_handle.queue.submit([encoder.finish()]);
        surface_texture.present();
    }
}
