//! The designer's own chrome — dogfooded in `.nowui`, bundled into the
//! binary (`resources/designer.nowui`, embedded via `include_str!`, no disk
//! access at runtime — same convention `#[nowui(view(...))]`-bundled apps
//! use, just done by hand here since this crate isn't itself a
//! `NowUiState`). Bound to `state::DesignerState` (currently just the
//! scanned project tree, for the explorer's own recursive `TreeView`).

use nowui_core::{NodeId, NodeKind, NowUiState, Rect, Ui};
use nowui_runtime::semantic::Semantic;

/// Matches `nowui-runtime`'s own private `CHEVRON_COLOR` (Tailwind
/// gray-700) — duplicated here because the designer builds its `Ui` via
/// `Semantic::build` directly rather than `nowui_runtime::run_ast`, so it
/// must populate `Ui::chevron_up`/`chevron_down`/`chevron_right` itself.
const CHEVRON_COLOR: [u8; 4] = [0x37, 0x41, 0x51, 255];

pub struct Chrome {
    pub ui: Ui,
    semantic: Semantic,
    /// The new-file/-folder name prompt — the *first* `TextInput` found
    /// anywhere in the built tree (it sits in the sidebar, the first
    /// column — see `resources/designer.nowui`'s own comment: a documented
    /// simplification, not a dedicated marker).
    pub new_item_node: NodeId,
    /// The raw-source editor — the *second* `TextInput` found.
    pub editor_node: NodeId,
}

impl Chrome {
    pub fn load(state: &dyn NowUiState) -> Result<Self, String> {
        let src = include_str!("../resources/designer.nowui");
        let ast = nowui_syntax::parse(src).map_err(|errors| format!("designer.nowui failed to parse: {errors:?}"))?;
        let mut semantic = Semantic::new(&ast);
        let mut ui = semantic.build("App", state).ok_or_else(|| "designer.nowui has no `layout: App`".to_string())?;
        // See `run_ast`'s own equivalent population in nowui-runtime/src/lib.rs —
        // `nowui-core` can't rasterize these itself (no chumsky/tiny-skia/vello
        // hard rule), so whichever harness builds the `Ui` must hand it
        // already-rasterized chevron glyphs, or paint falls back to the
        // plain-square/triangle glyph (the "black box" the explorer used to show).
        ui.chevron_up = nowui_icons::icon_frame("FaChevronUp", CHEVRON_COLOR).ok();
        ui.chevron_down = nowui_icons::icon_frame("FaChevronDown", CHEVRON_COLOR).ok();
        ui.chevron_right = nowui_icons::icon_frame("FaChevronRight", CHEVRON_COLOR).ok();
        ui.icon_add_file = nowui_icons::icon_frame("FaFileCirclePlus", CHEVRON_COLOR).ok();
        ui.icon_add_folder = nowui_icons::icon_frame("FaFolderPlus", CHEVRON_COLOR).ok();
        let mut text_inputs = ui.nodes.iter().enumerate().filter(|(_, n)| matches!(n.kind, NodeKind::TextInput { .. })).map(|(i, _)| NodeId(i as u32));
        let new_item_node = text_inputs.next().ok_or_else(|| "designer.nowui has no TextInput for the new-item prompt".to_string())?;
        let editor_node = text_inputs.next().ok_or_else(|| "designer.nowui has no TextInput for the editor".to_string())?;
        Ok(Chrome { ui, semantic, new_item_node, editor_node })
    }

    /// Overwrite the editor's own buffer — used once at startup to seed it
    /// with the opened file's real content (`resources/designer.nowui`'s
    /// own `TextInput` starts with an empty/placeholder value).
    pub fn set_editor_text(&mut self, text: &str) {
        if let NodeKind::TextInput { label, cursor, selection_anchor, .. } = &mut self.ui.get_mut(self.editor_node).kind {
            *label = text.to_string();
            *cursor = nowui_core::text_input::char_len(label);
            *selection_anchor = None;
        }
        self.update_editor_highlighting();
    }

