//! The rendering boundary. `nowui-core` walks the tree and issues these calls;
//! `nowui-render` implements the trait against tiny-skia. Core never sees a
//! `Pixmap`.

use crate::geometry::{Color, Edges, Point, Rect};
use crate::style::{TextAlign, Transform2D};

pub struct TextStyle {
    pub color: Color,
    pub size: f32,
    pub align: TextAlign,
    pub weight: u16,
    pub letter_spacing: f32,
}

pub trait Painter {
    /// `radius`: per-corner (`top`=top-left, `right`=top-right, `bottom`=
    /// bottom-right, `left`=bottom-left) — see `Style::radius`.
    fn fill_rect(&mut self, rect: Rect, color: Color, radius: Edges);
    fn stroke_rect(&mut self, rect: Rect, color: Color, width: f32, radius: Edges);
    /// Draw `text` inside `bounds`, respecting alignment. Implementations that
    /// lack a text stack may no-op (boxes-first milestone).
    fn draw_text(&mut self, text: &str, bounds: Rect, style: &TextStyle);
    /// Push a rectangular clip; subsequent draws are masked to the intersection.
    fn push_clip(&mut self, rect: Rect);
    fn pop_clip(&mut self);
    /// Measure the pixel size of `text` at `size`. Used by the solver for `Hug`
    /// text nodes. A crude fallback is fine before a real text stack lands.
    fn measure_text(&mut self, text: &str, size: f32) -> Point {
        // Fallback: assume ~0.55em advance, 1.3em line height.
        Point::new(text.chars().count() as f32 * size * 0.55, size * 1.3)
    }

    /// Push a 2D affine transform (`translate-*`/`scale-*`/`rotate-*`/`skew-*`),
    /// composed with whatever transform is already active. No-op by default —
    /// implementations that don't support transforms may ignore it entirely.
    fn push_transform(&mut self, _transform: Transform2D, _origin: Point) {}
    fn pop_transform(&mut self) {}

    /// Push a multiplicative opacity (`opacity-*`), composed with any active
    /// opacity. No-op by default.
    fn push_opacity(&mut self, _opacity: f32) {}
    fn pop_opacity(&mut self) {}
}
