//! The resolved style struct. Produced by the semantic pass from the raw
//! `(key, value)` pairs the parser emits.

use crate::geometry::{Color, Edges};
use crate::tailwind::Easing;

/// How a node is sized along an axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Sizing {
    /// Fixed pixels.
    Fixed(f32),
    /// A fraction (0.0..=1.0) of the parent's available extent (`w-1/2`, ...).
    Percent(f32),
    /// Grow to fill available space, weighted (`fill` == weight 1.0).
    Fill(f32),
    /// Shrink to fit content.
    Hug,
}

impl Default for Sizing {
    fn default() -> Self {
        Sizing::Hug
    }
}

/// Main-axis direction for a container's children.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Direction {
    #[default]
    Column,
    Row,
    /// `flex-row-reverse` / `flex-col-reverse`.
    RowReverse,
    ColumnReverse,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// Cross/main alignment for `align-[...]` on containers (simplified).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}

/// A container's layout mode: normal flex-approximation flow, or grid.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Display {
    #[default]
    Flow,
    Grid,
}

/// `position-static`/`position-relative`/`position-absolute`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Position {
    /// Normal in-flow placement (the default for every node).
    #[default]
    Static,
    /// Still in-flow, but `left`/`top`/`right`/`bottom` (if set) nudge it from
    /// its normal-flow position without affecting siblings — and it becomes
    /// the containing block for any `Absolute` descendant.
    Relative,
    /// Removed from normal flow entirely (doesn't consume space in its
    /// parent's sizing) and positioned via `left`/`top`/`right`/`bottom`
    /// against its direct parent's content box. NowUI simplification: real
    /// CSS resolves against the nearest *positioned* ancestor found by
    /// walking up any number of levels; here it's always the direct parent.
    Absolute,
}

/// One track of a `grid-template-columns`/`grid-template-rows` list.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GridTrack {
    /// `1fr`, `2fr`, ... — shares leftover space by weight.
    Fr(f32),
    Fixed(f32),
    Auto,
}

/// A 2D affine transform (`translate-*`, `scale-*`, `rotate-*`, `skew-*`).
/// No 3D transforms (`rotate-x/y`, `perspective`, ...) — out of scope for a
/// 2D layer/box-model renderer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform2D {
    pub translate_x: f32,
    pub translate_y: f32,
    pub scale_x: f32,
    pub scale_y: f32,
    pub rotate_deg: f32,
    pub skew_x_deg: f32,
    pub skew_y_deg: f32,
}

impl Default for Transform2D {
    fn default() -> Self {
        Transform2D {
            translate_x: 0.0,
            translate_y: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            rotate_deg: 0.0,
            skew_x_deg: 0.0,
            skew_y_deg: 0.0,
        }
    }
}

impl Transform2D {
    pub fn is_identity(&self) -> bool {
        *self == Transform2D::default()
    }

    pub fn lerp(a: Transform2D, b: Transform2D, t: f32) -> Transform2D {
        let l = |x: f32, y: f32| x + (y - x) * t;
        Transform2D {
            translate_x: l(a.translate_x, b.translate_x),
            translate_y: l(a.translate_y, b.translate_y),
            scale_x: l(a.scale_x, b.scale_x),
            scale_y: l(a.scale_y, b.scale_y),
            rotate_deg: l(a.rotate_deg, b.rotate_deg),
            skew_x_deg: l(a.skew_x_deg, b.skew_x_deg),
            skew_y_deg: l(a.skew_y_deg, b.skew_y_deg),
        }
    }
}

/// `transition-*` / `duration-*` / `ease-*` / `delay-*`, resolved together.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transition {
    pub duration_ms: f32,
    pub delay_ms: f32,
    pub easing: Easing,
}