    /// Re-tokenizes the editor's own current buffer (`editor::
    /// compute_highlight_spans`, the `nowui-lsp` tokenizer called
    /// in-process) and writes the result into its `highlight_spans` —
    /// called after every edit, same "cheap enough to redo whole" precedent
    /// `nowui-lsp` itself already sets for full-document re-tokenization on
    /// every keystroke.
    pub fn update_editor_highlighting(&mut self) {
        let spans = crate::editor::compute_highlight_spans(self.editor_text());
        if let NodeKind::TextInput { highlight_spans, .. } = &mut self.ui.get_mut(self.editor_node).kind {
            *highlight_spans = spans;
        }
    }

    /// The editor's own current buffer content.
    pub fn editor_text(&self) -> &str {
        match &self.ui.get(self.editor_node).kind {
            NodeKind::TextInput { label, .. } => label,
            _ => "",
        }
    }

    /// The new-item prompt's own current buffer content (the name being
    /// typed for a new file/folder).
    pub fn new_item_text(&self) -> &str {
        match &self.ui.get(self.new_item_node).kind {
            NodeKind::TextInput { label, .. } => label,
            _ => "",
        }
    }

    /// Overwrite the new-item prompt's own buffer — used to clear it back
    /// to empty once a creation is confirmed/cancelled, or focused with a
    /// blank buffer when "+ File"/"+ Folder" is clicked.
    pub fn set_new_item_text(&mut self, text: &str) {
        if let NodeKind::TextInput { label, cursor, selection_anchor, .. } = &mut self.ui.get_mut(self.new_item_node).kind {
            *label = text.to_string();
            *cursor = nowui_core::text_input::char_len(label);
            *selection_anchor = None;
        }
    }

    /// Re-expand any `if`/`for` region whose underlying state actually
    /// changed (e.g. the project tree got rescanned) and re-render every
    /// `${state.path}`/`{value: ...}`/`key-[${state.path}]` binding against
    /// the current state — the same per-redraw sequence `nowui-runtime`'s
    /// own `App::redraw` runs, reused via `nowui_runtime::resolve` (see its
    /// module doc for why those were extracted into free functions). Cheap
    /// to call every frame regardless of whether anything changed —
    /// `refresh_dynamic_regions` is a no-op when a region's signature is
    /// unchanged, and the resolve passes just skip any node with nothing
    /// dynamic bound.
    ///
    /// Returns whether any region actually rebuilt this call (the same
    /// `ui.nodes.len()` signal used to gate `gc()` below) — `DesignerApp::
    /// redraw` uses this to invalidate its own `tree_item_index_cache`
    /// (a `NodeId` is only ever meaningful for as long as the `Ui` that
    /// handed it out hasn't rebuilt the region it lived in).
    pub fn refresh(&mut self, state: &dyn NowUiState) -> bool {
        // `Ui::gc`'s own doc comment: a rebuild never reuses a `NodeId`, it
        // always `push`es fresh ones — so `ui.nodes.len()` strictly grows
        // whenever (and *only* whenever) `refresh_dynamic_regions` actually
        // rebuilt at least one region this call, even if the resulting
        // *visible* node count nets out the same. That makes "did the
        // length change" a free, exact proxy for "is there anything new to
        // sweep" — skipping `gc()`'s own full-arena walk on the (typical)
        // frame where nothing rebuilt, rather than paying for it
        // unconditionally 60 times a second regardless of whether it has
        // any work to do.
        let len_before = self.ui.nodes.len();
        self.semantic.refresh_dynamic_regions(&mut self.ui, state);
        let rebuilt = self.ui.nodes.len() != len_before;
        if rebuilt {
            // Frees whatever that rebuild just orphaned (e.g. the
            // explorer's whole tree, rebuilt every time the active-file
            // highlight changes — see `Ui::gc`'s own doc comment).
            // `editor_node`/`new_item_node` are always structurally present
            // outside any dynamic region, so they're always reachable and
            // never swept.
            self.ui.gc();
        }
        nowui_runtime::resolve::resolve_values(&mut self.ui, state, None);
        nowui_runtime::resolve::resolve_dropdown_values(&mut self.ui, state);
        nowui_runtime::resolve::resolve_templates(&mut self.ui, state, &self.semantic.template_exprs);
        nowui_runtime::resolve::resolve_dynamic_styles(&mut self.ui, state);
        // Must run *before* `apply_effective_styles` — it reads `Node::
        // disabled` to decide whether to apply the `disabled:` style
        // variant (see `nowui_runtime::App::redraw`'s own identical
        // ordering, which this mirrors).
        nowui_runtime::resolve::resolve_disabled(&mut self.ui, state);
        // `resolve_dynamic_styles` (just above) only writes into each
        // node's `base_style` — `apply_effective_styles` is what actually
        // copies that (variant-overlaid) result into `style`, the field
        // `layout::solve`/`paint::paint` read. Without this, a `${state.
        // path}` bracket (a popup's own `left`/`top`, the explorer's
        // active-row `bg`/`text` highlight, ...) would resolve correctly
        // into `base_style` and then never actually show up on screen —
        // this crate's own equivalent of `nowui_runtime::App::apply_
        // dynamic_styles`, minus hover/pressed tracking and transition
        // smoothing (neither exists in this harness — see this module's
        // own doc comment on why it isn't built on `nowui_runtime::App`).
        apply_effective_styles(&mut self.ui);
        // Must run after `apply_effective_styles` — it reads each node's
        // *effective* `text_color` off `node.style`, not `base_style`.
        nowui_runtime::resolve::resolve_tree_icons(&mut self.ui);
        rebuilt
    }

