//! The retained node arena. Nodes live in a flat `Vec` and reference each other
//! by index — this sidesteps the borrow-checker pain of a recursive owned tree
//! and makes parent pointers / focus tracking cheap.

use crate::geometry::Rect;
use crate::style::Style;

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
        /// Path into app state that holds this input's value, e.g. state.username.
        value_path: Vec<String>,
        masked: bool,
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
        /// Path into app state that holds the selected value, e.g. state.role.
        value_path: Vec<String>,
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
    pub dirty: bool,
}

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
