//! tiny-skia implementation of `nowui_core::Painter`, plus the bridge that
//! packs a rasterized `Pixmap` into a softbuffer `0RGB` u32 buffer.
//!
//! Text: tiny-skia has NO font support, so glyphs are shaped (via
//! `nowui-text`, shared with any other `Painter` backend) and rasterized by
//! cosmic-text/swash, then blitted onto the pixmap pixel-by-pixel here.

use cosmic_text::Buffer;
use nowui_core::{Color, Edges, Painter, Point, Rect, TextAlign, TextStyle, Transform2D};
pub use nowui_text::TextContext;
use tiny_skia::{
    FillRule, Mask, Paint, PathBuilder, Pixmap, Rect as SkRect, Stroke, Transform,
};

pub struct SkiaPainter<'a> {
    pixmap: &'a mut Pixmap,
    font_system: &'a mut cosmic_text::FontSystem,
    swash_cache: &'a mut cosmic_text::SwashCache,
    /// Stack of clip masks; the top is the active clip (intersection of all).
    clips: Vec<Mask>,
    /// Stack of composed 2D transforms; the top is the active one. Applies to
    /// `fill_rect`/`stroke_rect` (backgrounds, borders, shapes) via tiny-skia's
    /// native per-draw `Transform`. Text is NOT transformed: glyphs are
    /// blitted pixel-by-pixel (see `draw_text`) rather than filled as
    /// tiny-skia paths, so rotate/skew/scale on a text node's own transform
    /// won't visibly rotate/skew/scale its glyphs — a known limitation, see
    /// CLAUDE.md.
    transforms: Vec<Transform>,
    /// Stack of multiplicative opacities; the top is the active (cumulative) one.
    opacities: Vec<f32>,
}

impl<'a> SkiaPainter<'a> {
    pub fn new(pixmap: &'a mut Pixmap, text: &'a mut TextContext) -> Self {
        SkiaPainter {
            pixmap,
            font_system: &mut text.font_system,
            swash_cache: &mut text.swash_cache,
            clips: Vec::new(),
            transforms: Vec::new(),
            opacities: Vec::new(),
        }
    }

    fn active_transform(&self) -> Transform {
        self.transforms.last().copied().unwrap_or(Transform::identity())
    }

    fn active_opacity(&self) -> f32 {
        self.opacities.last().copied().unwrap_or(1.0)
    }

    /// Apply the active opacity to a straight-alpha color.
    fn with_opacity(&self, mut color: Color) -> Color {
        color.a = (color.a as f32 * self.active_opacity()).round().clamp(0.0, 255.0) as u8;
        color
    }

    /// Compose a 2D affine transform around `origin`, in the order Tailwind's
    /// utilities apply (translate is an absolute pixel shift, applied outside
    /// the origin-relative rotate/skew/scale).
    fn compose_transform(t: Transform2D, origin: Point) -> Transform {
        if t.is_identity() {
            return Transform::identity();
        }
        let kx = t.skew_x_deg.to_radians().tan();
        let ky = t.skew_y_deg.to_radians().tan();
        Transform::identity()
            .pre_translate(t.translate_x, t.translate_y)
            .pre_translate(origin.x, origin.y)
            .pre_concat(Transform::from_rotate(t.rotate_deg))
            .pre_concat(Transform::from_skew(kx, ky))
            .pre_concat(Transform::from_scale(t.scale_x, t.scale_y))
            .pre_translate(-origin.x, -origin.y)
    }

    /// Shape `text` at `size`, wrapping to `width` if given (`None` measures
    /// the text's natural, unwrapped extent), aligned per `align`. Delegates
    /// to `nowui-text` — the shared shaping logic every `Painter` backend uses.
    fn shape(&mut self, text: &str, size: f32, width: Option<f32>, align: TextAlign) -> Buffer {
        nowui_text::shape_text(self.font_system, text, size, width, align)
    }

    fn active_clip(&self) -> Option<&Mask> {
        self.clips.last()
    }

    fn skia_color(c: Color) -> tiny_skia::Color {
        tiny_skia::Color::from_rgba8(c.r, c.g, c.b, c.a)
    }

