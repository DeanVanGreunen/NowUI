//! The inspector: given a preview node's own source `Span` (already looked
//! up via `PreviewDoc::node_span` the same way `App::select_in_source`
//! highlights it in the editor), reconstructs that widget's kind, style
//! tokens, and bindings for display — and, going the other way, patches a
//! single field's value back into the source text.
//!
//! `nowui-runtime::semantic::Semantic::node_spans` only keeps the *whole*
//! widget's own span, not its individual `StylePair`/`Binding` spans (the
//! originating AST node isn't kept anywhere past the semantic pass — see
//! that field's own doc comment) — so recovering "what are this widget's
//! own style/binding tokens, each with its own further span I can edit"
//! means a fresh, cheap reparse of the (small, single) `.nowui` file the
//! click happened in, then a byte-range match against the widget span
//! already in hand. This is the same "span-based text surgery, never a
//! whole-file AST serializer" approach used everywhere in this crate that
//! writes back into a `.nowui` file — see `editor.rs`'s own module doc for
//! why: a generated serializer would silently drop every `//` comment and
//! reflow formatting on first save.

use nowui_syntax::ast::{BindValue, Node as AstNode, Span};

/// One editable row on the selected widget: a style token (`key-[value]`,
/// `is_binding: false`) or a `{key: value}` binding (`is_binding: true`).
#[derive(Debug, Clone, PartialEq)]
pub struct InspectorField {
    pub label: String,
    pub value: String,
    pub span: Span,
    pub is_binding: bool,
}

/// Everything the inspector shows for one selected widget.
#[derive(Debug, Clone, PartialEq)]
pub struct InspectorSelection {
    pub kind: String,
    pub fields: Vec<InspectorField>,
}

/// Re-parses `source` and finds the `Widget` whose own recorded span
/// exactly equals `target`, returning its kind plus every style/binding
/// token it carries directly (not from a nested child). `None` if `source`
/// doesn't parse, or no widget's span matches exactly (e.g. `target` came
/// from a different, `#`-imported file — a documented limitation shared
/// with `App::select_in_source`, which makes the same single-buffer
/// assumption).
pub fn inspect(source: &str, target: Span) -> Option<InspectorSelection> {
    let ast = nowui_syntax::parse(source).ok()?;
    find_widget(&ast, target)
}

fn find_widget(nodes: &[AstNode], target: Span) -> Option<InspectorSelection> {
    for node in nodes {
        match node {
            AstNode::Widget { kind, styles, bindings, children, span, .. } => {
                if *span == target {
                    let mut fields: Vec<InspectorField> =
                        styles.iter().map(|s| InspectorField { label: s.key.clone(), value: s.value.clone(), span: s.span, is_binding: false }).collect();
                    fields.extend(bindings.iter().map(|b| InspectorField {
                        label: b.key.clone(),
                        value: bind_value_display(&b.value),
                        span: b.span,
                        is_binding: true,
                    }));
                    return Some(InspectorSelection { kind: kind.clone(), fields });
                }
                if let Some(found) = find_widget(children, target) {
                    return Some(found);
                }
            }
            AstNode::LayoutDef { children, .. } => {
                if let Some(found) = find_widget(children, target) {
                    return Some(found);
                }
            }
            AstNode::If { branches, else_branch } => {
                for (_, body) in branches {
                    if let Some(found) = find_widget(body, target) {
                        return Some(found);
                    }
                }
                if let Some(found) = find_widget(else_branch, target) {
                    return Some(found);
                }
            }
            AstNode::For { body, .. } => {
                if let Some(found) = find_widget(body, target) {
                    return Some(found);
                }
            }
            AstNode::Import { .. } => {}
        }
    }
    None
}

fn bind_value_display(v: &BindValue) -> String {
    match v {
        BindValue::Path(segs) => segs.join("."),
        BindValue::Bool(b) => b.to_string(),
        BindValue::Number(n) => n.to_string(),
        BindValue::Str(s) => s.clone(),
    }
}

/// Replaces `field`'s own style-token span with `new_token` verbatim (e.g.
/// `"bg-blue-700"` for a compact Tailwind class, or `"bg-[#1177bb]"` for
/// the bracket form) — everything else in `source` (comments, unrelated
/// whitespace/tokens) is untouched. Unlike a binding, a style token has no
/// fixed `key: value` shape to preserve separately: a compact class like
/// `bg-blue-500` folds its whole tail into the key at parse time (no
/// bracket at all — `field.value` is `""` for one of these, see
/// `nowui-syntax`'s own parser gotcha #2 on `-` joining), so only the
/// caller — which knows whether it's replacing a compact class or a
/// bracket value — can format the right replacement text. `field.span`
/// must still be valid against `source` (the same buffer `inspect` was
/// just called with) — a caller editing a stale selection against an
/// already-changed buffer should re-run `inspect` first rather than trust
/// an old span.
pub fn apply_style_edit(source: &str, field: &InspectorField, new_token: &str) -> String {
    splice(source, field.span, new_token)
}

