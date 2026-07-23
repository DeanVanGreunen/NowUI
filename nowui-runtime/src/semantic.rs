//! Semantic pass: AST (nowui-syntax) -> arena (nowui-core).
//!
//! Responsibilities:
//!   * resolve generic (key, value) style pairs into a `Style` struct —
//!     both the legacy `key-[value]` bracket escape hatch and the compact
//!     Tailwind-style scale classes (`p-4`, `bg-blue-500`, `grid-cols-3`, ...),
//!   * split out `variant:key` prefixes (`hover:`, `focus:`, `active:`,
//!     `sm:`/`md:`/`lg:`/`xl:`/`2xl:`) into `Style::variants`,
//!   * expand `LayoutDef` uses into their primitive bodies (custom widgets),
//!   * substitute layout params with the args passed at the use site,
//!   * report unknown style keys and undefined widget uses.
//!
//! `${var}` interpolation in templates is left unresolved here — it is resolved
//! against live application state at bind time, keeping the tree re-usable.
//!
//! Not supported (see CLAUDE.md for why): `dark:`, `group-*`/`peer-*`, and
//! stacked variants (`sm:hover:x`) — no theme or group-state model exists to
//! drive them. 3D transforms, filters/shadows, `@keyframes` animations, and
//! CSS Grid features beyond fixed tracks + span (named lines, `minmax()`,
//! `auto-fit`/`auto-fill`, dense packing) are likewise out of scope.

use std::collections::HashMap;

use nowui_core::{
    tailwind, Align, Color, Direction, Display, Easing, Edges, GridTrack, Node as ArenaNode,
    NodeId, NodeKind, NowUiState, Position, Sizing, Style, TextAlign, Transition, Ui, EVENT_BINDING_KEYS,
};
use nowui_syntax::ast::{BindValue, Expr, NamedArg, Node as AstNode, Param, StylePair, Template, TplPart};

use crate::dynamic::{self, RegionAst, RegionSignature};

const MAX_EXPANSION_DEPTH: usize = 64;

pub struct Semantic {
    defs: HashMap<String, LayoutDef>,
    pub warnings: Vec<String>,
    /// Every *top-level* `if`/`for` discovered so far — one whose ancestor
    /// chain doesn't pass through another dynamic region. Persisted across
    /// redraws (unlike a nested region's own bookkeeping — see `dynamic.rs`'s
    /// module doc) so `refresh_dynamic_regions` can skip re-expanding one
    /// whose `RegionSignature` hasn't actually changed.
    pub(crate) regions: Vec<DynamicRegion>,
    /// Every node id created by `expand` since the last `take_pending_on_load`
    /// drain — both from the initial static tree and from a `for`/`if`
    /// region's (re-)expansion. `App::dispatch_pending_on_load` drains this
    /// after each build/refresh and fires `"onLoad"` on whichever of them
    /// actually bound it; nodes with no `{onLoad: ...}` binding are pushed
    /// here too (cheaper than checking at creation time) and simply no-op in
    /// `dispatch_event`.
    pending_on_load: Vec<NodeId>,
}

/// A live top-level dynamic region: the still-unexpanded AST it came from,
/// where its currently-generated nodes sit within its parent's children
/// list, and what it last computed (to detect a real change before
/// rebuilding). See `dynamic.rs`'s module doc for the full design.
pub(crate) struct DynamicRegion {
    parent: NodeId,
    ast: RegionAst,
    /// Captured at registration time — needed to correctly re-resolve any
    /// `NamedArg`/callback-path bindings inside the region's body if it sits
    /// inside a custom-widget use (e.g. `onSubmit` bound to a param).
    scope: Scope,
    depth: usize,
    start: usize,
    len: usize,
    signature: RegionSignature,
}

#[derive(Clone)]
struct LayoutDef {
    params: Vec<Param>,
    styles: Vec<StylePair>,
    children: Vec<AstNode>,
}

/// Argument scope during expansion: param name -> value bound at the use site.
type Scope = HashMap<String, BindValue>;

impl Semantic {
    /// Build from the parsed file (its list of top-level `LayoutDef`s).
    pub fn new(file: &[AstNode]) -> Self {
        let mut defs = HashMap::new();
        for node in file {
            if let AstNode::LayoutDef { name, params, styles, children, .. } = node {
                defs.insert(
                    name.clone(),
                    LayoutDef {
                        params: params.clone(),
                        styles: styles.clone(),
                        children: children.clone(),
                    },
                );
            }
        }
        Semantic { defs, warnings: Vec::new(), regions: Vec::new(), pending_on_load: Vec::new() }
    }

    /// Drain every node id created (by `expand`) since the last call to
    /// this — called once by `nowui-runtime`'s `App` right after the initial
    /// build and after every `refresh_dynamic_regions`, to fire `onLoad`.
    pub(crate) fn take_pending_on_load(&mut self) -> Vec<NodeId> {
        std::mem::take(&mut self.pending_on_load)
    }

    /// Expand `entry` (a top-level layout name) into a fresh `Ui` with one
    /// layer per top-level child of the entry layout, against `state`'s
    /// *current* values — an `if`/`for` in `entry`'s body is expanded for
    /// real here (not left empty until the first redraw), so e.g. a `for`
    /// over a list that already has 3 items starts with 3 items' worth of
    /// nodes, not zero.
    pub fn build(&mut self, entry: &str, state: &dyn NowUiState) -> Option<Ui> {
        let def = self.defs.get(entry)?.clone();
        let mut ui = Ui::new();
        let scope = Scope::new();

        // The entry layout becomes the root container of a single layer.
        let root_style = self.resolve_styles(&def.styles, &Style::default());
        let root = ui.push(ArenaNode::new(NodeKind::Container, root_style));
        let kids = self.expand_children(&mut ui, root, &def.children, &scope, state, 0, true);
        ui.get_mut(root).children = kids;
        ui.add_layer(root, entry);
        Some(ui)
    }

    /// Expand `children` into `parent`'s children list, intercepting `if`/
    /// `for` (which expand to zero, one, or many sibling nodes, unlike every
    /// other `AstNode` variant, which is 1:1 with `expand()`) as dynamic
    /// regions instead of delegating them to `expand()` (which simply skips
    /// any `AstNode` it doesn't recognize, `If`/`For` included). `track`
    /// governs whether a region discovered here gets persisted into
    /// `self.regions` for later change-detection — `true` for "ordinary"
    /// (non-region) contexts: the root layout, a widget's own children, a
    /// custom-layout-def's children; `false` while already inside another
    /// region's own re-expansion, since nested regions are simply
    /// recomputed fresh every time their ancestor rebuilds rather than
    /// independently tracked (see `dynamic.rs`'s module doc for why).
    fn expand_children(
        &mut self,
        ui: &mut Ui,
        parent: NodeId,
        children: &[AstNode],
        scope: &Scope,
        state: &dyn NowUiState,
        depth: usize,
        track: bool,
    ) -> Vec<NodeId> {
        let mut kids = Vec::new();
        for child in children {
            match child {
                AstNode::If { branches, else_branch } => {
                    let start = kids.len();
                    let ast = RegionAst::If { branches: branches.clone(), else_branch: else_branch.clone() };
                    let (ids, signature) = self.expand_region(ui, parent, &ast, scope, state, depth);
                    let len = ids.len();
                    kids.extend(ids);
                    if track {
                        self.regions.push(DynamicRegion { parent, ast, scope: scope.clone(), depth, start, len, signature });
                    }
                }
                AstNode::For { var, iter, body } => {
                    let start = kids.len();
                    let ast = RegionAst::For { var: var.clone(), iter: iter.clone(), body: body.clone() };
                    let (ids, signature) = self.expand_region(ui, parent, &ast, scope, state, depth);
                    let len = ids.len();
                    kids.extend(ids);
                    if track {
                        self.regions.push(DynamicRegion { parent, ast, scope: scope.clone(), depth, start, len, signature });
                    }
                }
                _ => {
                    if let Some(id) = self.expand(ui, child, scope, state, depth) {
                        kids.push(id);
                    }
                }
            }
        }
        kids
    }

