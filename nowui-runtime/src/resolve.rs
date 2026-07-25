//! The "read half" of reactivity: re-render every node's `{value:
//! state.path}`/`` `${state.path}` ``/`key-[${state.path}]` bindings against
//! *live* state every redraw. Extracted out of `App<S>`'s own (private)
//! methods into free functions so `nowui-designer` can reuse the exact same
//! resolution logic in its own harness (built directly on `nowui_core`
//! rather than `App`/`run_path` — see its `preview.rs`/`app.rs` module docs
//! for why) without duplicating it. `App`'s own `resolve_values`/
//! `resolve_templates`/`resolve_dynamic_styles` are now thin wrappers around
//! these.

use nowui_core::{display_string, NodeId, NodeKind, NowUiState, TemplatePart, Ui};

/// `.nowui` binding paths are rooted at the literal `state` segment (see
/// `nowui-syntax`'s dotted-path grammar), but `NowUiState` impls are rooted
/// at their own struct's fields, so that leading segment is stripped before
/// crossing the reflection boundary.
pub(crate) fn state_subpath(path: &[String]) -> Vec<&str> {
    let skip = usize::from(path.first().is_some_and(|s| s == "state"));
    path.iter().skip(skip).map(String::as_str).collect()
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
        let path = ui.get(id).value_path.clone();
        if path.is_empty() {
            continue;
        }
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
            NodeKind::Dropdown { options, selected, .. } => {
                if let Some(s) = value.as_str() {
                    *selected = options.iter().position(|o| o == s);
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

/// Re-render every node's `templates` (backticks containing `${state.path}`
/// interpolation, e.g. `` `Count: ${state.counter.count}` ``) against live
/// state and write the result into the widget field(s) that backtick
/// originally built — the same read-half-of-reactivity idea as
/// `resolve_values`, just for inline text instead of a `{value: ...}`
/// binding. A node with no dynamic backticks has empty `templates` and is
/// skipped entirely.
pub fn resolve_templates(ui: &mut Ui, state: &dyn NowUiState) {
    for i in 0..ui.nodes.len() {
        let id = NodeId(i as u32);
        let templates = ui.get(id).templates.clone();
        if templates.is_empty() {
            continue;
        }
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
pub fn resolve_dynamic_styles(ui: &mut Ui, state: &dyn NowUiState) {
    for i in 0..ui.nodes.len() {
        let id = NodeId(i as u32);
        let dynamic = ui.get(id).base_style.dynamic.clone();
        if dynamic.is_empty() {
            continue;
        }
        for (key, path) in &dynamic {
            let Some(value) = state.get(&state_subpath(path)) else { continue };
            let v = display_string(&value);
            let style = &mut ui.get_mut(id).base_style;
            let _ = crate::semantic::apply_exact(style, key, &v) || crate::semantic::apply_prefixed(style, key, &v);
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
        NodeKind::Slider { .. } | NodeKind::ProgressBar { .. } | NodeKind::Container | NodeKind::DataGrid | NodeKind::TreeView { .. } => {}
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
        resolve_templates(&mut ui, &state);

        let NodeKind::Text { content } = &ui.get(id).kind else { panic!() };
        assert_eq!(content, "Hi, Ada");
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
}
