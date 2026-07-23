//! NowUI file-format front-end: lexing + parsing into an AST.
//!
//! This crate has no knowledge of layout, rendering, or state — it turns
//! source text into `ast::Node`s and nothing more.

pub mod ast;
pub mod import_path;
pub mod parser;

pub use ast::*;
pub use import_path::{import_dirname, join_import_path};
pub use parser::parse;

#[cfg(test)]
mod tests {
    use super::*;

    const LOGIN: &str = include_str!("../../examples/counter-app/src/login.nowui");

    #[test]
    fn parses_login_example() {
        let ast = parse(LOGIN).expect("login.nowui should parse");
        assert!(!ast.is_empty(), "expected at least one layout def");
        assert!(
            ast.iter().any(|n| matches!(n, Node::LayoutDef { name, .. } if name == "Login")),
            "expected a `Login` layout definition"
        );
    }

    #[test]
    fn preserves_empty_string_arg() {
        // The empty label backtick must be kept so the placeholder stays in slot 2.
        let src = r#"layout: T { TextInput `` `Enter Username` {value: state.username} }"#;
        let ast = parse(src).expect("should parse");
        let Node::LayoutDef { children, .. } = &ast[0] else { panic!() };
        let Node::Widget { string_args, .. } = &children[0] else { panic!() };
        assert_eq!(string_args.len(), 2);
        assert!(string_args[0].is_empty(), "first arg (label) should be empty");
        assert_eq!(string_args[1].render_flat(), "Enter Username");
    }