    /// Evaluate `ast` against `state`/`scope` right now and expand the
    /// chosen branch (`If`) or every iteration (`For`) into fresh arena
    /// nodes under `parent`. Nested `if`/`for` inside the chosen body are
    /// expanded via `expand_children` with `track: false` — see its doc.
    fn expand_region(
        &mut self,
        ui: &mut Ui,
        parent: NodeId,
        ast: &RegionAst,
        scope: &Scope,
        state: &dyn NowUiState,
        depth: usize,
    ) -> (Vec<NodeId>, RegionSignature) {
        if depth > MAX_EXPANSION_DEPTH {
            self.warnings.push("expansion depth exceeded (recursive layout, or if/for nested too deeply?)".into());
            return (Vec::new(), RegionSignature::Branch(0));
        }
        match ast {
            RegionAst::If { branches, else_branch } => {
                let mut resolve = dynamic::make_resolver(state, None);
                let chosen = branches.iter().position(|(cond, _)| dynamic::eval_bool(cond, &mut resolve));
                let body = match chosen {
                    Some(i) => &branches[i].1,
                    None => else_branch,
                };
                let ids = self.expand_children(ui, parent, body, scope, state, depth + 1, false);
                (ids, RegionSignature::Branch(chosen.unwrap_or(branches.len())))
            }
            RegionAst::For { var, iter, body } => {
                let mut resolve = dynamic::make_resolver(state, None);
                let items = dynamic::eval_expr(iter, &mut resolve)
                    .and_then(|v| v.as_list().map(<[_]>::to_vec))
                    .unwrap_or_default();
                // Only a simple dotted path (`state.rows`) gives a real slot
                // to rewrite an `{onClick: x.handleMe}` binding onto — see
                // `dynamic::substitute_loop_var`.
                let iter_path: Vec<String> = match iter {
                    Expr::Path(p) => p.clone(),
                    _ => Vec::new(),
                };

                let mut ids = Vec::new();
                let mut signature_items = Vec::with_capacity(items.len());
                for (index, item) in items.iter().enumerate() {
                    signature_items.push(dynamic::signature_string(item));
                    let substituted: Vec<AstNode> = body
                        .iter()
                        .map(|c| dynamic::substitute_loop_var(c, var, item, &iter_path, index))
                        .collect();
                    let iter_ids = self.expand_children(ui, parent, &substituted, scope, state, depth + 1, false);
                    ids.extend(iter_ids);
                }
                (ids, RegionSignature::Items(signature_items))
            }
        }
    }

    /// Re-evaluate every top-level dynamic region against `state`'s current
    /// values, rebuilding only the ones whose `RegionSignature` actually
    /// changed since last time (an unrelated redraw — a hover, a transition
    /// tick — leaves every region's nodes/`NodeId`s untouched, so e.g. a
    /// `TextInput` inside one doesn't lose focus/cursor state for no
    /// reason). Called once per redraw, before `layout::solve`, by
    /// `nowui-runtime`'s `App`.
    pub fn refresh_dynamic_regions(&mut self, ui: &mut Ui, state: &dyn NowUiState) {
        for i in 0..self.regions.len() {
            let region = &self.regions[i];
            let (parent, ast, scope, depth) = (region.parent, region.ast.clone(), region.scope.clone(), region.depth);
            // `expand_region` runs speculatively here on *every* redraw, just
            // to get a `RegionSignature` to compare — even a no-op redraw
            // (a hover, a transition tick) calls it, and its `ui.push`ed
            // nodes get thrown away below when nothing actually changed (see
            // this fn's own doc comment, and `dynamic.rs`'s module doc on
            // orphaned nodes). `expand`/`primitive`-adjacent code pushes
            // every node it creates onto `pending_on_load` unconditionally,
            // so a discarded speculative expansion must roll those back too
            // — otherwise `onLoad` would refire every single redraw instead
            // of only on a genuine rebuild.
            let on_load_mark = self.pending_on_load.len();
            let (new_ids, new_signature) = self.expand_region(ui, parent, &ast, &scope, state, depth);
            let region = &mut self.regions[i];
            if new_signature == region.signature {
                self.pending_on_load.truncate(on_load_mark);
                continue;
            }
            let (start, len) = (region.start, region.len);
            ui.get_mut(parent).children.splice(start..start + len, new_ids.iter().copied());
            region.len = new_ids.len();
            region.signature = new_signature;
        }
    }

    /// Expand one AST node into the arena, returning its id (None if
    /// skipped — including for `If`/`For`, which `expand_children` (not
    /// this function) is responsible for turning into 0, 1, or many ids).
    fn expand(&mut self, ui: &mut Ui, node: &AstNode, scope: &Scope, state: &dyn NowUiState, depth: usize) -> Option<NodeId> {
        if depth > MAX_EXPANSION_DEPTH {
            self.warnings.push("expansion depth exceeded (recursive layout?)".into());
            return None;
        }

        let AstNode::Widget { kind, args, string_args, styles, bindings, children } = node else {
            // A nested LayoutDef, or a bare If/For (handled by
            // `expand_children`, never reaches here) inside a body is
            // unusual; ignore for now.
            return None;
        };

        // Is this a use of a custom layout/widget?
        if let Some(def) = self.defs.get(kind).cloned() {
            let inner = bind_scope(&def.params, args, scope);
            // Merge use-site styles over the definition's own styles.
            let base = self.resolve_styles(&def.styles, &Style::default());
            let merged = self.resolve_styles(styles, &base);
            let container = ui.push(ArenaNode::new(NodeKind::Container, merged));
            apply_generic_bindings(ui, container, bindings);
            self.pending_on_load.push(container);
            let kids = self.expand_children(ui, container, &def.children, &inner, state, depth + 1, true);
            ui.get_mut(container).children = kids;
            return Some(container);
        }

        // Otherwise a primitive.
        let style = self.resolve_styles(styles, &Style::default());
        let arena_kind = self.primitive(kind, string_args, bindings, scope)?;
        let id = ui.push(ArenaNode::new(arena_kind, style));
        apply_generic_bindings(ui, id, bindings);
        self.pending_on_load.push(id);

        // Only worth storing (and re-rendering each frame) if at least one
        // backtick actually has a `${...}` in it — the common all-literal
        // case leaves `templates` empty, same cost as before this existed.
        if string_args.iter().any(|t| t.parts.iter().any(|p| matches!(p, TplPart::Var(_)))) {
            ui.get_mut(id).templates = string_args.iter().map(to_core_template).collect();
        }

        let kids = self.expand_children(ui, id, children, scope, state, depth + 1, true);
        ui.get_mut(id).children = kids;
        Some(id)
    }

    /// Map a primitive widget kind + its string args/bindings into a NodeKind.
    fn primitive(
        &mut self,
        kind: &str,
        string_args: &[Template],
        bindings: &[nowui_syntax::ast::Binding],
        _scope: &Scope,
    ) -> Option<NodeKind> {
        let arg = |i: usize| string_args.get(i).map(|t| t.render_flat()).unwrap_or_default();
        match kind {
            "Text" => Some(NodeKind::Text { content: arg(0) }),
            "Button" => Some(NodeKind::Button { label: arg(0) }),
            "Checkbox" => Some(NodeKind::Checkbox { label: arg(0), checked: false }),
            "TextInput" => {
                let masked = bindings
                    .iter()
                    .find(|b| b.key == "mask")
                    .map(|b| matches!(b.value, BindValue::Bool(true)))
                    .unwrap_or(false);
                Some(NodeKind::TextInput {
                    label: arg(0),
                    placeholder: arg(1),
                    masked,
                    cursor: 0,
                    selection_anchor: None,
                    ime_preview: String::new(),
                })
            }
            "Dropdown" => Some(NodeKind::Dropdown {
                placeholder: arg(0),
                options: string_args.iter().skip(1).map(|t| t.render_flat()).collect(),
                selected: None,
                open: false,
            }),
            // `value` (0..=100) is a `value:` binding like everywhere else (see
            // `apply_generic_bindings`) when it's a live state path; a literal
            // starting position is also accepted as a plain number so the
            // widget isn't stuck at the default until state binding lands.
            "Slider" => {
                let initial = literal_percent(bindings).unwrap_or(0.5);
                Some(NodeKind::Slider { value: initial })
            }
            "ProgressBar" => {
                let initial = literal_percent(bindings).unwrap_or(0.0);
                Some(NodeKind::ProgressBar { value: initial })
            }
            // `Menu` is the only primitive besides layout-defs/custom-widget
            // uses that keeps real children (typically `MenuItem`, but
            // anything works) — see `NodeKind::Menu`'s doc comment. Its
            // `children` are expanded completely normally by the caller
            // (`expand`/`expand_children` don't special-case any widget
            // `kind` for children handling); only *whether they occupy any
            // layout space or paint* is gated on `open`, in `layout.rs`/
            // `paint.rs`.
            "Menu" => Some(NodeKind::Menu { label: arg(0), open: false }),
            "MenuItem" => Some(NodeKind::MenuItem { label: arg(0) }),
            // Bare containers used directly in a body (e.g. `Card`, `Row`).
            "Card" | "Container" | "Row" | "Column" | "Bar" | "Grid" | "List" => Some(NodeKind::Container),
            other => {
                self.warnings.push(format!("unknown widget `{other}` — treated as container"));
                Some(NodeKind::Container)
            }
        }
    }

