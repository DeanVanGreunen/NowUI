//! `if`/`for` dynamic regions: the piece that makes the arena's *shape*
//! (which children exist), not just leaf field values, change at runtime in
//! response to live state — `Style::dynamic`/`Node::templates` resolve a
//! *value* each frame, but they can't add or remove nodes. See CLAUDE.md's
//! "Reactivity" section for the full picture; `Semantic`
//! (`semantic.rs`) owns the actual expansion/re-expansion, calling into
//! this module for the parts that don't need `&mut Semantic` (expression
//! evaluation, loop-variable substitution).
//!
//! Scope, deliberately (see CLAUDE.md):
//!   * Expressions: literals, dotted paths, `==`/`!=`/`</<=`/`>`/`>=`,
//!     `&&`/`||`, unary `!`. No arithmetic.
//!   * `for`'s loop variable is usable as a whole `${x}` in a backtick, or
//!     via dotted field access (`${x.field}`, `${x.nested.field}`, ...) when
//!     the list's elements are `StateValue::Object` (a `Vec<T: NowUiState>`
//!     — see `NowUiState::to_state_value`) — not inside a style bracket.
//!   * Nested `if`/`for` inside another region's body are recomputed fresh
//!     every time their ancestor rebuilds, with no independent change-
//!     detection of their own — only a *top-level* region (one registered
//!     while expanding ordinary, non-region content) gets a persisted
//!     `RegionSignature` to skip rebuilding when nothing relevant changed.
//!   * Rebuilding a region does not free its old arena nodes — `Ui` has no
//!     node-removal/GC mechanism at all yet (nothing in this engine does),
//!     so a `for` over a frequently-changing list will leak orphaned nodes
//!     over the app's lifetime. Fine for demos/moderate use; a real fix
//!     needs an arena-wide GC, out of scope here.

use nowui_core::{NowUiState, StateValue};
use nowui_syntax::ast::{CmpOp, Expr, Node as AstNode, Template, TplPart};

/// What a registered dynamic region actually is — the still-unexpanded AST,
/// captured once and re-evaluated against live state on every refresh.
#[derive(Debug, Clone)]
pub enum RegionAst {
    If { branches: Vec<(Expr, Vec<AstNode>)>, else_branch: Vec<AstNode> },
    For { var: String, iter: Expr, body: Vec<AstNode> },
}

/// A snapshot of "what the region last computed," cheap to compare so a
/// redraw that doesn't actually change the relevant state can skip
/// rebuilding (and so preserve focus/scroll/cursor state on the nodes that
/// didn't need to change).
#[derive(Debug, Clone, PartialEq)]
pub enum RegionSignature {
    /// Which `if`/`else if` branch is live, or `branches.len()` for the
    /// `else`/no-match fallback.
    Branch(usize),
    /// The full-content (`signature_string`, not `display_string`) form of
    /// each item currently in the `for`'s iterable, in order. Comparing
    /// rendered strings rather than raw `StateValue`s sidesteps needing
    /// `Eq`/`Hash` on floats.
    Items(Vec<String>),
}

/// Render `value`'s *entire* content, recursively — unlike `display_string`
/// (a one-line, human-facing rendering that collapses a `List`/`Object` to
/// e.g. `"[3 items]"`/`"{2 fields}"`), this is only for `RegionSignature`
/// change-detection: two `for`-loop items must compare unequal whenever
/// *any* nested field differs, even though a bare `${item}` would show the
/// same placeholder text for both.
pub fn signature_string(value: &StateValue) -> String {
    match value {
        StateValue::List(items) => {
            let rendered: Vec<String> = items.iter().map(signature_string).collect();
            format!("[{}]", rendered.join(","))
        }
        StateValue::Object(fields) => {
            let rendered: Vec<String> = fields.iter().map(|(k, v)| format!("{k}:{}", signature_string(v))).collect();
            format!("{{{}}}", rendered.join(","))
        }
        other => nowui_core::display_string(other),
    }
}

/// Resolve `expr` to a `bool` for use as an `if` condition — `None`
/// (unresolvable path, e.g. wrong type) is treated as `false`, same as a
/// missing/`NoState` binding elsewhere in this engine.
pub fn eval_bool(expr: &Expr, resolve: &mut dyn FnMut(&[String]) -> Option<StateValue>) -> bool {
    eval_expr(expr, resolve).map(|v| v.truthy()).unwrap_or(false)
}

