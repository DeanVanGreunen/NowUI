//! Abstract syntax tree for the NowUI file format.
//!
//! A file is a list of top-level `LayoutDef`s. A layout definition is a
//! reusable, parameterized widget (see `params`). Referencing a definition by
//! name inside another layout's body is a *use*, represented as a `Widget`
//! whose `kind` matches a definition name — it is expanded in the semantic
//! pass (see nowui-runtime::semantic).

/// A byte-offset range into the source text a node/token was parsed from.
/// Additive editor-tooling metadata — nothing in the parser or grammar
/// depends on it, and it carries no meaning across files (see `FileId` in
/// nowui-runtime's loader for how spans are disambiguated across
/// `#`-imports). `Default` yields `0..0`, used by any construction site that
/// synthesizes a node with no real source (tests, loop-var substitution
/// fallbacks) rather than a real parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

/// A top-level node. Either a reusable layout definition or a widget instance.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    /// `layout: Name(params...) styles... { bindings } { children }`
    LayoutDef {
        name: String,
        params: Vec<Param>,
        styles: Vec<StylePair>,
        bindings: Vec<Binding>,
        children: Vec<Node>,
    },
    /// `# relative/path/to/File.nowui` — a whole-file import. The referenced
    /// file's top-level `LayoutDef`s become usable in this file as if they'd
    /// been defined here. Resolved by `nowui-runtime`'s loader (I/O and path
    /// resolution live there, not in this crate).
    Import {
        path: String,
    },
    /// A primitive (`Text`, `TextInput`, ...) OR a use of a `LayoutDef`.
    Widget {
        kind: String,
        /// `name=value` args passed at a use site: `Login theme=`dark``.
        args: Vec<NamedArg>,
        /// Positional backtick literals. Empty ones are preserved.
        string_args: Vec<Template>,
        styles: Vec<StylePair>,
        bindings: Vec<Binding>,
        children: Vec<Node>,
        /// The whole widget's own byte range, `kind` through its last
        /// trailing block. Used by editor tooling (click-to-select a
        /// rendered node, then locate/patch its source) — see nowui-designer.
        span: Span,
    },
    /// `if EXPR { ... } else if EXPR { ... } else { ... }` — `branches` is
    /// the `if` condition plus every `else if`, in source order; `else_branch`
    /// is empty when there's no trailing `else`. Which branch (if any) is
    /// live is re-evaluated against *live* state every time the enclosing
    /// dynamic region refreshes — not decided once at parse time. See
    /// `nowui-runtime`'s `dynamic.rs`.
    If {
        branches: Vec<(Expr, Vec<Node>)>,
        else_branch: Vec<Node>,
    },
    /// `for IDENT in EXPR { ... }` — `body` is re-expanded once per item in
    /// the list `iter` resolves to, with `${IDENT}` in a backtick
    /// substituted for that item's value. `var` is a bare loop-local name,
    /// not rooted at `state`. See `nowui-runtime`'s `dynamic.rs`.
    For {
        var: String,
        iter: Expr,
        body: Vec<Node>,
    },
}

/// A boolean/comparison expression — an `if`'s condition or a `for`'s
/// iterable. Deliberately small and non-Turing-complete (see CLAUDE.md):
/// literals, dotted paths, comparisons, `&&`/`||`, unary `!`. No arithmetic
/// operators — nothing in the language needs them yet, and adding them is a
/// mechanical extension of this same enum if that changes.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Bool(bool),
    Number(f64),
    Str(String),
    /// A dotted path: `state.username`, or a bare `for` loop variable name
    /// like `x`. Resolving it — state lookup vs. loop-variable substitution,
    /// and the special `.length` pseudo-property — is nowui-runtime's job;
    /// this crate just records the segments.
    Path(Vec<String>),
    Not(Box<Expr>),
    Cmp(Box<Expr>, CmpOp, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    /// `cond ? then : else` — the loosest-binding operator (parsed after
    /// `||`), so `a || b ? c : d` parses as `(a || b) ? c : d`, matching
    /// how most C-family languages precedence-order the two. Currently only
    /// reachable from a backtick template's `${...}` interpolation (see
    /// `TplPart::Expr`) — `if`/`for` conditions can syntactically contain
    /// one too (this is the same shared `expr()` parser), but nothing
    /// downstream gives that a meaningful use yet.
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// A layout parameter, optionally with a default value.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub default: Option<BindValue>,
}

/// A `name=value` argument supplied at a use site.
#[derive(Debug, Clone, PartialEq)]
pub struct NamedArg {
    pub name: String,
    pub value: BindValue,
}

/// A generic Tailwind-style style token: `key-[value]` or a bare flag `key`.
#[derive(Debug, Clone, PartialEq)]
pub struct StylePair {
    pub key: String,
    /// Empty string for bare flags like `grid`.
    pub value: String,
    /// This token's own byte range — lets the inspector replace/remove just
    /// this one `key-[value]` without touching the rest of the widget line.
    pub span: Span,
}

/// A `key: value` entry inside a `{ ... }` bindings block.
#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    pub key: String,
    pub value: BindValue,
    /// This binding's own byte range (`key: value`), same purpose as
    /// `StylePair::span`.
    pub span: Span,
}

/// The value side of a binding or named arg.
#[derive(Debug, Clone, PartialEq)]
pub enum BindValue {
    /// A possibly-dotted path: `state.username` -> `["state", "username"]`.
    Path(Vec<String>),
    Bool(bool),
    Number(f64),
    Str(String),
}

/// A string or identifier containing `${var}` interpolation, resolved at
/// runtime against application state (keeps the retained tree re-resolvable
/// without re-parsing).
#[derive(Debug, Clone, PartialEq)]
pub struct Template {
    pub parts: Vec<TplPart>,
}

impl Template {
    /// True if the template has no parts (an empty `` `` `` literal).
    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    /// Flatten to a display string, leaving `${var}`/`${expr}` markers
    /// intact. Used only where a raw form is convenient (e.g. binding
    /// string values) — never resolved output.
    pub fn render_flat(&self) -> String {
        self.parts
            .iter()
            .map(|p| match p {
                TplPart::Lit(s) => s.clone(),
                TplPart::Var(v) => format!("${{{v}}}"),
                TplPart::Expr(_) => "${...}".to_string(),
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TplPart {
    Lit(String),
    /// A bare dotted path (`${state.counter.count}`) — kept as its own
    /// variant, distinct from the more general `Expr` below, purely so the
    /// overwhelmingly common case keeps its existing simple `Vec<String>`
    /// shape everywhere downstream (`nowui-core`'s own `TemplatePart::Var`
    /// mirrors it directly) instead of every consumer having to pattern-
    /// match out a trivial `Expr::Path` just to get the same thing.
    Var(String),
    /// Anything else a backtick's `${...}` can hold once it's a full
    /// `expr()` — currently only reachable via a ternary
    /// (`${cond ? "a" : "b"}`, see `Expr::Ternary`), since that's the only
    /// non-bare-path form `interp()` actually produces. `nowui-core` can't
    /// hold a raw `Expr` (see its own "no chumsky"/no-`nowui-syntax`-
    /// dependency hard rule), so a template containing one of these is kept
    /// as this original, un-lowered form in `nowui-runtime`'s own
    /// `Semantic::template_exprs` side table instead of being lowered into
    /// `nowui_core::TemplatePart` — see that field's own doc comment.
    Expr(Expr),
}
