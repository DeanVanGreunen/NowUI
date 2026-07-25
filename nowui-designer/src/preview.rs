//! Owns one live, reloadable `.nowui` document: its `Semantic`/`Ui` pair,
//! rebuilt from disk (or in-memory overrides, for unsaved editor buffers —
//! not wired up yet, see `reload_with_overrides`) whenever the entry file or
//! anything it `#`-imports changes. No `NowUiState` binding — every open
//! document previews as `NoState`, same as the `nowui` CLI binary, since the
//! designer edits arbitrary `.nowui` files with no Rust struct behind them.
//!
//! Deliberately built on `nowui_runtime`'s lower-level pieces (`loader`,
//! `semantic::Semantic`, `nowui_core::layout`/`paint`) rather than
//! `nowui_runtime::App`/`run_path` — those own a whole `winit::EventLoop`
//! and exactly one top-level window each, which can't be called twice in
//! one process and has no reload hook. `App` (this crate's own, in `app.rs`)
//! drives one or more `PreviewDoc`s inside a single shared `EventLoop`
//! instead.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use nowui_core::{NoState, Ui};
use nowui_runtime::semantic::Semantic;

pub struct PreviewDoc {
    pub entry_path: PathBuf,
    pub ui: Ui,
    semantic: Semantic,
    /// Set on the most recent successful (re)build; a reload that fails to
    /// parse keeps showing the last good `ui`/`entry_layout` rather than
    /// blanking the preview on every keystroke of a mid-edit syntax error.
    entry_layout: String,
}

impl PreviewDoc {
    /// Load `entry_path` fresh and build its `entry_layout` (by convention,
    /// and matching every other NowUI entry point, `"App"`) into a `Ui`.
    pub fn load(entry_path: &Path, entry_layout: &str) -> Result<Self, String> {
        let mut doc = PreviewDoc {
            entry_path: entry_path.to_path_buf(),
            ui: Ui::new(),
            semantic: Semantic::new(&[]),
            entry_layout: entry_layout.to_string(),
        };
        doc.reload_with_overrides(&HashMap::new())?;
        Ok(doc)
    }

    /// Re-resolve `entry_path` (honoring `overrides` for any unsaved editor
    /// buffer — see `nowui_runtime::loader::load_and_resolve_with_overrides`)
    /// and rebuild the `Ui` from scratch. On a parse/build error, `self` is
    /// left untouched (see the struct doc comment) and the error is
    /// returned for the caller to surface (e.g. a diagnostic in the editor
    /// tab), not panicked on.
    pub fn reload_with_overrides(&mut self, overrides: &HashMap<PathBuf, String>) -> Result<(), String> {
        let ast = nowui_runtime::loader::load_and_resolve_with_overrides(&self.entry_path, overrides)?;
        let mut semantic = Semantic::new(&ast);
        let ui = semantic.build(&self.entry_layout, &NoState).ok_or_else(|| format!("no `layout: {}` found in `{}`", self.entry_layout, self.entry_path.display()))?;
        self.semantic = semantic;
        self.ui = ui;
        Ok(())
    }

    /// Every file this document transitively `#`-imports — used by the
    /// watcher (not wired up yet) to know which paths trigger a reload.
    pub fn imported_files(&self) -> Result<Vec<PathBuf>, String> {
        let (_, files) = nowui_runtime::loader::load_and_resolve_tagged(&self.entry_path)?;
        Ok(files)
    }

    /// Solve layout with the root forced to `root_rect` (`layout::
    /// solve_into` — see its own doc comment: this document composites into
    /// a region another, independently-solved chrome document's own layout
    /// defined, not a whole window), clip to it, then paint — the two-step
    /// sequence every backend (`nowui-runtime`'s `redraw_gpu`/`redraw_cpu`)
    /// already follows, just with `solve_into` instead of `solve`.
    pub fn render_into(&mut self, root_rect: nowui_core::Rect, painter: &mut dyn nowui_core::Painter) {
        nowui_core::layout::solve_into(&mut self.ui, root_rect, painter);
        painter.push_clip(root_rect);
        nowui_core::paint::paint(&self.ui, painter);
        painter.pop_clip();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nowui_designer_preview_test_{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn load_builds_a_ui_from_disk() {
        let dir = scratch_dir("load");
        let path = dir.join("main.nowui");
        fs::write(&path, "layout: App { Text `hi` }").unwrap();

        let doc = PreviewDoc::load(&path, "App").expect("should load");
        let root = doc.ui.get(doc.ui.layers[0].root);
        assert_eq!(root.children.len(), 1);
    }

    #[test]
    fn reload_with_overrides_reflects_unsaved_buffer_content() {
        let dir = scratch_dir("overrides");
        let path = dir.join("main.nowui");
        fs::write(&path, "layout: App { Text `on disk` }").unwrap();

        let mut doc = PreviewDoc::load(&path, "App").unwrap();
        let canonical = path.canonicalize().unwrap();
        let mut overrides = HashMap::new();
        overrides.insert(canonical, "layout: App { Text `unsaved` }".to_string());
        doc.reload_with_overrides(&overrides).unwrap();

        let root = doc.ui.get(doc.ui.layers[0].root);
        let nowui_core::NodeKind::Text { content } = &doc.ui.get(root.children[0]).kind else { panic!() };
        assert_eq!(content, "unsaved");
    }

    #[test]
    fn render_into_pins_the_root_to_the_given_rect_not_a_whole_window() {
        struct NullPainter;
        impl nowui_core::Painter for NullPainter {
            fn fill_rect(&mut self, _: nowui_core::Rect, _: nowui_core::Color, _: nowui_core::Edges) {}
            fn stroke_rect(&mut self, _: nowui_core::Rect, _: nowui_core::Color, _: f32, _: nowui_core::Edges) {}
            fn draw_text(&mut self, _: &str, _: nowui_core::Rect, _: &nowui_core::TextStyle) {}
            fn push_clip(&mut self, _: nowui_core::Rect) {}
            fn pop_clip(&mut self) {}
        }

        let dir = scratch_dir("render_into");
        let path = dir.join("main.nowui");
        fs::write(&path, "layout: App w-[fill] h-[fill] { Text `hi` }").unwrap();
        let mut doc = PreviewDoc::load(&path, "App").unwrap();

        let slot = nowui_core::Rect::new(40.0, 20.0, 300.0, 150.0);
        doc.render_into(slot, &mut NullPainter);

        let root = doc.ui.get(doc.ui.layers[0].root);
        assert_eq!(root.computed, slot, "the document's own root is pinned to the chrome-provided slot rect");
    }

    #[test]
    fn a_reload_that_fails_to_parse_leaves_the_last_good_ui_in_place() {
        let dir = scratch_dir("bad_reload");
        let path = dir.join("main.nowui");
        fs::write(&path, "layout: App { Text `good` }").unwrap();
        let mut doc = PreviewDoc::load(&path, "App").unwrap();

        let canonical = path.canonicalize().unwrap();
        let mut overrides = HashMap::new();
        overrides.insert(canonical, "layout: App { Text `unterminated".to_string());
        let result = doc.reload_with_overrides(&overrides);
        assert!(result.is_err());

        let root = doc.ui.get(doc.ui.layers[0].root);
        let nowui_core::NodeKind::Text { content } = &doc.ui.get(root.children[0]).kind else { panic!() };
        assert_eq!(content, "good", "the last successful build is still showing");
    }
}
