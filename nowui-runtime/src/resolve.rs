//! The "read half" of reactivity: re-render every node's `{value:
//! state.path}`/`` `${state.path}` ``/`key-[${state.path}]` bindings against
//! *live* state every redraw. Extracted out of `App<S>`'s own (private)
//! methods into free functions so `nowui-designer` can reuse the exact same
//! resolution logic in its own harness (built directly on `nowui_core`
//! rather than `App`/`run_path` — see its `preview.rs`/`app.rs` module docs
//! for why) without duplicating it. `App`'s own `resolve_values`/
//! `resolve_templates`/`resolve_dynamic_styles` are now thin wrappers around
//! these.

use std::collections::HashMap;

use nowui_core::{display_string, NodeId, NodeKind, NowUiState, Style, TemplatePart, Ui};

/// `.nowui` binding paths are rooted at the literal `state` segment (see
/// `nowui-syntax`'s dotted-path grammar), but `NowUiState` impls are rooted
/// at their own struct's fields, so that leading segment is stripped before
/// crossing the reflection boundary.
pub(crate) fn state_subpath(path: &[String]) -> Vec<&str> {
    let skip = usize::from(path.first().is_some_and(|s| s == "state"));
    path.iter().skip(skip).map(String::as_str).collect()
}

/// Resolve every node's `disabled_path` (`{disabled: state.path}`) against
/// `state` into `Node::disabled` — must run before `App::apply_dynamic_
/// styles`, which reads `Node::disabled` to decide whether to apply the
/// `disabled:` style variant this frame. A path that doesn't currently
/// resolve to a `bool` (wrong type, unknown field, `NoState`, unbound) is
/// treated as `false` — same "never disabled unless the binding clearly
/// says so" default a missing/unbound `disabled_path` already has.
pub fn resolve_disabled(ui: &mut Ui, state: &dyn NowUiState) {
    for i in 0..ui.nodes.len() {
        let id = NodeId(i as u32);
        // Checked by reference first — a `Vec<String>` clone (even of an
        // empty one) on every node in the arena, every frame, just to find
        // out it's unbound is real, wasted cost for the overwhelmingly
        // common case of a node with no `{disabled: ...}` binding at all.
        if ui.get(id).disabled_path.is_empty() {
            continue;
        }
        let path = ui.get(id).disabled_path.clone();
        let disabled = state.get(&state_subpath(&path)).and_then(|v| v.as_bool()).unwrap_or(false);
        ui.get_mut(id).disabled = disabled;
    }
}

/// Resolve every node's `value_path` (`{value: state.path}`) against `state`
/// and write it into whichever `NodeKind` field a widget reads its bound
/// value from. `dragging_slider` — a `Slider` currently being dragged is the
/// source of truth for its own value this frame, so a stale read doesn't
/// fight the live gesture; pass `None` from a caller with no drag concept of
/// its own (nowui-designer's preview/chrome, which is read-only).
pub fn resolve_values(ui: &mut Ui, state: &dyn NowUiState, dragging_slider: Option<NodeId>) {
    for i in 0..ui.nodes.len() {
        let id = NodeId(i as u32);
        // See `resolve_disabled`'s own comment on checking by reference
        // before cloning — same reasoning here.
        if ui.get(id).value_path.is_empty() {
            continue;
        }
        let path = ui.get(id).value_path.clone();
        let sub = state_subpath(&path);
        let Some(value) = state.get(&sub) else { continue };
        let dragging = dragging_slider == Some(id);

        let node = ui.get_mut(id);
        match &mut node.kind {
            NodeKind::Text { content } => *content = display_string(&value),
            NodeKind::Checkbox { checked, .. } => {
                if let Some(b) = value.as_bool() {
                    *checked = b;
                }
            }
            NodeKind::Dropdown { option_ids, options, selected, .. } => {
                if let Some(s) = value.as_str() {
                    // Matches `option_ids` first (a `values`-bound item's
                    // real identity), falling back to `options` (labels) —
                    // for a legacy plain-string option, `option_ids[i] ==
                    // options[i]` anyway, so this stays exactly as
                    // backward-compatible as it was before `option_ids`
                    // existed.
                    *selected = option_ids.iter().position(|o| o == s).or_else(|| options.iter().position(|o| o == s));
                }
            }
            NodeKind::TextInput { label, .. } => {
                if let Some(s) = value.as_str() {
                    *label = s.to_string();
                }
            }
            NodeKind::Date { value: v, .. } | NodeKind::Time { value: v, .. } | NodeKind::DateTime { value: v, .. } => {
                if let Some(s) = value.as_str() {
                    *v = s.to_string();
                }
            }
            NodeKind::Slider { value: v } if !dragging => {
                if let Some(n) = value.as_f64() {
                    *v = (n as f32 / 100.0).clamp(0.0, 1.0);
                }
            }
            NodeKind::ProgressBar { value: v } => {
                if let Some(n) = value.as_f64() {
                    *v = (n as f32 / 100.0).clamp(0.0, 1.0);
                }
            }
            _ => {}
        }
    }
}

