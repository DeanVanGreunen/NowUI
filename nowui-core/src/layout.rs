//! Row/column/grid layout solver: a two-pass measure-then-distribute algorithm.
//!
//! Pass 1 (measure): bottom-up, compute each node's intrinsic (hug) size.
//! Pass 2 (arrange): top-down, hand each node a concrete rect and place its
//! children — along the main axis for `Display::Flow` containers (a flex
//! approximation: no min/max or wrap), or into a track grid for
//! `Display::Grid` containers (fixed/auto/fr tracks, row-major auto-place,
//! no named lines/auto-fit/dense-packing/minmax()).
//!
//! This is intentionally a compact, correct-enough implementation. For full
//! flexbox/grid semantics you'd swap the internals for `taffy` without
//! changing the arena or painter — keep that boundary clean.

use std::collections::HashMap;

use crate::arena::{NodeId, Ui};
use crate::geometry::{Point, Rect, Size};
use crate::painter::Painter;
use crate::style::{Align, Direction, Display, GridTrack, Position, Sizing, Style};

/// Solve every layer against the given viewport size.
pub fn solve(ui: &mut Ui, viewport: Size, painter: &mut dyn Painter) {
    let roots: Vec<NodeId> = ui.layers.iter().map(|l| l.root).collect();
    for root in roots {
        let mut sizes = HashMap::new();
        measure(ui, root, painter, &mut sizes);
        // Root fills the viewport unless it has an explicit fixed size.
        let root_rect = Rect::new(0.0, 0.0, viewport.w, viewport.h);
        arrange(ui, root, root_rect, &sizes);
        // After the normal pass, so every `Menu`'s own `computed` rect
        // (which anchors its popup) is final.
        arrange_menu_popups(ui, root, &sizes);
    }
}

/// Give every *open, non-empty* `Menu`'s children real `computed` rects,
/// stacked in a column floating directly below the header — same
/// "floating, doesn't affect normal layout" principle as `Dropdown`'s open
/// option list, but with real arena-node children (typically `MenuItem`),
/// so (unlike `Dropdown`'s hand-drawn text rows) they get a genuine
/// `arrange_flow` pass and can be arbitrarily complex widgets themselves.
/// `Node::content_size` is repurposed here to record the popup's own
/// resolved (width, height) — `paint_menu_popup` and the runtime's click
/// hit-testing both need it and would otherwise have to recompute it.
fn arrange_menu_popups(ui: &mut Ui, id: NodeId, sizes: &HashMap<NodeId, Size>) {
    let children = ui.get(id).children.clone();
    let is_open_with_children = matches!(&ui.get(id).kind, crate::arena::NodeKind::Menu { open: true, .. }) && !children.is_empty();
    if is_open_with_children {
        let style = ui.get(id).style.clone();
        let rect = ui.get(id).computed;
        let gap_total = style.gap * children.len().saturating_sub(1) as f32;
        let popup_h: f32 = children.iter().map(|c| sizes.get(c).map(|s| s.h).unwrap_or(0.0)).sum::<f32>()
            + gap_total
            + style.padding.top
            + style.padding.bottom
            + style.border_width.top
            + style.border_width.bottom;
        let popup_rect = Rect::new(rect.x, rect.y + rect.h, rect.w, popup_h);
        let inner = popup_rect.inset(style.padding).inset(style.border_width);
        arrange_flow(ui, &style, &children, inner, sizes, Point::default());
        ui.get_mut(id).content_size = Size::new(popup_rect.w, popup_rect.h);
    }
    // Recurse regardless — a Menu can be nested inside another popup/widget,
    // and popups need this same treatment wherever they occur in the tree.
    for c in children {
        arrange_menu_popups(ui, c, sizes);
    }
}

fn axis_is_row(d: Direction) -> bool {
    matches!(d, Direction::Row | Direction::RowReverse)
}

