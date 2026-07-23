//! chumsky parser producing the `ast` types.
//!
//! Grammar (informal):
//!   file        := (import | layout_def)*
//!   import      := "#" <rest of line, trimmed, a relative file path>
//!   layout_def  := "layout" ":" ident params? style* bindings? "{" node* "}"
//!   params      := "(" (param ("," param)*)? ")"
//!   param       := ident ("=" bind_value)?
//!   node        := ident named_arg* string* style* bindings? child_block?
//!   named_arg   := ident "=" bind_value
//!   string      := "`" (lit | interp)* "`"
//!   style       := ident ("-" ident)* ("[" raw "]")?      // bare form = flag
//!   bindings    := "{" (binding ("," binding)*)? "}"
//!   binding     := ident ":" bind_value
//!   bind_value  := "true" | "false" | number | string | path
//!   path        := ident ("." ident)*
//!   child_block := "{" node* "}"

use crate::ast::*;
use chumsky::prelude::*;

/// Whitespace + `//` line comments, used everywhere `.padded()` was.
fn pad<T>(
    p: impl Parser<char, T, Error = Simple<char>> + Clone,
) -> impl Parser<char, T, Error = Simple<char>> + Clone {
    let comment = just("//")
        .then(take_until(text::newline().or(end())))
        .ignored();
    let ws = filter(|c: &char| c.is_whitespace()).ignored();
    let skip = comment.or(ws).repeated();
    p.padded_by(skip)
}

/// Parse a full file into its list of top-level layout definitions.
pub fn parse(src: &str) -> Result<Vec<Node>, Vec<Simple<char>>> {
    file_parser().parse(src)
}

fn file_parser() -> impl Parser<char, Vec<Node>, Error = Simple<char>> {
    pad(choice((import_directive(), layout_def())))
        .repeated()
        .then_ignore(end())
}

/// `# relative/path/to/File.nowui` — only valid at the top level, between
/// layout defs. Everything after `#` up to the newline is the path, trimmed;
/// resolving it (reading the file, joining the relative path, cycle
/// detection) is the loader's job in `nowui-runtime`, not this crate's.
fn import_directive() -> impl Parser<char, Node, Error = Simple<char>> + Clone {
    just('#')
        .ignore_then(take_until(text::newline().or(end())))
        .map(|(chars, _): (Vec<char>, _)| Node::Import { path: chars.into_iter().collect::<String>().trim().to_string() })
}

/// `${name}`
fn interp() -> impl Parser<char, TplPart, Error = Simple<char>> + Clone {
    just("${")
        .ignore_then(text::ident())
        .then_ignore(just('}'))
        .map(TplPart::Var)
}

/// A backtick string literal with embedded `${...}`. Empty `` is allowed.
fn template_str() -> impl Parser<char, Template, Error = Simple<char>> + Clone {
    let lit = filter(|c: &char| *c != '`' && *c != '$')
        .repeated()
        .at_least(1)
        .collect::<String>()
        .map(TplPart::Lit);
    just('`')
        .ignore_then(interp().or(lit).repeated())
        .then_ignore(just('`'))
        .map(|parts| Template { parts })
}

/// A dotted path: `state.username`.
fn path() -> impl Parser<char, BindValue, Error = Simple<char>> + Clone {
    text::ident()
        .separated_by(just('.'))
        .at_least(1)
        .map(BindValue::Path)
}

/// The value side of a binding / named arg / param default.
fn bind_value() -> impl Parser<char, BindValue, Error = Simple<char>> + Clone {
    let number = text::int(10)
        .then(just('.').ignore_then(text::digits(10)).or_not())
        .map(|(int, frac): (String, Option<String>)| {
            let s = match frac {
                Some(f) => format!("{int}.{f}"),
                None => int,
            };
            BindValue::Number(s.parse().unwrap())
        });

    choice((
        just("true").to(BindValue::Bool(true)),
        just("false").to(BindValue::Bool(false)),
        number,
        template_str().map(|t| BindValue::Str(t.render_flat())),
        path(),
    ))
}