/// Rebuilds a `Dropdown`'s rendered options every redraw from a live
/// `{values: state.path}` binding — a `Vec<DropdownItem>`-shaped
/// `StateValue::List` (each item a `StateValue::Object`, i.e. a
/// `#[derive(NowUiState)]` struct with `label`/`id` string fields — see
/// `NodeKind::Dropdown`'s own doc comment). A no-op for a `Dropdown` with no
/// `values` binding (`values_path` empty), or if the bound path doesn't
/// currently resolve to a `List` (wrong type, `NoState`, ...).
///
/// Static `DropdownItem` children (`static_items`) always render first,
/// the resolved dynamic items follow. Selection is preserved *by id*
/// across a rebuild — if the previously-selected id no longer exists in
/// the new list, falls back to whichever static item declared
/// `default-selected`, else clears to `None` (shows the placeholder).
pub fn resolve_dropdown_values(ui: &mut Ui, state: &dyn NowUiState) {
    for i in 0..ui.nodes.len() {
        let id = NodeId(i as u32);
        // See `resolve_disabled`'s own comment on checking by reference
        // before cloning — same reasoning here.
        if ui.get(id).values_path.is_empty() {
            continue;
        }
        let values_path = ui.get(id).values_path.clone();
        let Some(value) = state.get(&state_subpath(&values_path)) else { continue };
        let Some(items) = value.as_list() else { continue };

        // A `values`-bound item can't opt into `disabled` explicitly (the
        // plain `DropdownItem` struct it comes from has no such field) —
        // only the blank-id rule applies, same as a static item's own
        // *implicit* disabling (see `NodeKind::Dropdown`'s own doc comment).
        let dynamic_items: Vec<(String, String, bool)> = items
            .iter()
            .filter_map(|item| {
                let item_id = item.get_field("id")?.as_str()?.to_string();
                let label = item.get_field("label")?.as_str()?.to_string();
                let disabled = item_id.is_empty();
                Some((item_id, label, disabled))
            })
            .collect();

        let NodeKind::Dropdown { options, option_ids, option_disabled, static_items, default_selected_id, selected, .. } =
            &mut ui.get_mut(id).kind
        else {
            continue;
        };

        let prev_selected_id = selected.and_then(|i| option_ids.get(i)).cloned();

        options.clear();
        option_ids.clear();
        option_disabled.clear();
        for (item_id, label, disabled) in static_items.iter() {
            options.push(label.clone());
            option_ids.push(item_id.clone());
            option_disabled.push(*disabled);
        }
        for (item_id, label, disabled) in &dynamic_items {
            options.push(label.clone());
            option_ids.push(item_id.clone());
            option_disabled.push(*disabled);
        }

        *selected = prev_selected_id
            .and_then(|pid| option_ids.iter().position(|i| *i == pid))
            .or_else(|| default_selected_id.as_ref().and_then(|did| option_ids.iter().position(|i| i == did)));
    }
}

/// Re-render every node's `templates` (backticks containing `${state.path}`
/// interpolation, e.g. `` `Count: ${state.counter.count}` ``) against live
/// state and write the result into the widget field(s) that backtick
/// originally built — the same read-half-of-reactivity idea as
/// `resolve_values`, just for inline text instead of a `{value: ...}`
/// binding. A node with no dynamic backticks has empty `templates` and is
/// skipped entirely.
///
/// `template_exprs` (`Semantic::template_exprs`) takes priority when a node
/// has an entry there — a backtick containing something richer than a bare
/// path (currently only a ternary, e.g. `` `${state.isSaving == true ?
/// "Saving..." : "Save"}` ``) can't be represented in `nowui-core`'s own
/// `TemplatePart` (see that field's own doc comment for why), so it's
/// rendered from the original, un-lowered `nowui_syntax` AST instead, via
/// `dynamic::eval_expr` — the exact same evaluator `if`/`for` conditions
/// already use. Pass an empty map from a caller with no such side table of
/// its own (e.g. a `nowui-core`-only test `Ui`).
pub fn resolve_templates(ui: &mut Ui, state: &dyn NowUiState, template_exprs: &HashMap<NodeId, Vec<nowui_syntax::ast::Template>>) {
    for i in 0..ui.nodes.len() {
        let id = NodeId(i as u32);
        if let Some(raw) = template_exprs.get(&id) {
            let rendered: Vec<String> = raw.iter().map(|t| render_syntax_template(state, t)).collect();
            apply_resolved_templates(&mut ui.get_mut(id).kind, &rendered);
            continue;
        }
        // See `resolve_disabled`'s own comment on checking by reference
        // before cloning — same reasoning here.
        if ui.get(id).templates.is_empty() {
            continue;
        }
        let templates = ui.get(id).templates.clone();
        let rendered: Vec<String> = templates.iter().map(|t| render_template(state, t)).collect();
        apply_resolved_templates(&mut ui.get_mut(id).kind, &rendered);
    }
}