/// Pass 1: intrinsic size of a subtree (what it wants when hugging content).
/// Memoizes every node's result into `sizes` so pass 2 can look up a child's
/// true intrinsic extent instead of re-deriving a crude estimate.
fn measure(ui: &mut Ui, id: NodeId, painter: &mut dyn Painter, sizes: &mut HashMap<NodeId, Size>) -> Size {
    // Measure children first (bottom-up).
    let children = ui.get(id).children.clone();
    let mut child_sizes = Vec::with_capacity(children.len());
    for &c in &children {
        child_sizes.push(measure(ui, c, painter, sizes));
    }

    let node = ui.get(id);
    let style = node.style.clone();
    let pad = style.padding;
    let bw = style.border_width;

    // Content size from this node's own kind (e.g. text advance).
    let own = match &node.kind {
        crate::arena::NodeKind::Text { content } => {
            let m = painter.measure_text(content, style.font_size);
            Size::new(m.x, m.y)
        }
        crate::arena::NodeKind::Button { label } => {
            let m = painter.measure_text(label, style.font_size);
            Size::new(m.x, m.y)
        }
        crate::arena::NodeKind::TextInput { placeholder, .. } => {
            let m = painter.measure_text(placeholder, style.font_size);
            Size::new(m.x, m.y)
        }
        crate::arena::NodeKind::Checkbox { label, .. } => {
            let m = painter.measure_text(label, style.font_size);
            Size::new(m.x + style.font_size + 6.0, m.y)
        }
        crate::arena::NodeKind::Dropdown { placeholder, options, selected, .. } => {
            // The open option list is a floating popup (see `paint.rs`), not
            // part of the flow — it never contributes to this node's own
            // size, open or not, so it can't push later siblings around.
            let (box_h, _) = crate::style::dropdown_metrics(style.font_size);
            let label = selected.and_then(|i| options.get(i)).cloned().unwrap_or_else(|| placeholder.clone());
            let m = painter.measure_text(&label, style.font_size);
            Size::new(m.x + 24.0, box_h)
        }
        crate::arena::NodeKind::Slider { .. } => {
            let (_, thumb_d) = crate::style::slider_metrics(style.font_size);
            Size::new(crate::style::DEFAULT_CONTROL_WIDTH, thumb_d)
        }
        crate::arena::NodeKind::ProgressBar { .. } => {
            let (track_h, _) = crate::style::slider_metrics(style.font_size);
            Size::new(crate::style::DEFAULT_CONTROL_WIDTH, track_h)
        }
        crate::arena::NodeKind::Menu { label, .. } => {
            let m = painter.measure_text(label, style.font_size);
            Size::new(m.x, m.y)
        }
        crate::arena::NodeKind::MenuItem { label } => {
            let m = painter.measure_text(label, style.font_size);
            Size::new(m.x, m.y)
        }
        crate::arena::NodeKind::Container => Size::default(),
    };

    // A `Menu`'s children (real arena nodes, typically `MenuItem`) never
    // contribute to its own size, open or not — its popup floats below the
    // header (see `arrange_menu_popups`/`paint_menu_popup`), same principle
    // as `Dropdown`'s open option list never affecting its box size. They're
    // still measured above (bottom-up, unconditionally) so the popup pass
    // has their real sizes memoized in `sizes` when it needs them.
    let is_menu = matches!(&node.kind, crate::arena::NodeKind::Menu { .. });

    let content = if is_menu {
        Size::new(own.w, own.h)
    } else if style.display == Display::Grid {
        grid_intrinsic(&children, &style, sizes)
    } else {
        // Fold children along the main axis. Absolutely-positioned children
        // are out of flow — they don't consume space in the parent's own
        // intrinsic size (matching real CSS).
        let in_flow_sizes: Vec<Size> = children
            .iter()
            .zip(child_sizes.iter())
            .filter(|(&c, _)| ui.get(c).style.position != Position::Absolute)
            .map(|(_, &s)| s)
            .collect();
        let gap_total = style.gap * in_flow_sizes.len().saturating_sub(1) as f32;
        let (mut main, mut cross) = (0.0f32, 0.0f32);
        for cs in &in_flow_sizes {
            if axis_is_row(style.direction) {
                main += cs.w;
                cross = cross.max(cs.h);
            } else {
                main += cs.h;
                cross = cross.max(cs.w);
            }
        }
        main += gap_total;

        if axis_is_row(style.direction) {
            Size::new(main.max(own.w), cross.max(own.h))
        } else {
            Size::new(cross.max(own.w), main.max(own.h))
        }
    };

    let intrinsic = Size::new(
        content.w + pad.left + pad.right + bw.left + bw.right,
        content.h + pad.top + pad.bottom + bw.top + bw.bottom,
    );

    // Apply explicit fixed sizes where present.
    let w = match style.width {
        Sizing::Fixed(v) => v,
        _ => intrinsic.w,
    };
    let h = match style.height {
        Sizing::Fixed(v) => v,
        _ => intrinsic.h,
    };
    let size = Size::new(w, h);
    sizes.insert(id, size);
    size
}

