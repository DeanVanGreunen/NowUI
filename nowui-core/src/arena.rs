//! The retained node arena. Nodes live in a flat `Vec` and reference each other
//! by index — this sidesteps the borrow-checker pain of a recursive owned tree
//! and makes parent pointers / focus tracking cheap.

use std::collections::HashMap;

use crate::geometry::Rect;
use crate::style::Style;

/// The event/binding names the semantic pass recognizes generically on *any*
/// widget: `{onClick: ..., onMouseMove: ..., value: ...}` etc. Stored as
/// plain strings (matching the "keep the parser dumb, semantic resolves"
/// rule) so adding a new one is a one-line addition here, not a schema change.
pub const EVENT_BINDING_KEYS: &[&str] = &[
    "onMouseMove",
    "onMouseDown",
    "onMouseUp",
    "onKeyPress",
    "onKeyDown",
    "onKeyUp",
    "onClick",
    "onResize",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub u32);

/// What a node actually is, after custom-widget expansion. Only primitives
/// remain in the arena — layouts and custom widgets are expanded away.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeKind {
    /// A layout container (row/column of children).
    Container,
    Text {
        content: String,
    },
    TextInput {
        label: String,
        placeholder: String,
        masked: bool,
        /// Caret position, in **chars** (not bytes — see `text_input.rs`)
        /// into `label`; always in `0..=text_input::char_len(label)`.
        cursor: usize,
        /// `Some(a)` means a selection spans `min(a, cursor)..max(a,
        /// cursor)`; `None` means no selection, just a bare caret at
        /// `cursor`. Order-independent on purpose — shift-selecting left
        /// vs. right both just move `cursor` and leave `anchor` where the
        /// selection started, same as every mainstream text editor.
        selection_anchor: Option<usize>,
        /// In-progress IME composition text (winit `Ime::Preedit`) —
        /// spliced into the *displayed* string at `cursor` by
        /// `text_input::display_string`, but not yet part of `label` until
        /// `Ime::Commit` lands. No inner composition-cursor is tracked (a
        /// simplification — see CLAUDE.md); the caret always renders at the
        /// end of the preview while composing.
        ime_preview: String,
    },
    Button {
        label: String,
    },
    Checkbox {
        label: String,
        checked: bool,
    },
    /// A closed-box-plus-optional-open-list select control. Unlike a real
    /// browser `<select>`, the open option list isn't a floating overlay —
    /// there's no popup/layer-outside-ancestor-clips system in this engine
    /// (see CLAUDE.md) — it occupies real layout space below the box,
    /// pushing later siblings down, and is clipped like anything else if a
    /// clipping ancestor is scrolled/too small.
    Dropdown {
        placeholder: String,
        options: Vec<String>,
        selected: Option<usize>,
        open: bool,
    },
    /// A draggable value picker, `0.0..=1.0` normalized. Dragging is real,
    /// intrinsic interaction (`App` in `nowui-runtime` tracks mouse
    /// down/move/up on it directly) — independent of the generic
    /// `onMouseDown`/`onMouseMove`/`onMouseUp` bindings below, which are a
    /// separate, still-inert hook mechanism (see `Node::events`).
    Slider {
        value: f32,
    },
    /// A read-only fill indicator, `0.0..=1.0` normalized. No interaction.
    ProgressBar {
        value: f32,
    },
    /// A clickable header that expands/collapses its own `Node::children` in
    /// place (accordion-style — column-stacked beneath the header, pushing
    /// later siblings down when open) rather than as a floating popup like
    /// `Dropdown`. Its children are real arena nodes (typically `MenuItem`,
    /// but anything works), so — unlike `Dropdown`'s flat `Vec<String>`
    /// options — each one can carry its own styles/`onClick`/further
    /// children like any other widget. Self-contained `open` state, toggled
    /// on click same as `Checkbox`/`Dropdown` (`App::handle_click`); no
    /// value to write back (there's no single "selected" value the way
    /// `Dropdown` has one), so it's one-way, not two-way, bound.
    Menu {
        label: String,
        open: bool,
    },
    /// A single item inside a `Menu`'s child list — just a clickable label;
    /// a real arena node, so its styles/`onClick`/children work exactly
    /// like any other widget's (see `Node::events`), not the flattened-
    /// string mechanism `Dropdown`'s options use.
    MenuItem {
        label: String,
    },
}