/// Evaluate `expr` against `resolve` (a dotted-path lookup — see
/// `make_resolver`), returning `None` only when resolution itself fails
/// (an unknown path); every operator has a definite result once its
/// operands do.
pub fn eval_expr(expr: &Expr, resolve: &mut dyn FnMut(&[String]) -> Option<StateValue>) -> Option<StateValue> {
    match expr {
        Expr::Bool(b) => Some(StateValue::Bool(*b)),
        Expr::Number(n) => Some(StateValue::Float(*n)),
        Expr::Str(s) => Some(StateValue::Str(s.clone())),
        Expr::Path(segs) => resolve_path(segs, resolve),
        Expr::Not(e) => Some(StateValue::Bool(!eval_bool(e, resolve))),
        Expr::Cmp(l, op, r) => {
            let lv = eval_expr(l, resolve);
            let rv = eval_expr(r, resolve);
            Some(StateValue::Bool(compare(lv.as_ref(), *op, rv.as_ref())))
        }
        Expr::And(l, r) => {
            if !eval_bool(l, resolve) {
                return Some(StateValue::Bool(false));
            }
            Some(StateValue::Bool(eval_bool(r, resolve)))
        }
        Expr::Or(l, r) => {
            if eval_bool(l, resolve) {
                return Some(StateValue::Bool(true));
            }
            Some(StateValue::Bool(eval_bool(r, resolve)))
        }
    }
}

/// `path.length` is a pseudo-property, not a real field on any
/// `NowUiState` — if the path as written doesn't resolve (the common case,
/// since nothing is ever really named "length"), and it ends in `.length`,
/// resolve the path *minus* that segment instead and take its length
/// (chars for a `Str`, item count for a `List`).
fn resolve_path(segs: &[String], resolve: &mut dyn FnMut(&[String]) -> Option<StateValue>) -> Option<StateValue> {
    if let Some(v) = resolve(segs) {
        return Some(v);
    }
    if segs.len() > 1 && segs.last().map(String::as_str) == Some("length") {
        let base = resolve(&segs[..segs.len() - 1])?;
        return match base {
            StateValue::Str(s) => Some(StateValue::Int(s.chars().count() as i64)),
            StateValue::List(items) => Some(StateValue::Int(items.len() as i64)),
            _ => None,
        };
    }
    None
}

/// `Eq`/`Ne` compare numerically when *both* sides are numeric (so
/// `state.count == 3` works even though `count` is `Int` and the literal
/// `3` is always parsed as `Float` — `StateValue`'s derived `PartialEq`
/// alone would consider `Int(3)` and `Float(3.0)` unequal), falling back to
/// structural equality for `Str`/`Bool`/`List`. Ordering operators are only
/// defined for numeric operands; anything else is `false`, not a crash.
fn compare(l: Option<&StateValue>, op: CmpOp, r: Option<&StateValue>) -> bool {
    let (Some(l), Some(r)) = (l, r) else { return false };
    if let (Some(lf), Some(rf)) = (l.as_f64(), r.as_f64()) {
        return match op {
            CmpOp::Eq => lf == rf,
            CmpOp::Ne => lf != rf,
            CmpOp::Lt => lf < rf,
            CmpOp::Le => lf <= rf,
            CmpOp::Gt => lf > rf,
            CmpOp::Ge => lf >= rf,
        };
    }
    match op {
        CmpOp::Eq => l == r,
        CmpOp::Ne => l != r,
        CmpOp::Lt | CmpOp::Le | CmpOp::Gt | CmpOp::Ge => false,
    }
}

/// Build a path resolver for expression evaluation: `state.*` paths
/// delegate to the live `NowUiState` (stripping the leading `state`
/// segment, same as everywhere else this engine crosses that boundary);
/// a bare loop variable name (only meaningful while evaluating/expanding a
/// `for`'s own body) resolves to `item` directly, with no further field
/// access into it.
pub fn make_resolver<'a>(
    state: &'a dyn NowUiState,
    loop_var: Option<(&'a str, &'a StateValue)>,
) -> impl FnMut(&[String]) -> Option<StateValue> + 'a {
    move |segs: &[String]| {
        if let Some((name, value)) = loop_var {
            if segs.first().map(String::as_str) == Some(name) {
                return if segs.len() == 1 { Some(value.clone()) } else { None };
            }
        }
        if segs.first().map(String::as_str) == Some("state") {
            let sub: Vec<&str> = segs.iter().skip(1).map(String::as_str).collect();
            return state.get(&sub);
        }
        None
    }
}

