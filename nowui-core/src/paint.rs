//! Walk the solved arena and issue `Painter` calls, layer by layer,
//! back-to-front. This is the "retained tree, immediate paint" model: the tree
//! persists, but each redraw re-walks it rather than caching draw commands.

use crate::arena::{NodeId, NodeKind, Ui};
use crate::geometry::{Color, Edges, Point, Rect, Size};
use crate::painter::{Painter, TextStyle};
use crate::style::{Position, TextAlign};

pub fn paint(ui: &Ui, painter: &mut dyn Painter) {
    // `Ui::auto_scroll` (see its doc comment) shifts every layer's root
    // origin away from `(0, 0)` to keep an offscreen picker popup in view —
    // the root's own background fill moves right along with it, no longer
    // covering the whole physical window, so an "overflow" strip at
    // whichever edge got revealed would otherwise show raw clear-color
    // instead of the app's actual background. Painting the first layer's
    // root's own background across the *entire* viewport first, before
    // anything else, fixes that seam regardless of whether `auto_scroll` is
    // currently active — harmless (and skipped) if that root has no `bg` at
    // all.
    if let Some(first_root) = ui.layers.first().map(|l| l.root) {
        if let Some(bg) = ui.get(first_root).style.bg {
            painter.fill_rect(Rect::new(0.0, 0.0, ui.viewport.w, ui.viewport.h), bg, Edges::default());
        }
    }

    // Open `Dropdown`s/`Menu`s are collected here instead of drawn inline, so
    // their popup floats on top of *everything* (drawn after every layer,
    // once no ancestor clip is active) instead of being clipped by whatever
    // container it happens to sit in — see `paint_dropdown_popup`/
    // `paint_menu_popup`. `popups` mixes both kinds; each pop-up fn matches
    // its own `NodeKind` and no-ops (returns early) on the other's ids.
    let mut popups = Vec::new();
    for layer in &ui.layers {
        paint_node(ui, layer.root, painter, &mut popups);
    }
    for id in popups {
        paint_dropdown_popup(ui, id, painter);
        paint_menu_popup(ui, id, painter);
        paint_date_popup(ui, id, painter);
        paint_time_popup(ui, id, painter);
        paint_datetime_popup(ui, id, painter);
    }
    paint_page_scrollbars(ui, painter);
}