/// Pass 2: give `id` a concrete rect and place its children within it.
fn arrange(ui: &mut Ui, id: NodeId, rect: Rect, sizes: &HashMap<NodeId, Size>) {
    ui.get_mut(id).computed = rect;

    let style = ui.get(id).style.clone();
    let children = ui.get(id).children.clone();
    if children.is_empty() {
        return;
    }
    // A `Menu`'s children are never part of its own normal-flow arrangement
    // (open or closed) — they only ever get real rects via
    // `arrange_menu_popups`, positioned as a floating popup below the
    // header, once every layer's normal arrange pass has finished (so the
    // header's own `computed` rect, which anchors the popup, is final).
    if matches!(&ui.get(id).kind, crate::arena::NodeKind::Menu { .. }) {
        return;
    }

    let inner = rect.inset(style.padding).inset(style.border_width);

    // Absolutely-positioned children are out of flow: skipped by the normal
    // arrangement pass entirely, then positioned separately against `inner`
    // (this node's content box — see `Position::Absolute`'s docs for the
    // "always the direct parent" simplification).
    let mut in_flow = Vec::with_capacity(children.len());
    let mut out_of_flow = Vec::new();
    for &c in &children {
        if ui.get(c).style.position == Position::Absolute {
            out_of_flow.push(c);
        } else {
            in_flow.push(c);
        }
    }

    let scroll_offset = ui.get(id).scroll_offset;
    let content_size = if style.display == Display::Grid {
        arrange_grid(ui, &style, &in_flow, inner, sizes, scroll_offset)
    } else {
        arrange_flow(ui, &style, &in_flow, inner, sizes, scroll_offset)
    };
    ui.get_mut(id).content_size = content_size;

    for c in out_of_flow {
        arrange_absolute(ui, c, inner, sizes);
    }
}

/// Position an out-of-flow child against `containing_block` (its direct
/// parent's content box) via `left`/`top`/`right`/`bottom`. If both opposing
/// offsets are set and the corresponding axis is `Hug`, they define the
/// extent directly (matching plain CSS `left`+`right` with `width: auto`).
fn arrange_absolute(ui: &mut Ui, id: NodeId, containing_block: Rect, sizes: &HashMap<NodeId, Size>) {
    let style = ui.get(id).style.clone();
    let natural = sizes.get(&id).copied().unwrap_or_default();

    let (x, w) = match (style.left, style.right, style.width) {
        (Some(l), Some(r), Sizing::Hug) => (containing_block.x + l, (containing_block.w - l - r).max(0.0)),
        (Some(l), _, sizing) => (containing_block.x + l, resolve_absolute_extent(sizing, natural.w, containing_block.w)),
        (None, Some(r), sizing) => {
            let w = resolve_absolute_extent(sizing, natural.w, containing_block.w);
            (containing_block.x + containing_block.w - r - w, w)
        }
        (None, None, sizing) => (containing_block.x, resolve_absolute_extent(sizing, natural.w, containing_block.w)),
    };
    let (y, h) = match (style.top, style.bottom, style.height) {
        (Some(t), Some(b), Sizing::Hug) => (containing_block.y + t, (containing_block.h - t - b).max(0.0)),
        (Some(t), _, sizing) => (containing_block.y + t, resolve_absolute_extent(sizing, natural.h, containing_block.h)),
        (None, Some(b), sizing) => {
            let h = resolve_absolute_extent(sizing, natural.h, containing_block.h);
            (containing_block.y + containing_block.h - b - h, h)
        }
        (None, None, sizing) => (containing_block.y, resolve_absolute_extent(sizing, natural.h, containing_block.h)),
    };

    arrange(ui, id, Rect::new(x, y, w, h), sizes);
}

fn resolve_absolute_extent(sizing: Sizing, natural: f32, containing: f32) -> f32 {
    match sizing {
        Sizing::Fixed(v) => v,
        Sizing::Percent(p) => p * containing,
        Sizing::Fill(_) => containing,
        Sizing::Hug => natural,
    }
}