    /// Fold a list of raw style pairs onto a base style, splitting out
    /// `variant:key` prefixes into `Style::variants` along the way.
    fn resolve_styles(&mut self, pairs: &[StylePair], base: &Style) -> Style {
        let mut own_pairs = Vec::new();
        let mut hover_pairs = Vec::new();
        let mut focus_pairs = Vec::new();
        let mut active_pairs = Vec::new();
        // (min_width, pairs), one bucket per breakpoint name encountered.
        let mut responsive: Vec<(u32, Vec<StylePair>)> = Vec::new();

        for p in pairs {
            match p.key.split_once(':') {
                Some((variant, rest)) => {
                    let stripped = StylePair { key: rest.to_string(), value: p.value.clone() };
                    match variant {
                        "hover" => hover_pairs.push(stripped),
                        "focus" => focus_pairs.push(stripped),
                        "active" => active_pairs.push(stripped),
                        bp if tailwind::breakpoint(bp).is_some() => {
                            let min_w = tailwind::breakpoint(bp).unwrap();
                            match responsive.iter_mut().find(|(w, _)| *w == min_w) {
                                Some((_, list)) => list.push(stripped),
                                None => responsive.push((min_w, vec![stripped])),
                            }
                        }
                        other => {
                            self.warnings.push(format!(
                                "unsupported variant `{other}:` (no dark/group/peer/stacked-variant \
                                 state model exists) — ignoring `{other}:{rest}`"
                            ));
                        }
                    }
                }
                None => own_pairs.push(p.clone()),
            }
        }

        let mut resolved = base.clone();
        for p in &own_pairs {
            self.apply_style(&mut resolved, p);
        }

        if !hover_pairs.is_empty() {
            let mut s = resolved.clone();
            for p in &hover_pairs {
                self.apply_style(&mut s, p);
            }
            resolved.variants.hover = Some(Box::new(s));
        }
        if !focus_pairs.is_empty() {
            let mut s = resolved.clone();
            for p in &focus_pairs {
                self.apply_style(&mut s, p);
            }
            resolved.variants.focus = Some(Box::new(s));
        }
        if !active_pairs.is_empty() {
            let mut s = resolved.clone();
            for p in &active_pairs {
                self.apply_style(&mut s, p);
            }
            resolved.variants.active = Some(Box::new(s));
        }

        responsive.sort_by_key(|(w, _)| *w);
        let mut cascade = resolved.clone();
        let mut out = Vec::with_capacity(responsive.len());
        for (min_w, list) in &responsive {
            for p in list {
                self.apply_style(&mut cascade, p);
            }
            out.push((*min_w, cascade.clone()));
        }
        resolved.variants.responsive = out;

        resolved
    }

    fn apply_style(&mut self, s: &mut Style, p: &StylePair) {
        // `key-[${state.path}]`: the whole bracket is one interpolation, so
        // record the path generically instead of trying to parse it as a
        // literal (which would fail and warn as an unknown/malformed value).
        // The field it would have set is left untouched until reactivity
        // (roadmap step 6) can resolve `s.dynamic` against live state.
        if let Some(path) = dynamic_var_path(&p.value) {
            s.dynamic.insert(p.key.clone(), path);
            return;
        }

        let v = p.value.as_str();
        let key = p.key.as_str();

        if apply_exact(s, key, v) || apply_prefixed(s, key, v) {
            return;
        }
        self.warnings.push(format!("unknown style key `{key}`"));
    }
}

/// `nowui_syntax::ast::Template` (parser-side, `TplPart::Var` is a raw dotted
/// string) -> `nowui_core::Template` (runtime-side, `TemplatePart::Var` is
/// already split into path segments) — nowui-core can't depend on
/// nowui-syntax (the hard rule: no chumsky in core), so this conversion is
/// the boundary between the two.
fn to_core_template(t: &Template) -> nowui_core::Template {
    t.parts
        .iter()
        .map(|p| match p {
            TplPart::Lit(s) => nowui_core::TemplatePart::Lit(s.clone()),
            TplPart::Var(v) => nowui_core::TemplatePart::Var(v.split('.').map(str::to_string).collect()),
        })
        .collect()
}

/// `${a.b.c}` -> `Some(["a", "b", "c"])`, only when the *entire* trimmed
/// value is one interpolation — `"10${x}px"` or plain `"10px"` both return
/// `None` (the former isn't supported; the latter isn't dynamic at all).
fn dynamic_var_path(v: &str) -> Option<Vec<String>> {
    let inner = v.trim().strip_prefix("${")?.strip_suffix('}')?;
    if inner.is_empty() {
        return None;
    }
    Some(inner.split('.').map(str::to_string).collect())
}