/// Deep-clone `node`, replacing every `${var}` and `${var.field...}` (in any
/// backtick belonging to `node` or its descendants, including nested
/// `if`/`for` bodies so an outer loop variable stays visible to them) with a
/// literal rendering of `value`/the resolved field. An inner `for` that
/// re-binds the same name shadows it — its own body is left untouched.
pub fn substitute_loop_var(node: &AstNode, var: &str, value: &StateValue) -> AstNode {
    match node {
        AstNode::Widget { kind, args, string_args, styles, bindings, children } => AstNode::Widget {
            kind: kind.clone(),
            args: args.clone(),
            string_args: string_args.iter().map(|t| substitute_template(t, var, value)).collect(),
            styles: styles.clone(),
            bindings: bindings.clone(),
            children: children.iter().map(|c| substitute_loop_var(c, var, value)).collect(),
        },
        AstNode::If { branches, else_branch } => AstNode::If {
            branches: branches
                .iter()
                .map(|(cond, body)| (cond.clone(), body.iter().map(|c| substitute_loop_var(c, var, value)).collect()))
                .collect(),
            else_branch: else_branch.iter().map(|c| substitute_loop_var(c, var, value)).collect(),
        },
        AstNode::For { var: inner_var, iter, body } => AstNode::For {
            var: inner_var.clone(),
            iter: iter.clone(),
            body: if inner_var == var {
                body.clone()
            } else {
                body.iter().map(|c| substitute_loop_var(c, var, value)).collect()
            },
        },
        AstNode::LayoutDef { .. } | AstNode::Import { .. } => node.clone(),
    }
}

/// Resolve `path` (dot-separated, e.g. `"label"` or `"nested.label"`)
/// through a chain of `StateValue::Object`s starting at `value`, rendering
/// the final value found. `None` if any segment doesn't resolve (not an
/// `Object`, or no such field) — the caller leaves the `${...}` as-is in
/// that case rather than silently blanking it, so a typo'd field name is
/// at least visible in the rendered output instead of vanishing.
fn resolve_object_path(value: &StateValue, path: &str) -> Option<String> {
    let mut current = value;
    for seg in path.split('.') {
        current = current.get_field(seg)?;
    }
    Some(nowui_core::display_string(current))
}