fn arrange_flow(
    ui: &mut Ui,
    style: &Style,
    children: &[NodeId],
    inner: Rect,
    sizes: &HashMap<NodeId, Size>,
    scroll_offset: Point,
) -> Size {
    let gap = style.gap;
    let is_row = axis_is_row(style.direction);
    let reversed = matches!(style.direction, Direction::RowReverse | Direction::ColumnReverse);

    let avail_main = if is_row { inner.w } else { inner.h };
    let cross_avail = if is_row { inner.h } else { inner.w };

    // Sum fixed/hug/percent extents and total fill weight along the main axis.
    let mut fixed_main = 0.0f32;
    let mut fill_weight = 0.0f32;
    let mut child_main = Vec::with_capacity(children.len());
    for &c in children {
        let cs = &ui.get(c).style;
        let sizing = if is_row { cs.width } else { cs.height };
        let intrinsic = if is_row { sizes[&c].w } else { sizes[&c].h };
        match sizing {
            Sizing::Fill(w) => {
                fill_weight += w;
                child_main.push((c, None));
            }
            Sizing::Fixed(v) => {
                fixed_main += v;
                child_main.push((c, Some(v)));
            }
            Sizing::Percent(p) => {
                let v = p * avail_main;
                fixed_main += v;
                child_main.push((c, Some(v)));
            }
            Sizing::Hug => {
                fixed_main += intrinsic;
                child_main.push((c, Some(intrinsic)));
            }
        }
    }

    let gaps = gap * children.len().saturating_sub(1) as f32;
    let leftover = (avail_main - fixed_main - gaps).max(0.0);

    // Starting offset for main-axis alignment when there's slack and no fills.
    let mut cursor = if is_row { inner.x } else { inner.y };
    if fill_weight == 0.0 {
        cursor += match style.align_main {
            Align::Start => 0.0,
            Align::Center => leftover / 2.0,
            Align::End => leftover,
        };
    }

    if reversed {
        child_main.reverse();
    }

    let mut content_cross = 0.0f32;
    for (c, main_size) in child_main {
        let main_extent = match main_size {
            Some(v) => v,
            None => {
                let cs = &ui.get(c).style;
                let w = if is_row { cs.width } else { cs.height };
                if let Sizing::Fill(weight) = w {
                    leftover * (weight / fill_weight)
                } else {
                    0.0
                }
            }
        };

        let cs = ui.get(c).style.clone();
        let cross_sizing = if is_row { cs.height } else { cs.width };
        let cross_extent = match cross_sizing {
            Sizing::Fixed(v) => v,
            Sizing::Percent(p) => p * cross_avail,
            Sizing::Fill(_) => cross_avail,
            Sizing::Hug => {
                if is_row {
                    sizes[&c].h
                } else {
                    sizes[&c].w
                }
            }
        };
        let cross_off = match style.align_cross {
            Align::Start => 0.0,
            Align::Center => (cross_avail - cross_extent) / 2.0,
            Align::End => cross_avail - cross_extent,
        };
        content_cross = content_cross.max(cross_extent);

        let mut child_rect = if is_row {
            Rect::new(cursor, inner.y + cross_off, main_extent, cross_extent)
        } else {
            Rect::new(inner.x + cross_off, cursor, cross_extent, main_extent)
        };

        // `position-relative`: nudge this child alone within its normal-flow
        // slot; siblings already advanced past its unshifted extent.
        if cs.position == Position::Relative {
            if let Some(l) = cs.left {
                child_rect.x += l;
            } else if let Some(r) = cs.right {
                child_rect.x -= r;
            }
            if let Some(t) = cs.top {
                child_rect.y += t;
            } else if let Some(b) = cs.bottom {
                child_rect.y -= b;
            }
        }
        // `scroll-h`/`scroll-v`: pan the whole subtree by the runtime-tracked
        // offset. Screen-space X/Y, not main/cross — independent of direction.
        if style.scroll_x {
            child_rect.x -= scroll_offset.x;
        }
        if style.scroll_y {
            child_rect.y -= scroll_offset.y;
        }

        arrange(ui, c, child_rect, sizes);
        cursor += main_extent + gap;
    }

    let content_main = if fill_weight > 0.0 { avail_main } else { fixed_main + gaps };
    if is_row {
        Size::new(content_main, content_cross)
    } else {
        Size::new(content_cross, content_main)
    }
}

/// Where an auto-placed grid child lands.
#[derive(Clone, Copy)]
struct GridCell {
    col: usize,
    row: usize,
    col_span: usize,
    row_span: usize,
}

