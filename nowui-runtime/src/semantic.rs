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
    NodeId, NodeKind, Position, Sizing, Style, TextAlign, Transition, Ui,
};
use nowui_syntax::ast::{BindValue, NamedArg, Node as AstNode, Param, StylePair, Template};

const MAX_EXPANSION_DEPTH: usize = 64;

pub struct Semantic {
    defs: HashMap<String, LayoutDef>,
    pub warnings: Vec<String>,
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
        Semantic { defs, warnings: Vec::new() }
    }

    /// Expand `entry` (a top-level layout name) into a fresh `Ui` with one layer
    /// per top-level child of the entry layout.
    pub fn build(&mut self, entry: &str) -> Option<Ui> {
        let def = self.defs.get(entry)?.clone();
        let mut ui = Ui::new();
        let scope = Scope::new();

        // The entry layout becomes the root container of a single layer.
        let root_style = self.resolve_styles(&def.styles, &Style::default());
        let root = ui.push(ArenaNode::new(NodeKind::Container, root_style));
        let mut kids = Vec::new();
        for child in &def.children {
            if let Some(id) = self.expand(&mut ui, child, &scope, 0) {
                kids.push(id);
            }
        }
        ui.get_mut(root).children = kids;
        ui.add_layer(root, entry);
        Some(ui)
    }

    /// Expand one AST node into the arena, returning its id (None if skipped).
    fn expand(&mut self, ui: &mut Ui, node: &AstNode, scope: &Scope, depth: usize) -> Option<NodeId> {
        if depth > MAX_EXPANSION_DEPTH {
            self.warnings.push("expansion depth exceeded (recursive layout?)".into());
            return None;
        }

        let AstNode::Widget { kind, args, string_args, styles, bindings, children } = node else {
            // A nested LayoutDef inside a body is unusual; ignore for now.
            return None;
        };

        // Is this a use of a custom layout/widget?
        if let Some(def) = self.defs.get(kind).cloned() {
            let inner = bind_scope(&def.params, args, scope);
            // Merge use-site styles over the definition's own styles.
            let base = self.resolve_styles(&def.styles, &Style::default());
            let merged = self.resolve_styles(styles, &base);
            let container = ui.push(ArenaNode::new(NodeKind::Container, merged));
            let mut kids = Vec::new();
            for c in &def.children {
                if let Some(id) = self.expand(ui, c, &inner, depth + 1) {
                    kids.push(id);
                }
            }
            ui.get_mut(container).children = kids;
            return Some(container);
        }

        // Otherwise a primitive.
        let style = self.resolve_styles(styles, &Style::default());
        let arena_kind = self.primitive(kind, string_args, bindings, scope)?;
        let id = ui.push(ArenaNode::new(arena_kind, style));

        let mut kids = Vec::new();
        for c in children {
            if let Some(cid) = self.expand(ui, c, scope, depth + 1) {
                kids.push(cid);
            }
        }
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
                let value_path = bindings
                    .iter()
                    .find(|b| b.key == "value")
                    .and_then(|b| match &b.value {
                        BindValue::Path(p) => Some(p.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                let masked = bindings
                    .iter()
                    .find(|b| b.key == "mask")
                    .map(|b| matches!(b.value, BindValue::Bool(true)))
                    .unwrap_or(false);
                Some(NodeKind::TextInput {
                    label: arg(0),
                    placeholder: arg(1),
                    value_path,
                    masked,
                })
            }
            "Dropdown" => {
                let value_path = bindings
                    .iter()
                    .find(|b| b.key == "value")
                    .and_then(|b| match &b.value {
                        BindValue::Path(p) => Some(p.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                Some(NodeKind::Dropdown {
                    placeholder: arg(0),
                    options: string_args.iter().skip(1).map(|t| t.render_flat()).collect(),
                    selected: None,
                    open: false,
                    value_path,
                })
            }
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
        let v = p.value.as_str();
        let key = p.key.as_str();

        if apply_exact(s, key, v) || apply_prefixed(s, key, v) {
            return;
        }
        self.warnings.push(format!("unknown style key `{key}`"));
    }
}

/// Exact-key matches: bare flags and the legacy `key-[value]` bracket forms
/// (kept for arbitrary values Tailwind would spell `p-[13px]`, `bg-[#fff]`, etc).
fn apply_exact(s: &mut Style, key: &str, v: &str) -> bool {
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
fn apply_prefixed(s: &mut Style, key: &str, v: &str) -> bool {
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
        let ui = sem.build("T").expect("entry layout");
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
        let ui = sem.build("T").unwrap();
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
        let ui = sem.build("T").unwrap();
        let root = ui.get(ui.layers[0].root);
        let nowui_core::NodeKind::Dropdown { placeholder, options, value_path, selected, open } =
            &ui.get(root.children[0]).kind
        else {
            panic!("expected a Dropdown node");
        };
        assert_eq!(placeholder, "Choose a role");
        assert_eq!(options, &vec!["Admin".to_string(), "Editor".to_string(), "Viewer".to_string()]);
        assert_eq!(value_path, &vec!["state".to_string(), "role".to_string()]);
        assert_eq!(*selected, None);
        assert!(!*open);
    }
}