/// A single `key-[value]` style token, or a bare `key` flag.
fn style() -> impl Parser<char, StylePair, Error = Simple<char>> + Clone {
    // `/` and `.` are included so fraction (`w-1/2`) and decimal-scale
    // (`py-3.5`) tokens join into one segment. Neither appears as a key's
    // first character, so they can't feed the sibling-swallowing ambiguity
    // `key_start` guards against below.
    let key_char = filter(|c: &char| c.is_alphanumeric() || *c == '_' || *c == '/' || *c == '.');
    let segment = key_char.repeated().at_least(1).collect::<String>();

    // Style keys are lowercase by convention (`grid`, `bg-color`, ...), while widget
    // `kind` idents are Capitalized (`Text`, `Bar`, ...). Requiring a lowercase/`_`
    // first character keeps a bare flag from swallowing the next sibling's kind ident
    // when a node's style list ends with no bracketed value to terminate it on.
    let key_start = filter(|c: &char| *c == '_' || (c.is_alphabetic() && c.is_lowercase()));
    let first_segment = key_start
        .then(key_char.repeated())
        .map(|(first, rest): (char, Vec<char>)| {
            let mut s = String::new();
            s.push(first);
            s.extend(rest);
            s
        });

    let dash_seg = just('-')
        .ignore_then(key_char.rewind())
        .ignore_then(segment.clone());

    let base_key = first_segment
        .clone()
        .then(dash_seg.repeated())
        .map(|(first, rest): (String, Vec<String>)| {
            let mut s = first;
            for seg in rest {
                s.push('-');
                s.push_str(&seg);
            }
            s
        });

    // Optional `variant:` prefix (`hover:`, `focus:`, `active:`, `sm:`, `md:`, `lg:`,
    // `xl:`, `2xl:`, ...). Uses the permissive `segment` (not `first_segment`) since
    // breakpoint names like `2xl` start with a digit. Kept as a plain string prefix on
    // the key — the parser stays dumb; resolving what a variant means is the semantic
    // pass's job.
    let variant = segment.clone().then_ignore(just(':'));
    let key = variant.or_not().then(base_key).map(|(variant, base): (Option<String>, String)| {
        match variant {
            Some(v) => format!("{v}:{base}"),
            None => base,
        }
    });

    let value = just('-')
        .or_not()
        .ignore_then(
            filter(|c| *c != ']')
                .repeated()
                .collect::<String>()
                .delimited_by(just('['), just(']')),
        )
        .or_not();

    key.then(value)
        .map(|(key, value)| StylePair {
            key,
            value: value.map(|v| v.trim().to_string()).unwrap_or_default(),
        })
        .padded()
}

/// A `{ key: value, ... }` bindings block.
fn bindings() -> impl Parser<char, Vec<Binding>, Error = Simple<char>> + Clone {
    text::ident()
        .then_ignore(pad(just(':')))
        .then(bind_value())
        .map(|(key, value)| Binding { key, value })
        .separated_by(pad(just(',')))
        .allow_trailing()
        .delimited_by(pad(just('{')), pad(just('}')))
}

enum Trailer {
    Bindings(Vec<Binding>),
    Children(Vec<Node>),
}

/// A widget instance (recursive: may hold a `{ ... }` child block).
fn node() -> impl Parser<char, Node, Error = Simple<char>> + Clone {
    recursive(|node| {
        let named_arg = text::ident()
            .then_ignore(just('=').padded())
            .then(bind_value())
            .map(|(name, value)| NamedArg { name, value })
            .padded();

        let child_block = pad(node.clone())
            .repeated()
            .delimited_by(pad(just('{')), pad(just('}')));

        let trailer = choice((
            bindings().map(Trailer::Bindings),
            child_block.map(Trailer::Children),
        ))
        .or_not();

        text::ident() // kind
            .then(named_arg.repeated())
            .then(template_str().padded().repeated())
            .then(style().repeated())
            .then(trailer)
            .map(|((((kind, args), string_args), styles), trailer)| {
                let (bindings, children) = match trailer {
                    Some(Trailer::Bindings(b)) => (b, Vec::new()),
                    Some(Trailer::Children(c)) => (Vec::new(), c),
                    None => (Vec::new(), Vec::new()),
                };
                Node::Widget { kind, args, string_args, styles, bindings, children }
            })
            .padded()
    })
}

/// `layout: Name(params) styles { bindings } { children }`
fn layout_def() -> impl Parser<char, Node, Error = Simple<char>> + Clone {
    let param = text::ident()
        .then(just('=').padded().ignore_then(bind_value()).or_not())
        .map(|(name, default)| Param { name, default });

    let params = param
        .separated_by(just(',').padded())
        .allow_trailing()
        .delimited_by(just('(').padded(), just(')').padded());

    pad(just("layout")
        .then_ignore(just(':').padded())
        .ignore_then(text::ident().padded())
        .then(params.or_not())
        .then(style().repeated())
        .then(bindings().or_not())
        .then(
            pad(node())
                .repeated()
                .delimited_by(pad(just('{')), pad(just('}'))),
        )
        .map(|((((name, params), styles), bindings), children)| Node::LayoutDef {
            name,
            params: params.unwrap_or_default(),
            styles,
            bindings: bindings.unwrap_or_default(),
            children,
        })
        )
}

#[test]
fn ignores_line_comments() {
    let src = "// a comment\nlayout: T { Text `hi` } // trailing\n";
    assert!(parse(src).is_ok());
}