/// Row-major auto-placement: no named lines, explicit start/end, `auto-fit`/
/// `auto-fill`, `minmax()`, or dense packing — each child takes the next free
/// slot, wrapping to a new row when its span doesn't fit the remaining columns.
fn place_grid_children(children: &[NodeId], ui: &Ui, ncols: usize) -> (Vec<GridCell>, usize) {
    let mut cells = Vec::with_capacity(children.len());
    let (mut col, mut row) = (0usize, 0usize);
    for &c in children {
        let cs = &ui.get(c).style;
        let col_span = (cs.grid_column_span as usize).max(1).min(ncols.max(1));
        let row_span = (cs.grid_row_span as usize).max(1);
        if col + col_span > ncols {
            col = 0;
            row += 1;
        }
        cells.push(GridCell { col, row, col_span, row_span });
        col += col_span;
    }
    let nrows = cells.iter().map(|p| p.row + p.row_span).max().unwrap_or(1).max(1);
    (cells, nrows)
}

/// Resolve `Fixed`/`Auto`/`Fr` tracks against `avail` pixels, where `auto_estimate(i)`
/// gives the intrinsic size for an `Auto` track at index `i`.
fn resolve_tracks(tracks: &[GridTrack], avail: f32, auto_estimate: impl Fn(usize) -> f32) -> Vec<f32> {
    let mut out = vec![0.0f32; tracks.len()];
    let mut fr_total = 0.0f32;
    let mut fixed_total = 0.0f32;
    for (i, t) in tracks.iter().enumerate() {
        match t {
            GridTrack::Fixed(v) => {
                out[i] = *v;
                fixed_total += v;
            }
            GridTrack::Auto => {
                let v = auto_estimate(i);
                out[i] = v;
                fixed_total += v;
            }
            GridTrack::Fr(_) => {}
        }
    }
    let leftover = (avail - fixed_total).max(0.0);
    if fr_total > 0.0 {
        for (i, t) in tracks.iter().enumerate() {
            if let GridTrack::Fr(w) = t {
                out[i] = leftover * (w / fr_total);
            }
        }
    } else {
        for t in tracks {
            if let GridTrack::Fr(w) = t {
                fr_total += w;
            }
        }
        if fr_total > 0.0 {
            for (i, t) in tracks.iter().enumerate() {
                if let GridTrack::Fr(w) = t {
                    out[i] = leftover * (w / fr_total);
                }
            }
        }
    }
    out
}

fn track_positions(track_sizes: &[f32], gap: f32, start: f32) -> Vec<f32> {
    let mut pos = Vec::with_capacity(track_sizes.len());
    let mut cursor = start;
    for &s in track_sizes {
        pos.push(cursor);
        cursor += s + gap;
    }
    pos
}

fn grid_tracks_and_cells(
    ui: &Ui,
    style: &Style,
    children: &[NodeId],
) -> (Vec<GridTrack>, Vec<GridTrack>, Vec<GridCell>) {
    let cols = if style.grid_template_columns.is_empty() {
        vec![GridTrack::Fr(1.0)]
    } else {
        style.grid_template_columns.clone()
    };
    let (cells, nrows) = place_grid_children(children, ui, cols.len());
    let mut rows = style.grid_template_rows.clone();
    if rows.is_empty() {
        rows = vec![GridTrack::Auto; nrows];
    } else {
        while rows.len() < nrows {
            rows.push(GridTrack::Auto);
        }
    }
    (cols, rows, cells)
}

