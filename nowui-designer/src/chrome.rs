//! The designer's own chrome — dogfooded in `.nowui`, bundled into the
//! binary (`resources/designer.nowui`, embedded via `include_str!`, no disk
//! access at runtime — same convention `#[nowui(view(...))]`-bundled apps
//! use, just done by hand here since this crate isn't itself a
//! `NowUiState`). Bound to `state::DesignerState` (currently just the
//! scanned project tree, for the explorer's own recursive `TreeView`).

use nowui_core::{NodeId, NodeKind, NowUiState, Rect, Ui};
use nowui_runtime::semantic::Semantic;

pub struct Chrome {
    pub ui: Ui,
    semantic: Semantic,
    /// The raw-source `TextInput` — the first one found anywhere in the
    /// built tree (see `resources/designer.nowui`'s own comment: a
    /// documented simplification, not a dedicated marker).
    pub editor_node: NodeId,
}

impl Chrome {
    pub fn load(state: &dyn NowUiState) -> Result<Self, String> {
        let src = include_str!("../resources/designer.nowui");
        let ast = nowui_syntax::parse(src).map_err(|errors| format!("designer.nowui failed to parse: {errors:?}"))?;
        let mut semantic = Semantic::new(&ast);
        let ui = semantic.build("App", state).ok_or_else(|| "designer.nowui has no `layout: App`".to_string())?;
        let editor_node = ui
            .nodes
            .iter()
            .position(|n| matches!(n.kind, NodeKind::TextInput { .. }))
            .map(|i| NodeId(i as u32))
            .ok_or_else(|| "designer.nowui has no TextInput for the editor".to_string())?;
        Ok(Chrome { ui, semantic, editor_node })
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
    pub fn refresh(&mut self, state: &dyn NowUiState) {
        self.semantic.refresh_dynamic_regions(&mut self.ui, state);
        nowui_runtime::resolve::resolve_values(&mut self.ui, state, None);
        nowui_runtime::resolve::resolve_templates(&mut self.ui, state);
        nowui_runtime::resolve::resolve_dynamic_styles(&mut self.ui, state);
    }

    /// The rect the live preview should be composited into this frame —
    /// currently just "the chrome root's own last child" (see
    /// `resources/designer.nowui`'s own comment on why: no dedicated
    /// marker mechanism yet, a documented simplification for this stage,
    /// not the final design).
    pub fn preview_slot_rect(&self) -> Rect {
        let root = self.ui.get(self.ui.layers[0].root);
        let slot = *root.children.last().expect("designer.nowui's root must have at least one child");
        self.ui.get(slot).computed
    }
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
            tree: vec![crate::state::VfsNode { name: "main.nowui".to_string(), is_dir: false, truncated: false, children: Vec::new() }],
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