    /// Every live `Button` inside the new-item/rename popup, in source
    /// order (`Cancel`, `Create`) — see `resources/designer.nowui`'s own
    /// top comment on why button clicks are now identified by *which
    /// structural region they live in* (this popup, the context menu, the
    /// tab strip, the inspector) rather than one flat, whole-app positional
    /// index: a variable-length list in one region (the tab strip, the
    /// inspector's own per-field rows) used to silently shift what every
    /// *later* button index meant.
    pub fn popup_buttons(&self) -> Vec<NodeId> {
        let root = self.ui.get(self.ui.layers[0].root);
        buttons_in(&self.ui, root.children[0])
    }

    /// The context menu's own four rows, in source order (Add Folder, Add
    /// File, Rename, Delete — always present, see `DesignerState::
    /// context_menu_add_h`'s own doc comment).
    pub fn context_menu_buttons(&self) -> Vec<NodeId> {
        let root = self.ui.get(self.ui.layers[0].root);
        buttons_in(&self.ui, root.children[1])
    }

    /// Every currently-open tab's own `Button`, in `state.tabs` order —
    /// scoped to just the tab-strip container (`App`'s 4th child's own 1st
    /// child), so its length varies freely with how many tabs are open
    /// without affecting any other region's own button indices.
    pub fn tab_strip_buttons(&self) -> Vec<NodeId> {
        let root = self.ui.get(self.ui.layers[0].root);
        let middle = self.ui.get(root.children[3]);
        buttons_in(&self.ui, middle.children[0])
    }

    /// Every inspector field row's own `Button`, in `state.inspector_fields`
    /// order — scoped to just the inspector's own container (`App`'s 5th
    /// child's own 2nd child, between the layout-picker row and the preview
    /// slot — see `resources/designer.nowui`'s own comment on that
    /// position), so it varies freely with how many fields the currently
    /// selected node has.
    pub fn inspector_field_buttons(&self) -> Vec<NodeId> {
        let root = self.ui.get(self.ui.layers[0].root);
        let right_pane = self.ui.get(root.children[4]);
        buttons_in(&self.ui, right_pane.children[1])
    }

