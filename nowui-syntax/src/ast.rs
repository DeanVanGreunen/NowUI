//! Abstract syntax tree for the NowUI file format.
//!
//! A file is a list of top-level `LayoutDef`s. A layout definition is a
//! reusable, parameterized widget (see `params`). Referencing a definition by
//! name inside another layout's body is a *use*, represented as a `Widget`
//! whose `kind` matches a definition name — it is expanded in the semantic
//! pass (see nowui-runtime::semantic).

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
}

/// A `key: value` entry inside a `{ ... }` bindings block.
#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    pub key: String,
    pub value: BindValue,
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

    /// Flatten to a display string, leaving `${var}` markers intact. Used only
    /// where a raw form is convenient (e.g. binding string values).
    pub fn render_flat(&self) -> String {
        self.parts
            .iter()
            .map(|p| match p {
                TplPart::Lit(s) => s.clone(),
                TplPart::Var(v) => format!("${{{v}}}"),
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TplPart {
    Lit(String),
    Var(String),
}