/// Same as `apply_style_edit`, for a `{key: value}` binding — `new_value_src`
/// is the raw source text that should appear after the `:` (e.g. `state.save`,
/// `true`, `42`, or `"a quoted string"`), since a binding's value can be any
/// of `BindValue`'s four shapes and only the caller (which knows what kind of
/// field this is) can format that correctly.
pub fn apply_binding_edit(source: &str, field: &InspectorField, new_value_src: &str) -> String {
    let replacement = format!("{}: {}", field.label, new_value_src);
    splice(source, field.span, &replacement)
}

fn splice(source: &str, span: Span, replacement: &str) -> String {
    let mut out = String::with_capacity(source.len() - (span.end - span.start) + replacement.len());
    out.push_str(&source[..span.start]);
    out.push_str(replacement);
    out.push_str(&source[span.end..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn widget_span(source: &str, kind: &str) -> Span {
        let ast = nowui_syntax::parse(source).unwrap();
        fn find(nodes: &[AstNode], kind: &str) -> Option<Span> {
            for n in nodes {
                match n {
                    AstNode::Widget { kind: k, span, children, .. } => {
                        if k == kind {
                            return Some(*span);
                        }
                        if let Some(s) = find(children, kind) {
                            return Some(s);
                        }
                    }
                    AstNode::LayoutDef { children, .. } => {
                        if let Some(s) = find(children, kind) {
                            return Some(s);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        find(&ast, kind).unwrap_or_else(|| panic!("no {kind} widget found"))
    }

    #[test]
    fn inspect_lists_a_widgets_own_styles_and_bindings_not_a_childs() {
        let src = "layout: App { Button `Save` bg-[blue-500] text-[white] {onClick: state.save} { Text `nested` } }";
        let span = widget_span(src, "Button");

        let sel = inspect(src, span).expect("Button should be found");
        assert_eq!(sel.kind, "Button");
        assert_eq!(sel.fields.iter().filter(|f| f.label == "bg").count(), 1);
        assert_eq!(sel.fields.iter().find(|f| f.label == "bg").unwrap().value, "blue-500");
        assert_eq!(sel.fields.iter().find(|f| f.label == "text").unwrap().value, "white");
        let binding = sel.fields.iter().find(|f| f.label == "onClick").expect("onClick binding");
        assert!(binding.is_binding);
        assert_eq!(binding.value, "state.save");
        assert!(sel.fields.iter().all(|f| f.label != "nested"), "nested Text's own content isn't this widget's field");
    }

    #[test]
    fn inspect_treats_a_compact_tailwind_class_as_one_bare_flag_token_not_a_split_key_value() {
        // `bg-blue-500`'s own trailing `-500` folds into the *key* at parse
        // time (no bracket, so nothing marks where a "value" would start —
        // see nowui-syntax's own parser gotcha #2), unlike `bg-[blue-500]`
        // above. The inspector must reflect that raw shape rather than
        // pretending it already knows how to split a compact class.
        let src = "layout: App { Container bg-blue-500 } ";
        let span = widget_span(src, "Container");

        let sel = inspect(src, span).unwrap();
        let field = sel.fields.iter().find(|f| f.label == "bg-blue-500").expect("the whole compact class is the label");
        assert_eq!(field.value, "", "no bracket, so no separate value half");
    }

    #[test]
    fn inspect_returns_none_for_a_span_that_matches_no_widget() {
        let src = "layout: App { Text `hi` }";
        assert!(inspect(src, Span { start: 0, end: 0 }).is_none());
    }

    #[test]
    fn apply_style_edit_replaces_only_the_targeted_tokens_own_span() {
        let src = "layout: App { Button `Save` bg-blue-500 text-white } // trailing comment";
        let span = widget_span(src, "Button");
        let sel = inspect(src, span).unwrap();
        let bg = sel.fields.iter().find(|f| f.label == "bg-blue-500").unwrap();

        let patched = apply_style_edit(src, bg, "bg-blue-700");

        assert_eq!(patched, "layout: App { Button `Save` bg-blue-700 text-white } // trailing comment");
    }

    #[test]
    fn apply_style_edit_on_a_bracket_value_replaces_the_whole_token() {
        let src = "layout: App { Container grid p-[4px] } ";
        let span = widget_span(src, "Container");
        let sel = inspect(src, span).unwrap();
        let p = sel.fields.iter().find(|f| f.label == "p").unwrap();
        assert_eq!(p.value, "4px");

        let patched = apply_style_edit(src, p, "p-[8px]");

        assert_eq!(patched, "layout: App { Container grid p-[8px] } ");
    }

    #[test]
    fn apply_binding_edit_replaces_the_whole_key_value_pair() {
        let src = "layout: App { Button `Save` {onClick: state.save} }";
        let span = widget_span(src, "Button");
        let sel = inspect(src, span).unwrap();
        let on_click = sel.fields.iter().find(|f| f.label == "onClick").unwrap();

        let patched = apply_binding_edit(src, on_click, "state.cancel");

        assert_eq!(patched, "layout: App { Button `Save` {onClick: state.cancel} }");
    }
}