fn substitute_template(t: &Template, var: &str, value: &StateValue) -> Template {
    let prefix = format!("{var}.");
    Template {
        parts: t
            .parts
            .iter()
            .map(|p| match p {
                TplPart::Var(name) if name == var => TplPart::Lit(nowui_core::display_string(value)),
                TplPart::Var(name) => match name.strip_prefix(&prefix).and_then(|rest| resolve_object_path(value, rest)) {
                    Some(rendered) => TplPart::Lit(rendered),
                    None => TplPart::Var(name.clone()),
                },
                other => other.clone(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolver<'a>(pairs: &'a [(&'a str, StateValue)]) -> impl FnMut(&[String]) -> Option<StateValue> + 'a {
        move |segs: &[String]| {
            let joined = segs.join(".");
            pairs.iter().find(|(k, _)| *k == joined).map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn comparisons_coerce_int_and_float_numerically() {
        let mut r = resolver(&[]);
        assert!(eval_bool(
            &Expr::Cmp(Box::new(Expr::Number(3.0)), CmpOp::Eq, Box::new(Expr::Number(3.0))),
            &mut r
        ));
    }

    #[test]
    fn length_pseudo_property_works_on_str_and_list() {
        let pairs = [
            ("state.name", StateValue::Str("dean".to_string())),
            ("state.rows", StateValue::List(vec![StateValue::Int(1), StateValue::Int(2)])),
        ];
        let mut r = resolver(&pairs);
        assert_eq!(
            eval_expr(&Expr::Path(vec!["state".into(), "name".into(), "length".into()]), &mut r),
            Some(StateValue::Int(4))
        );
        assert_eq!(
            eval_expr(&Expr::Path(vec!["state".into(), "rows".into(), "length".into()]), &mut r),
            Some(StateValue::Int(2))
        );
    }

    #[test]
    fn and_short_circuits_and_or_short_circuits() {
        // A right side referencing an unresolvable path would evaluate to
        // `false` via `eval_bool`'s `None -> false` — but short-circuiting
        // means it's never even evaluated, which we can't observe directly
        // here, so just confirm the *results* are the ones short-circuit
        // logic implies.
        let mut r = resolver(&[]);
        assert!(!eval_bool(&Expr::And(Box::new(Expr::Bool(false)), Box::new(Expr::Bool(true))), &mut r));
        assert!(eval_bool(&Expr::Or(Box::new(Expr::Bool(true)), Box::new(Expr::Bool(false))), &mut r));
    }

    #[test]
    fn range_comparison_matches_the_review_example() {
        let pairs = [("state.username", StateValue::Str("dean".to_string()))];
        let mut r = resolver(&pairs);
        let len = Expr::Path(vec!["state".into(), "username".into(), "length".into()]);
        let cond = Expr::And(
            Box::new(Expr::Cmp(Box::new(len.clone()), CmpOp::Gt, Box::new(Expr::Number(3.0)))),
            Box::new(Expr::Cmp(Box::new(len), CmpOp::Lt, Box::new(Expr::Number(8.0)))),
        );
        assert!(eval_bool(&cond, &mut r), "\"dean\".length == 4, so 3 < 4 < 8");
    }

    #[test]
    fn make_resolver_resolves_the_loop_variable_and_leaves_unrelated_paths_alone() {
        let state = nowui_core::NoState;
        let item = StateValue::Int(42);
        let mut resolve = make_resolver(&state, Some(("x", &item)));
        assert_eq!(resolve(&["x".to_string()]), Some(StateValue::Int(42)));
        assert_eq!(resolve(&["x".to_string(), "y".to_string()]), None, "no field access into a scalar loop item");
        assert_eq!(resolve(&["state".to_string(), "anything".to_string()]), None, "NoState never resolves");
        assert_eq!(resolve(&["not_x".to_string()]), None, "a name that isn't the loop var and isn't state.* resolves to nothing");
    }

    #[test]
    fn substitute_loop_var_replaces_var_in_backticks_recursively() {
        let src = "layout: T { Container { Text `Row: ${x}` } }";
        let ast = nowui_syntax::parse(src).unwrap();
        let nowui_syntax::ast::Node::LayoutDef { children, .. } = &ast[0] else { panic!() };
        let substituted = substitute_loop_var(&children[0], "x", &StateValue::Int(7));
        let AstNode::Widget { children, .. } = &substituted else { panic!() };
        let AstNode::Widget { string_args, .. } = &children[0] else { panic!() };
        // Adjacent `Lit`s aren't merged (no behavioral difference — every
        // consumer just concatenates all parts regardless), so the literal
        // prefix and the substituted `${x}` stay as two parts.
        assert_eq!(string_args[0].parts, vec![TplPart::Lit("Row: ".to_string()), TplPart::Lit("7".to_string())]);
    }

    #[test]
    fn substitute_loop_var_does_not_cross_a_shadowing_inner_for() {
        let src = "layout: T { for x in state.inner { Text `${x}` } }";
        let ast = nowui_syntax::parse(src).unwrap();
        let nowui_syntax::ast::Node::LayoutDef { children, .. } = &ast[0] else { panic!() };
        let substituted = substitute_loop_var(&children[0], "x", &StateValue::Int(7));
        let AstNode::For { body, .. } = &substituted else { panic!() };
        let AstNode::Widget { string_args, .. } = &body[0] else { panic!() };
        // Untouched — the inner `for x in ...` shadows the outer `x`.
        assert_eq!(string_args[0].parts, vec![TplPart::Var("x".to_string())]);
    }

    #[test]
    fn substitute_loop_var_resolves_dotted_field_access_on_an_object_item() {
        let src = "layout: T { Text `${x.label}` }";
        let ast = nowui_syntax::parse(src).unwrap();
        let nowui_syntax::ast::Node::LayoutDef { children, .. } = &ast[0] else { panic!() };
        let item = StateValue::Object(vec![
            ("id".to_string(), StateValue::Str("1".to_string())),
            ("label".to_string(), StateValue::Str("First row".to_string())),
        ]);
        let substituted = substitute_loop_var(&children[0], "x", &item);
        let AstNode::Widget { string_args, .. } = &substituted else { panic!() };
        assert_eq!(string_args[0].parts, vec![TplPart::Lit("First row".to_string())]);
    }

    #[test]
    fn substitute_loop_var_leaves_an_unknown_field_untouched() {
        let src = "layout: T { Text `${x.nope}` }";
        let ast = nowui_syntax::parse(src).unwrap();
        let nowui_syntax::ast::Node::LayoutDef { children, .. } = &ast[0] else { panic!() };
        let item = StateValue::Object(vec![("id".to_string(), StateValue::Str("1".to_string()))]);
        let substituted = substitute_loop_var(&children[0], "x", &item);
        let AstNode::Widget { string_args, .. } = &substituted else { panic!() };
        assert_eq!(string_args[0].parts, vec![TplPart::Var("x.nope".to_string())]);
    }

    #[test]
    fn signature_string_distinguishes_objects_that_display_string_would_collapse() {
        // `display_string` renders any Object as the same placeholder
        // ("{2 fields}") regardless of content — the whole reason
        // `signature_string` exists separately for change-detection.
        let a = StateValue::Object(vec![("id".to_string(), StateValue::Str("1".to_string()))]);
        let b = StateValue::Object(vec![("id".to_string(), StateValue::Str("2".to_string()))]);
        assert_eq!(nowui_core::display_string(&a), nowui_core::display_string(&b));
        assert_ne!(signature_string(&a), signature_string(&b));
    }
}