#[derive(Debug, Clone)]
pub struct Node {
    pub kind: NodeKind,
    /// As resolved by the semantic pass — never mutated afterward. The source
    /// of truth `compute_effective` reads from each frame; if `style` (below)
    /// were mutated in place instead, a resting value overwritten by an
    /// in-flight transition would be lost, corrupting the *next* transition's
    /// starting point.
    pub base_style: Style,
    /// This frame's effective style (`base_style` + responsive/state variants,
    /// with transitions smoothed) — what the solver and painter actually read.
    /// Equal to `base_style` until the runtime starts animating it.
    pub style: Style,
    pub children: Vec<NodeId>,
    /// Filled by the solver each layout pass.
    pub computed: Rect,
    /// Filled by the solver for `scroll_x`/`scroll_y` containers: the union
    /// bounding size of (unscrolled) children, i.e. the full scrollable
    /// content extent — used to clamp `scroll_offset` and size the thumb.
    pub content_size: crate::geometry::Size,
    /// Runtime-only scroll pan, in pixels, along whichever axes `scroll_x`/
    /// `scroll_y` enable. Persists across frames; never touched by the
    /// solver itself — only by the wheel handler in `nowui-runtime`.
    pub scroll_offset: crate::geometry::Point,
    /// Path into app state that holds this widget's value, from a `{value:
    /// state.path}` binding — generic across `Text`, `TextInput`,
    /// `Checkbox`, `Dropdown`, `Slider`, and `ProgressBar` (any widget kind
    /// can carry one; it's simply unused if the kind doesn't read it).
    /// Parsed and stored by the semantic pass; each redraw, `nowui-runtime`'s
    /// `App::resolve_values` reads it against the live `NowUiState` and
    /// writes the result into the widget (and, on interaction, writes back
    /// the other direction — see `App::write_back_value`).
    pub value_path: Vec<String>,
    /// `{onClick: ..., onMouseMove: ..., ...}` — see `EVENT_BINDING_KEYS`.
    /// Parsed and stored generically on every widget by the semantic pass;
    /// dispatched each frame by `nowui-runtime`'s `App::dispatch_event` to
    /// the bound `NowUiState::call` path.
    pub events: HashMap<String, Vec<String>>,
    /// Per-positional-backtick-argument templates, index-aligned with the
    /// widget's original `string_args` (so `templates[0]` is the arg that
    /// became e.g. `Text.content`/`Button.label`/`TextInput.label`,
    /// `templates[1]` the next, and so on) — populated by the semantic pass
    /// only when at least one of those backticks contains a `${state.path}`
    /// interpolation; empty otherwise (the common case), so a node with no
    /// dynamic text costs nothing extra to redraw. Resolved each frame by
    /// `nowui-runtime`'s `App::resolve_templates` against the live
    /// `NowUiState`, same as `value_path` — a widget can carry both.
    pub templates: Vec<Template>,
    pub dirty: bool,
}

/// One backtick string's literal/variable parts. `Var` holds the dotted path
/// split on `.` (leading `state` segment included, stripped the same way as
/// `value_path` by `nowui-runtime`'s `state_subpath`).
#[derive(Debug, Clone, PartialEq)]
pub enum TemplatePart {
    Lit(String),
    Var(Vec<String>),
}

pub type Template = Vec<TemplatePart>;