/// Exact-key matches: bare flags and the legacy `key-[value]` bracket forms
/// (kept for arbitrary values Tailwind would spell `p-[13px]`, `bg-[#fff]`, etc).
pub(crate) fn apply_exact(s: &mut Style, key: &str, v: &str) -> bool {
    match key {
        "row" => s.direction = Direction::Row,
        "column" | "col" => s.direction = Direction::Column,
        "row-reverse" => s.direction = Direction::RowReverse,
        "column-reverse" | "col-reverse" => s.direction = Direction::ColumnReverse,
        "flex" => s.display = Display::Flow,
        "grid" => s.display = Display::Grid,

        "position-static" => s.position = Position::Static,
        "position-relative" => s.position = Position::Relative,
        "position-absolute" => s.position = Position::Absolute,
        "left" => s.left = Some(parse_px(v)),
        "right" => s.right = Some(parse_px(v)),
        "top" => s.top = Some(parse_px(v)),
        "bottom" => s.bottom = Some(parse_px(v)),
        "scroll-h" => s.scroll_x = true,
        "scroll-v" => s.scroll_y = true,
        // `TextInput multi { ... }` — wraps at the box width and treats
        // Enter as a literal newline instead of the single-line,
        // horizontally-scrolling default. Harmlessly unused on anything else.
        "multi" | "multiline" => s.multiline = true,

        "w" => s.width = parse_sizing(v),
        "h" => s.height = parse_sizing(v),
        "width" => s.width = parse_sizing(v),
        "height" => s.height = parse_sizing(v),
        "gap" => s.gap = parse_px(v),
        "gap-x" => s.gap_x = Some(parse_px(v)),
        "gap-y" => s.gap_y = Some(parse_px(v)),
        "p" | "padding" => s.padding = Edges::parse(v),
        "m" | "margin" => s.margin = Edges::parse(v),
        // Bracket-value per-side padding/margin (`px-[10px]`, `mt-[4px]`, ...).
        // The compact scale forms (`px-4`) are handled in `apply_prefixed`.
        "px" if !v.is_empty() => apply_side(&mut s.padding, "x", parse_px(v)),
        "py" if !v.is_empty() => apply_side(&mut s.padding, "y", parse_px(v)),
        "pt" if !v.is_empty() => apply_side(&mut s.padding, "t", parse_px(v)),
        "pr" if !v.is_empty() => apply_side(&mut s.padding, "r", parse_px(v)),
        "pb" if !v.is_empty() => apply_side(&mut s.padding, "b", parse_px(v)),
        "pl" if !v.is_empty() => apply_side(&mut s.padding, "l", parse_px(v)),
        "mx" if !v.is_empty() => apply_side(&mut s.margin, "x", parse_px(v)),
        "my" if !v.is_empty() => apply_side(&mut s.margin, "y", parse_px(v)),
        "mt" if !v.is_empty() => apply_side(&mut s.margin, "t", parse_px(v)),
        "mr" if !v.is_empty() => apply_side(&mut s.margin, "r", parse_px(v)),
        "mb" if !v.is_empty() => apply_side(&mut s.margin, "b", parse_px(v)),
        "ml" if !v.is_empty() => apply_side(&mut s.margin, "l", parse_px(v)),
        // `rounded-[N]` (all corners), `rounded-[a b]` (top-left/bottom-right,
        // top-right/bottom-left), or `rounded-[a b c d]` (top-left, top-right,
        // bottom-right, bottom-left) — reuses `Edges::parse`'s CSS shorthand.
        "radius" | "rounded" if !v.is_empty() => s.radius = Edges::parse(v),
        "rounded" => s.radius = Edges::all(tailwind::radius("default").unwrap()),
        "font-size" | "text-size" => s.font_size = parse_px(v),
        "font-weight" => s.font_weight = v.parse().unwrap_or(400),
        "leading" if !v.is_empty() => s.line_height = Some(parse_px(v)),
        "tracking" if !v.is_empty() => s.letter_spacing = parse_px(v),
        "opacity" if !v.is_empty() => s.opacity = parse_px(v) / 100.0,
        "z-index" if !v.is_empty() => s.z_index = v.trim().parse().unwrap_or(0),
        "border" if v.is_empty() => s.border_width = Edges::all(1.0),
        "border" => s.border_width = Edges::all(parse_px(v)),
        "border-color" => s.border_color = Color::from_hex(v),
        "transition" => {
            s.transition.get_or_insert(Transition { duration_ms: 150.0, delay_ms: 0.0, easing: Easing::InOut });
        }
        "duration" => {
            let t = s.transition.get_or_insert(default_transition());
            t.duration_ms = v.parse().unwrap_or(150.0);
        }
        "delay" => {
            let t = s.transition.get_or_insert(default_transition());
            t.delay_ms = v.parse().unwrap_or(0.0);
        }
        "ease" => {
            let t = s.transition.get_or_insert(default_transition());
            t.easing = Easing::from_token(v).unwrap_or(Easing::InOut);
        }
        "grid-cols" => {
            s.grid_template_columns = (0..v.parse().unwrap_or(1)).map(|_| GridTrack::Fr(1.0)).collect();
        }
        "grid-rows" => {
            s.grid_template_rows = (0..v.parse().unwrap_or(1)).map(|_| GridTrack::Fr(1.0)).collect();
        }
        "col-span" => s.grid_column_span = v.parse().unwrap_or(1),
        "row-span" => s.grid_row_span = v.parse().unwrap_or(1),
        "translate-x" => s.transform.translate_x = parse_px(v),
        "translate-y" => s.transform.translate_y = parse_px(v),
        "scale" => {
            let n = v.parse::<f32>().unwrap_or(100.0) / 100.0;
            s.transform.scale_x = n;
            s.transform.scale_y = n;
        }
        "scale-x" => s.transform.scale_x = v.parse::<f32>().unwrap_or(100.0) / 100.0,
        "scale-y" => s.transform.scale_y = v.parse::<f32>().unwrap_or(100.0) / 100.0,
        "rotate" => s.transform.rotate_deg = v.parse().unwrap_or(0.0),
        "skew-x" => s.transform.skew_x_deg = v.parse().unwrap_or(0.0),
        "skew-y" => s.transform.skew_y_deg = v.parse().unwrap_or(0.0),

        // Colors (legacy bracket-only aliases).
        "bg-color" | "bg" => s.bg = Color::from_hex(v),
        "text-color" | "color" => {
            if let Some(c) = Color::from_hex(v) {
                s.text_color = c;
            }
        }

        // Alignment (legacy multi-token bracket form: `align-[main cross]`).
        "align-text" => {
            s.text_align = match v {
                "center" => TextAlign::Center,
                "right" => TextAlign::Right,
                _ => TextAlign::Left,
            }
        }
        "align" => {
            let mut it = v.split_whitespace();
            if let Some(a) = it.next() {
                let al = parse_align(a);
                s.align_main = al;
                s.align_cross = it.next().map(parse_align).unwrap_or(al);
            }
        }
        "items" => s.align_cross = parse_align(v),
        "justify" => s.align_main = parse_align(v),

        _ => return false,
    }
    true
}

fn default_transition() -> Transition {
    Transition { duration_ms: 150.0, delay_ms: 0.0, easing: Easing::InOut }
}