/// Concatenate one backtick's literal/`${state.path}` parts into the string
/// it should currently display.
fn render_template(state: &dyn NowUiState, parts: &[TemplatePart]) -> String {
    let mut out = String::new();
    for part in parts {
        match part {
            TemplatePart::Lit(s) => out.push_str(s),
            TemplatePart::Var(path) => {
                if let Some(v) = state.get(&state_subpath(path)) {
                    out.push_str(&display_string(&v));
                }
            }
        }
    }
    out
}

/// Same idea as `render_template`, for a raw `nowui_syntax::ast::Template`
/// (`Semantic::template_exprs`' own entries) instead of the lowered
/// `nowui_core` form — a `Var` still renders as a plain state lookup, but
/// an `Expr` part (a ternary) goes through `dynamic::eval_expr`, which also
/// means it picks up the same `.length` pseudo-property support `if`/`for`
/// conditions already have, for free.
fn render_syntax_template(state: &dyn NowUiState, t: &nowui_syntax::ast::Template) -> String {
    let mut resolve = |segs: &[String]| state.get(&state_subpath(segs));
    let mut out = String::new();
    for part in &t.parts {
        match part {
            nowui_syntax::ast::TplPart::Lit(s) => out.push_str(s),
            nowui_syntax::ast::TplPart::Var(v) => {
                let path: Vec<String> = v.split('.').map(str::to_string).collect();
                if let Some(val) = crate::dynamic::eval_expr(&nowui_syntax::ast::Expr::Path(path), &mut resolve) {
                    out.push_str(&display_string(&val));
                }
            }
            nowui_syntax::ast::TplPart::Expr(e) => {
                if let Some(val) = crate::dynamic::eval_expr(e, &mut resolve) {
                    out.push_str(&display_string(&val));
                }
            }
        }
    }
    out
}

/// Resolve every node's `Style::dynamic` entries (a `key-[${state.path}]`
/// bracket value, e.g. `w-[${state.myWidth}]`) against live state and
/// re-apply them onto `base_style` — the same read-half-of-reactivity idea
/// as `resolve_values`/`resolve_templates`, but for style values instead of
/// widget content. A caller should run this before applying hover/focus/
/// responsive variants and transitions, so those compute from the resolved
/// value, not the stale default `apply_style` left in place at parse time.
/// Written into `base_style` (not the transient, recomputed-every-frame
/// `style`) since that's the field variant/transition application treats as
/// ground truth.
///
/// Reuses `semantic::apply_exact`/`apply_prefixed` — the exact same
/// key-dispatch `resolve_styles` uses for the static (parse-time) case — so
/// a dynamic value is interpreted identically to a literal one; keep this in
/// sync if that dispatch ever changes.
/// Advance every `NodeKind::Image`'s animated-GIF playback position by one
/// frame tick worth of wall-clock time. Called once per redraw (a fixed
/// 60fps loop — see `CLAUDE.md`'s "Runtime gotchas"), so `dt_ms` is always
/// `FRAME_INTERVAL` in practice; taken as a parameter rather than hardcoded
/// so `nowui-designer`'s own harness (which doesn't necessarily redraw at
/// exactly 60fps) can pass its own real elapsed time instead.
///
/// A single-frame image (`frames.len() <= 1`, true for every non-GIF format
/// and for a non-animated GIF) is skipped outright — no per-frame cost for
/// the overwhelmingly common case. An animated GIF holds on its last frame
/// once played through, unless `Style::loop_playback` (the bare `loop` style
/// flag) is set, in which case it wraps back to frame 0.
pub fn advance_image_animations(ui: &mut Ui, dt_ms: f32) {
    for i in 0..ui.nodes.len() {
        let id = NodeId(i as u32);
        let loop_playback = ui.get(id).base_style.loop_playback;
        let NodeKind::Image { decoded: Some(img), current_frame, frame_elapsed_ms, .. } = &mut ui.get_mut(id).kind
        else {
            continue;
        };
        if img.frames.len() <= 1 {
            continue;
        }
        *frame_elapsed_ms += dt_ms;
        while let Some(delay) = img.frames.get(*current_frame).map(|f| f.delay_ms as f32) {
            // A 0ms delay (some encoders emit this) would spin forever —
            // treat it as "advance immediately, once" instead of looping.
            let delay = delay.max(1.0);
            if *frame_elapsed_ms < delay {
                break;
            }
            *frame_elapsed_ms -= delay;
            if *current_frame + 1 < img.frames.len() {
                *current_frame += 1;
            } else if loop_playback {
                *current_frame = 0;
            } else {
                *frame_elapsed_ms = 0.0;
                break;
            }
        }
    }
}