    /// The rect the live preview should be composited into this frame —
    /// currently just "the chrome root's own last child's own last child"
    /// (that outer child is the third column's `col` wrapper, holding the
    /// layout-picker row above the actual preview slot — see
    /// `resources/designer.nowui`'s own comment on why this is recognized
    /// structurally rather than via a dedicated marker: a documented
    /// simplification for this stage, not the final design).
    pub fn preview_slot_rect(&self) -> Rect {
        let root = self.ui.get(self.ui.layers[0].root);
        let column_id = *root.children.last().expect("designer.nowui's root must have at least one child");
        let column = self.ui.get(column_id);
        let slot_id = *column.children.last().expect("the third column must have at least one child (the preview slot)");
        let slot = self.ui.get(slot_id);
        // Inset by the slot container's own padding (`resources/
        // designer.nowui`'s `p-6` breathing room around the preview) —
        // the composited document should sit inside that padding, not
        // ignore it and fill the container's full outer rect.
        slot.computed.inset(slot.style.padding)
    }
}

/// This crate's own minimal stand-in for `nowui_runtime::App::apply_
/// dynamic_styles` — copies each node's `base_style` (just updated by
/// `resolve_dynamic_styles`/`resolve_disabled`) into the *effective* `style`
/// `layout::solve`/`paint::paint` actually read, via the same `compute_
/// effective` every real app's own redraw loop uses. No hover/pressed
/// tracking and no transition smoothing — this harness doesn't model
/// either (see `Chrome`'s own module doc for why it isn't built on
/// `nowui_runtime::App` at all) — `focused` comes from `Ui::focus` (the
/// editor/new-item prompt's own caret focus) and `hovered` from a single
/// `Ui::hit_test(Ui::cursor)` call up front (both real, already-tracked
/// concepts here), so `hover:` variants work on any chrome widget, not
/// just focus ones.
///
/// Walks only the *reachable* nodes (same `Layer::root`-rooted depth-first
/// walk `Ui::gc`'s own mark phase uses), not `0..ui.nodes.len()` — that
/// used to include every orphaned tombstone `gc()` has ever swept, whose
/// count only ever grows across a session (see `Ui::gc`'s own doc comment
/// on why it never shrinks `Ui::nodes`), silently turning this into
/// slower and slower per-redraw work the longer the app ran.
fn apply_effective_styles(ui: &mut Ui) {
    let viewport_w = ui.viewport.w;
    let hovered_id = ui.hit_test(ui.cursor);

    let mut reachable = Vec::new();
    for layer in &ui.layers {
        collect_reachable(ui, layer.root, &mut reachable);
    }

    for id in reachable {
        let node = ui.get(id);
        let base = node.base_style.clone();
        let disabled = node.disabled;
        let focused = ui.focus == Some(id);
        let hovered = hovered_id == Some(id);
        let effective = nowui_core::compute_effective(&base, viewport_w, hovered, focused, false, disabled);
        ui.get_mut(id).style = effective;
    }
}

/// Depth-first `Node::children` walk from `id`, appending every node
/// visited to `out` — shared by `apply_effective_styles` above; matches
/// `Ui::gc`'s own mark-phase walk exactly (reachable there means paintable
/// here too).
fn collect_reachable(ui: &Ui, id: NodeId, out: &mut Vec<NodeId>) {
    out.push(id);
    for &child in &ui.get(id).children {
        collect_reachable(ui, child, out);
    }
}