/// Compact Tailwind scale classes (`p-4`, `bg-blue-500`, `grid-cols-3`, ...):
/// the parser folds the whole thing into `key` with an empty `value`, so this
/// dispatches on `key` prefixes. Longer/more-specific prefixes (`border-t-`,
/// `scale-x-`, `gap-x-`, ...) are checked before their shorter, generic
/// relatives (`border-`, `scale-`, `gap-`) since `strip_prefix` alone can't
/// tell "the whole rest is the token" from "there's more prefix to strip".
pub(crate) fn apply_prefixed(s: &mut Style, key: &str, v: &str) -> bool {
    if !v.is_empty() {
        // Compact classes never carry a bracket value; if one is present this
        // was already handled (or not) by `apply_exact`.
        return false;
    }

    // Padding / margin, per-side then uniform.
    for (prefix, side) in [
        ("px-", "x"), ("py-", "y"), ("pt-", "t"), ("pr-", "r"), ("pb-", "b"), ("pl-", "l"),
    ] {
        if let Some(rest) = key.strip_prefix(prefix) {
            if let Some(px) = spacing_token(rest) {
                apply_side(&mut s.padding, side, px);
                return true;
            }
        }
    }
    if let Some(rest) = key.strip_prefix("p-") {
        if let Some(px) = spacing_token(rest) {
            s.padding = Edges::all(px);
            return true;
        }
    }
    for (prefix, side) in [
        ("mx-", "x"), ("my-", "y"), ("mt-", "t"), ("mr-", "r"), ("mb-", "b"), ("ml-", "l"),
    ] {
        if let Some(rest) = key.strip_prefix(prefix) {
            if let Some(px) = spacing_token(rest) {
                apply_side(&mut s.margin, side, px);
                return true;
            }
        }
    }
    if let Some(rest) = key.strip_prefix("m-") {
        if let Some(px) = spacing_token(rest) {
            s.margin = Edges::all(px);
            return true;
        }
    }

    // Gap (x/y before the generic form).
    if let Some(rest) = key.strip_prefix("gap-x-") {
        if let Some(px) = spacing_token(rest) {
            s.gap_x = Some(px);
            return true;
        }
    }
    if let Some(rest) = key.strip_prefix("gap-y-") {
        if let Some(px) = spacing_token(rest) {
            s.gap_y = Some(px);
            return true;
        }
    }
    if let Some(rest) = key.strip_prefix("gap-") {
        if let Some(px) = spacing_token(rest) {
            s.gap = px;
            return true;
        }
    }

    // Position offsets.
    if let Some(rest) = key.strip_prefix("left-") {
        if let Some(px) = spacing_token(rest) {
            s.left = Some(px);
            return true;
        }
    }
    if let Some(rest) = key.strip_prefix("right-") {
        if let Some(px) = spacing_token(rest) {
            s.right = Some(px);
            return true;
        }
    }
    if let Some(rest) = key.strip_prefix("top-") {
        if let Some(px) = spacing_token(rest) {
            s.top = Some(px);
            return true;
        }
    }
    if let Some(rest) = key.strip_prefix("bottom-") {
        if let Some(px) = spacing_token(rest) {
            s.bottom = Some(px);
            return true;
        }
    }

    // Sizing.
    if let Some(rest) = key.strip_prefix("w-") {
        s.width = compact_sizing(rest);
        return true;
    }
    if let Some(rest) = key.strip_prefix("h-") {
        s.height = compact_sizing(rest);
        return true;
    }

    // Grid template / placement.
    if let Some(rest) = key.strip_prefix("grid-cols-") {
        s.grid_template_columns = (0..rest.parse().unwrap_or(1)).map(|_| GridTrack::Fr(1.0)).collect();
        return true;
    }
    if let Some(rest) = key.strip_prefix("grid-rows-") {
        s.grid_template_rows = (0..rest.parse().unwrap_or(1)).map(|_| GridTrack::Fr(1.0)).collect();
        return true;
    }
    if let Some(rest) = key.strip_prefix("col-span-") {
        s.grid_column_span = rest.parse().unwrap_or(1);
        return true;
    }
    if let Some(rest) = key.strip_prefix("row-span-") {
        s.grid_row_span = rest.parse().unwrap_or(1);
        return true;
    }

    // 2D transforms (specific axis forms before the bare `scale-`).
    if let Some(rest) = key.strip_prefix("translate-x-") {
        if let Some(px) = spacing_token(rest) {
            s.transform.translate_x = px;
            return true;
        }
    }
    if let Some(rest) = key.strip_prefix("translate-y-") {
        if let Some(px) = spacing_token(rest) {
            s.transform.translate_y = px;
            return true;
        }
    }
    if let Some(rest) = key.strip_prefix("scale-x-") {
        s.transform.scale_x = rest.parse::<f32>().unwrap_or(100.0) / 100.0;
        return true;
    }
    if let Some(rest) = key.strip_prefix("scale-y-") {
        s.transform.scale_y = rest.parse::<f32>().unwrap_or(100.0) / 100.0;
        return true;
    }
    if let Some(rest) = key.strip_prefix("scale-") {
        let n = rest.parse::<f32>().unwrap_or(100.0) / 100.0;
        s.transform.scale_x = n;
        s.transform.scale_y = n;
        return true;
    }
    if let Some(rest) = key.strip_prefix("rotate-") {
        s.transform.rotate_deg = rest.parse().unwrap_or(0.0);
        return true;
    }
    if let Some(rest) = key.strip_prefix("skew-x-") {
        s.transform.skew_x_deg = rest.parse().unwrap_or(0.0);
        return true;
    }
    if let Some(rest) = key.strip_prefix("skew-y-") {
        s.transform.skew_y_deg = rest.parse().unwrap_or(0.0);
        return true;
    }

    // Opacity / z-index / transition timing.
    if let Some(rest) = key.strip_prefix("opacity-") {
        if let Some(o) = tailwind::opacity(rest) {
            s.opacity = o;
            return true;
        }
    }
    if let Some(rest) = key.strip_prefix("z-index-") {
        if let Ok(z) = rest.parse() {
            s.z_index = z;
            return true;
        }
    }
    if let Some(rest) = key.strip_prefix("duration-") {
        if let Some(ms) = tailwind::duration_ms(rest) {
            s.transition.get_or_insert(default_transition()).duration_ms = ms;
            return true;
        }
    }
    if let Some(rest) = key.strip_prefix("delay-") {
        if let Some(ms) = tailwind::delay_ms(rest) {
            s.transition.get_or_insert(default_transition()).delay_ms = ms;
            return true;
        }
    }
    if key == "transition" || key.starts_with("transition-") {
        s.transition.get_or_insert(default_transition());
        return true;
    }
    if let Some(rest) = key.strip_prefix("ease-") {
        if let Some(e) = Easing::from_token(rest) {
            s.transition.get_or_insert(default_transition()).easing = e;
            return true;
        }
    }

    // Borders: per-side widths before the generic `border-`.
    for (prefix, side) in [("border-t-", "t"), ("border-r-", "r"), ("border-b-", "b"), ("border-l-", "l")] {
        if let Some(rest) = key.strip_prefix(prefix) {
            if let Some(w) = tailwind::border_width(rest) {
                apply_side(&mut s.border_width, side, w);
                return true;
            }
        }
    }
    if let Some(rest) = key.strip_prefix("border-") {
        if let Some(w) = tailwind::border_width(rest) {
            s.border_width = Edges::all(w);
            return true;
        }
        if let Some(c) = tailwind::color(rest) {
            s.border_color = Some(c);
            return true;
        }
    }

    // Radius.
    if let Some(rest) = key.strip_prefix("rounded-") {
        if let Some(r) = tailwind::radius(rest) {
            s.radius = Edges::all(r);
            return true;
        }
    }

    // Typography: size vs. weight vs. leading/tracking vs. alignment vs. color.
    if let Some(rest) = key.strip_prefix("font-") {
        if let Some(w) = tailwind::font_weight(rest) {
            s.font_weight = w;
            return true;
        }
    }
    if let Some(rest) = key.strip_prefix("leading-") {
        if let Some(lh) = tailwind::leading(rest, s.font_size) {
            s.line_height = Some(lh);
            return true;
        }
    }
    if let Some(rest) = key.strip_prefix("tracking-") {
        if let Some(ls) = tailwind::tracking(rest) {
            s.letter_spacing = ls;
            return true;
        }
    }
    if let Some(rest) = key.strip_prefix("text-") {
        match rest {
            "left" => {
                s.text_align = TextAlign::Left;
                return true;
            }
            "center" => {
                s.text_align = TextAlign::Center;
                return true;
            }
            "right" => {
                s.text_align = TextAlign::Right;
                return true;
            }
            _ => {}
        }
        if let Some((size, lh)) = tailwind::font_size(rest) {
            s.font_size = size;
            s.line_height.get_or_insert(lh);
            return true;
        }
        if let Some(c) = tailwind::color(rest) {
            s.text_color = c;
            return true;
        }
    }

    // Background / items / justify shorthands.
    if let Some(rest) = key.strip_prefix("bg-") {
        if let Some(c) = tailwind::color(rest) {
            s.bg = Some(c);
            return true;
        }
    }
    if let Some(rest) = key.strip_prefix("items-") {
        s.align_cross = parse_align(rest);
        return true;
    }
    if let Some(rest) = key.strip_prefix("justify-") {
        s.align_main = parse_align(rest);
        return true;
    }

    false
}

fn apply_side(edges: &mut Edges, side: &str, px: f32) {
    match side {
        "x" => {
            edges.left = px;
            edges.right = px;
        }
        "y" => {
            edges.top = px;
            edges.bottom = px;
        }
        "t" => edges.top = px,
        "r" => edges.right = px,
        "b" => edges.bottom = px,
        "l" => edges.left = px,
        _ => {}
    }
}

/// A spacing-scale token (`4`, `0.5`, `px`) or `[Npx]`-style raw px string.
fn spacing_token(token: &str) -> Option<f32> {
    tailwind::spacing(token).or_else(|| token.trim_end_matches("px").parse().ok())
}

fn compact_sizing(token: &str) -> Sizing {
    if token == "full" {
        Sizing::Percent(1.0)
    } else if let Some(f) = tailwind::fraction(token) {
        Sizing::Percent(f)
    } else if let Some(px) = tailwind::spacing(token) {
        Sizing::Fixed(px)
    } else {
        Sizing::Hug
    }
}

/// Extract the cross-cutting bindings every widget (primitive or custom
/// layout use) can carry, generically: `{value: state.path}` (stored on
/// `Node::value_path` — read by `Text`/`TextInput`/`Checkbox`/`Dropdown`/
/// `Slider`/`ProgressBar`, ignored by anything else) and any of
/// `EVENT_BINDING_KEYS` (stored on `Node::events`, keyed by the binding
/// name). Both are parsed and stored only — see CLAUDE.md for why nothing
/// dispatches them yet (no callback/state system exists until roadmap step 6).
fn apply_generic_bindings(ui: &mut Ui, id: NodeId, bindings: &[nowui_syntax::ast::Binding]) {
    for b in bindings {
        let BindValue::Path(path) = &b.value else { continue };
        if b.key == "value" {
            ui.get_mut(id).value_path = path.clone();
        } else if EVENT_BINDING_KEYS.contains(&b.key.as_str()) {
            ui.get_mut(id).events.insert(b.key.clone(), path.clone());
        }
    }
}