/// Re-resolves every `NodeKind::Icon`'s rasterized frame from its node's
/// current *effective* `line_color`/`text_color` — called once per redraw,
/// **after** `App::apply_dynamic_styles` has written `node.style` for this
/// frame (hover/focus/active variants and `${state.path}` dynamic values
/// already folded in — see `resolve_dynamic_styles` for the latter), so a
/// `hover:fill-color-[...]`/`hover:fill-color-[${state.path}]` style
/// actually changes what's painted while hovered, unlike `Image`'s own
/// decode-once-at-build-time source.
///
/// Looks wasteful (re-rasterizing every `Icon` every redraw) but isn't:
/// `nowui_icons::icon_frame` caches by `(name, color)` for the process's
/// lifetime, so an unhovered icon's own call here is just a cache hit plus
/// one small `Vec` clone, not a re-rasterization.
pub fn resolve_icon_colors(ui: &mut Ui) {
    for i in 0..ui.nodes.len() {
        let id = NodeId(i as u32);
        let node = ui.get(id);
        let NodeKind::Icon { name, .. } = &node.kind else { continue };
        let color = node.style.line_color.unwrap_or(node.style.text_color);
        let name = name.clone();
        let frame = nowui_icons::icon_frame(&name, [color.r, color.g, color.b, color.a]).ok();
        // A lookup failure here (shouldn't happen for a name that resolved
        // fine at build time) leaves the existing `decoded`/`error` alone
        // rather than clobbering a previously-successful frame with `None`.
        if let (NodeKind::Icon { decoded, error, .. }, Some(frame)) = (&mut ui.get_mut(id).kind, frame) {
            *decoded = Some(frame);
            *error = None;
        }
    }
}

/// Re-resolves every `TreeViewItem`'s own `Style::tree_icon` (if non-empty)
/// into `NodeKind::TreeViewItem::icon` — same "this crate can't call
/// nowui-icons, cache-friendly so calling it every redraw is cheap" shape as
/// `resolve_icon_colors` above, just for the `TreeView` widget's own
/// opt-in per-row icon instead of the standalone `Icon` widget. Tinted with
/// the row's own effective `text_color` (a `TreeViewItem` has no dedicated
/// `line_color`-style override of its own).
pub fn resolve_tree_icons(ui: &mut Ui) {
    for i in 0..ui.nodes.len() {
        let id = NodeId(i as u32);
        let node = ui.get(id);
        let NodeKind::TreeViewItem { .. } = &node.kind else { continue };
        let name = node.style.tree_icon.clone();
        if name.is_empty() {
            continue;
        }
        let color = node.style.text_color;
        let frame = nowui_icons::icon_frame(&name, [color.r, color.g, color.b, color.a]).ok();
        if let (NodeKind::TreeViewItem { icon, .. }, Some(frame)) = (&mut ui.get_mut(id).kind, frame) {
            *icon = Some(frame);
        }
    }
}

/// Applies every `${state.path}` bracket in `dynamic` onto `style` — shared
/// by the base style and each present hover/focus/active variant below,
/// since a `key-[${state.path}]` bracket can appear inside any of them.
fn resolve_dynamic_map(style: &mut Style, dynamic: &std::collections::HashMap<String, Vec<String>>, state: &dyn NowUiState) {
    for (key, path) in dynamic {
        let Some(value) = state.get(&state_subpath(path)) else { continue };
        let v = display_string(&value);
        let _ = crate::semantic::apply_exact(style, key, &v) || crate::semantic::apply_prefixed(style, key, &v);
    }
}