    /// `radius`: per-corner (`top`=top-left, `right`=top-right, `bottom`=
    /// bottom-right, `left`=bottom-left), matching `Style::radius`'s reuse of
    /// `Edges`'s CSS-shorthand ordering.
    fn rounded_path(r: Rect, radius: Edges) -> Option<tiny_skia::Path> {
        if r.w <= 0.0 || r.h <= 0.0 {
            return None;
        }
        let max_r = (r.w / 2.0).min(r.h / 2.0);
        let tl = radius.top.max(0.0).min(max_r);
        let tr = radius.right.max(0.0).min(max_r);
        let br = radius.bottom.max(0.0).min(max_r);
        let bl = radius.left.max(0.0).min(max_r);
        if tl <= 0.5 && tr <= 0.5 && br <= 0.5 && bl <= 0.5 {
            let rect = SkRect::from_xywh(r.x, r.y, r.w, r.h)?;
            return PathBuilder::from_rect(rect).into();
        }
        // Manual rounded rect (tiny-skia has no direct round-rect builder),
        // one quadratic per corner so each can have its own radius.
        let mut pb = PathBuilder::new();
        let (l, t, rt, b) = (r.x, r.y, r.x + r.w, r.y + r.h);
        pb.move_to(l + tl, t);
        pb.line_to(rt - tr, t);
        if tr > 0.0 {
            pb.quad_to(rt, t, rt, t + tr);
        }
        pb.line_to(rt, b - br);
        if br > 0.0 {
            pb.quad_to(rt, b, rt - br, b);
        }
        pb.line_to(l + bl, b);
        if bl > 0.0 {
            pb.quad_to(l, b, l, b - bl);
        }
        pb.line_to(l, t + tl);
        if tl > 0.0 {
            pb.quad_to(l, t, l + tl, t);
        }
        pb.close();
        pb.finish()
    }
}