/// A literal starting position for `Slider`/`ProgressBar` — `{value: 50}`
/// (0..=100) rather than a state path. Only meaningful before state binding
/// exists; once `value_path` resolves against live state, that should win.
fn literal_percent(bindings: &[nowui_syntax::ast::Binding]) -> Option<f32> {
    bindings.iter().find(|b| b.key == "value").and_then(|b| match b.value {
        BindValue::Number(n) => Some((n as f32 / 100.0).clamp(0.0, 1.0)),
        _ => None,
    })
}

/// Bind a definition's params to the args at a use site (named; falls back to
/// param defaults; captures nothing from the outer scope by default).
fn bind_scope(params: &[Param], args: &[NamedArg], _outer: &Scope) -> Scope {
    let mut scope = Scope::new();
    for p in params {
        let supplied = args.iter().find(|a| a.name == p.name).map(|a| a.value.clone());
        if let Some(val) = supplied.or_else(|| p.default.clone()) {
            scope.insert(p.name.clone(), val);
        }
    }
    scope
}

fn parse_px(v: &str) -> f32 {
    v.trim().trim_end_matches("px").parse().unwrap_or(0.0)
}

fn parse_sizing(v: &str) -> Sizing {
    let v = v.trim();
    if v == "fill" {
        Sizing::Fill(1.0)
    } else if let Some(rest) = v.strip_prefix("fill-") {
        Sizing::Fill(rest.parse().unwrap_or(1.0))
    } else if v == "hug" {
        Sizing::Hug
    } else if v == "full" {
        Sizing::Percent(1.0)
    } else if let Some(f) = tailwind::fraction(v) {
        Sizing::Percent(f)
    } else {
        Sizing::Fixed(parse_px(v))
    }
}

