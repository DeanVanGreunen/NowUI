//! A standalone lexer for `.nowui` syntax highlighting, driving
//! `textDocument/semanticTokens/full`.
//!
//! Deliberately **not** built on `nowui_syntax::parse`'s AST — `ast::Node`
//! carries no source spans at all (see its own module doc comment), and
//! threading them through every AST variant just for editor tooling would
//! be a large, unrelated change to the parser crate. This is a separate,
//! single-pass, best-effort scan over the raw text instead: good enough for
//! coloring, not a second source of truth for the grammar. Diagnostics
//! (actual parse errors) still come from the real parser — see `main.rs`.
//!
//! Known, deliberate simplifications (disclosed, not bugs):
//!   * `${...}` interpolation inside a backtick string is not highlighted
//!     specially — the whole backtick span is one `String` token.
//!   * No punctuation/operator tokens (`{`/`}`/`:`/`=`/...) — most themes
//!     don't color these distinctly anyway, and it keeps the token stream
//!     small.
//!   * The `variant:key` compound style token (`hover:bg-blue-600`) is
//!     distinguished from a `{key: value}` binding's colon by a single
//!     heuristic (colon immediately followed by an identifier char, no
//!     whitespace) rather than the parser's real (grammar-position-aware)
//!     rule — matches how both forms are conventionally written.

use tower_lsp::lsp_types::SemanticTokenType;

/// Legend order — index into this list is a token's `kind` below, and is
/// exactly what `main.rs` advertises as the server's semantic token legend.
pub const TOKEN_TYPES: &[SemanticTokenType] = &[
    SemanticTokenType::COMMENT,   // 0
    SemanticTokenType::KEYWORD,   // 1
    SemanticTokenType::STRING,    // 2
    SemanticTokenType::NUMBER,    // 3
    SemanticTokenType::TYPE,      // 4 — widget kind / layout name
    SemanticTokenType::VARIABLE,  // 5 — dotted state/loop-var path
    SemanticTokenType::PROPERTY,  // 6 — style key / binding key / arg name / bare loop var
    SemanticTokenType::NAMESPACE, // 7 — `#` import path
];

pub const COMMENT: u32 = 0;
pub const KEYWORD: u32 = 1;
pub const STRING: u32 = 2;
pub const NUMBER: u32 = 3;
pub const TYPE: u32 = 4;
pub const VARIABLE: u32 = 5;
pub const PROPERTY: u32 = 6;
pub const NAMESPACE: u32 = 7;

const KEYWORDS: &[&str] = &["if", "else", "for", "in", "true", "false", "layout"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    /// Char-index (not byte-index) the token starts at.
    pub start: usize,
    pub len: usize,
    pub kind: u32,
}

/// A `key_char` per the real parser's grammar gotcha #6 (`nowui-syntax`):
/// style/binding identifiers may contain `-`, `_`, `.`, `/` in addition to
/// alphanumerics (compact scale classes like `w-1/2`, `py-3.5`).
fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/')
}

pub fn tokenize(source: &str) -> Vec<Token> {
    let chars: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];

        if c.is_whitespace() {
            i += 1;
            continue;
        }

        // Line comment.
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            let start = i;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            tokens.push(Token { start, len: i - start, kind: COMMENT });
            continue;
        }

        // Backtick template string.
        if c == '`' {
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != '`' {
                i += 1;
            }
            if i < chars.len() {
                i += 1; // consume the closing backtick
            }
            tokens.push(Token { start, len: i - start, kind: STRING });
            continue;
        }

        // Quoted string (`Expr` string literals in `if`/`for` conditions).
        if c == '"' {
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                i += 1;
            }
            if i < chars.len() {
                i += 1;
            }
            tokens.push(Token { start, len: i - start, kind: STRING });
            continue;
        }

        // `# relative/path.nowui` import directive — rest of the line.
        if c == '#' {
            let start = i;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            tokens.push(Token { start, len: i - start, kind: NAMESPACE });
            continue;
        }

        // A standalone number literal (Expr comparisons, `{value: 60}`) —
        // only when not itself continuing an identifier (that case is
        // handled by the identifier scan below, e.g. `z-index-20`).
        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            if i < chars.len() && chars[i] == '.' && chars.get(i + 1).is_some_and(|c| c.is_ascii_digit()) {
                i += 1;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            tokens.push(Token { start, len: i - start, kind: NUMBER });
            continue;
        }

        // Identifier / keyword / widget kind / dotted path / compact class.
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            i += 1;
            while i < chars.len() && is_ident_continue(chars[i]) {
                i += 1;
            }
            // `variant:key` compound (`hover:bg-blue-600`) — a colon
            // immediately followed by another identifier char extends the
            // same token; a colon followed by whitespace (a `{key: value}`
            // binding) does not.
            while i < chars.len() && chars[i] == ':' && chars.get(i + 1).is_some_and(|&c| c.is_ascii_alphabetic() || c == '_') {
                i += 1;
                while i < chars.len() && is_ident_continue(chars[i]) {
                    i += 1;
                }
            }
            let text: String = chars[start..i].iter().collect();
            let kind = classify_ident(&text);
            tokens.push(Token { start, len: i - start, kind });
            continue;
        }

        // Punctuation/operators — deliberately un-highlighted (see module docs).
        i += 1;
    }

    tokens
}