    #[test]
    fn parses_a_widget_with_both_bindings_and_children() {
        // A widget can now have `{onClick: ...}` on itself *and* a real
        // child list — the original design only allowed one or the other
        // (an either-or `Trailer`), which can't express e.g. a `Menu` that
        // needs a click handler on its own header plus real `MenuItem`
        // children.
        let src = "layout: T { Menu `Preferences` { onClick: state.onClick } { MenuItem `Open Preferences` } }";
        let ast = parse(src).expect("should parse");
        let Node::LayoutDef { children, .. } = &ast[0] else { panic!() };
        let Node::Widget { bindings, children, .. } = &children[0] else { panic!() };
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].key, "onClick");
        assert_eq!(children.len(), 1);
        assert!(matches!(&children[0], Node::Widget { kind, .. } if kind == "MenuItem"));
    }

    #[test]
    fn widget_with_only_children_still_parses_bindings_as_empty() {
        let src = "layout: T { Container { Text `hi` } }";
        let ast = parse(src).expect("should parse");
        let Node::LayoutDef { children, .. } = &ast[0] else { panic!() };
        let Node::Widget { bindings, children, .. } = &children[0] else { panic!() };
        assert!(bindings.is_empty());
        assert_eq!(children.len(), 1);
    }

    #[test]
    fn parses_dotted_binding_path() {
        let src = r#"layout: T { Button `Go` {onClick: state.signIn} }"#;
        let ast = parse(src).expect("should parse");
        let Node::LayoutDef { children, .. } = &ast[0] else { panic!() };
        let Node::Widget { bindings, .. } = &children[0] else { panic!() };
        assert_eq!(bindings[0].key, "onClick");
        assert_eq!(
            bindings[0].value,
            BindValue::Path(vec!["state".into(), "signIn".into()])
        );
    }

    #[test]
    fn parses_dotted_interpolation_in_backtick_template() {
        // `${state.counter.count}` must parse as one Var("state.counter.count")
        // part, not stop at the first `.` (interp() used to only accept a bare
        // ident, which would fail on anything but a single-segment path).
        let src = "layout: T { Text `Count: ${state.counter.count}!` }";
        let ast = parse(src).expect("should parse");
        let Node::LayoutDef { children, .. } = &ast[0] else { panic!() };
        let Node::Widget { string_args, .. } = &children[0] else { panic!() };
        assert_eq!(
            string_args[0].parts,
            vec![
                TplPart::Lit("Count: ".into()),
                TplPart::Var("state.counter.count".into()),
                TplPart::Lit("!".into()),
            ]
        );
    }

    #[test]
    fn parses_if_else_if_else_chain() {
        let src = r#"
            layout: T {
                if state.username.length > 3 && state.username.length < 8 {
                    Text `ok`
                } else if state.username.length > 8 {
                    Text `too long`
                } else {
                    Text `enter username`
                }
            }
        "#;
        let ast = parse(src).expect("should parse");
        let Node::LayoutDef { children, .. } = &ast[0] else { panic!() };
        let Node::If { branches, else_branch } = &children[0] else { panic!("expected an If node") };
        assert_eq!(branches.len(), 2, "the `if` plus one `else if`");
        assert_eq!(else_branch.len(), 1);

        let (cond, body) = &branches[0];
        assert_eq!(body.len(), 1);
        assert_eq!(
            *cond,
            Expr::And(
                Box::new(Expr::Cmp(
                    Box::new(Expr::Path(vec!["state".into(), "username".into(), "length".into()])),
                    CmpOp::Gt,
                    Box::new(Expr::Number(3.0)),
                )),
                Box::new(Expr::Cmp(
                    Box::new(Expr::Path(vec!["state".into(), "username".into(), "length".into()])),
                    CmpOp::Lt,
                    Box::new(Expr::Number(8.0)),
                )),
            )
        );
    }

    #[test]
    fn parses_if_with_no_else() {
        let src = "layout: T { if state.loggedIn { Text `hi` } }";
        let ast = parse(src).expect("should parse");
        let Node::LayoutDef { children, .. } = &ast[0] else { panic!() };
        let Node::If { branches, else_branch } = &children[0] else { panic!() };
        assert_eq!(branches.len(), 1);
        assert!(else_branch.is_empty());
        assert_eq!(branches[0].0, Expr::Path(vec!["state".into(), "loggedIn".into()]));
    }

    #[test]
    fn parses_for_loop() {
        let src = "layout: T { for x in state.rows { Text `Row: ${x}` } }";
        let ast = parse(src).expect("should parse");
        let Node::LayoutDef { children, .. } = &ast[0] else { panic!() };
        let Node::For { var, iter, body } = &children[0] else { panic!("expected a For node") };
        assert_eq!(var, "x");
        assert_eq!(*iter, Expr::Path(vec!["state".into(), "rows".into()]));
        assert_eq!(body.len(), 1);
    }

    #[test]
    fn expr_precedence_and_binds_tighter_than_or_and_not_binds_tightest() {
        // `!a && b || c` should parse as `(!a && b) || c`.
        let src = "layout: T { if !state.a && state.b || state.c { Text `x` } }";
        let ast = parse(src).expect("should parse");
        let Node::LayoutDef { children, .. } = &ast[0] else { panic!() };
        let Node::If { branches, .. } = &children[0] else { panic!() };
        let a = Expr::Path(vec!["state".into(), "a".into()]);
        let b = Expr::Path(vec!["state".into(), "b".into()]);
        let c = Expr::Path(vec!["state".into(), "c".into()]);
        assert_eq!(
            branches[0].0,
            Expr::Or(Box::new(Expr::And(Box::new(Expr::Not(Box::new(a))), Box::new(b))), Box::new(c))
        );
    }

    #[test]
    fn expr_supports_quoted_string_literals_and_parens() {
        let src = r#"layout: T { if (state.role == "admin") { Text `x` } }"#;
        let ast = parse(src).expect("should parse");
        let Node::LayoutDef { children, .. } = &ast[0] else { panic!() };
        let Node::If { branches, .. } = &children[0] else { panic!() };
        assert_eq!(
            branches[0].0,
            Expr::Cmp(
                Box::new(Expr::Path(vec!["state".into(), "role".into()])),
                CmpOp::Eq,
                Box::new(Expr::Str("admin".into())),
            )
        );
    }

    #[test]
    fn if_and_for_dont_swallow_a_similarly_named_widget_kind() {
        // `if`/`for` are matched as plain `just("if")`/`just("for")` with no
        // explicit word-boundary check (same convention as `true`/`false`
        // in `bind_value()`) — confirm a widget kind that happens to start
        // with those letters still parses as an ordinary widget, not a
        // malformed/partial control-flow node.
        let src = "layout: T { Iffy `a` Formatter `b` }";
        let ast = parse(src).expect("should parse");
        let Node::LayoutDef { children, .. } = &ast[0] else { panic!() };
        assert_eq!(children.len(), 2);
        assert!(matches!(&children[0], Node::Widget { kind, .. } if kind == "Iffy"));
        assert!(matches!(&children[1], Node::Widget { kind, .. } if kind == "Formatter"));
    }

    #[test]
    fn sibling_nodes_dont_swallow_next_kind_as_bare_style() {
        // A node with no bracketed trailing style must not let `style()` greedily
        // consume the next sibling's Capitalized `kind` ident as a bare flag.
        let src = "layout: T { Text `A` w-[fill] Text `B` }";
        let ast = parse(src).expect("should parse");
        let Node::LayoutDef { children, .. } = &ast[0] else { panic!() };
        assert_eq!(children.len(), 2);
    }

    #[test]
    fn parses_top_level_import_directive() {
        let src = "# widgets/BillingCard.nowui\nlayout: T { Text `hi` }\n";
        let ast = parse(src).expect("should parse");
        assert_eq!(ast[0], Node::Import { path: "widgets/BillingCard.nowui".to_string() });
        assert!(matches!(ast[1], Node::LayoutDef { .. }));
    }

    #[test]
    fn parses_variant_prefixed_style_keys() {
        let src = r#"layout: T { Button `Go` hover:bg-color-[#2563eb] sm:grid-cols-[2] 2xl:p-[4px] }"#;
        let ast = parse(src).expect("should parse");
        let Node::LayoutDef { children, .. } = &ast[0] else { panic!() };
        let Node::Widget { styles, .. } = &children[0] else { panic!() };
        assert!(styles.iter().any(|s| s.key == "hover:bg-color" && s.value == "#2563eb"));
        assert!(styles.iter().any(|s| s.key == "sm:grid-cols" && s.value == "2"));
        assert!(styles.iter().any(|s| s.key == "2xl:p" && s.value == "4px"));
    }

    #[test]
    fn parses_bare_flag_style() {
        let src = r#"layout: Root grid p-[8px 8px] { Text `hi` }"#;
        let ast = parse(src).expect("should parse");
        let Node::LayoutDef { styles, .. } = &ast[0] else { panic!() };
        assert!(styles.iter().any(|s| s.key == "grid" && s.value.is_empty()));
        assert!(styles.iter().any(|s| s.key == "p" && s.value == "8px 8px"));
    }

    #[test]
    fn parses_layout_params_and_use() {
        let src = r#"
            layout: Login(theme, onSubmit) {
                Button `SIGN IN` {onClick: onSubmit}
            }
            layout: App {
                Login theme=`dark` onSubmit=state.signIn
            }
        "#;
        let ast = parse(src).expect("should parse");
        let Node::LayoutDef { params, .. } = &ast[0] else { panic!() };
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, "theme");

        let Node::LayoutDef { children, .. } = &ast[1] else { panic!() };
        let Node::Widget { kind, args, .. } = &children[0] else { panic!() };
        assert_eq!(kind, "Login");
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].name, "theme");
    }
}