fn parse_align(v: &str) -> Align {
    match v {
        "center" => Align::Center,
        "end" | "right" | "bottom" => Align::End,
        _ => Align::Start,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nowui_core::style::{Display, GridTrack, Sizing};

    fn first_child_style(src: &str) -> Style {
        let ast = nowui_syntax::parse(src).expect("should parse");
        let mut sem = Semantic::new(&ast);
        let ui = sem.build("T", &nowui_core::NoState).expect("entry layout");
        let root = ui.get(ui.layers[0].root);
        ui.get(root.children[0]).style.clone()
    }

    #[test]
    fn resolves_compact_scale_classes() {
        let s = first_child_style(
            "layout: T { Card p-4 gap-2 bg-blue-500 text-white rounded-lg font-bold { Text `hi` } }",
        );
        assert_eq!(s.padding.top, 16.0, "p-4 == 1rem == 16px");
        assert_eq!(s.gap, 8.0, "gap-2 == 0.5rem == 8px");
        assert_eq!(s.bg, Color::from_hex("#3b82f6"), "bg-blue-500");
        assert_eq!(s.text_color, Color::rgb(255, 255, 255), "text-white");
        assert_eq!(s.radius, nowui_core::Edges::all(12.0), "rounded-lg");
        assert_eq!(s.font_weight, 700, "font-bold");
    }

    #[test]
    fn resolves_grid_classes() {
        let s = first_child_style("layout: T { Card grid grid-cols-3 col-span-2 gap-4 { Text `hi` } }");
        assert_eq!(s.display, Display::Grid);
        assert_eq!(s.grid_template_columns.len(), 3);
        assert!(matches!(s.grid_template_columns[0], GridTrack::Fr(_)));
        assert_eq!(s.grid_column_span, 2);
    }

    #[test]
    fn resolves_per_corner_radius_shorthands() {
        use nowui_core::Edges;
        let all = first_child_style("layout: T { Card rounded-[6px] { Text `hi` } }");
        assert_eq!(all.radius, Edges::all(6.0));

        let diag = first_child_style("layout: T { Card rounded-[6px 12px] { Text `hi` } }");
        assert_eq!(diag.radius, Edges { top: 6.0, right: 12.0, bottom: 6.0, left: 12.0 });

        let four = first_child_style("layout: T { Card rounded-[1px 2px 3px 4px] { Text `hi` } }");
        assert_eq!(four.radius, Edges { top: 1.0, right: 2.0, bottom: 3.0, left: 4.0 });
    }

    #[test]
    fn resolves_width_fraction_and_full() {
        let s = first_child_style("layout: T { Card w-1/2 { Text `hi` } }");
        assert_eq!(s.width, Sizing::Percent(0.5));
        let s2 = first_child_style("layout: T { Card w-full { Text `hi` } }");
        assert_eq!(s2.width, Sizing::Percent(1.0));
    }

    #[test]
    fn splits_hover_variant_without_touching_other_fields() {
        let s = first_child_style(
            "layout: T { Button `Go` bg-blue-500 hover:bg-blue-600 transition duration-150 { onClick: state.go } }",
        );
        assert_eq!(s.bg, Color::from_hex("#3b82f6"));
        let hover = s.variants.hover.as_ref().expect("hover variant resolved");
        assert_eq!(hover.bg, Color::from_hex("#2563eb"));
        assert!(s.transition.is_some());
    }

    #[test]
    fn responsive_breakpoints_cascade_cumulatively() {
        let s = first_child_style("layout: T { Card w-4 sm:w-8 md:grid-cols-2 { Text `hi` } }");
        assert_eq!(s.width, Sizing::Fixed(16.0));
        let (min_w, sm) = &s.variants.responsive[0];
        assert_eq!(*min_w, 640);
        assert_eq!(sm.width, Sizing::Fixed(32.0));
        let (min_w2, md) = &s.variants.responsive[1];
        assert_eq!(*min_w2, 768);
        // md: cascades on top of sm:, so the sm width carries forward.
        assert_eq!(md.width, Sizing::Fixed(32.0));
        assert_eq!(md.grid_template_columns.len(), 2);
    }

    #[test]
    fn unsupported_variant_warns_instead_of_silently_applying() {
        let ast = nowui_syntax::parse("layout: T { Card dark:bg-black { Text `hi` } }").unwrap();
        let mut sem = Semantic::new(&ast);
        let ui = sem.build("T", &nowui_core::NoState).unwrap();
        let root = ui.get(ui.layers[0].root);
        let s = &ui.get(root.children[0]).style;
        assert_eq!(s.bg, None, "dark: has no state model, so it must not apply");
        assert!(sem.warnings.iter().any(|w| w.contains("dark")));
    }

    #[test]
    fn resolves_position_and_offset_classes() {
        use nowui_core::Position;
        let s = first_child_style(
            "layout: T { Card position-absolute top-[4px] right-4 { Text `hi` } }",
        );
        assert_eq!(s.position, Position::Absolute);
        assert_eq!(s.top, Some(4.0));
        assert_eq!(s.right, Some(16.0));

        let rel = first_child_style("layout: T { Card position-relative left-[0px] bottom-[0px] { Text `hi` } }");
        assert_eq!(rel.position, Position::Relative);
        assert_eq!(rel.left, Some(0.0));
        assert_eq!(rel.bottom, Some(0.0));
    }

    #[test]
    fn resolves_scroll_classes() {
        let s = first_child_style("layout: T { Card scroll-v scroll-h { Text `hi` } }");
        assert!(s.scroll_y);
        assert!(s.scroll_x);
    }

    #[test]
    fn resolves_multi_bare_flag_on_a_text_input() {
        let ast = nowui_syntax::parse("layout: T { TextInput `` `placeholder` multi }").unwrap();
        let mut sem = Semantic::new(&ast);
        let ui = sem.build("T", &nowui_core::NoState).unwrap();
        let root = ui.get(ui.layers[0].root);
        assert!(ui.get(root.children[0]).style.multiline);
    }

    #[test]
    fn resolves_z_index_bracket_and_compact_forms() {
        let bracket = first_child_style("layout: T { Card z-index-[5] { Text `hi` } }");
        assert_eq!(bracket.z_index, 5);
        let compact = first_child_style("layout: T { Card z-index-20 { Text `hi` } }");
        assert_eq!(compact.z_index, 20);
    }

    #[test]
    fn resolves_dropdown_placeholder_and_options() {
        let ast = nowui_syntax::parse(
            "layout: T { Dropdown `Choose a role` `Admin` `Editor` `Viewer` {value: state.role} }",
        )
        .unwrap();
        let mut sem = Semantic::new(&ast);
        let ui = sem.build("T", &nowui_core::NoState).unwrap();
        let root = ui.get(ui.layers[0].root);
        let node = ui.get(root.children[0]);
        let nowui_core::NodeKind::Dropdown { placeholder, options, selected, open } = &node.kind else {
            panic!("expected a Dropdown node");
        };
        assert_eq!(placeholder, "Choose a role");
        assert_eq!(options, &vec!["Admin".to_string(), "Editor".to_string(), "Viewer".to_string()]);
        assert_eq!(node.value_path, vec!["state".to_string(), "role".to_string()]);
        assert_eq!(*selected, None);
        assert!(!*open);
    }

    #[test]
    fn resolves_menu_with_its_own_onclick_and_menu_item_children() {
        // The exact shape a `Menu` needs and `Dropdown` doesn't: an `onClick`
        // binding on the widget itself *and* real children, each independently
        // styleable/bindable (unlike `Dropdown`'s flat `Vec<String>` options)
        // — the reason the parser's bindings/children trailer had to stop
        // being an either-or choice.
        let src = r#"layout: T {
            Menu `Preferences` w-[400px] text-center {onClick: state.menuClick} {
                MenuItem `Open Preferences` w-[400px] {onClick: state.itemClick}
            }
        }"#;
        let ast = nowui_syntax::parse(src).unwrap();
        let mut sem = Semantic::new(&ast);
        let ui = sem.build("T", &nowui_core::NoState).unwrap();
        let root = ui.get(ui.layers[0].root);
        let menu = ui.get(root.children[0]);

        let nowui_core::NodeKind::Menu { label, open } = &menu.kind else { panic!("expected a Menu node") };
        assert_eq!(label, "Preferences");
        assert!(!open, "starts closed");
        assert_eq!(menu.events.get("onClick"), Some(&vec!["state".to_string(), "menuClick".to_string()]));
        assert_eq!(menu.style.width, Sizing::Fixed(400.0));

        assert_eq!(menu.children.len(), 1);
        let item = ui.get(menu.children[0]);
        let nowui_core::NodeKind::MenuItem { label } = &item.kind else { panic!("expected a MenuItem node") };
        assert_eq!(label, "Open Preferences");
        assert_eq!(item.events.get("onClick"), Some(&vec!["state".to_string(), "itemClick".to_string()]));
    }

    #[test]
    fn resolves_slider_and_progress_bar_initial_values() {
        let ast = nowui_syntax::parse(
            "layout: T { Slider {value: 75} ProgressBar {value: 30} }",
        )
        .unwrap();
        let mut sem = Semantic::new(&ast);
        let ui = sem.build("T", &nowui_core::NoState).unwrap();
        let root = ui.get(ui.layers[0].root);
        let nowui_core::NodeKind::Slider { value } = &ui.get(root.children[0]).kind else {
            panic!("expected a Slider node");
        };
        assert!((*value - 0.75).abs() < 1e-6);
        let nowui_core::NodeKind::ProgressBar { value } = &ui.get(root.children[1]).kind else {
            panic!("expected a ProgressBar node");
        };
        assert!((*value - 0.3).abs() < 1e-6);
    }

    #[test]
    fn resolves_generic_value_and_event_bindings() {
        let ast = nowui_syntax::parse(
            "layout: T { Button `Go` {onClick: state.save, onMouseMove: state.track, value: state.count} }",
        )
        .unwrap();
        let mut sem = Semantic::new(&ast);
        let ui = sem.build("T", &nowui_core::NoState).unwrap();
        let root = ui.get(ui.layers[0].root);
        let node = ui.get(root.children[0]);
        assert_eq!(node.value_path, vec!["state".to_string(), "count".to_string()]);
        assert_eq!(node.events.get("onClick"), Some(&vec!["state".to_string(), "save".to_string()]));
        assert_eq!(node.events.get("onMouseMove"), Some(&vec!["state".to_string(), "track".to_string()]));
        assert_eq!(node.events.get("onKeyDown"), None);
    }

    #[test]
    fn dynamic_backtick_interpolation_is_recorded_as_a_template() {
        let ast = nowui_syntax::parse("layout: T { Text `Count: ${state.counter.count}` } ").unwrap();
        let mut sem = Semantic::new(&ast);
        let ui = sem.build("T", &nowui_core::NoState).unwrap();
        let root = ui.get(ui.layers[0].root);
        let node = ui.get(root.children[0]);
        assert_eq!(
            node.templates,
            vec![vec![
                nowui_core::TemplatePart::Lit("Count: ".to_string()),
                nowui_core::TemplatePart::Var(vec!["state".to_string(), "counter".to_string(), "count".to_string()]),
            ]]
        );
        // The initial (pre-resolution) content is still the raw `${...}` form —
        // `App::resolve_templates` is what substitutes it each frame.
        assert_eq!(node.kind, NodeKind::Text { content: "Count: ${state.counter.count}".to_string() });
    }

    #[test]
    fn purely_literal_backtick_leaves_templates_empty() {
        let ast = nowui_syntax::parse("layout: T { Text `Static label` }").unwrap();
        let mut sem = Semantic::new(&ast);
        let ui = sem.build("T", &nowui_core::NoState).unwrap();
        let root = ui.get(ui.layers[0].root);
        let node = ui.get(root.children[0]);
        assert!(node.templates.is_empty());
    }

    #[test]
    fn dynamic_style_var_is_recorded_and_leaves_the_field_at_its_default() {
        let s = first_child_style("layout: T { Card w-[${state.myWidth}] { Text `hi` } }");
        assert_eq!(s.dynamic.get("w"), Some(&vec!["state".to_string(), "myWidth".to_string()]));
        // Left untouched (Hug, the default) rather than parsed as a bogus literal.
        assert_eq!(s.width, Sizing::Hug);
    }

    #[test]
    fn dynamic_style_var_does_not_warn_as_an_unknown_or_malformed_value() {
        let ast = nowui_syntax::parse("layout: T { Card w-[${state.myWidth}] { Text `hi` } }").unwrap();
        let mut sem = Semantic::new(&ast);
        sem.build("T", &nowui_core::NoState).unwrap();
        assert!(sem.warnings.is_empty(), "unexpected warnings: {:?}", sem.warnings);
    }

    #[derive(Default, Clone, nowui_core::NowUiState)]
    struct DynamicTestState {
        show: bool,
        rows: Vec<i64>,
    }

    fn text_contents(ui: &Ui, ids: &[NodeId]) -> Vec<String> {
        ids.iter()
            .map(|&id| match &ui.get(id).kind {
                NodeKind::Text { content } => content.clone(),
                other => panic!("expected a Text node, got {other:?}"),
            })
            .collect()
    }

    #[test]
    fn if_picks_the_matching_branch_at_initial_build() {
        let src = "layout: T { if state.show { Text `yes` } else { Text `no` } }";
        let ast = nowui_syntax::parse(src).unwrap();
        let mut sem = Semantic::new(&ast);

        let ui = sem.build("T", &DynamicTestState { show: true, rows: Vec::new() }).unwrap();
        let root = ui.get(ui.layers[0].root);
        assert_eq!(text_contents(&ui, &root.children), vec!["yes".to_string()]);
    }

    #[test]
    fn if_falls_back_to_else_and_re_expands_on_refresh() {
        let src = "layout: T { if state.show { Text `yes` } else { Text `no` } }";
        let ast = nowui_syntax::parse(src).unwrap();
        let mut sem = Semantic::new(&ast);

        let mut state = DynamicTestState { show: false, rows: Vec::new() };
        let mut ui = sem.build("T", &state).unwrap();
        let root_id = ui.layers[0].root;
        assert_eq!(text_contents(&ui, &ui.get(root_id).children), vec!["no".to_string()]);

        state.show = true;
        sem.refresh_dynamic_regions(&mut ui, &state);
        assert_eq!(text_contents(&ui, &ui.get(root_id).children), vec!["yes".to_string()]);
    }

    #[test]
    fn refresh_is_a_noop_and_preserves_node_ids_when_nothing_changed() {
        let src = "layout: T { if state.show { Text `yes` } else { Text `no` } }";
        let ast = nowui_syntax::parse(src).unwrap();
        let mut sem = Semantic::new(&ast);

        let state = DynamicTestState { show: true, rows: Vec::new() };
        let mut ui = sem.build("T", &state).unwrap();
        let root_id = ui.layers[0].root;
        let before = ui.get(root_id).children.clone();

        sem.refresh_dynamic_regions(&mut ui, &state);

        assert_eq!(ui.get(root_id).children, before, "same NodeIds — nothing was rebuilt");
    }

    #[test]
    fn take_pending_on_load_returns_every_node_built_initially_then_drains() {
        let src = "layout: T { Container { Text `a` Text `b` } }";
        let ast = nowui_syntax::parse(src).unwrap();
        let mut sem = Semantic::new(&ast);

        let state = DynamicTestState::default();
        let ui = sem.build("T", &state).unwrap();

        let pending = sem.take_pending_on_load();
        // `ui.nodes.len() - 1`: `build()` pushes the entry layout's own root
        // container directly (not through `expand`), since it has no
        // `AstNode::Widget` of its own to carry an `onLoad` binding — every
        // other node (the `Container` and its 2 `Text` children) goes
        // through `expand` and is tracked.
        assert_eq!(pending.len(), ui.nodes.len() - 1, "every expand()-created node, container and leaves alike");
        assert!(sem.take_pending_on_load().is_empty(), "draining again after the first take returns nothing new");
    }

    #[test]
    fn take_pending_on_load_reports_freshly_created_ids_again_after_a_for_rebuild() {
        // A `for`'s region rebuild replaces *every* item's nodes with fresh
        // ids when its list signature changes (see `expand_region` — there's
        // no per-item diffing/keying), so `onLoad` genuinely refires for
        // every row, not just the newly appended one. Documented behavior,
        // not a bug: this test locks in that shape rather than a
        // finer-grained "only the new row" semantics the engine doesn't have.
        let src = "layout: T { for x in state.rows { Text `${x}` } }";
        let ast = nowui_syntax::parse(src).unwrap();
        let mut sem = Semantic::new(&ast);

        let mut state = DynamicTestState { show: false, rows: vec![1, 2] };
        let mut ui = sem.build("T", &state).unwrap();
        sem.take_pending_on_load();

        state.rows.push(3);
        sem.refresh_dynamic_regions(&mut ui, &state);
        let pending = sem.take_pending_on_load();
        assert_eq!(pending.len(), 3, "all 3 rows got fresh NodeIds, not just the appended one");

        sem.refresh_dynamic_regions(&mut ui, &state);
        assert!(sem.take_pending_on_load().is_empty(), "signature unchanged — no rebuild, nothing newly created");
    }

    #[test]
    fn for_expands_to_flat_siblings_not_wrapped_in_an_extra_container() {
        // A `for` inside a `Grid` must produce its children directly as the
        // grid's own cells, not one wrapper container occupying a single
        // cell — otherwise it'd defeat the point of a multi-column grid.
        let src = "layout: T { Grid grid grid-cols-2 { for x in state.rows { Text `a` Text `b` } } }";
        let ast = nowui_syntax::parse(src).unwrap();
        let mut sem = Semantic::new(&ast);

        let state = DynamicTestState { show: false, rows: vec![1, 2, 3] };
        let ui = sem.build("T", &state).unwrap();
        let root = ui.get(ui.layers[0].root);
        let grid = ui.get(root.children[0]);
        assert_eq!(grid.children.len(), 6, "3 items x 2 Text nodes each, flattened directly under Grid");
        for &id in &grid.children {
            assert!(matches!(ui.get(id).kind, NodeKind::Text { .. }));
        }
    }

    #[test]
    fn for_substitutes_the_loop_variable_into_backticks() {
        let src = "layout: T { for x in state.rows { Text `Row ${x}` } }";
        let ast = nowui_syntax::parse(src).unwrap();
        let mut sem = Semantic::new(&ast);

        let state = DynamicTestState { show: false, rows: vec![10, 20, 30] };
        let ui = sem.build("T", &state).unwrap();
        let root = ui.get(ui.layers[0].root);
        assert_eq!(
            text_contents(&ui, &root.children),
            vec!["Row 10".to_string(), "Row 20".to_string(), "Row 30".to_string()]
        );
    }

    #[derive(Default, Clone, nowui_core::NowUiState)]
    struct RowItem {
        label: String,
    }

    #[derive(Default, Clone, nowui_core::NowUiState)]
    struct RowListState {
        rows: Vec<RowItem>,
    }

    #[test]
    fn for_resolves_dotted_field_access_into_a_vec_of_struct_field() {
        let src = "layout: T { for x in state.rows { Text `${x.label}` } }";
        let ast = nowui_syntax::parse(src).unwrap();
        let mut sem = Semantic::new(&ast);

        let state = RowListState {
            rows: vec![
                RowItem { label: "First".to_string() },
                RowItem { label: "Second".to_string() },
            ],
        };
        let ui = sem.build("T", &state).unwrap();
        let root = ui.get(ui.layers[0].root);
        assert_eq!(text_contents(&ui, &root.children), vec!["First".to_string(), "Second".to_string()]);
    }

    #[test]
    fn for_rebuilds_when_the_list_changes_length_and_is_a_noop_when_it_does_not() {
        let src = "layout: T { for x in state.rows { Text `${x}` } }";
        let ast = nowui_syntax::parse(src).unwrap();
        let mut sem = Semantic::new(&ast);

        let mut state = DynamicTestState { show: false, rows: vec![1, 2] };
        let mut ui = sem.build("T", &state).unwrap();
        let root_id = ui.layers[0].root;
        assert_eq!(ui.get(root_id).children.len(), 2);

        // Unchanged list -> refresh is a no-op (same NodeIds).
        let before = ui.get(root_id).children.clone();
        sem.refresh_dynamic_regions(&mut ui, &state);
        assert_eq!(ui.get(root_id).children, before);

        // Longer list -> rebuilds with the new count.
        state.rows = vec![1, 2, 3, 4];
        sem.refresh_dynamic_regions(&mut ui, &state);
        assert_eq!(ui.get(root_id).children.len(), 4);
        assert_eq!(
            text_contents(&ui, &ui.get(root_id).children),
            vec!["1".to_string(), "2".to_string(), "3".to_string(), "4".to_string()]
        );
    }

    #[derive(Default, Clone, nowui_core::NowUiState)]
    struct UsernameState {
        username: String,
    }

    #[test]
    fn if_condition_using_length_pseudo_property_updates_when_the_field_changes() {
        let src = "layout: T { if state.username.length > 3 { Text `long` } else { Text `short` } }";
        let ast = nowui_syntax::parse(src).unwrap();
        let mut sem = Semantic::new(&ast);

        let mut state = UsernameState { username: String::new() };
        let mut ui = sem.build("T", &state).unwrap();
        let root_id = ui.layers[0].root;
        assert_eq!(text_contents(&ui, &ui.get(root_id).children), vec!["short".to_string()]);

        state.username = "dean".to_string();
        sem.refresh_dynamic_regions(&mut ui, &state);
        assert_eq!(
            text_contents(&ui, &ui.get(root_id).children),
            vec!["long".to_string()],
            "username.length went from 0 to 4, so the branch should flip"
        );
    }
}