fn classify_ident(text: &str) -> u32 {
    if KEYWORDS.contains(&text) {
        return KEYWORD;
    }
    if text.starts_with(|c: char| c.is_ascii_uppercase()) {
        return TYPE;
    }
    // A real dotted path (`state.count`, `x.label`) has a letter/`_`
    // starting the segment right after some `.` — a decimal point in a
    // compact Tailwind class (`py-3.5`) never does (the segment after it is
    // just more digits), so this tells the two apart without needing to
    // know anything about the surrounding grammar.
    let looks_like_a_path = text.split('.').skip(1).any(|seg| seg.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_'));
    if looks_like_a_path {
        VARIABLE
    } else {
        PROPERTY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<(String, u32)> {
        tokenize(source).into_iter().map(|t| (source.chars().skip(t.start).take(t.len).collect(), t.kind)).collect()
    }

    #[test]
    fn line_comment_is_one_token() {
        assert_eq!(kinds("// hello\nText"), vec![("// hello".to_string(), COMMENT), ("Text".to_string(), TYPE)]);
    }

    #[test]
    fn backtick_string_including_empty_ones() {
        assert_eq!(kinds("`hello`"), vec![("`hello`".to_string(), STRING)]);
        assert_eq!(kinds("``"), vec![("``".to_string(), STRING)]);
    }

    #[test]
    fn quoted_expr_string_literal() {
        assert_eq!(kinds(r#""admin""#), vec![(r#""admin""#.to_string(), STRING)]);
    }

    #[test]
    fn import_directive_is_one_namespace_token() {
        assert_eq!(kinds("# widgets/Badge.nowui"), vec![("# widgets/Badge.nowui".to_string(), NAMESPACE)]);
    }

    #[test]
    fn widget_kind_is_type_style_key_is_property_state_path_is_variable() {
        assert_eq!(
            kinds("Text text-lg {value: state.username}"),
            vec![
                ("Text".to_string(), TYPE),
                ("text-lg".to_string(), PROPERTY),
                ("value".to_string(), PROPERTY),
                ("state.username".to_string(), VARIABLE),
            ]
        );
    }

    #[test]
    fn keywords_are_recognized() {
        assert_eq!(
            kinds("if state.x { } else { }"),
            vec![
                ("if".to_string(), KEYWORD),
                ("state.x".to_string(), VARIABLE),
                ("else".to_string(), KEYWORD),
            ]
        );
    }

    #[test]
    fn compact_fraction_and_decimal_classes_stay_one_token() {
        assert_eq!(kinds("w-1/2"), vec![("w-1/2".to_string(), PROPERTY)]);
        assert_eq!(kinds("py-3.5"), vec![("py-3.5".to_string(), PROPERTY)]);
    }

    #[test]
    fn variant_prefixed_style_key_is_one_token() {
        assert_eq!(kinds("hover:bg-blue-600"), vec![("hover:bg-blue-600".to_string(), PROPERTY)]);
    }

    #[test]
    fn binding_colon_with_space_does_not_merge_key_and_value() {
        assert_eq!(
            kinds("{onClick: state.save}"),
            vec![("onClick".to_string(), PROPERTY), ("state.save".to_string(), VARIABLE)]
        );
    }

    #[test]
    fn standalone_number_in_a_comparison() {
        assert_eq!(
            kinds("state.count == 3"),
            vec![("state.count".to_string(), VARIABLE), ("3".to_string(), NUMBER)]
        );
    }
}