/// Intrinsic size of a grid container: `Fixed`/`Auto` tracks sum up; `Fr`
/// tracks contribute nothing (they only claim leftover space once a concrete
/// size is available, in `arrange_grid`).
fn grid_intrinsic(children: &[NodeId], style: &Style, sizes: &HashMap<NodeId, Size>) -> Size {
    // Needs `ui` for per-child span/style lookups, but at measure() time we
    // only have `sizes`; span info isn't needed for the intrinsic estimate
    // (a `Hug` grid just sums the natural track sizes), so approximate with
    // span 1 row-major placement using column count alone.
    let ncols = style.grid_template_columns.len().max(1);
    let ncells = children.len();
    let nrows = ncells.div_ceil(ncols).max(1);

    let col_w = |i: usize| -> f32 {
        children
            .iter()
            .enumerate()
            .filter(|(idx, _)| idx % ncols == i)
            .map(|(_, c)| sizes.get(c).map(|s| s.w).unwrap_or(0.0))
            .fold(0.0f32, f32::max)
    };
    let row_h = |i: usize| -> f32 {
        children
            .iter()
            .enumerate()
            .filter(|(idx, _)| idx / ncols == i)
            .map(|(_, c)| sizes.get(c).map(|s| s.h).unwrap_or(0.0))
            .fold(0.0f32, f32::max)
    };

    let cols = if style.grid_template_columns.is_empty() {
        vec![GridTrack::Auto; ncols]
    } else {
        style.grid_template_columns.clone()
    };
    let rows = if style.grid_template_rows.is_empty() {
        vec![GridTrack::Auto; nrows]
    } else {
        style.grid_template_rows.clone()
    };

    let col_gap = style.gap_x.unwrap_or(style.gap);
    let row_gap = style.gap_y.unwrap_or(style.gap);

    let w: f32 = cols
        .iter()
        .enumerate()
        .map(|(i, t)| match t {
            GridTrack::Fixed(v) => *v,
            GridTrack::Auto => col_w(i),
            GridTrack::Fr(_) => 0.0,
        })
        .sum::<f32>()
        + col_gap * cols.len().saturating_sub(1) as f32;

    let h: f32 = rows
        .iter()
        .enumerate()
        .map(|(i, t)| match t {
            GridTrack::Fixed(v) => *v,
            GridTrack::Auto => row_h(i),
            GridTrack::Fr(_) => 0.0,
        })
        .sum::<f32>()
        + row_gap * rows.len().saturating_sub(1) as f32;

    Size::new(w, h)
}

fn arrange_grid(
    ui: &mut Ui,
    style: &Style,
    children: &[NodeId],
    inner: Rect,
    sizes: &HashMap<NodeId, Size>,
    scroll_offset: Point,
) -> Size {
    let (cols, rows, cells) = grid_tracks_and_cells(ui, style, children);
    let ncols = cols.len();
    let col_gap = style.gap_x.unwrap_or(style.gap);
    let row_gap = style.gap_y.unwrap_or(style.gap);

    let col_gaps_total = col_gap * ncols.saturating_sub(1) as f32;
    let avail_w = (inner.w - col_gaps_total).max(0.0);
    let col_widths = resolve_tracks(&cols, avail_w, |i| {
        cells
            .iter()
            .zip(children.iter())
            .filter(|(p, _)| p.col == i && p.col_span == 1)
            .map(|(_, c)| sizes.get(c).map(|s| s.w).unwrap_or(0.0))
            .fold(0.0f32, f32::max)
    });

    let row_gaps_total = row_gap * rows.len().saturating_sub(1) as f32;
    let avail_h = (inner.h - row_gaps_total).max(0.0);
    let row_heights = resolve_tracks(&rows, avail_h, |i| {
        cells
            .iter()
            .zip(children.iter())
            .filter(|(p, _)| p.row == i && p.row_span == 1)
            .map(|(_, c)| sizes.get(c).map(|s| s.h).unwrap_or(0.0))
            .fold(0.0f32, f32::max)
    });

    let col_x = track_positions(&col_widths, col_gap, inner.x);
    let row_y = track_positions(&row_heights, row_gap, inner.y);

    for (cell, &c) in cells.iter().zip(children.iter()) {
        let x = col_x[cell.col];
        let y = row_y[cell.row];
        let col_end = (cell.col + cell.col_span).min(col_widths.len());
        let row_end = (cell.row + cell.row_span).min(row_heights.len());
        let w = col_widths[cell.col..col_end].iter().sum::<f32>()
            + col_gap * (col_end - cell.col).saturating_sub(1) as f32;
        let h = row_heights[cell.row..row_end].iter().sum::<f32>()
            + row_gap * (row_end - cell.row).saturating_sub(1) as f32;
        let mut child_rect = Rect::new(x, y, w, h);
        if style.scroll_x {
            child_rect.x -= scroll_offset.x;
        }
        if style.scroll_y {
            child_rect.y -= scroll_offset.y;
        }
        arrange(ui, c, child_rect, sizes);
    }

    Size::new(
        col_widths.iter().sum::<f32>() + col_gaps_total,
        row_heights.iter().sum::<f32>() + row_gaps_total,
    )
}

/// Convenience: point-in-rect for the root of a layer.
pub fn root_bounds(ui: &Ui, root: NodeId) -> Rect {
    ui.get(root).computed
}

/// Re-exported so callers don't need the geometry import for hit tests.
pub use crate::geometry::Point as HitPoint;