/// Browser-style page scrollbars: a thin translucent track + thumb along
/// the right/bottom edge of the window, shown only on whichever axis
/// `Ui::page_scroll_min`/`page_scroll_max` currently define a real range on
/// (i.e. only while a `Date`/`Time`/`DateTime` popup has panned the page to
/// stay in view — see `nowui-runtime`'s `App::update_auto_scroll`/
/// `MouseWheel` handler). Deliberately keyed off that *persisted* range, not
/// whether `Ui::auto_scroll`'s current value happens to be non-zero right
/// now — the latter collapses to nothing (hiding the scrollbar entirely)
/// the instant the user scrolls back to exactly `0`, which is just an
/// ordinary position *within* the range, not the end of it.
fn paint_page_scrollbars(ui: &Ui, painter: &mut dyn Painter) {
    const THICKNESS: f32 = 10.0;
    const MIN_THUMB: f32 = 24.0;
    const TRACK: Color = Color { r: 0, g: 0, b: 0, a: 24 };
    const THUMB: Color = Color { r: 0, g: 0, b: 0, a: 110 };

    let vp = ui.viewport;
    let scroll = ui.auto_scroll;

    if ui.page_scroll_min.y != ui.page_scroll_max.y {
        let doc_start = ui.page_scroll_min.y;
        let extent = vp.h + (ui.page_scroll_max.y - ui.page_scroll_min.y);
        let track = Rect::new(vp.w - THICKNESS, 0.0, THICKNESS, vp.h);
        painter.fill_rect(track, TRACK, Edges::default());
        let thumb_h = (vp.h * vp.h / extent).clamp(MIN_THUMB.min(vp.h), vp.h);
        let view_start = scroll.y - doc_start;
        let thumb_y = (vp.h * (view_start / extent)).clamp(0.0, vp.h - thumb_h);
        let thumb = Rect::new(vp.w - THICKNESS + 2.0, thumb_y, THICKNESS - 4.0, thumb_h);
        painter.fill_rect(thumb, THUMB, Edges::all((THICKNESS - 4.0) / 2.0));
    }

    if ui.page_scroll_min.x != ui.page_scroll_max.x {
        let doc_start = ui.page_scroll_min.x;
        let extent = vp.w + (ui.page_scroll_max.x - ui.page_scroll_min.x);
        let track = Rect::new(0.0, vp.h - THICKNESS, vp.w, THICKNESS);
        painter.fill_rect(track, TRACK, Edges::default());
        let thumb_w = (vp.w * vp.w / extent).clamp(MIN_THUMB.min(vp.w), vp.w);
        let view_start = scroll.x - doc_start;
        let thumb_x = (vp.w * (view_start / extent)).clamp(0.0, vp.w - thumb_w);
        let thumb = Rect::new(thumb_x, vp.h - THICKNESS + 2.0, thumb_w, THICKNESS - 4.0);
        painter.fill_rect(thumb, THUMB, Edges::all((THICKNESS - 4.0) / 2.0));
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
        NodeKind::TextInput { label, placeholder, masked, cursor, selection_anchor, ime_preview } => {
            paint_text_input(
                painter,
                content_rect,
                &text_style,
                style,
                label,
                placeholder,
                *masked,
                *cursor,
                *selection_anchor,
                ime_preview,
                ui.focus == Some(id),
                node.scroll_offset,
            );
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
        NodeKind::Menu { label, open, .. } => {
            painter.draw_text(label, content_rect, &text_style);
            // Deferred: see `paint_menu_popup` — floats on top of everything
            // once the whole tree has painted, same as `Dropdown`'s popup.
            // Only queued when there's actually something to show: a Menu
            // with no children must never produce a popup, open or not.
            if *open && !node.children.is_empty() {
                popups.push(id);
            }
        }
        NodeKind::MenuItem { label } => {
            painter.draw_text(label, content_rect, &text_style);
        }
        NodeKind::Date { value, placeholder, open, .. }
        | NodeKind::Time { value, placeholder, open, .. }
        | NodeKind::DateTime { value, placeholder, open, .. } => {
            paint_picker_box(painter, content_rect, style, value, placeholder);
            // Deferred: see `paint_date_popup`/`paint_time_popup`/
            // `paint_datetime_popup` — floats on top of everything once the
            // whole tree has painted, same as `Dropdown`'s popup.
            if *open {
                popups.push(id);
            }
        }
        NodeKind::Container => {}
    }

    // A `Menu`'s children never paint as normal in-flow children — open or
    // closed, they never got a real in-flow `computed` rect either (see
    // `layout::arrange`'s own comment); they only ever appear via the
    // floating `paint_menu_popup` above, using the rects `arrange_menu_
    // popups` gave them.
    let paint_children = !matches!(&node.kind, NodeKind::Menu { .. });

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
    if paint_children && !node.children.is_empty() {
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

/// Draws an open `Menu`'s popup: a background panel sized from the popup
/// rect `layout::arrange_menu_popups` computed and stashed in `Node::
/// content_size`, then each `MenuItem` (or whatever real widget the author
/// nested) painted via the ordinary `paint_node` recursion — unlike
/// `Dropdown`'s hand-drawn text rows, Menu's children are genuine arena
/// nodes with their own real `computed` rects from the popup arrange pass,
/// so they can be arbitrarily complex and just paint normally. Any popups
/// nested inside (e.g. a `Dropdown` or another `Menu` used as a `MenuItem`)
/// are drained into their own pass so they still float correctly.
fn paint_menu_popup(ui: &Ui, id: NodeId, painter: &mut dyn Painter) {
    let node = ui.get(id);
    let NodeKind::Menu { .. } = &node.kind else { return };

    let style = &node.style;
    let popup_size = node.content_size;
    let popup_rect = Rect::new(node.computed.x, node.computed.y + node.computed.h, popup_size.w, popup_size.h);

    if let Some(bg) = style.bg {
        painter.fill_rect(popup_rect, bg, style.radius);
    }
    if let Some(border) = style.border_color {
        painter.stroke_rect(popup_rect, border, 1.0, style.radius);
    }

    let mut nested_popups = Vec::new();
    for &child in &node.children {
        paint_node(ui, child, painter, &mut nested_popups);
    }
    for nested in nested_popups {
        paint_dropdown_popup(ui, nested, painter);
        paint_menu_popup(ui, nested, painter);
        paint_date_popup(ui, nested, painter);
        paint_time_popup(ui, nested, painter);
        paint_datetime_popup(ui, nested, painter);
    }
}

/// Draws a `Date`/`Time`/`DateTime`'s closed box: a bordered rect holding
/// `value` (or `placeholder` while empty) plus a small icon glyph — the same
/// shape as `Dropdown`'s own box (see `paint_node`'s `Dropdown` arm), just
/// without an options-list caret since there's no fixed option set here.
/// Draws a `Date`/`Time`/`DateTime`'s closed box: `value` (or `placeholder`
/// while empty) plus a small icon glyph — deliberately styled exactly like
/// `TextInput` (see `paint_text_input`): no border/bg of its own here, the
/// generic top-of-`paint_node` `bg`/`border-color`/`radius` block (already
/// run before this match arm) is the *only* thing drawing the box, so every
/// `p-*`/`h-*`/`bg-*`/`border-*`/`rounded-*` class works exactly as it would
/// on a `TextInput`.
fn paint_picker_box(painter: &mut dyn Painter, content_rect: Rect, style: &crate::style::Style, value: &str, placeholder: &str) {
    let label = if value.is_empty() { placeholder } else { value };
    let label_style = TextStyle {
        color: style.text_color,
        size: style.font_size,
        align: TextAlign::Left,
        weight: style.font_weight,
        letter_spacing: style.letter_spacing,
    };
    let mut label_rect = content_rect;
    label_rect.w -= 20.0; // leave room for the icon glyph
    painter.draw_text(label, vcenter_text(label_rect, style.font_size), &label_style);

    // Icon glyph: a small filled square (no path primitive for a real
    // calendar/clock icon — same crude-box convention as Dropdown's caret).
    let icon_size = (style.font_size * 0.4).max(4.0);
    let icon =
        Rect::new(content_rect.x + content_rect.w - icon_size, content_rect.y + (content_rect.h - icon_size) / 2.0, icon_size, icon_size);
    painter.fill_rect(icon, style.text_color, Edges::default());
}

// ---------------------------------------------------------------------
// Popup theme: fixed white/black/indigo regardless of the widget's own
// style (the closed box above still respects the widget's own styling —
// only the popup's own internals use this fixed palette, per the design).
// ---------------------------------------------------------------------

const POPUP_BG: Color = Color { r: 255, g: 255, b: 255, a: 255 };
const POPUP_TEXT: Color = Color { r: 20, g: 20, b: 20, a: 255 };
const POPUP_MUTED: Color = Color { r: 130, g: 130, b: 130, a: 255 };
const POPUP_BORDER: Color = Color { r: 224, g: 224, b: 228, a: 255 };
const INDIGO: Color = Color { r: 0x63, g: 0x66, b: 0xf1, a: 255 }; // indigo-500
const INDIGO_DARK: Color = Color { r: 0x4f, g: 0x46, b: 0xe5, a: 255 }; // indigo-600, pressed
const INDIGO_LIGHT: Color = Color { r: 0xe0, g: 0xe7, b: 0xff, a: 255 }; // indigo-100, hovered
const WHITE: Color = Color { r: 255, g: 255, b: 255, a: 255 };

/// `rect`'s current interaction state against the live cursor/mouse-down
/// flags `paint`'s hand-drawn popup controls key their hover/press
/// highlight off (see `Ui::cursor`/`Ui::mouse_down`'s doc comment) — these
/// controls are drawn directly, not real per-control arena nodes with their
/// own `hover:`/`active:` variants.
fn button_highlight(rect: Rect, ui: &Ui) -> Option<Color> {
    if !rect.contains(ui.cursor) {
        return None;
    }
    Some(if ui.mouse_down { INDIGO_DARK } else { INDIGO_LIGHT })
}

/// `Painter::draw_text` always anchors a single line to `bounds.y` (see
/// `nowui-render`'s `draw_text`) rather than centering it — using
/// `nowui-text`'s own `size * 1.3` line-height convention
/// (`shape_text`/`measure`'s `Metrics::new(size, size * 1.3)`). Every
/// hand-drawn popup control below draws one line of text into a rect
/// that's deliberately taller than that (a whole grid cell, a footer/tab
/// row, ...), so it must center that line manually or it visibly drifts
/// toward the rect's top — most obviously wherever a separate "true
/// center" shape (a selected day/hour bubble, the clock hand's tip) is
/// positioned from that same rect's *geometric* center and needs to
/// actually line up with the glyph drawn over it.
fn vcenter_text(rect: Rect, font_size: f32) -> Rect {
    let line_h = font_size * 1.3;
    Rect::new(rect.x, rect.y + (rect.h - line_h) / 2.0, rect.w, line_h)
}

fn draw_text_button(painter: &mut dyn Painter, rect: Rect, label: &str, font_size: f32, ui: &Ui, selected: bool) {
    if let Some(hi) = button_highlight(rect, ui) {
        painter.fill_rect(rect, hi, Edges::all((rect.h.min(rect.w) / 6.0).min(8.0)));
    }
    let color = if selected { INDIGO } else { POPUP_TEXT };
    let style = TextStyle { color, size: font_size, align: TextAlign::Center, weight: if selected { 600 } else { 400 }, letter_spacing: 0.0 };
    painter.draw_text(label, vcenter_text(rect, font_size), &style);
}

/// Draws the Cancel/Confirm footer row shared by every picker popup.
fn paint_footer(painter: &mut dyn Painter, cancel_rect: Rect, confirm_rect: Rect, font_size: f32, ui: &Ui) {
    let divider = Rect::new(cancel_rect.x + cancel_rect.w - 0.5, cancel_rect.y + 8.0, 1.0, cancel_rect.h - 16.0);
    painter.fill_rect(divider, POPUP_BORDER, Edges::default());
    draw_text_button(painter, cancel_rect, "CANCEL", font_size, ui, false);
    draw_text_button(painter, confirm_rect, "CONFIRM", font_size, ui, true);
}

const MONTH_NAMES: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const WEEKDAY_LABELS: [&str; 7] = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];

/// Draws a calendar popup: month stepper, year stepper/dropdown (or, while
/// open, the paged year-grid overlay in the same area), weekday labels, day
/// grid, and the Cancel/Confirm footer — shared by `paint_date_popup` and
/// `paint_datetime_popup`.
fn paint_calendar(painter: &mut dyn Painter, ui: &Ui, font_size: f32, layout: &crate::datetime::CalendarLayout, year: i32, month: u32, staged_day: u32) {
    painter.fill_rect(layout.popup_rect, POPUP_BG, Edges::all(8.0));
    painter.stroke_rect(layout.popup_rect, POPUP_BORDER, 1.0, Edges::all(8.0));

    draw_text_button(painter, layout.month_prev_rect, "<", font_size, ui, false);
    draw_text_button(painter, layout.month_next_rect, ">", font_size, ui, false);
    let month_style = TextStyle { color: POPUP_TEXT, size: font_size, align: TextAlign::Center, weight: 600, letter_spacing: 0.0 };
    painter.draw_text(MONTH_NAMES[(month - 1) as usize], vcenter_text(layout.month_label_rect, font_size), &month_style);

    if let Some(hi) = button_highlight(layout.year_dropdown_rect, ui) {
        painter.fill_rect(layout.year_dropdown_rect, hi, Edges::all(6.0));
    }
    painter.draw_text(&format!("{year} \u{25be}"), vcenter_text(layout.year_dropdown_rect, font_size), &month_style);
    draw_text_button(painter, layout.year_prev_rect, "<", font_size, ui, false);
    draw_text_button(painter, layout.year_next_rect, ">", font_size, ui, false);

    let muted = TextStyle { color: POPUP_MUTED, size: font_size * 0.85, align: TextAlign::Center, weight: 500, letter_spacing: 0.0 };
    let weekday_cell_w = layout.weekday_row.w / 7.0;
    for (i, label) in WEEKDAY_LABELS.iter().enumerate() {
        let rect = Rect::new(layout.weekday_row.x + i as f32 * weekday_cell_w, layout.weekday_row.y, weekday_cell_w, layout.weekday_row.h);
        painter.draw_text(label, vcenter_text(rect, font_size * 0.85), &muted);
    }

    if let Some(year_list) = &layout.year_list {
        draw_text_button(painter, year_list.prev_page_rect, "<", font_size, ui, false);
        draw_text_button(painter, year_list.next_page_rect, ">", font_size, ui, false);
        if let (Some((_, first)), Some((_, last))) = (year_list.year_cells.first(), year_list.year_cells.last()) {
            painter.draw_text(&format!("{first} \u{2013} {last}"), vcenter_text(year_list.range_label_rect, font_size), &month_style);
        }
        let cell_style = TextStyle { color: POPUP_TEXT, size: font_size, align: TextAlign::Center, weight: 400, letter_spacing: 0.0 };
        for (rect, y) in &year_list.year_cells {
            if *y == year {
                let d = rect.h.min(rect.w) * 0.7;
                let bubble = Rect::new(rect.x + (rect.w - d) / 2.0, rect.y + (rect.h - d) / 2.0, d, d);
                painter.fill_rect(bubble, INDIGO, Edges::all(d / 2.0));
                let sel = TextStyle { color: WHITE, ..cell_style };
                painter.draw_text(&y.to_string(), vcenter_text(*rect, font_size), &sel);
            } else {
                if let Some(hi) = button_highlight(*rect, ui) {
                    painter.fill_rect(*rect, hi, Edges::all(6.0));
                }
                painter.draw_text(&y.to_string(), vcenter_text(*rect, font_size), &cell_style);
            }
        }
        return;
    }

    let cell_style = TextStyle { color: POPUP_TEXT, size: font_size, align: TextAlign::Center, weight: 400, letter_spacing: 0.0 };
    for (rect, day) in &layout.day_cells {
        let Some(day) = day else { continue };
        if *day == staged_day {
            let d = rect.h.min(rect.w) * 0.72;
            let bubble = Rect::new(rect.x + (rect.w - d) / 2.0, rect.y + (rect.h - d) / 2.0, d, d);
            painter.fill_rect(bubble, INDIGO, Edges::all(d / 2.0));
            let sel = TextStyle { color: WHITE, ..cell_style };
            painter.draw_text(&day.to_string(), vcenter_text(*rect, font_size), &sel);
        } else {
            if let Some(hi) = button_highlight(*rect, ui) {
                let d = rect.h.min(rect.w) * 0.72;
                let bubble = Rect::new(rect.x + (rect.w - d) / 2.0, rect.y + (rect.h - d) / 2.0, d, d);
                painter.fill_rect(bubble, hi, Edges::all(d / 2.0));
            }
            painter.draw_text(&day.to_string(), vcenter_text(*rect, font_size), &cell_style);
        }
    }
    paint_footer(painter, layout.cancel_rect, layout.confirm_rect, font_size, ui);
}

/// Draws a draggable analog clock popup: hour/minute/[second] mode
/// segments, the dial face with a hand pointing at the active mode's
/// staged value, an AM/PM toggle, and the Cancel/Confirm footer — shared by
/// `paint_time_popup` and `paint_datetime_popup`.
fn paint_clock(painter: &mut dyn Painter, ui: &Ui, font_size: f32, layout: &crate::datetime::ClockLayout, picker: &crate::datetime::TimePickerState) {
    use crate::datetime::ClockMode;

    painter.fill_rect(layout.popup_rect, POPUP_BG, Edges::all(8.0));
    painter.stroke_rect(layout.popup_rect, POPUP_BORDER, 1.0, Edges::all(8.0));

    let (h12, is_pm) = crate::datetime::to_12_hour(picker.staged_hour);
    let segments: &[(Rect, ClockMode, String)] = &{
        let mut v = vec![
            (layout.hour_segment_rect, ClockMode::Hour, format!("{h12:02}")),
            (layout.minute_segment_rect, ClockMode::Minute, format!("{:02}", picker.staged_minute)),
        ];
        if let Some(r) = layout.second_segment_rect {
            v.push((r, ClockMode::Second, format!("{:02}", picker.staged_second)));
        }
        v
    };
    for (rect, mode, label) in segments {
        let inset = rect.inset(Edges::all(4.0));
        let active = picker.mode == *mode;
        if active {
            painter.fill_rect(inset, INDIGO_LIGHT, Edges::all(8.0));
        } else if let Some(hi) = button_highlight(inset, ui) {
            painter.fill_rect(inset, hi, Edges::all(8.0));
        }
        let style =
            TextStyle { color: if active { INDIGO } else { POPUP_TEXT }, size: font_size * 1.4, align: TextAlign::Center, weight: 600, letter_spacing: 0.0 };
        painter.draw_text(label, vcenter_text(*rect, font_size * 1.4), &style);
    }

    // Dial face.
    painter.fill_rect(layout.dial_area, Color { r: 244, g: 244, b: 247, a: 255 }, Edges::all(layout.dial_area.w / 2.0));

    let tick_style = TextStyle { color: POPUP_MUTED, size: font_size * 0.9, align: TextAlign::Center, weight: 500, letter_spacing: 0.0 };
    let label_r = 18.0_f32.max(font_size);
    match picker.mode {
        ClockMode::Hour => {
            for hour in 1..=12u32 {
                let p = crate::datetime::point_for_hour_12(layout.dial_center, layout.dial_radius, hour);
                let rect = Rect::new(p.x - label_r, p.y - label_r, label_r * 2.0, label_r * 2.0);
                let selected = hour == h12;
                if selected {
                    painter.fill_rect(rect.inset(Edges::all(2.0)), INDIGO, Edges::all(label_r));
                }
                let style = TextStyle { color: if selected { WHITE } else { POPUP_TEXT }, ..tick_style };
                painter.draw_text(&hour.to_string(), vcenter_text(rect, tick_style.size), &style);
            }
        }
        ClockMode::Minute | ClockMode::Second => {
            let active_val = if picker.mode == ClockMode::Minute { picker.staged_minute } else { picker.staged_second };
            // Highlight whichever 5-unit tick is nearest the actual (possibly
            // in-between-ticks) dragged value, wrapping 55 -> 0.
            let nearest_tick = (((active_val as f32 / 5.0).round() as u32) % 12) * 5;
            for tick in 0..12u32 {
                let minute_val = tick * 5;
                let p = crate::datetime::point_for_60_tick(layout.dial_center, layout.dial_radius, tick);
                let rect = Rect::new(p.x - label_r, p.y - label_r, label_r * 2.0, label_r * 2.0);
                let selected = minute_val == nearest_tick;
                if selected {
                    painter.fill_rect(rect.inset(Edges::all(2.0)), INDIGO, Edges::all(label_r));
                }
                let style = TextStyle { color: if selected { WHITE } else { POPUP_TEXT }, ..tick_style };
                painter.draw_text(&format!("{minute_val:02}"), vcenter_text(rect, tick_style.size), &style);
            }
        }
    }

    // Hand: a thin rect drawn pointing straight up from the center, then
    // rotated clockwise by the active mode's angle — see `hand_angle`.
    let hand_deg = crate::datetime::hand_angle(picker, picker.mode);
    let thickness = 3.0;
    let hand_rect = Rect::new(layout.dial_center.x - thickness / 2.0, layout.dial_center.y - layout.dial_radius, thickness, layout.dial_radius);
    painter.push_transform(crate::style::Transform2D { rotate_deg: hand_deg, ..Default::default() }, layout.dial_center);
    painter.fill_rect(hand_rect, INDIGO, Edges::all(thickness / 2.0));
    painter.pop_transform();
    let dot_d = 8.0;
    painter.fill_rect(
        Rect::new(layout.dial_center.x - dot_d / 2.0, layout.dial_center.y - dot_d / 2.0, dot_d, dot_d),
        INDIGO,
        Edges::all(dot_d / 2.0),
    );

    // AM/PM toggle: two halves, the active one filled.
    let half_w = layout.ampm_rect.w / 2.0;
    let am_rect = Rect::new(layout.ampm_rect.x, layout.ampm_rect.y, half_w, layout.ampm_rect.h);
    let pm_rect = Rect::new(layout.ampm_rect.x + half_w, layout.ampm_rect.y, half_w, layout.ampm_rect.h);
    for (rect, label, active) in [(am_rect, "AM", !is_pm), (pm_rect, "PM", is_pm)] {
        if active {
            painter.fill_rect(rect.inset(Edges::all(2.0)), INDIGO, Edges::all(rect.h / 2.0));
        } else if let Some(hi) = button_highlight(rect, ui) {
            painter.fill_rect(rect.inset(Edges::all(2.0)), hi, Edges::all(rect.h / 2.0));
        }
        let style = TextStyle { color: if active { WHITE } else { POPUP_TEXT }, size: font_size * 0.85, align: TextAlign::Center, weight: 600, letter_spacing: 0.0 };
        painter.draw_text(label, vcenter_text(rect, font_size * 0.85), &style);
    }

    paint_footer(painter, layout.cancel_rect, layout.confirm_rect, font_size, ui);
}

/// Draws an open `Date`'s calendar popup below/above its box — called after
/// the whole tree has painted (see `paint`), same floating convention as
/// `paint_dropdown_popup`.
fn paint_date_popup(ui: &Ui, id: NodeId, painter: &mut dyn Painter) {
    let node = ui.get(id);
    let font_size = node.style.font_size;
    let NodeKind::Date { picker, .. } = &node.kind else { return };
    let layout = crate::datetime::layout_calendar(
        node.computed,
        ui.viewport,
        font_size,
        picker.staged_year,
        picker.staged_month,
        picker.year_list_open,
        picker.year_list_page_start,
        picker.min_year,
        picker.max_year,
    );
    paint_calendar(painter, ui, font_size, &layout, picker.staged_year, picker.staged_month, picker.staged_day);
}

/// Draws an open `Time`'s dial popup below/above its box.
fn paint_time_popup(ui: &Ui, id: NodeId, painter: &mut dyn Painter) {
    let node = ui.get(id);
    let font_size = node.style.font_size;
    let with_seconds = node.style.with_seconds;
    let NodeKind::Time { picker, .. } = &node.kind else { return };
    let layout = crate::datetime::layout_clock(node.computed, ui.viewport, font_size, with_seconds);
    paint_clock(painter, ui, font_size, &layout, picker);
}

/// Draws an open `DateTime`'s combined popup (a Calendar/Clock tab row plus
/// whichever one sub-view is active) below/above its box.
fn paint_datetime_popup(ui: &Ui, id: NodeId, painter: &mut dyn Painter) {
    let node = ui.get(id);
    let font_size = node.style.font_size;
    let with_seconds = node.style.with_seconds;
    let NodeKind::DateTime { date_picker, time_picker, active_tab, .. } = &node.kind else { return };
    let layout = crate::datetime::layout_datetime(
        node.computed,
        ui.viewport,
        font_size,
        with_seconds,
        *active_tab,
        date_picker.staged_year,
        date_picker.staged_month,
        date_picker.year_list_open,
        date_picker.year_list_page_start,
        date_picker.min_year,
        date_picker.max_year,
    );

    painter.fill_rect(layout.popup_rect, POPUP_BG, Edges::all(8.0));
    painter.stroke_rect(layout.popup_rect, POPUP_BORDER, 1.0, Edges::all(8.0));
    draw_text_button(painter, layout.tab_calendar_rect, "CALENDAR", font_size, ui, *active_tab == crate::datetime::DateTimeTab::Calendar);
    draw_text_button(painter, layout.tab_clock_rect, "CLOCK", font_size, ui, *active_tab == crate::datetime::DateTimeTab::Clock);

    if let Some(cal) = &layout.calendar {
        paint_calendar(painter, ui, font_size, cal, date_picker.staged_year, date_picker.staged_month, date_picker.staged_day);
    }
    if let Some(clock) = &layout.clock {
        paint_clock(painter, ui, font_size, clock, time_picker);
    }
}

/// Width, in pixels, of the first `char_count` chars of `shown` — the shared
/// building block for placing the caret and the selection highlight, both of
/// which just need "how wide is the text up to here."
fn char_prefix_width(painter: &mut dyn Painter, shown: &str, char_count: usize, font_size: f32) -> f32 {
    if char_count == 0 {
        return 0.0;
    }
    let prefix: String = shown.chars().take(char_count).collect();
    painter.measure_text(&prefix, font_size).x
}

/// A `TextInput`'s full paint: placeholder-vs-value, selection highlight,
/// caret, and an IME composition underline — all keyed off the exact same
/// `text_input::display_string` the runtime's click hit-testing measures
/// against (see `App::char_index_for_click`), so what's drawn and what a
/// click lands on always agree.
///
/// `scroll` shifts the drawn text (and every position derived from it) so a
/// caret past the box's edge is still visible — `App::update_text_input_
/// scroll` (`nowui-runtime`) computes and persists it on `Node::scroll_
/// offset` each redraw (reused here for a TextInput's own internal text
/// view, unrelated to its normal use for `scroll-h`/`scroll-v` *containers*
/// — a `TextInput` has no children for that mechanism to apply to).
/// Everything is clipped to `content_rect` so the scrolled-out portion
/// doesn't bleed past the box.
///
/// `style.multiline` (`multi` bare flag) switches between two entirely
/// different layouts (see CLAUDE.md's `TextInput` section for the full
/// design and its disclosed limitation — caret/selection placement is
/// accurate for explicit `\n` line breaks, only approximate for word-wrap-
/// induced ones):
///   * single-line (default): horizontal scroll, no wrapping at all (the
///     `draw_text` bounds are sized to the text's full natural width so
///     cosmic-text never wraps it).
///   * multiline: vertical scroll, real wrapping at `content_rect.w` (both
///     explicit `\n` and word-wrap). Caret/selection Y position is derived
///     from a *hard-line* count (`text_input::line_and_col`, split on `\n`
///     only) — a hard line that itself wraps into multiple visual lines
///     renders correctly as text, but the caret/selection overlay doesn't
///     account for those extra wrapped visual lines.
///
/// Known simplification (see CLAUDE.md): only left-aligned text positions
/// the caret/selection/underline correctly — `text-right`/`text-center` on a
/// `TextInput` isn't accounted for here.
#[allow(clippy::too_many_arguments)]
fn paint_text_input(
    painter: &mut dyn Painter,
    content_rect: Rect,
    text_style: &TextStyle,
    style: &crate::style::Style,
    label: &str,
    placeholder: &str,
    masked: bool,
    cursor: usize,
    selection_anchor: Option<usize>,
    ime_preview: &str,
    focused: bool,
    scroll: Point,
) {
    let shown = crate::text_input::display_string(label, cursor, ime_preview, masked);

    if shown.is_empty() {
        painter.draw_text(placeholder, content_rect, text_style);
        return;
    }

    painter.push_clip(content_rect);

    if style.multiline {
        paint_multiline_text_input(painter, content_rect, text_style, style, &shown, cursor, selection_anchor, ime_preview, focused, scroll.y);
        painter.pop_clip();
        return;
    }

    // Every x position below is computed unscrolled (as if the text started
    // at `content_rect.x`), then shifted left by `scroll.x` right before use
    // — so a `scroll.x` that keeps the caret in view shifts the highlight/
    // caret/underline/text together, in lockstep.
    //
    // `text_rect.w` must reach at least the full natural width of `shown`,
    // not just `content_rect.w` (or `content_rect.w + scroll.x`): `draw_text`
    // passes `bounds.w` straight through as cosmic-text's word-wrap boundary,
    // measured from `bounds.x`. `scroll.x` only keeps the *caret* in view —
    // if the caret isn't at the very end (Left arrow, or a click mid-string),
    // there can still be text after it extending further right than
    // `content_rect.w + scroll.x`, which would then wrap onto a second,
    // clipped-away line instead of continuing on this one. Sizing off the
    // full measured width (not off `scroll.x`) rules that out regardless of
    // where the caret sits — single-line mode never wants wrapping at all,
    // only scroll+clip, so effectively-infinite width here is exactly right.
    let full_width = painter.measure_text(&shown, style.font_size).x;
    let text_rect = Rect { x: content_rect.x - scroll.x, w: full_width.max(content_rect.w) + 1.0, ..content_rect };

    // Caret sits after the whole in-progress composition, if any — there's
    // no inner preedit-cursor tracking (see the `ime_preview` field doc).
    let caret_char = cursor + crate::text_input::char_len(ime_preview);

    // A selection is only drawn while not mid-composition — composing while
    // a selection is active is a rare combination this doesn't model (the
    // composition would normally replace the selection on commit).
    if focused && ime_preview.is_empty() {
        if let Some(anchor) = selection_anchor {
            let (lo, hi) = crate::text_input::ordered_range(cursor, anchor);
            if lo != hi {
                let x0 = text_rect.x + char_prefix_width(painter, &shown, lo, style.font_size);
                let x1 = text_rect.x + char_prefix_width(painter, &shown, hi, style.font_size);
                // No dedicated `selection-*` class — reuses `text_color` at
                // low alpha, same convention as the scrollbar thumb/track.
                let highlight = Color { a: 60, ..style.text_color };
                painter.fill_rect(Rect::new(x0, content_rect.y, x1 - x0, content_rect.h), highlight, Edges::default());
            }
        }
    }

    painter.draw_text(&shown, text_rect, text_style);

    if !focused {
        painter.pop_clip();
        return;
    }

    if !ime_preview.is_empty() {
        let x0 = text_rect.x + char_prefix_width(painter, &shown, cursor, style.font_size);
        let w = char_prefix_width(painter, &shown, caret_char, style.font_size) - (x0 - text_rect.x);
        let underline_y = content_rect.y + content_rect.h - 2.0;
        painter.fill_rect(Rect::new(x0, underline_y, w.max(1.0), 1.5), style.text_color, Edges::default());
    }

    let caret_x = text_rect.x + char_prefix_width(painter, &shown, caret_char, style.font_size);
    painter.fill_rect(Rect::new(caret_x, content_rect.y, 1.5, content_rect.h), style.text_color, Edges::default());

    painter.pop_clip();
}

/// The `style.multiline` half of `paint_text_input` — see its doc comment
/// for the overall design and disclosed hard-line-only caret/selection
/// limitation. Draws `shown` as a *single* `draw_text` call at
/// `content_rect.w` (letting cosmic-text wrap it for real, both on `\n` and
/// on overflow), then overlays selection/caret/underline positioned by
/// hard-line count — never horizontally scrolled (wrapping replaces the
/// need for it), only vertically, via `scroll_y`.
#[allow(clippy::too_many_arguments)]
fn paint_multiline_text_input(
    painter: &mut dyn Painter,
    content_rect: Rect,
    text_style: &TextStyle,
    style: &crate::style::Style,
    shown: &str,
    cursor: usize,
    selection_anchor: Option<usize>,
    ime_preview: &str,
    focused: bool,
    scroll_y: f32,
) {
    let line_h = crate::text_input::line_height(style.font_size);
    let lines = crate::text_input::hard_lines(shown);
    let line_y = |line: usize| content_rect.y + line as f32 * line_h - scroll_y;
    let line_width = |painter: &mut dyn Painter, line: usize, chars: usize| -> f32 {
        let text: String = lines[line].chars().take(chars).collect();
        painter.measure_text(&text, style.font_size).x
    };

    let text_rect = Rect { y: content_rect.y - scroll_y, w: content_rect.w, ..content_rect };

    let caret_char = cursor + crate::text_input::char_len(ime_preview);

    if focused && ime_preview.is_empty() {
        if let Some(anchor) = selection_anchor {
            let (lo, hi) = crate::text_input::ordered_range(cursor, anchor);
            if lo != hi {
                let (lo_line, lo_col) = crate::text_input::line_and_col(shown, lo);
                let (hi_line, hi_col) = crate::text_input::line_and_col(shown, hi);
                let highlight = Color { a: 60, ..style.text_color };
                for line in lo_line..=hi_line {
                    let x0 = if line == lo_line { content_rect.x + line_width(painter, line, lo_col) } else { content_rect.x };
                    let x1 = if line == hi_line { content_rect.x + line_width(painter, line, hi_col) } else { content_rect.x + content_rect.w };
                    if x1 > x0 {
                        painter.fill_rect(Rect::new(x0, line_y(line), x1 - x0, line_h), highlight, Edges::default());
                    }
                }
            }
        }
    }

    painter.draw_text(shown, text_rect, text_style);

    if !focused {
        return;
    }

    let (caret_line, caret_col) = crate::text_input::line_and_col(shown, caret_char);
    let caret_x = content_rect.x + line_width(painter, caret_line, caret_col);

    if !ime_preview.is_empty() {
        let (start_line, start_col) = crate::text_input::line_and_col(shown, cursor);
        let x0 = content_rect.x + line_width(painter, start_line, start_col);
        let underline_y = line_y(caret_line) + line_h - 2.0;
        painter.fill_rect(Rect::new(x0, underline_y, (caret_x - x0).max(1.0), 1.5), style.text_color, Edges::default());
    }

    painter.fill_rect(Rect::new(caret_x, line_y(caret_line), 1.5, line_h), style.text_color, Edges::default());
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

    /// Records every `draw_text` call's string, in call order.
    #[derive(Default)]
    struct TextRecordingPainter(Vec<String>);
    impl Painter for TextRecordingPainter {
        fn fill_rect(&mut self, _: Rect, _: Color, _: Edges) {}
        fn stroke_rect(&mut self, _: Rect, _: Color, _: f32, _: Edges) {}
        fn draw_text(&mut self, text: &str, _: Rect, _: &TextStyle) {
            self.0.push(text.to_string());
        }
        fn push_clip(&mut self, _: Rect) {}
        fn pop_clip(&mut self) {}
    }

    /// Builds a `TextInput` with the given label/placeholder/masked and
    /// cursor at the end, no selection, no IME composition in progress —
    /// the common case tests start from before customizing further.
    fn text_input_kind(label: &str, placeholder: &str, masked: bool) -> NodeKind {
        NodeKind::TextInput {
            label: label.to_string(),
            placeholder: placeholder.to_string(),
            masked,
            cursor: label.chars().count(),
            selection_anchor: None,
            ime_preview: String::new(),
        }
    }

    #[test]
    fn text_input_shows_placeholder_only_while_label_is_empty() {
        // Regression: `paint_node`'s TextInput arm used to render the
        // placeholder unconditionally — `label` (the actual typed/bound
        // value) was destructured but never read, so nothing ever appeared
        // to update on screen even once the value did.
        let mut ui = Ui::new();
        let empty = ui.push(Node::new(text_input_kind("", "Enter Username", false), Style::default()));
        let filled = ui.push(Node::new(text_input_kind("dean", "Enter Username", false), Style::default()));
        let root = ui.push(Node::new(NodeKind::Container, Style::default()));
        ui.get_mut(root).children = vec![empty, filled];
        ui.add_layer(root, "main");

        let mut painter = TextRecordingPainter::default();
        paint(&ui, &mut painter);

        assert_eq!(painter.0, vec!["Enter Username".to_string(), "dean".to_string()]);
    }

    #[test]
    fn masked_text_input_shows_bullets_not_the_real_value() {
        let mut ui = Ui::new();
        let id = ui.push(Node::new(text_input_kind("hunter2", "", true), Style::default()));
        ui.add_layer(id, "main");

        let mut painter = TextRecordingPainter::default();
        paint(&ui, &mut painter);

        assert_eq!(painter.0, vec!["*******".to_string()]);
    }

    /// Records every `fill_rect` (rect + color), `draw_text` (string + the
    /// `bounds` it was drawn at), and `push_clip` call, in order — lets
    /// caret/selection/underline/scroll tests check both *that* they drew
    /// and *where*/*when* relative to the text itself.
    #[derive(Default)]
    struct FullRecordingPainter {
        fills: Vec<(Rect, Color)>,
        texts: Vec<String>,
        text_bounds: Vec<Rect>,
        clips: Vec<Rect>,
    }
    impl Painter for FullRecordingPainter {
        fn fill_rect(&mut self, rect: Rect, color: Color, _: Edges) {
            self.fills.push((rect, color));
        }
        fn stroke_rect(&mut self, _: Rect, _: Color, _: f32, _: Edges) {}
        fn draw_text(&mut self, text: &str, bounds: Rect, _: &TextStyle) {
            self.texts.push(text.to_string());
            self.text_bounds.push(bounds);
        }
        fn push_clip(&mut self, rect: Rect) {
            self.clips.push(rect);
        }
        fn pop_clip(&mut self) {}
    }

    #[test]
    fn unfocused_text_input_draws_no_caret_or_selection() {
        let mut ui = Ui::new();
        let mut kind = text_input_kind("hello", "", false);
        if let NodeKind::TextInput { selection_anchor, .. } = &mut kind {
            *selection_anchor = Some(0);
        }
        let id = ui.push(Node::new(kind, Style::default()));
        ui.add_layer(id, "main");
        // Not focused: `ui.focus` stays `None`.

        let mut painter = FullRecordingPainter::default();
        paint(&ui, &mut painter);

        assert_eq!(painter.texts, vec!["hello".to_string()]);
        assert!(painter.fills.is_empty(), "no caret/selection while unfocused, even with a selection set");
    }

    #[test]
    fn focused_text_input_draws_a_caret() {
        let mut ui = Ui::new();
        let id = ui.push(Node::new(text_input_kind("hello", "", false), Style::default()));
        ui.add_layer(id, "main");
        ui.focus = Some(id);

        let mut painter = FullRecordingPainter::default();
        paint(&ui, &mut painter);

        assert_eq!(painter.fills.len(), 1, "exactly one fill: the caret (no selection, no IME)");
    }

    #[test]
    fn focused_text_input_with_selection_draws_a_highlight_before_the_text_and_a_caret_after() {
        let mut ui = Ui::new();
        let mut kind = text_input_kind("hello", "", false);
        if let NodeKind::TextInput { cursor, selection_anchor, .. } = &mut kind {
            *cursor = 5;
            *selection_anchor = Some(1);
        }
        let id = ui.push(Node::new(kind, Style::default()));
        ui.add_layer(id, "main");
        ui.focus = Some(id);

        let mut painter = FullRecordingPainter::default();
        paint(&ui, &mut painter);

        assert_eq!(painter.fills.len(), 2, "selection highlight + caret");
        let (highlight_rect, _) = painter.fills[0];
        assert!(highlight_rect.w > 1.5, "highlight spans the 4 selected chars, wider than a 1.5px caret");
    }

    #[test]
    fn focused_text_input_composing_shows_preview_and_underline_not_selection() {
        let mut ui = Ui::new();
        let mut kind = text_input_kind("ab", "", false);
        if let NodeKind::TextInput { cursor, selection_anchor, ime_preview, .. } = &mut kind {
            *cursor = 1;
            *selection_anchor = Some(0); // active selection...
            *ime_preview = "X".to_string(); // ...but composing takes priority
        }
        let id = ui.push(Node::new(kind, Style::default()));
        ui.add_layer(id, "main");
        ui.focus = Some(id);

        let mut painter = FullRecordingPainter::default();
        paint(&ui, &mut painter);

        assert_eq!(painter.texts, vec!["aXb".to_string()], "preview spliced in at the cursor");
        assert_eq!(painter.fills.len(), 2, "underline + caret, no selection highlight while composing");
    }

    #[test]
    fn text_input_clips_to_its_content_rect() {
        let mut ui = Ui::new();
        let id = ui.push(Node::new(text_input_kind("hello", "", false), Style::default()));
        ui.get_mut(id).computed = Rect::new(5.0, 5.0, 100.0, 30.0);
        ui.add_layer(id, "main");

        let mut painter = FullRecordingPainter::default();
        paint(&ui, &mut painter);

        assert_eq!(painter.clips, vec![Rect::new(5.0, 5.0, 100.0, 30.0)]);
    }

    #[test]
    fn empty_text_input_showing_the_placeholder_does_not_clip() {
        // The early-return-to-placeholder path skips push_clip entirely —
        // nothing to scroll/clip when there's no value yet.
        let mut ui = Ui::new();
        let id = ui.push(Node::new(text_input_kind("", "Enter Username", false), Style::default()));
        ui.add_layer(id, "main");

        let mut painter = FullRecordingPainter::default();
        paint(&ui, &mut painter);

        assert!(painter.clips.is_empty());
    }

    #[test]
    fn scroll_x_shifts_the_drawn_text_and_caret_by_the_same_amount() {
        fn paint_at(scroll_x: f32) -> FullRecordingPainter {
            let mut ui = Ui::new();
            let id = ui.push(Node::new(text_input_kind("hello world", "", false), Style::default()));
            ui.get_mut(id).computed = Rect::new(0.0, 0.0, 200.0, 30.0);
            ui.get_mut(id).scroll_offset.x = scroll_x;
            ui.add_layer(id, "main");
            ui.focus = Some(id);
            let mut painter = FullRecordingPainter::default();
            paint(&ui, &mut painter);
            painter
        }

        let unscrolled = paint_at(0.0);
        let scrolled = paint_at(15.0);

        assert_eq!(unscrolled.text_bounds[0].x - scrolled.text_bounds[0].x, 15.0, "text shifts left by scroll_x");
        let unscrolled_caret = unscrolled.fills.last().unwrap().0.x;
        let scrolled_caret = scrolled.fills.last().unwrap().0.x;
        assert_eq!(unscrolled_caret - scrolled_caret, 15.0, "caret shifts by the same amount as the text");
    }

    #[test]
    fn draw_text_bounds_always_reach_the_full_natural_text_width() {
        // Regression: `draw_text`'s `bounds.w` is cosmic-text's word-wrap
        // boundary, measured from `bounds.x`. Sizing it off `content_rect.w`
        // (or even `content_rect.w + scroll_x`, an earlier, incomplete fix)
        // could still fall short of the text's true width whenever the caret
        // isn't at the very end — e.g. after Left/Home or a mid-string click
        // — since `scroll_x` only guarantees *the caret* is in view, not
        // whatever text comes after it. Sizing off the actual measured width
        // instead rules that out regardless of scroll/caret position: a
        // `TextInput` never wants wrapping at all, only scroll+clip.
        let mut ui = Ui::new();
        let mut kind = text_input_kind("hello world", "", false);
        if let NodeKind::TextInput { cursor, .. } = &mut kind {
            *cursor = 3; // nowhere near the true end — the scenario this test guards
        }
        let id = ui.push(Node::new(kind, Style::default()));
        // A box far narrower than "hello world"'s natural width, so this
        // only means something if the box width isn't what determines
        // `bounds.w` here.
        ui.get_mut(id).computed = Rect::new(0.0, 0.0, 50.0, 30.0);
        ui.get_mut(id).scroll_offset.x = 15.0;
        ui.add_layer(id, "main");
        ui.focus = Some(id);

        let mut painter = FullRecordingPainter::default();
        let natural_width = painter.measure_text("hello world", 16.0).x;
        paint(&ui, &mut painter);

        assert!(
            painter.text_bounds[0].w > natural_width,
            "wrap boundary ({}) must clear the text's full natural width ({natural_width}), not just the box or box+scroll_x",
            painter.text_bounds[0].w
        );
    }

    #[test]
    fn multiline_text_input_draws_the_whole_value_in_one_call_wrapped_at_the_box_width() {
        let mut ui = Ui::new();
        let style = Style { multiline: true, ..Style::default() };
        let id = ui.push(Node::new(text_input_kind("line one\nline two", "", false), style));
        ui.get_mut(id).computed = Rect::new(0.0, 0.0, 200.0, 60.0);
        ui.add_layer(id, "main");

        let mut painter = FullRecordingPainter::default();
        paint(&ui, &mut painter);

        assert_eq!(painter.texts, vec!["line one\nline two".to_string()]);
        assert_eq!(painter.text_bounds[0].w, 200.0, "wraps at the box width, not the full text width");
    }

    #[test]
    fn multiline_text_input_places_the_caret_on_its_hard_line() {
        let mut ui = Ui::new();
        let style = Style { multiline: true, ..Style::default() };
        let mut kind = text_input_kind("ab\ncd", "", false);
        if let NodeKind::TextInput { cursor, .. } = &mut kind {
            *cursor = 4; // char 4 is 'd' -> line 1 ("cd"), col 1
        }
        let id = ui.push(Node::new(kind, style));
        ui.get_mut(id).computed = Rect::new(0.0, 0.0, 200.0, 60.0);
        ui.add_layer(id, "main");
        ui.focus = Some(id);

        let mut painter = FullRecordingPainter::default();
        paint(&ui, &mut painter);

        let line_h = nowui_core_line_height();
        let caret = painter.fills.last().unwrap().0;
        assert_eq!(caret.y, line_h, "caret sits on the second hard line, one line_height down");
        assert!(caret.x > 0.0, "col 1 into \"cd\" is past the box's left edge");
    }

    #[test]
    fn multiline_text_input_selection_spanning_lines_draws_one_highlight_per_line() {
        let mut ui = Ui::new();
        let style = Style { multiline: true, ..Style::default() };
        let mut kind = text_input_kind("aaa\nbbb\nccc", "", false);
        if let NodeKind::TextInput { cursor, selection_anchor, .. } = &mut kind {
            *cursor = 9; // into the third line
            *selection_anchor = Some(1); // from the first line
        }
        let id = ui.push(Node::new(kind, style));
        ui.get_mut(id).computed = Rect::new(0.0, 0.0, 200.0, 90.0);
        ui.add_layer(id, "main");
        ui.focus = Some(id);

        let mut painter = FullRecordingPainter::default();
        paint(&ui, &mut painter);

        // 3 highlight rects (one per spanned line) + 1 caret.
        assert_eq!(painter.fills.len(), 4);
    }

    fn nowui_core_line_height() -> f32 {
        crate::text_input::line_height(Style::default().font_size)
    }

    #[test]
    fn closed_menu_paints_its_header_but_not_its_items() {
        let mut ui = Ui::new();
        let item = ui.push(Node::new(NodeKind::MenuItem { label: "Open Preferences".to_string() }, Style::default()));
        let menu = ui.push(Node::new(NodeKind::Menu { label: "Preferences".to_string(), open: false }, Style::default()));
        ui.get_mut(menu).children = vec![item];
        ui.add_layer(menu, "main");

        let mut painter = FullRecordingPainter::default();
        paint(&ui, &mut painter);

        assert_eq!(painter.texts, vec!["Preferences".to_string()], "header paints, item does not");
    }

    #[test]
    fn open_menu_paints_its_header_and_its_items() {
        let mut ui = Ui::new();
        let item = ui.push(Node::new(NodeKind::MenuItem { label: "Open Preferences".to_string() }, Style::default()));
        let menu = ui.push(Node::new(NodeKind::Menu { label: "Preferences".to_string(), open: true }, Style::default()));
        ui.get_mut(menu).children = vec![item];
        ui.add_layer(menu, "main");

        let mut painter = FullRecordingPainter::default();
        paint(&ui, &mut painter);

        assert_eq!(painter.texts, vec!["Preferences".to_string(), "Open Preferences".to_string()]);
    }

    #[test]
    fn open_menu_with_no_children_paints_only_its_header() {
        let mut ui = Ui::new();
        let menu = ui.push(Node::new(NodeKind::Menu { label: "Preferences".to_string(), open: true }, Style::default()));
        ui.add_layer(menu, "main");

        let mut painter = FullRecordingPainter::default();
        paint(&ui, &mut painter);

        assert_eq!(painter.texts, vec!["Preferences".to_string()], "no children means no popup, even while open");
    }

    #[test]
    fn open_menu_popup_draws_a_background_panel_from_the_menus_own_style() {
        let mut ui = Ui::new();
        let panel_color = Color::rgb(30, 30, 30);
        let item = ui.push(Node::new(NodeKind::MenuItem { label: "Open Preferences".to_string() }, Style::default()));
        let menu = ui.push(Node::new(
            NodeKind::Menu { label: "Preferences".to_string(), open: true },
            Style { bg: Some(panel_color), ..Default::default() },
        ));
        ui.get_mut(menu).children = vec![item];
        ui.get_mut(menu).content_size = Size::new(100.0, 40.0);
        ui.add_layer(menu, "main");

        let mut painter = FullRecordingPainter::default();
        paint(&ui, &mut painter);

        assert!(painter.fills.iter().any(|&(_, color)| color == panel_color), "popup panel painted with Menu's own bg");
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

    #[test]
    fn root_background_prefills_the_whole_viewport_even_when_auto_scroll_shifts_its_origin() {
        let mut ui = Ui::new();
        let bg = Color::rgb(240, 240, 240);
        let root = ui.push(Node::new(NodeKind::Container, Style { bg: Some(bg), ..Default::default() }));
        // Simulate `Ui::auto_scroll` having shifted the root's own computed
        // rect away from `(0, 0)` — its own fill no longer covers the whole
        // window, which is exactly the seam this pre-fill exists to hide.
        ui.get_mut(root).computed = Rect::new(0.0, -50.0, 800.0, 600.0);
        ui.add_layer(root, "main");
        ui.viewport = Size::new(800.0, 600.0);

        let mut painter = FullRecordingPainter::default();
        paint(&ui, &mut painter);

        let (first_rect, first_color) = painter.fills[0];
        assert_eq!(first_color, bg);
        assert_eq!(first_rect, Rect::new(0.0, 0.0, 800.0, 600.0), "prefilled across the entire physical window, not just the shifted root rect");
    }
}