/// Per-variant fully-resolved style overlays. Each is a *complete* `Style`
/// (base + just that variant's own class list applied on top), computed once
/// by the semantic pass — the runtime picks one and diffs it against the
/// unmodified base to find which fields it actually touched (see
/// `compute_effective`), so unrelated fields (e.g. a responsive width change)
/// aren't clobbered when a hover style is applied on top.
///
/// Only variants backed by real, already-tracked runtime state are
/// supported: `hover:`/`focus:`/`active:` (cursor/focus/mouse-down) and
/// responsive `sm:`/`md:`/`lg:`/`xl:`/`2xl:` (viewport width). `dark:`,
/// `group-*`/`peer-*`, and stacked variants (`sm:hover:x`) are not — there's
/// no theme or group-state model in this engine to drive them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StyleVariants {
    pub hover: Option<Box<Style>>,
    pub focus: Option<Box<Style>>,
    pub active: Option<Box<Style>>,
    /// `(min_width_px, cumulatively-resolved style)`, ascending by min-width.
    pub responsive: Vec<(u32, Style)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Style {
    pub direction: Direction,
    pub display: Display,
    pub position: Position,
    /// `left-*`/`top-*`/`right-*`/`bottom-*`. Only meaningful when `position`
    /// is `Relative` (nudges within the normal-flow slot) or `Absolute`
    /// (positions against the parent's content box) — ignored on `Static`.
    pub left: Option<f32>,
    pub right: Option<f32>,
    pub top: Option<f32>,
    pub bottom: Option<f32>,
    pub width: Sizing,
    pub height: Sizing,
    pub padding: Edges,
    pub margin: Edges,
    /// `scroll-h`/`scroll-v`: clip overflow along that axis and allow the
    /// mouse wheel to pan it (see `nowui-runtime`'s wheel handler).
    pub scroll_x: bool,
    pub scroll_y: bool,
    pub bg: Option<Color>,
    pub text_color: Color,
    pub text_align: TextAlign,
    pub align_main: Align,
    pub align_cross: Align,
    /// Corner radii, reusing `Edges`'s 1/2/3/4-value CSS shorthand — but as
    /// corners, not sides: `top`=top-left, `right`=top-right, `bottom`=
    /// bottom-right, `left`=bottom-left (clockwise from top-left, matching
    /// real CSS `border-radius`'s own corner order). The 2-value shorthand
    /// (`rounded-[a b]`) is then the two diagonal corner pairs: `a` for
    /// top-left/bottom-right, `b` for top-right/bottom-left — again matching
    /// plain CSS `border-radius: a b`.
    pub radius: Edges,
    pub font_size: f32,
    pub font_weight: u16,
    /// `None` = derive from `font_size` (the painter's own default leading).
    pub line_height: Option<f32>,
    pub letter_spacing: f32,
    /// Gap between children along the main axis (flow) or both grid axes
    /// unless `gap_x`/`gap_y` narrow it to one.
    pub gap: f32,
    pub gap_x: Option<f32>,
    pub gap_y: Option<f32>,
    pub border_width: Edges,
    pub border_color: Option<Color>,
    pub opacity: f32,
    /// `z-index-[N]`: paint order among *sibling* nodes only (there's no
    /// global stacking-context tree — a low-z child of a high-z sibling still
    /// paints within its own parent's turn). Higher paints later, i.e. on top.
    /// Ties keep source order (stable sort) — see `paint::paint_children`.
    pub z_index: i32,
    pub transform: Transform2D,
    pub transition: Option<Transition>,
    pub grid_template_columns: Vec<GridTrack>,
    pub grid_template_rows: Vec<GridTrack>,
    pub grid_column_span: u32,
    pub grid_row_span: u32,
    pub variants: StyleVariants,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            direction: Direction::Column,
            display: Display::Flow,
            position: Position::Static,
            left: None,
            right: None,
            top: None,
            bottom: None,
            width: Sizing::Hug,
            height: Sizing::Hug,
            padding: Edges::default(),
            margin: Edges::default(),
            scroll_x: false,
            scroll_y: false,
            bg: None,
            text_color: Color::BLACK,
            text_align: TextAlign::Left,
            align_main: Align::Start,
            align_cross: Align::Start,
            radius: Edges::default(),
            font_size: 16.0,
            font_weight: 400,
            line_height: None,
            letter_spacing: 0.0,
            gap: 0.0,
            gap_x: None,
            gap_y: None,
            border_width: Edges::default(),
            border_color: None,
            opacity: 1.0,
            z_index: 0,
            transform: Transform2D::default(),
            transition: None,
            grid_template_columns: Vec::new(),
            grid_template_rows: Vec::new(),
            grid_column_span: 1,
            grid_row_span: 1,
            variants: StyleVariants::default(),
        }
    }
}

/// A snapshot of the subset of `Style` fields we animate on transition
/// (colors, opacity, transform). Non-animatable fields (sizing, typography,
/// grid tracks, ...) snap instantly — see CLAUDE.md for the rationale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimatableStyle {
    pub bg: Option<Color>,
    pub text_color: Color,
    pub border_color: Option<Color>,
    pub radius: Edges,
    pub opacity: f32,
    pub transform: Transform2D,
}

impl AnimatableStyle {
    pub fn from_style(s: &Style) -> Self {
        AnimatableStyle {
            bg: s.bg,
            text_color: s.text_color,
            border_color: s.border_color,
            radius: s.radius,
            opacity: s.opacity,
            transform: s.transform,
        }
    }