/// Every live `Button` in `container`'s own subtree, depth-first — the
/// scoped counterpart to `collect_reachable` the `*_buttons` accessors
/// above build on, so each structural region's own button list is found by
/// *where it lives*, not a single flat whole-app index.
fn buttons_in(ui: &Ui, container: NodeId) -> Vec<NodeId> {
    fn walk(ui: &Ui, id: NodeId, out: &mut Vec<NodeId>) {
        if matches!(ui.get(id).kind, NodeKind::Button { .. }) {
            out.push(id);
        }
        for &child in &ui.get(id).children {
            walk(ui, child, out);
        }
    }
    let mut out = Vec::new();
    walk(ui, container, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NullPainter;
    impl nowui_core::Painter for NullPainter {
        fn fill_rect(&mut self, _: Rect, _: nowui_core::Color, _: nowui_core::Edges) {}
        fn stroke_rect(&mut self, _: Rect, _: nowui_core::Color, _: f32, _: nowui_core::Edges) {}
        fn draw_text(&mut self, _: &str, _: Rect, _: &nowui_core::TextStyle) {}
        fn push_clip(&mut self, _: Rect) {}
        fn pop_clip(&mut self) {}
    }

    #[test]
    fn designer_nowui_parses_and_builds_with_a_nonzero_preview_slot() {
        let mut chrome = Chrome::load(&nowui_core::NoState).expect("resources/designer.nowui should load");
        nowui_core::layout::solve(&mut chrome.ui, nowui_core::Size::new(1200.0, 800.0), &mut NullPainter);
        let slot = chrome.preview_slot_rect();
        assert!(slot.w > 0.0 && slot.h > 0.0, "the preview slot has a real, nonzero rect once the chrome is solved");
        assert!(slot.x > 0.0, "the slot sits to the right of the sidebar, not at the window's own left edge");
    }

    #[test]
    fn refresh_copies_resolved_dynamic_styles_into_the_effective_style_not_just_base_style() {
        // `left-[${state.popup_left}]`/`top-[${state.popup_top}]` on
        // `designer.nowui`'s own new-item popup overlay — the exact bug
        // this test guards: `resolve_dynamic_styles` alone only writes
        // into `base_style`; without `apply_effective_styles` also
        // running, `style` (what `layout::solve`/`paint::paint` actually
        // read) would keep showing whatever it resolved to at the *first*
        // build, never picking up a later change — which is why the popup
        // and context menu used to render stuck in place instead of
        // parked off-screen or repositioned.
        let mut state = crate::state::DesignerState { popup_left: "-9999px".to_string(), popup_top: "-9999px".to_string(), ..Default::default() };
        let mut chrome = Chrome::load(&state).expect("resources/designer.nowui should load");
        let root = chrome.ui.get(chrome.ui.layers[0].root);
        let popup_overlay = *root.children.first().expect("App's own first child is the new-item popup overlay");

        // A `${state.path}` style bracket resolves to nothing at build time
        // (only registered as dynamic — see `Style::dynamic`'s own doc
        // comment) — an initial `refresh()` is what first applies it.
        chrome.refresh(&state);
        assert_eq!(chrome.ui.get(popup_overlay).style.left, Some(-9999.0));

        state.popup_left = "42px".to_string();
        state.popup_top = "99px".to_string();
        chrome.refresh(&state);

        assert_eq!(chrome.ui.get(popup_overlay).style.left, Some(42.0), "the effective style must reflect the new value after refresh");
        assert_eq!(chrome.ui.get(popup_overlay).style.top, Some(99.0));
    }

    #[test]
    fn set_editor_text_seeds_the_buffer_and_places_the_caret_at_the_end() {
        let mut chrome = Chrome::load(&nowui_core::NoState).expect("resources/designer.nowui should load");
        chrome.set_editor_text("layout: App { Text `hi` }");
        assert_eq!(chrome.editor_text(), "layout: App { Text `hi` }");
        let nowui_core::NodeKind::TextInput { cursor, selection_anchor, .. } = &chrome.ui.get(chrome.editor_node).kind else { panic!() };
        assert_eq!(*cursor, "layout: App { Text `hi` }".chars().count());
        assert!(selection_anchor.is_none());
    }

    #[test]
    fn refresh_renders_the_explorer_tree_from_live_state() {
        let state = crate::state::DesignerState {
            tree: vec![crate::state::VfsNode {
                name: "main.nowui".to_string(),
                path: "/project/main.nowui".to_string(),
                is_dir: false,
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut chrome = Chrome::load(&state).expect("resources/designer.nowui should load");
        chrome.refresh(&state);

        // The explorer renders `${entry.name}` as each TreeViewItem's own
        // label — walk the arena looking for the real file name, not the
        // raw "${entry.name}" placeholder (what an unresolved template
        // would leave in place — see `resolve::resolve_templates`'s own doc
        // comment).
        let found = chrome
            .ui
            .nodes
            .iter()
            .any(|n| matches!(&n.kind, nowui_core::NodeKind::TreeViewItem { label, .. } if label == "main.nowui"));
        assert!(found, "the explorer should render the real file name from live DesignerState, not a placeholder");
    }
}