impl Node {
    pub fn new(kind: NodeKind, style: Style) -> Self {
        Node {
            kind,
            base_style: style.clone(),
            style,
            children: Vec::new(),
            computed: Rect::default(),
            content_size: crate::geometry::Size::default(),
            scroll_offset: crate::geometry::Point::default(),
            value_path: Vec::new(),
            events: HashMap::new(),
            templates: Vec::new(),
            dirty: true,
        }
    }
}

/// A stacking layer: its own layout root, composited back-to-front. Maps onto
/// the "Photoshop layers" concept — and later, its own cached pixmap.
#[derive(Debug, Clone)]
pub struct Layer {
    pub root: NodeId,
    pub name: String,
}

/// The whole retained UI: the node arena plus the ordered layer stack.
#[derive(Debug, Clone, Default)]
pub struct Ui {
    pub nodes: Vec<Node>,
    pub layers: Vec<Layer>,
    pub focus: Option<NodeId>,
    /// Coarse dirty flag; when set, the next redraw re-solves and repaints.
    pub dirty: bool,
}

impl Ui {
    pub fn new() -> Self {
        Ui { dirty: true, ..Default::default() }
    }

    pub fn push(&mut self, node: Node) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(node);
        id
    }

    pub fn get(&self, id: NodeId) -> &Node {
        &self.nodes[id.0 as usize]
    }

    pub fn get_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id.0 as usize]
    }

    pub fn add_layer(&mut self, root: NodeId, name: impl Into<String>) {
        self.layers.push(Layer { root, name: name.into() });
    }

    /// Hit-test all layers front-to-back; returns the topmost node under `p`.
    pub fn hit_test(&self, p: crate::geometry::Point) -> Option<NodeId> {
        for layer in self.layers.iter().rev() {
            if let Some(id) = self.hit_test_node(layer.root, p) {
                return Some(id);
            }
        }
        None
    }

    /// Like `hit_test`, but returns the whole ancestor chain (root-first,
    /// deepest-last) instead of just the topmost node — e.g. so a mouse-wheel
    /// handler can walk from the cursor's deepest hit upward looking for the
    /// nearest `scroll_x`/`scroll_y` container, since there's no parent
    /// pointer on `Node` itself.
    pub fn hit_test_chain(&self, p: crate::geometry::Point) -> Vec<NodeId> {
        for layer in self.layers.iter().rev() {
            let mut chain = Vec::new();
            if self.hit_test_chain_node(layer.root, p, &mut chain) {
                return chain;
            }
        }
        Vec::new()
    }

    fn hit_test_chain_node(&self, id: NodeId, p: crate::geometry::Point, chain: &mut Vec<NodeId>) -> bool {
        let node = self.get(id);
        if !node.computed.contains(p) {
            return false;
        }
        chain.push(id);
        for &child in node.children.iter().rev() {
            if self.hit_test_chain_node(child, p, chain) {
                return true;
            }
        }
        true
    }

    /// Hit-test just `id`'s children (not `id` itself) against `p`, using the
    /// same deepest-child-wins recursion as `hit_test` — for content that
    /// lives outside its own parent's `computed` rect (e.g. a `Menu`'s
    /// floating popup: the children have real rects from `arrange_menu_
    /// popups`, but the popup rect itself isn't `id`'s own `computed`, so
    /// `hit_test` can't reach them starting from the tree root).
    pub fn hit_test_within(&self, id: NodeId, p: crate::geometry::Point) -> Option<NodeId> {
        self.get(id).children.iter().rev().find_map(|&child| self.hit_test_node(child, p))
    }

    fn hit_test_node(&self, id: NodeId, p: crate::geometry::Point) -> Option<NodeId> {
        let node = self.get(id);
        if !node.computed.contains(p) {
            return None;
        }
        // Deepest child wins.
        for &child in node.children.iter().rev() {
            if let Some(hit) = self.hit_test_node(child, p) {
                return Some(hit);
            }
        }
        Some(id)
    }
}