    pub fn write_into(self, s: &mut Style) {
        s.bg = self.bg;
        s.text_color = self.text_color;
        s.border_color = self.border_color;
        s.radius = self.radius;
        s.opacity = self.opacity;
        s.transform = self.transform;
    }

    fn lerp_edges(a: Edges, b: Edges, t: f32) -> Edges {
        let l = |x: f32, y: f32| x + (y - x) * t;
        Edges { top: l(a.top, b.top), right: l(a.right, b.right), bottom: l(a.bottom, b.bottom), left: l(a.left, b.left) }
    }

    fn lerp_color(a: Option<Color>, b: Option<Color>, t: f32) -> Option<Color> {
        match (a, b) {
            (Some(a), Some(b)) => Some(Color {
                r: (a.r as f32 + (b.r as f32 - a.r as f32) * t).round() as u8,
                g: (a.g as f32 + (b.g as f32 - a.g as f32) * t).round() as u8,
                b: (a.b as f32 + (b.b as f32 - a.b as f32) * t).round() as u8,
                a: (a.a as f32 + (b.a as f32 - a.a as f32) * t).round() as u8,
            }),
            _ => {
                if t >= 1.0 {
                    b
                } else {
                    a
                }
            }
        }
    }

    pub fn lerp(a: AnimatableStyle, b: AnimatableStyle, t: f32) -> AnimatableStyle {
        AnimatableStyle {
            bg: Self::lerp_color(a.bg, b.bg, t),
            text_color: Self::lerp_color(Some(a.text_color), Some(b.text_color), t).unwrap(),
            border_color: Self::lerp_color(a.border_color, b.border_color, t),
            radius: Self::lerp_edges(a.radius, b.radius, t),
            opacity: a.opacity + (b.opacity - a.opacity) * t,
            transform: Transform2D::lerp(a.transform, b.transform, t),
        }
    }
}

/// Copy onto `working` every field where `variant` differs from `unvaried`
/// (the base the variant was itself resolved against) — i.e. apply only what
/// the variant's own class list actually changed, leaving everything else
/// (e.g. a responsive override already folded into `working`) alone.
fn overlay_touched_fields(working: &mut Style, unvaried: &Style, variant: &Style) {
    macro_rules! overlay {
        ($($field:ident),* $(,)?) => {
            $(
                if variant.$field != unvaried.$field {
                    working.$field = variant.$field.clone();
                }
            )*
        };
    }
    overlay!(
        direction,
        display,
        position,
        left,
        right,
        top,
        bottom,
        width,
        height,
        padding,
        margin,
        scroll_x,
        scroll_y,
        bg,
        text_color,
        text_align,
        align_main,
        align_cross,
        radius,
        font_size,
        font_weight,
        line_height,
        letter_spacing,
        gap,
        gap_x,
        gap_y,
        border_width,
        border_color,
        opacity,
        z_index,
        transform,
        transition,
        grid_template_columns,
        grid_template_rows,
        grid_column_span,
        grid_row_span,
    );
}

/// Compute this frame's *target* effective style (before transition
/// smoothing) from a node's base style, its variant overlays, the current
/// viewport width, and its live interaction state.
pub fn compute_effective(
    base: &Style,
    viewport_w: f32,
    hovered: bool,
    focused: bool,
    pressed: bool,
) -> Style {
    let mut working = base.clone();

    // Responsive cascade: pick the largest matching breakpoint. Each entry is
    // already cumulatively resolved (see semantic.rs), so this is a straight
    // replace, not a merge.
    for (min_w, resolved) in &base.variants.responsive {
        if viewport_w >= *min_w as f32 {
            working = resolved.clone();
        }
    }

    // State variants layer on top, diffed against the plain base so they only
    // touch what they explicitly declared.
    if pressed {
        if let Some(v) = &base.variants.active {
            overlay_touched_fields(&mut working, base, v);
        }
    } else if focused {
        if let Some(v) = &base.variants.focus {
            overlay_touched_fields(&mut working, base, v);
        }
    } else if hovered {
        if let Some(v) = &base.variants.hover {
            overlay_touched_fields(&mut working, base, v);
        }
    }

    working
}

/// Shared box-height/option-row-height formula for `Dropdown`, used by both
/// `layout::measure` (to size the node) and `paint`/the runtime's click hit
/// math (to know which region — the closed box, or which option row — a
/// point falls in). Keeping this in one place keeps the two in sync.
pub fn dropdown_metrics(font_size: f32) -> (f32, f32) {
    (font_size * 1.3 + 16.0, font_size * 1.3 + 12.0)
}
