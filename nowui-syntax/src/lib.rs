//! NowUI file-format front-end: lexing + parsing into an AST.
//!
//! This crate has no knowledge of layout, rendering, or state — it turns
//! source text into `ast::Node`s and nothing more.

pub mod ast;
pub mod parser;

pub use ast::*;
pub use parser::parse;

#[cfg(test)]
mod tests {
    use super::*;

    const LOGIN: &str = include_str!("../../examples/login.nowui");

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