impl<'a> Painter for SkiaPainter<'a> {
    fn fill_rect(&mut self, rect: Rect, color: Color, radius: Edges) {
        let Some(path) = Self::rounded_path(rect, radius) else { return };
        let mut paint = Paint::default();
        paint.set_color(Self::skia_color(self.with_opacity(color)));
        paint.anti_alias = true;
        self.pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            self.active_transform(),
            self.active_clip().cloned().as_ref(),
        );
    }

    fn stroke_rect(&mut self, rect: Rect, color: Color, width: f32, radius: Edges) {
        let Some(path) = Self::rounded_path(rect, radius) else { return };
        let mut paint = Paint::default();
        paint.set_color(Self::skia_color(self.with_opacity(color)));
        paint.anti_alias = true;
        let stroke = Stroke { width, ..Default::default() };
        self.pixmap.stroke_path(
            &path,
            &paint,
            &stroke,
            self.active_transform(),
            self.active_clip().cloned().as_ref(),
        );
    }

    fn draw_text(&mut self, text: &str, bounds: Rect, style: &TextStyle) {
        if text.is_empty() || bounds.w <= 0.0 || bounds.h <= 0.0 {
            return;
        }
        let buffer = self.shape(text, style.size, Some(bounds.w), style.align);
        let text_color = self.with_opacity(style.color);
        let color = cosmic_text::Color::rgba(text_color.r, text_color.g, text_color.b, text_color.a);

        // Disjoint reborrows of separate fields so the closure below can hold
        // `pixmap` and `clip` mutably/by-value while `font_system`/`swash_cache`
        // are borrowed separately for `buffer.draw`.
        let font_system = &mut *self.font_system;
        let swash_cache = &mut *self.swash_cache;
        let pixmap = &mut *self.pixmap;
        let clip = self.clips.last().cloned();
        let pw = pixmap.width() as i32;
        let ph = pixmap.height() as i32;

        buffer.draw(font_system, swash_cache, color, |x, y, _w, _h, color| {
            let px = bounds.x as i32 + x;
            let py = bounds.y as i32 + y;
            if px < 0 || py < 0 || px >= pw || py >= ph {
                return;
            }
            let mut alpha = color.a();
            if alpha == 0 {
                return;
            }
            if let Some(mask) = &clip {
                let coverage = mask.data()[(py as u32 * mask.width() + px as u32) as usize];
                alpha = ((alpha as u16 * coverage as u16) / 255) as u8;
                if alpha == 0 {
                    return;
                }
            }
            blend_pixel(pixmap, px as u32, py as u32, color.r(), color.g(), color.b(), alpha);
        });
    }

    fn draw_image(&mut self, frame: &nowui_image::Frame, bounds: Rect) {
        if frame.width == 0 || frame.height == 0 || bounds.w <= 0.0 || bounds.h <= 0.0 {
            return;
        }
        let opacity = self.active_opacity();
        let clip = self.clips.last().cloned();
        let pixmap = &mut *self.pixmap;
        let (pw, ph) = (pixmap.width() as i32, pixmap.height() as i32);

        // Nearest-neighbor stretch to `bounds` — the solver has already
        // done any aspect-ratio-preserving scaling (`w-[auto]`/`h-[auto]`),
        // so this only ever stretches uniformly, never distorts. Same
        // per-pixel blend path `draw_text` already uses, so clipping/
        // opacity/blending behave identically for text and images.
        let x0 = bounds.x.max(0.0) as i32;
        let y0 = bounds.y.max(0.0) as i32;
        let x1 = (bounds.x + bounds.w).min(pw as f32) as i32;
        let y1 = (bounds.y + bounds.h).min(ph as f32) as i32;
        for py in y0..y1 {
            let v = ((py as f32 - bounds.y) / bounds.h * frame.height as f32) as u32;
            let v = v.min(frame.height - 1);
            for px in x0..x1 {
                let u = ((px as f32 - bounds.x) / bounds.w * frame.width as f32) as u32;
                let u = u.min(frame.width - 1);
                let idx = ((v * frame.width + u) * 4) as usize;
                let [r, g, b, a] = [frame.rgba[idx], frame.rgba[idx + 1], frame.rgba[idx + 2], frame.rgba[idx + 3]];
                let mut alpha = (a as f32 * opacity).round().clamp(0.0, 255.0) as u8;
                if alpha == 0 {
                    continue;
                }
                if let Some(mask) = &clip {
                    let coverage = mask.data()[(py as u32 * mask.width() + px as u32) as usize];
                    alpha = ((alpha as u16 * coverage as u16) / 255) as u8;
                    if alpha == 0 {
                        continue;
                    }
                }
                blend_pixel(pixmap, px as u32, py as u32, r, g, b, alpha);
            }
        }
    }

    fn push_clip(&mut self, rect: Rect) {
        let w = self.pixmap.width();
        let h = self.pixmap.height();
        let mut mask = match self.active_clip() {
            Some(existing) => existing.clone(),
            None => {
                let mut m = Mask::new(w, h).expect("mask alloc");
                m.fill_path(
                    &PathBuilder::from_rect(SkRect::from_xywh(0.0, 0.0, w as f32, h as f32).unwrap()),
                    FillRule::Winding,
                    true,
                    Transform::identity(),
                );
                m
            }
        };
        // Intersect by re-filling the mask with the clip rect. (tiny-skia masks
        // are coverage buffers; intersecting = filling a fresh mask and AND-ing.
        // For rectangular clips a fresh mask of the rect is sufficient here.)
        if let Some(r) = SkRect::from_xywh(rect.x, rect.y, rect.w, rect.h) {
            let mut fresh = Mask::new(w, h).expect("mask alloc");
            fresh.fill_path(
                &PathBuilder::from_rect(r),
                FillRule::Winding,
                true,
                Transform::identity(),
            );
            mask = fresh;
        }
        self.clips.push(mask);
    }

    fn pop_clip(&mut self) {
        self.clips.pop();
    }

    fn measure_text(&mut self, text: &str, size: f32) -> Point {
        nowui_text::measure(self.font_system, text, size)
    }

    fn push_transform(&mut self, transform: Transform2D, origin: Point) {
        let local = Self::compose_transform(transform, origin);
        self.transforms.push(self.active_transform().pre_concat(local));
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

/// Source-over blend of a straight-alpha `(r, g, b, a)` glyph sample onto a
/// premultiplied pixmap pixel.
fn blend_pixel(pixmap: &mut Pixmap, x: u32, y: u32, r: u8, g: u8, b: u8, a: u8) {
    let idx = (y * pixmap.width() + x) as usize;
    let dst = pixmap.pixels()[idx];
    let sa = a as f32 / 255.0;
    let inv = 1.0 - sa;
    let out_a = (a as f32 + dst.alpha() as f32 * inv).round().clamp(0.0, 255.0) as u8;
    let out_r = ((r as f32 * sa + dst.red() as f32 * inv).round().clamp(0.0, 255.0) as u8).min(out_a);
    let out_g = ((g as f32 * sa + dst.green() as f32 * inv).round().clamp(0.0, 255.0) as u8).min(out_a);
    let out_b = ((b as f32 * sa + dst.blue() as f32 * inv).round().clamp(0.0, 255.0) as u8).min(out_a);
    if let Some(color) = tiny_skia::PremultipliedColorU8::from_rgba(out_r, out_g, out_b, out_a) {
        pixmap.pixels_mut()[idx] = color;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nowui_image::Frame;

    #[test]
    fn draw_image_stretches_a_solid_frame_onto_the_pixmap() {
        let mut pixmap = Pixmap::new(20, 20).expect("pixmap alloc");
        pixmap.fill(tiny_skia::Color::from_rgba8(0, 0, 0, 255));
        let mut text = TextContext::new();

        // A 2x2 solid-red source frame, stretched to a 10x10 destination rect.
        let frame = Frame { width: 2, height: 2, rgba: vec![255, 0, 0, 255].repeat(4), delay_ms: 0 };

        {
            let mut painter = SkiaPainter::new(&mut pixmap, &mut text);
            painter.draw_image(&frame, Rect::new(5.0, 5.0, 10.0, 10.0));
        }

        // Center of the stretched rect should be solid red.
        let px = pixmap.pixel(10, 10).expect("pixel in bounds");
        assert_eq!((px.red(), px.green(), px.blue(), px.alpha()), (255, 0, 0, 255));
        // Outside the rect stays the original black background.
        let outside = pixmap.pixel(2, 2).expect("pixel in bounds");
        assert_eq!((outside.red(), outside.green(), outside.blue()), (0, 0, 0));
    }

    #[test]
    fn draw_image_respects_the_active_clip() {
        let mut pixmap = Pixmap::new(20, 20).expect("pixmap alloc");
        pixmap.fill(tiny_skia::Color::from_rgba8(0, 0, 0, 255));
        let mut text = TextContext::new();
        let frame = Frame { width: 1, height: 1, rgba: vec![255, 0, 0, 255], delay_ms: 0 };

        {
            let mut painter = SkiaPainter::new(&mut pixmap, &mut text);
            painter.push_clip(Rect::new(0.0, 0.0, 5.0, 5.0));
            painter.draw_image(&frame, Rect::new(0.0, 0.0, 20.0, 20.0));
            painter.pop_clip();
        }

        // Inside the clip: painted red.
        let inside = pixmap.pixel(2, 2).expect("pixel in bounds");
        assert_eq!((inside.red(), inside.green(), inside.blue()), (255, 0, 0));
        // Outside the clip: still the untouched black background.
        let outside = pixmap.pixel(15, 15).expect("pixel in bounds");
        assert_eq!((outside.red(), outside.green(), outside.blue()), (0, 0, 0));
    }
}

/// Pack a rasterized RGBA pixmap into softbuffer's `0RGB` u32 layout.
/// Fill the pixmap with an opaque background first so premultiplied == straight.
pub fn present_to_softbuffer(pixmap: &Pixmap, out: &mut [u32]) {
    for (px, slot) in pixmap.pixels().iter().zip(out.iter_mut()) {
        let r = px.red() as u32;
        let g = px.green() as u32;
        let b = px.blue() as u32;
        *slot = (r << 16) | (g << 8) | b;
    }
}