pub fn resolve_dynamic_styles(ui: &mut Ui, state: &dyn NowUiState) {
    for i in 0..ui.nodes.len() {
        let id = NodeId(i as u32);
        // Checked by reference first — `base_style` is a whole `Style`
        // (colors, transform, every variant), not a small field, so cloning
        // it unconditionally for every node in the arena just to check
        // whether it happens to have any `${state.path}` bracket at all was
        // the single most expensive per-node tax among these resolvers. The
        // overwhelming majority of nodes have no dynamic style at all.
        let has_dynamic = {
            let s = &ui.get(id).base_style;
            !s.dynamic.is_empty()
                || s.variants.hover.as_ref().is_some_and(|v| !v.dynamic.is_empty())
                || s.variants.focus.as_ref().is_some_and(|v| !v.dynamic.is_empty())
                || s.variants.active.as_ref().is_some_and(|v| !v.dynamic.is_empty())
        };
        if !has_dynamic {
            continue;
        }
        let base = ui.get(id).base_style.clone();

        if !base.dynamic.is_empty() {
            resolve_dynamic_map(&mut ui.get_mut(id).base_style, &base.dynamic, state);
        }

        // `hover:`/`focus:`/`active:` each resolve their own `StylePair`s
        // into their own clone of the base `Style` (see `semantic::
        // resolve_styles`), `${state.path}` brackets included — so a
        // `hover:key-[${state.path}]` value lives in `variants.hover.
        // dynamic`, not the base style's own `dynamic` map, and needs this
        // same resolution applied to that nested `Style` instead.
        if base.variants.hover.as_ref().is_some_and(|v| !v.dynamic.is_empty()) {
            let dynamic = base.variants.hover.as_ref().unwrap().dynamic.clone();
            if let Some(hover) = ui.get_mut(id).base_style.variants.hover.as_mut() {
                resolve_dynamic_map(hover, &dynamic, state);
            }
        }
        if base.variants.focus.as_ref().is_some_and(|v| !v.dynamic.is_empty()) {
            let dynamic = base.variants.focus.as_ref().unwrap().dynamic.clone();
            if let Some(focus) = ui.get_mut(id).base_style.variants.focus.as_mut() {
                resolve_dynamic_map(focus, &dynamic, state);
            }
        }
        if base.variants.active.as_ref().is_some_and(|v| !v.dynamic.is_empty()) {
            let dynamic = base.variants.active.as_ref().unwrap().dynamic.clone();
            if let Some(active) = ui.get_mut(id).base_style.variants.active.as_mut() {
                resolve_dynamic_map(active, &dynamic, state);
            }
        }
    }
}

