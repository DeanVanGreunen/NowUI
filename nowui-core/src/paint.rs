//! Walk the solved arena and issue `Painter` calls, layer by layer,
//! back-to-front. This is the "retained tree, immediate paint" model: the tree
//! persists, but each redraw re-walks it rather than caching draw commands.

use crate::arena::{NodeId, NodeKind, Ui};
use crate::geometry::{Color, Edges, Point, Rect, Size};
use crate::painter::{Painter, TextStyle};
use crate::style::{Position, TextAlign};

pub fn paint(ui: &Ui, painter: &mut dyn Painter) {
    // Open `Dropdown`s are collected here instead of drawn inline, so their
    // option list floats on top of *everything* (drawn after every layer,
    // once no ancestor clip is active) instead of being clipped by whatever
    // container it happens to sit in — see `paint_dropdown_popup`.
    let mut popups = Vec::new();
    for layer in &ui.layers {
        paint_node(ui, layer.root, painter, &mut popups);
    }
    for id in popups {
        paint_dropdown_popup(ui, id, painter);
    }
}

fn paint_node(ui: &Ui, id: NodeId, painter: &mut dyn Painter, popups: &mut Vec<NodeId>) {
    let node = ui.get(id);
    let rect = node.computed;
    let style = &node.style;

    let pushed_transform = !style.transform.is_identity();
    if pushed_transform {
        let origin = Point::new(rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
        painter.push_transform(style.transform, origin);
    }
    let pushed_opacity = style.opacity < 1.0;
    if pushed_opacity {
        painter.push_opacity(style.opacity.max(0.0));
    }

    if let Some(bg) = style.bg {
        painter.fill_rect(rect, bg, style.radius);
    }
    if let Some(border_color) = style.border_color {
        let bw = style.border_width;
        if bw.top > 0.0 || bw.right > 0.0 || bw.bottom > 0.0 || bw.left > 0.0 {
            // Uniform-width approximation: stroke_rect takes one width, so use
            // the top edge's (matches the common `border-{width}` case; mixed
            // per-side widths aren't representable by a single stroked path).
            painter.stroke_rect(rect, border_color, bw.top.max(bw.left), style.radius);
        }
    }

    let text_style = TextStyle {
        color: style.text_color,
        size: style.font_size,
        align: style.text_align,
        weight: style.font_weight,
        letter_spacing: style.letter_spacing,
    };

    let content_rect = rect.inset(style.border_width).inset(style.padding);
    match &node.kind {
        NodeKind::Text { content } => {
            painter.draw_text(content, content_rect, &text_style);
        }
        NodeKind::Button { label } => {
            painter.draw_text(label, content_rect, &text_style);
        }
        NodeKind::TextInput { placeholder, masked, .. } => {
            // Show the bound value if present; otherwise the placeholder.
            // (Value resolution against `node.value_path` and live app state
            // happens in the runtime; here we render the placeholder as the
            // boxes-first default.)
            let shown = if *masked && !placeholder.is_empty() {
                placeholder.clone()
            } else {
                placeholder.clone()
            };
            painter.draw_text(&shown, content_rect, &text_style);
        }
        NodeKind::Checkbox { label, checked } => {
            let box_size = style.font_size;
            let mut box_rect = content_rect;
            box_rect.w = box_size;
            box_rect.h = box_size;
            let box_border = style.border_color.unwrap_or(style.text_color);
            if let Some(box_bg) = style.bg {
                painter.fill_rect(box_rect, box_bg, style.radius);
            }
            painter.stroke_rect(box_rect, box_border, 1.0, style.radius);
            if *checked {
                let mut inner = box_rect;
                inner.x += 3.0;
                inner.y += 3.0;
                inner.w -= 6.0;
                inner.h -= 6.0;
                painter.fill_rect(inner, style.text_color, style.radius);
            }
            let mut label_rect = content_rect;
            label_rect.x += box_size + 6.0;
            label_rect.w -= box_size + 6.0;
            painter.draw_text(label, label_rect, &text_style);
        }
        NodeKind::Slider { value } => {
            let (track_h, thumb_d) = crate::style::slider_metrics(style.font_size);
            let track_color = style.border_color.unwrap_or(Color::rgb(209, 213, 219));
            let fill_color = style.text_color;

            let track_rect = Rect::new(
                content_rect.x,
                content_rect.y + (content_rect.h - track_h) / 2.0,
                content_rect.w,
                track_h,
            );
            let track_radius = Edges::all(track_h / 2.0);
            painter.fill_rect(track_rect, track_color, track_radius);

            let v = value.clamp(0.0, 1.0);
            let filled_w = track_rect.w * v;
            if filled_w > 0.0 {
                painter.fill_rect(Rect::new(track_rect.x, track_rect.y, filled_w, track_h), fill_color, track_radius);
            }

            // Thumb: a filled circle-ish square (no circle primitive — same
            // crude-box convention as Checkbox/Dropdown's caret).
            let thumb_x = content_rect.x + filled_w - thumb_d / 2.0;
            let thumb_rect = Rect::new(thumb_x, content_rect.y + (content_rect.h - thumb_d) / 2.0, thumb_d, thumb_d);
            painter.fill_rect(thumb_rect, fill_color, Edges::all(thumb_d / 2.0));
            if let Some(border) = style.border_color {
                painter.stroke_rect(thumb_rect, border, 1.0, Edges::all(thumb_d / 2.0));
            }
        }
        NodeKind::ProgressBar { value } => {
            let (track_h, _) = crate::style::slider_metrics(style.font_size);
            let track_color = style.border_color.unwrap_or(Color::rgb(229, 231, 235));
            let fill_color = style.text_color;

            let track_rect = Rect::new(
                content_rect.x,
                content_rect.y + (content_rect.h - track_h) / 2.0,
                content_rect.w,
                track_h,
            );
            let radius = Edges::all(track_h / 2.0);
            painter.fill_rect(track_rect, track_color, radius);

            let filled_w = track_rect.w * value.clamp(0.0, 1.0);
            if filled_w > 0.0 {
                painter.fill_rect(Rect::new(track_rect.x, track_rect.y, filled_w, track_h), fill_color, radius);
            }
        }
        NodeKind::Dropdown { placeholder, options, selected, open, .. } => {
            let (box_h, _) = crate::style::dropdown_metrics(style.font_size);
            let box_border = style.border_color.unwrap_or(Color::rgb(209, 213, 219));

            let mut box_rect = content_rect;
            box_rect.h = box_h;
            painter.stroke_rect(box_rect, box_border, 1.0, style.radius);

            let label = selected.and_then(|i| options.get(i)).cloned().unwrap_or_else(|| placeholder.clone());
            let label_style = TextStyle {
                color: style.text_color,
                size: style.font_size,
                align: TextAlign::Left,
                weight: style.font_weight,
                letter_spacing: style.letter_spacing,
            };
            let mut label_rect = box_rect.inset(Edges::all(8.0));
            label_rect.w -= 14.0; // leave room for the caret indicator
            painter.draw_text(&label, label_rect, &label_style);

            // Caret indicator: a small filled square (no path primitive for a
            // real triangle — matches the Checkbox widget's own crude-box style).
            let caret_size = (style.font_size * 0.4).max(4.0);
            let caret = Rect::new(
                box_rect.x + box_rect.w - caret_size - 8.0,
                box_rect.y + (box_rect.h - caret_size) / 2.0,
                caret_size,
                caret_size,
            );
            painter.fill_rect(caret, style.text_color, Edges::default());

            // Deferred: see `paint_dropdown_popup` — floats on top of
            // everything once the whole tree has painted, rather than being
            // drawn (and clipped, and pushing layout) right here.
            if *open {
                popups.push(id);
            }
        }
        NodeKind::Container => {}
    }

    // Children paint on top. `z-index` reorders *paint* order only (higher
    // paints later, i.e. on top); it never changes layout, and ties keep
    // source order (stable sort).
    //
    // `position-absolute` children are split out and painted *after*
    // `pop_clip` — they intentionally escape this node's own clip (their
    // containing block, per `arrange_absolute` in the solver), so a badge
    // pinned outside its parent's box via negative offsets isn't cut off.
    // They're still subject to any ancestor's clip further up the call
    // stack (that push_clip is still active on the painter) — only this one
    // level of clipping is skipped, matching the "direct parent only"
    // containing-block simplification documented in CLAUDE.md.
    if !node.children.is_empty() {
        let mut in_flow = Vec::new();
        let mut absolute = Vec::new();
        for &c in &node.children {
            if ui.get(c).style.position == Position::Absolute {
                absolute.push(c);
            } else {
                in_flow.push(c);
            }
        }
        in_flow.sort_by_key(|&c| ui.get(c).style.z_index);
        absolute.sort_by_key(|&c| ui.get(c).style.z_index);

        painter.push_clip(rect);
        for child in in_flow {
            paint_node(ui, child, painter, popups);
        }
        painter.pop_clip();

        for child in absolute {
            paint_node(ui, child, painter, popups);
        }
    }

    if style.scroll_x || style.scroll_y {
        paint_scrollbars(painter, rect, style, node.content_size, node.scroll_offset);
    }

    if pushed_opacity {
        painter.pop_opacity();
    }
    if pushed_transform {
        painter.pop_transform();
    }
}

/// Draws an open `Dropdown`'s option list directly below its box, in screen
/// space — called after the whole tree has painted (see `paint`), so no
/// ancestor's clip or layout is in effect: it floats over everything and
/// doesn't push sibling content around.
fn paint_dropdown_popup(ui: &Ui, id: NodeId, painter: &mut dyn Painter) {
    let node = ui.get(id);
    let style = &node.style;
    let NodeKind::Dropdown { options, selected, .. } = &node.kind else { return };

    let (_, option_h) = crate::style::dropdown_metrics(style.font_size);
    let popup_rect = Rect::new(node.computed.x, node.computed.y + node.computed.h, node.computed.w, option_h * options.len() as f32);

    let bg = style.bg.unwrap_or(Color::WHITE);
    let border = style.border_color.unwrap_or(Color::rgb(209, 213, 219));
    painter.fill_rect(popup_rect, bg, style.radius);
    painter.stroke_rect(popup_rect, border, 1.0, style.radius);

    let text_style = TextStyle {
        color: style.text_color,
        size: style.font_size,
        align: TextAlign::Left,
        weight: style.font_weight,
        letter_spacing: style.letter_spacing,
    };

    let mut y = popup_rect.y;
    for (i, opt) in options.iter().enumerate() {
        let opt_rect = Rect::new(popup_rect.x, y, popup_rect.w, option_h);
        if Some(i) == *selected {
            painter.fill_rect(opt_rect, Color::rgb(243, 244, 246), Edges::default());
        }
        painter.draw_text(opt, opt_rect.inset(Edges::all(8.0)), &text_style);
        y += option_h;
    }
}

const SCROLLBAR_THICKNESS: f32 = 8.0;
const SCROLLBAR_TRACK_DEFAULT: Color = Color { r: 0, g: 0, b: 0, a: 18 };
const SCROLLBAR_THUMB_DEFAULT: Color = Color { r: 0, g: 0, b: 0, a: 110 };

/// Draw a thumb (and faint track) for each enabled scroll axis where the
/// content actually overflows `rect`. Purely visual — the offset that
/// positions the thumb is tracked and clamped by the runtime (mouse wheel
/// input), not the solver. Styled via `border-color` (the thumb; a faint
/// version of the same color tints the track) when set, falling back to a
/// neutral gray otherwise — no dedicated `scrollbar-*` classes were added.
fn paint_scrollbars(painter: &mut dyn Painter, rect: Rect, style: &crate::style::Style, content: Size, offset: Point) {
    let thumb_color = style.border_color.unwrap_or(SCROLLBAR_THUMB_DEFAULT);
    let track_color = style.border_color.map(|c| Color { a: 40, ..c }).unwrap_or(SCROLLBAR_TRACK_DEFAULT);

    if style.scroll_y && content.h > rect.h + 0.5 {
        let track = Rect::new(rect.x + rect.w - SCROLLBAR_THICKNESS, rect.y, SCROLLBAR_THICKNESS, rect.h);
        painter.fill_rect(track, track_color, Edges::default());
        let thumb_h = (rect.h * (rect.h / content.h)).clamp(20.0, rect.h);
        let max_offset = (content.h - rect.h).max(1.0);
        let thumb_y = rect.y + (offset.y.clamp(0.0, max_offset) / max_offset) * (rect.h - thumb_h);
        let thumb = Rect::new(track.x, thumb_y, SCROLLBAR_THICKNESS, thumb_h);
        painter.fill_rect(thumb, thumb_color, Edges::all(SCROLLBAR_THICKNESS / 2.0));
    }
    if style.scroll_x && content.w > rect.w + 0.5 {
        let track = Rect::new(rect.x, rect.y + rect.h - SCROLLBAR_THICKNESS, rect.w, SCROLLBAR_THICKNESS);
        painter.fill_rect(track, track_color, Edges::default());
        let thumb_w = (rect.w * (rect.w / content.w)).clamp(20.0, rect.w);
        let max_offset = (content.w - rect.w).max(1.0);
        let thumb_x = rect.x + (offset.x.clamp(0.0, max_offset) / max_offset) * (rect.w - thumb_w);
        let thumb = Rect::new(thumb_x, track.y, thumb_w, SCROLLBAR_THICKNESS);
        painter.fill_rect(thumb, thumb_color, Edges::all(SCROLLBAR_THICKNESS / 2.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::Node;
    use crate::style::Style;

    /// Records the fill color of every `fill_rect` call, in call order.
    #[derive(Default)]
    struct RecordingPainter(Vec<Color>);
    impl Painter for RecordingPainter {
        fn fill_rect(&mut self, _: Rect, color: Color, _: Edges) {
            self.0.push(color);
        }
        fn stroke_rect(&mut self, _: Rect, _: Color, _: f32, _: Edges) {}
        fn draw_text(&mut self, _: &str, _: Rect, _: &TextStyle) {}
        fn push_clip(&mut self, _: Rect) {}
        fn pop_clip(&mut self) {}
    }

    #[test]
    fn z_index_reorders_paint_but_not_source_order_ties() {
        let mut ui = Ui::new();
        let red = Color::rgb(255, 0, 0);
        let green = Color::rgb(0, 255, 0);
        let blue = Color::rgb(0, 0, 255);

        // Source order: red, green, blue. z-index: green=10 (paints last/on
        // top), red and blue tie at 0 (must keep source order between them).
        let r = ui.push(Node::new(NodeKind::Container, Style { bg: Some(red), ..Default::default() }));
        let g = ui.push(Node::new(NodeKind::Container, Style { bg: Some(green), z_index: 10, ..Default::default() }));
        let b = ui.push(Node::new(NodeKind::Container, Style { bg: Some(blue), ..Default::default() }));
        let root = ui.push(Node::new(NodeKind::Container, Style::default()));
        ui.get_mut(root).children = vec![r, g, b];
        ui.add_layer(root, "main");

        let mut painter = RecordingPainter::default();
        paint(&ui, &mut painter);

        assert_eq!(painter.0, vec![red, blue, green], "green (z=10) paints last despite being authored second");
    }

    #[derive(Debug, PartialEq)]
    enum Event {
        Push,
        Pop,
        Fill(Color),
    }

    /// Records clip pushes/pops interleaved with fills, so tests can check
    /// whether a given fill happened *inside* or *outside* a clip region.
    #[derive(Default)]
    struct TracingPainter(Vec<Event>);
    impl Painter for TracingPainter {
        fn fill_rect(&mut self, _: Rect, color: Color, _: Edges) {
            self.0.push(Event::Fill(color));
        }
        fn stroke_rect(&mut self, _: Rect, _: Color, _: f32, _: Edges) {}
        fn draw_text(&mut self, _: &str, _: Rect, _: &TextStyle) {}
        fn push_clip(&mut self, _: Rect) {
            self.0.push(Event::Push);
        }
        fn pop_clip(&mut self) {
            self.0.push(Event::Pop);
        }
    }

    #[test]
    fn absolute_child_paints_outside_parents_own_clip() {
        let mut ui = Ui::new();
        let red = Color::rgb(255, 0, 0);
        let green = Color::rgb(0, 255, 0);

        // A badge pinned outside its parent's box via a negative offset —
        // same shape as `login.nowui`'s "NEW" badge.
        let badge = ui.push(Node::new(
            NodeKind::Container,
            Style { position: Position::Absolute, bg: Some(red), ..Default::default() },
        ));
        ui.get_mut(badge).computed = Rect::new(-5.0, -5.0, 20.0, 20.0);

        let parent = ui.push(Node::new(NodeKind::Container, Style { bg: Some(green), ..Default::default() }));
        ui.get_mut(parent).children = vec![badge];
        ui.get_mut(parent).computed = Rect::new(0.0, 0.0, 100.0, 100.0);

        let root = ui.push(Node::new(NodeKind::Container, Style::default()));
        ui.get_mut(root).children = vec![parent];
        ui.get_mut(root).computed = Rect::new(0.0, 0.0, 200.0, 200.0);
        ui.add_layer(root, "main");

        let mut painter = TracingPainter::default();
        paint(&ui, &mut painter);

        // Find the parent's push/pop pair (the first one — root has no
        // children of its own besides `parent`, so root never pushes a clip
        // before `parent`'s fill happens).
        let push_idx = painter.0.iter().position(|e| *e == Event::Push).expect("parent pushes a clip");
        let pop_idx = painter.0.iter().position(|e| *e == Event::Pop).expect("parent pops its clip");
        let red_idx = painter.0.iter().position(|e| *e == Event::Fill(red)).expect("badge is painted");

        assert!(
            !(push_idx < red_idx && red_idx < pop_idx),
            "absolute child's fill must fall outside its parent's own push_clip/pop_clip pair, got trace {:?}",
            painter.0
        );
    }
}