/// Write `values` (one per original backtick, same order/count as
/// `nowui-runtime/src/semantic.rs`'s `primitive()` built the node's string
/// fields from) into whichever `NodeKind` fields came from those backticks.
/// Keep this index mapping in sync with `primitive()` if either changes.
fn apply_resolved_templates(kind: &mut NodeKind, values: &[String]) {
    match kind {
        NodeKind::Text { content } => {
            if let Some(v) = values.first() {
                *content = v.clone();
            }
        }
        NodeKind::Button { label } => {
            if let Some(v) = values.first() {
                *label = v.clone();
            }
        }
        NodeKind::Checkbox { label, .. } => {
            if let Some(v) = values.first() {
                *label = v.clone();
            }
        }
        NodeKind::TextInput { label, placeholder, .. } => {
            if let Some(v) = values.first() {
                *label = v.clone();
            }
            if let Some(v) = values.get(1) {
                *placeholder = v.clone();
            }
        }
        NodeKind::Dropdown { placeholder, options, .. } => {
            if let Some(v) = values.first() {
                *placeholder = v.clone();
            }
            for (opt, v) in options.iter_mut().zip(values.iter().skip(1)) {
                *opt = v.clone();
            }
        }
        NodeKind::Menu { label, .. } => {
            if let Some(v) = values.first() {
                *label = v.clone();
            }
        }
        NodeKind::MenuItem { label } => {
            if let Some(v) = values.first() {
                *label = v.clone();
            }
        }
        NodeKind::Date { placeholder, .. } | NodeKind::Time { placeholder, .. } | NodeKind::DateTime { placeholder, .. } => {
            if let Some(v) = values.first() {
                *placeholder = v.clone();
            }
        }
        NodeKind::TreeViewItem { label, .. } => {
            if let Some(v) = values.first() {
                *label = v.clone();
            }
        }
        // An `Image` decodes once, synchronously, at semantic-expansion time
        // (see `semantic::primitive`'s `"Image"` arm) — a templated
        // `${state.path}` source isn't re-decoded on change, so there's
        // nothing meaningful for this per-redraw pass to write back into.
        // Not yet built (dynamic image sources), a documented gap rather
        // than a silent no-op with no explanation.
        // Same story as `Image` just above — an `Icon`'s name isn't a
        // template either, and it's rasterized once at build time, see
        // `semantic::primitive`'s `"Icon"` arm.
        NodeKind::Slider { .. }
        | NodeKind::ProgressBar { .. }
        | NodeKind::Container
        | NodeKind::DataGrid
        | NodeKind::TreeView { .. }
        | NodeKind::Image { .. }
        | NodeKind::Icon { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nowui_core::{Node, Style};

    #[derive(Default, Clone, nowui_core::NowUiState)]
    struct S {
        name: String,
    }

    #[test]
    fn resolve_templates_renders_a_dynamic_backtick_against_live_state() {
        let mut ui = Ui::new();
        let id = ui.push(Node::new(NodeKind::Text { content: String::new() }, Style::default()));
        ui.get_mut(id).templates = vec![vec![TemplatePart::Lit("Hi, ".to_string()), TemplatePart::Var(vec!["name".to_string()])]];
        ui.add_layer(id, "main");

        let state = S { name: "Ada".to_string() };
        resolve_templates(&mut ui, &state, &HashMap::new());

        let NodeKind::Text { content } = &ui.get(id).kind else { panic!() };
        assert_eq!(content, "Hi, Ada");
    }

    #[test]
    fn resolve_templates_evaluates_a_ternary_from_template_exprs_and_reacts_to_state() {
        #[derive(Default, Clone, nowui_core::NowUiState)]
        struct SavingState {
            is_saving: bool,
        }

        let mut ui = Ui::new();
        let id = ui.push(Node::new(NodeKind::Button { label: String::new() }, Style::default()));
        ui.add_layer(id, "main");

        let cond = nowui_syntax::ast::Expr::Cmp(
            Box::new(nowui_syntax::ast::Expr::Path(vec!["state".to_string(), "is_saving".to_string()])),
            nowui_syntax::ast::CmpOp::Eq,
            Box::new(nowui_syntax::ast::Expr::Bool(true)),
        );
        let ternary = nowui_syntax::ast::Expr::Ternary(
            Box::new(cond),
            Box::new(nowui_syntax::ast::Expr::Str("Saving...".to_string())),
            Box::new(nowui_syntax::ast::Expr::Str("Save".to_string())),
        );
        let raw_template = nowui_syntax::ast::Template { parts: vec![nowui_syntax::ast::TplPart::Expr(ternary)] };
        let mut template_exprs = HashMap::new();
        template_exprs.insert(id, vec![raw_template]);

        let mut state = SavingState { is_saving: false };
        resolve_templates(&mut ui, &state, &template_exprs);
        let NodeKind::Button { label } = &ui.get(id).kind else { panic!() };
        assert_eq!(label, "Save");

        state.is_saving = true;
        resolve_templates(&mut ui, &state, &template_exprs);
        let NodeKind::Button { label } = &ui.get(id).kind else { panic!() };
        assert_eq!(label, "Saving...");
    }

    #[test]
    fn resolve_values_writes_a_bound_value_into_the_widget() {
        let mut ui = Ui::new();
        let id = ui.push(Node::new(NodeKind::Checkbox { label: String::new(), checked: false }, Style::default()));
        ui.get_mut(id).value_path = vec!["state".to_string(), "checked".to_string()];
        ui.add_layer(id, "main");

        #[derive(Default, Clone, nowui_core::NowUiState)]
        struct CheckState {
            checked: bool,
        }
        let state = CheckState { checked: true };
        resolve_values(&mut ui, &state, None);

        let NodeKind::Checkbox { checked, .. } = &ui.get(id).kind else { panic!() };
        assert!(checked);
    }

    fn gif_node(loop_playback: bool) -> (Ui, NodeId) {
        let mut ui = Ui::new();
        let img = nowui_image::DecodedImage {
            width: 2,
            height: 2,
            frames: vec![
                nowui_image::Frame { width: 2, height: 2, rgba: vec![0; 16], delay_ms: 100 },
                nowui_image::Frame { width: 2, height: 2, rgba: vec![0; 16], delay_ms: 100 },
            ],
        };
        let kind = NodeKind::Image { source: "a.gif".to_string(), decoded: Some(img), current_frame: 0, frame_elapsed_ms: 0.0, error: None };
        let mut style = Style::default();
        style.loop_playback = loop_playback;
        let id = ui.push(Node::new(kind, style));
        ui.add_layer(id, "main");
        (ui, id)
    }

    #[test]
    fn advance_image_animations_steps_to_the_next_frame_once_its_delay_elapses() {
        let (mut ui, id) = gif_node(false);
        advance_image_animations(&mut ui, 60.0);
        let NodeKind::Image { current_frame, .. } = &ui.get(id).kind else { panic!() };
        assert_eq!(*current_frame, 0);

        advance_image_animations(&mut ui, 60.0);
        let NodeKind::Image { current_frame, .. } = &ui.get(id).kind else { panic!() };
        assert_eq!(*current_frame, 1);
    }

    #[test]
    fn advance_image_animations_holds_the_last_frame_without_loop_playback() {
        let (mut ui, id) = gif_node(false);
        advance_image_animations(&mut ui, 250.0);
        let NodeKind::Image { current_frame, .. } = &ui.get(id).kind else { panic!() };
        assert_eq!(*current_frame, 1);

        advance_image_animations(&mut ui, 1000.0);
        let NodeKind::Image { current_frame, .. } = &ui.get(id).kind else { panic!() };
        assert_eq!(*current_frame, 1, "stays on the last frame once played through");
    }

    #[test]
    fn advance_image_animations_wraps_to_frame_zero_when_loop_playback_is_set() {
        let (mut ui, id) = gif_node(true);
        advance_image_animations(&mut ui, 150.0);
        let NodeKind::Image { current_frame, .. } = &ui.get(id).kind else { panic!() };
        assert_eq!(*current_frame, 1);

        advance_image_animations(&mut ui, 100.0);
        let NodeKind::Image { current_frame, .. } = &ui.get(id).kind else { panic!() };
        assert_eq!(*current_frame, 0, "wraps back around once loop_playback is set");
    }

    #[test]
    fn advance_image_animations_skips_a_single_frame_image() {
        let mut ui = Ui::new();
        let img = nowui_image::DecodedImage {
            width: 2,
            height: 2,
            frames: vec![nowui_image::Frame { width: 2, height: 2, rgba: vec![0; 16], delay_ms: 100 }],
        };
        let kind = NodeKind::Image { source: "a.png".to_string(), decoded: Some(img), current_frame: 0, frame_elapsed_ms: 0.0, error: None };
        let id = ui.push(Node::new(kind, Style::default()));
        ui.add_layer(id, "main");

        advance_image_animations(&mut ui, 10_000.0);
        let NodeKind::Image { current_frame, frame_elapsed_ms, .. } = &ui.get(id).kind else { panic!() };
        assert_eq!(*current_frame, 0);
        assert_eq!(*frame_elapsed_ms, 0.0);
    }

    fn icon_node(color: nowui_core::Color) -> (Ui, NodeId) {
        let mut ui = Ui::new();
        let mut style = Style::default();
        style.text_color = color;
        let kind = NodeKind::Icon { name: "FaUser".to_string(), decoded: None, error: None };
        let id = ui.push(Node::new(kind, style));
        ui.get_mut(id).style = ui.get(id).base_style.clone();
        ui.add_layer(id, "main");
        (ui, id)
    }

    #[test]
    fn resolve_icon_colors_rasterizes_from_the_effective_style() {
        let (mut ui, id) = icon_node(nowui_core::Color { r: 255, g: 0, b: 0, a: 255 });
        resolve_icon_colors(&mut ui);
        let NodeKind::Icon { decoded, error, .. } = &ui.get(id).kind else { panic!() };
        assert!(error.is_none());
        let frame = decoded.as_ref().expect("should have rasterized");
        assert!(frame.rgba.chunks_exact(4).any(|p| p[0] > 200 && p[1] < 50 && p[3] > 0), "expected some red pixels");
    }

    #[test]
    fn resolve_icon_colors_picks_up_a_changed_effective_color_on_the_next_call() {
        let (mut ui, id) = icon_node(nowui_core::Color { r: 255, g: 0, b: 0, a: 255 });
        resolve_icon_colors(&mut ui);
        let NodeKind::Icon { decoded, .. } = &ui.get(id).kind else { panic!() };
        let red_pixels = decoded.as_ref().unwrap().rgba.clone();

        // Simulate `apply_dynamic_styles` swapping in a hover-variant color
        // for this frame, same as the real redraw loop would.
        ui.get_mut(id).style.text_color = nowui_core::Color { r: 0, g: 0, b: 255, a: 255 };
        resolve_icon_colors(&mut ui);
        let NodeKind::Icon { decoded, .. } = &ui.get(id).kind else { panic!() };
        let blue_pixels = decoded.as_ref().unwrap().rgba.clone();

        assert_ne!(red_pixels, blue_pixels, "changing the effective color should re-tint the rasterized icon");
    }

    fn tree_view_item_node(icon_name: &str) -> (Ui, NodeId) {
        let mut ui = Ui::new();
        let style = Style { tree_icon: icon_name.to_string(), ..Default::default() };
        let kind = NodeKind::TreeViewItem {
            id: String::new(),
            label: "widgets".to_string(),
            collapsed: false,
            selected: false,
            checkbox: false,
            show_folder_actions: false,
            icon: None,
        };
        let id = ui.push(Node::new(kind, style));
        ui.add_layer(id, "main");
        (ui, id)
    }

    #[test]
    fn resolve_tree_icons_rasterizes_a_named_icon() {
        let (mut ui, id) = tree_view_item_node("FaFolder");
        resolve_tree_icons(&mut ui);
        let NodeKind::TreeViewItem { icon, .. } = &ui.get(id).kind else { panic!() };
        assert!(icon.is_some(), "FaFolder should rasterize");
    }

    #[test]
    fn resolve_tree_icons_leaves_an_empty_tree_icon_unresolved() {
        let (mut ui, id) = tree_view_item_node("");
        resolve_tree_icons(&mut ui);
        let NodeKind::TreeViewItem { icon, .. } = &ui.get(id).kind else { panic!() };
        assert!(icon.is_none(), "no tree-icon bound at all — nothing to rasterize");
    }

    #[derive(Default, Clone, nowui_core::NowUiState)]
    struct DropdownItemState {
        label: String,
        id: String,
    }

    fn dropdown_node(static_items: Vec<(String, String, bool)>, default_selected_id: Option<String>) -> (Ui, NodeId) {
        let mut ui = Ui::new();
        let options: Vec<String> = static_items.iter().map(|(_, l, _)| l.clone()).collect();
        let option_ids: Vec<String> = static_items.iter().map(|(id, _, _)| id.clone()).collect();
        let option_disabled: Vec<bool> = static_items.iter().map(|(_, _, d)| *d).collect();
        let selected = default_selected_id.as_ref().and_then(|did| option_ids.iter().position(|i| i == did));
        let kind = NodeKind::Dropdown {
            placeholder: "Choose".to_string(),
            options,
            option_ids,
            option_disabled,
            static_items,
            default_selected_id,
            selected,
            open: false,
        };
        let id = ui.push(Node::new(kind, Style::default()));
        ui.add_layer(id, "main");
        (ui, id)
    }

    #[test]
    fn resolve_dropdown_values_is_a_noop_without_a_values_binding() {
        let (mut ui, id) = dropdown_node(vec![("pinned".to_string(), "Pinned".to_string(), false)], None);
        resolve_dropdown_values(&mut ui, &nowui_core::NoState);
        let NodeKind::Dropdown { options, .. } = &ui.get(id).kind else { panic!() };
        assert_eq!(options, &vec!["Pinned".to_string()]);
    }

    #[test]
    fn resolve_dropdown_values_appends_dynamic_items_after_static_ones() {
        #[derive(Default, Clone, nowui_core::NowUiState)]
        struct S {
            items: Vec<DropdownItemState>,
        }
        let (mut ui, id) = dropdown_node(vec![("pinned".to_string(), "-- pinned --".to_string(), false)], None);
        ui.get_mut(id).values_path = vec!["state".to_string(), "items".to_string()];

        let state = S {
            items: vec![
                DropdownItemState { label: "Alice".to_string(), id: "a".to_string() },
                DropdownItemState { label: "Bob".to_string(), id: "b".to_string() },
            ],
        };
        resolve_dropdown_values(&mut ui, &state);

        let NodeKind::Dropdown { options, option_ids, .. } = &ui.get(id).kind else { panic!() };
        assert_eq!(options, &vec!["-- pinned --".to_string(), "Alice".to_string(), "Bob".to_string()]);
        assert_eq!(option_ids, &vec!["pinned".to_string(), "a".to_string(), "b".to_string()]);
    }

    #[test]
    fn resolve_dropdown_values_preserves_selection_by_id_across_a_rebuild() {
        #[derive(Default, Clone, nowui_core::NowUiState)]
        struct S {
            items: Vec<DropdownItemState>,
        }
        let (mut ui, id) = dropdown_node(Vec::new(), None);
        ui.get_mut(id).values_path = vec!["state".to_string(), "items".to_string()];

        let state = S { items: vec![DropdownItemState { label: "Alice".to_string(), id: "a".to_string() }] };
        resolve_dropdown_values(&mut ui, &state);
        if let NodeKind::Dropdown { selected, .. } = &mut ui.get_mut(id).kind {
            *selected = Some(0);
        }

        // Rebuild with Alice now second in the list — selection should
        // follow her id to the new index, not stay pinned at 0.
        let state = S {
            items: vec![
                DropdownItemState { label: "Zed".to_string(), id: "z".to_string() },
                DropdownItemState { label: "Alice".to_string(), id: "a".to_string() },
            ],
        };
        resolve_dropdown_values(&mut ui, &state);

        let NodeKind::Dropdown { selected, option_ids, .. } = &ui.get(id).kind else { panic!() };
        assert_eq!(option_ids[selected.unwrap()], "a");
    }

    #[test]
    fn resolve_dropdown_values_falls_back_to_default_selected_when_the_previous_id_is_gone() {
        #[derive(Default, Clone, nowui_core::NowUiState)]
        struct S {
            items: Vec<DropdownItemState>,
        }
        let (mut ui, id) = dropdown_node(vec![("pinned".to_string(), "-- pinned --".to_string(), false)], Some("pinned".to_string()));
        ui.get_mut(id).values_path = vec!["state".to_string(), "items".to_string()];

        let state = S { items: vec![DropdownItemState { label: "Alice".to_string(), id: "a".to_string() }] };
        resolve_dropdown_values(&mut ui, &state);
        if let NodeKind::Dropdown { selected, .. } = &mut ui.get_mut(id).kind {
            *selected = Some(1); // pick "Alice"
        }

        // Alice disappears from the next resolution — falls back to the
        // static `default-selected` item, not `None`.
        let state = S { items: Vec::new() };
        resolve_dropdown_values(&mut ui, &state);

        let NodeKind::Dropdown { selected, option_ids, .. } = &ui.get(id).kind else { panic!() };
        assert_eq!(option_ids[selected.unwrap()], "pinned");
    }
}
