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

/// Matches `nowui-runtime`'s own private `CHEVRON_COLOR` (Tailwind
/// gray-700) — see `chrome.rs`'s own copy of this constant for why every
/// `Ui` the designer builds by hand (not via `nowui_runtime::run_ast`) has
/// to populate `chevron_up`/`chevron_down`/`chevron_right` itself.
const CHEVRON_COLOR: [u8; 4] = [0x37, 0x41, 0x51, 255];

pub struct PreviewDoc {
    pub entry_path: PathBuf,
    pub ui: Ui,
    semantic: Semantic,
    /// Set on the most recent successful (re)build; a reload that fails to
    /// parse keeps showing the last good `ui`/`entry_layout` rather than
    /// blanking the preview on every keystroke of a mid-edit syntax error.
    entry_layout: String,
    /// Every top-level `layout:` name reachable from `entry_path` (its own
    /// file plus everything it transitively `#`-imports), in source order —
    /// recomputed alongside `ui`/`semantic` on every successful `reload_
    /// with_overrides`, so it's always in sync with what actually built.
    /// Lets a caller (the designer's own tab/preview UI) offer a picker
    /// when a file defines more than one `layout:`, instead of always
    /// rendering whichever `entry_layout` happens to be selected. Left
    /// untouched (not cleared) after a failed reload, same all-or-nothing
    /// "keep the last good build in place" contract `ui`/`semantic`
    /// already have.
    pub layout_names: Vec<String>,
    /// Every layout *reachable* from `layout_names`'s own first entry (by
    /// convention, and matching every other NowUI entry point, `"App"`),
    /// each listed once as `(full > path > label, layout_name)` — e.g.
    /// `("App > PageLogin", "PageLogin")`, `("App > PageLogin >
    /// ResultPopUp", "ResultPopUp")`. Computed alongside `layout_names` on
    /// every successful `reload_with_overrides`, so opening a different
    /// file (or editing the current one to add/remove/rewire a layout use)
    /// keeps it live. See `layout_hierarchy`'s own doc comment for how
    /// "reachable" is determined and how cycles (a recursive layout using
    /// itself) are handled.
    pub layout_hierarchy: Vec<(String, String)>,
}

/// Every top-level `layout:` name in `ast`, in source order — a `Node::
/// LayoutDef` filter, the same one `nowui_runtime::semantic::Semantic::new`
/// already does internally to build its own (private) `defs` map, just
/// exposed here since nothing in `nowui-runtime` surfaces it publicly.
fn layout_names(ast: &[nowui_syntax::ast::Node]) -> Vec<String> {
    ast.iter()
        .filter_map(|n| match n {
            nowui_syntax::ast::Node::LayoutDef { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect()
}

/// Every `kind` string of a `Widget` anywhere in `nodes` — including inside
/// `if`/`for` bodies, since a layout can use another one conditionally or in
/// a loop — in source order, duplicates included (the caller dedupes).
fn widget_kinds(nodes: &[nowui_syntax::ast::Node], out: &mut Vec<String>) {
    use nowui_syntax::ast::Node;
    for n in nodes {
        match n {
            Node::Widget { kind, children, .. } => {
                out.push(kind.clone());
                widget_kinds(children, out);
            }
            Node::If { branches, else_branch } => {
                for (_, body) in branches {
                    widget_kinds(body, out);
                }
                widget_kinds(else_branch, out);
            }
            Node::For { body, .. } => widget_kinds(body, out),
            Node::LayoutDef { .. } | Node::Import { .. } => {}
        }
    }
}

/// Every layout reachable from `root`, depth-first through each layout
/// body's own widget uses (a `Widget` whose `kind` names another `layout:`
/// def) — see `PreviewDoc::layout_hierarchy`'s own doc comment for the
/// shape this returns. A layout already on the *current* path is not
/// revisited (breaks a direct or transitive self-reference, e.g. a
/// recursive tree/list layout, without an unbounded walk) — but the same
/// layout reached via a *different* path is listed again under that path,
/// same as the tree it's actually rendered into would show it twice.
fn layout_hierarchy(ast: &[nowui_syntax::ast::Node], root: &str) -> Vec<(String, String)> {
    use nowui_syntax::ast::Node;
    let defs: std::collections::HashMap<&str, &[Node]> = ast
        .iter()
        .filter_map(|n| match n {
            Node::LayoutDef { name, children, .. } => Some((name.as_str(), children.as_slice())),
            _ => None,
        })
        .collect();

    fn walk(name: &str, path: &str, defs: &std::collections::HashMap<&str, &[nowui_syntax::ast::Node]>, on_path: &mut Vec<String>, out: &mut Vec<(String, String)>) {
        out.push((path.to_string(), name.to_string()));
        if on_path.iter().any(|n| n == name) {
            return; // cycle — don't recurse into it again, but it's still listed once above.
        }
        on_path.push(name.to_string());
        if let Some(children) = defs.get(name) {
            let mut kinds = Vec::new();
            widget_kinds(children, &mut kinds);
            let mut seen = std::collections::HashSet::new();
            for kind in kinds {
                if defs.contains_key(kind.as_str()) && seen.insert(kind.clone()) {
                    walk(&kind, &format!("{path} > {kind}"), defs, on_path, out);
                }
            }
        }
        on_path.pop();
    }

    let mut out = Vec::new();
    if defs.contains_key(root) {
        walk(root, root, &defs, &mut Vec::new(), &mut out);
    }
    out
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
            layout_names: Vec::new(),
            layout_hierarchy: Vec::new(),
        };
        doc.reload_with_overrides(&HashMap::new())?;
        Ok(doc)
    }

    /// Point this document at a different file entirely (e.g. the designer
    /// switched the active editor tab) — `entry_layout` resets to whatever
    /// the caller passes (typically `"App"`, or the first name in that
    /// file's own `layout_names` once known) rather than carrying over the
    /// previous file's own selection, which would almost never still make
    /// sense. Reloads immediately; same error-handling contract as
    /// `reload_with_overrides`.
    pub fn switch_entry(&mut self, entry_path: &Path, entry_layout: &str, overrides: &HashMap<PathBuf, String>) -> Result<(), String> {
        self.entry_path = entry_path.to_path_buf();
        self.entry_layout = entry_layout.to_string();
        self.reload_with_overrides(overrides)
    }

    /// Switch which of the current file's own `layout:` definitions
    /// renders, without touching `entry_path` — e.g. the designer's own
    /// layout picker, shown whenever `layout_names` has at least one entry.
    /// Reloads
    /// immediately against `overrides` (the caller's unsaved-buffer
    /// override map, same as any other reload) so the new selection takes
    /// effect right away.
    pub fn set_entry_layout(&mut self, name: &str, overrides: &HashMap<PathBuf, String>) -> Result<(), String> {
        self.entry_layout = name.to_string();
        self.reload_with_overrides(overrides)
    }

    pub fn entry_layout(&self) -> &str {
        &self.entry_layout
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
        let mut ui = semantic.build(&self.entry_layout, &NoState).ok_or_else(|| format!("no `layout: {}` found in `{}`", self.entry_layout, self.entry_path.display()))?;
        // Same population `nowui_runtime::run_ast` does for a normal app —
        // otherwise any previewed file using Dropdown/Date/Time/DateTime/
        // TreeView shows the plain-square/triangle fallback glyph instead.
        ui.chevron_up = nowui_icons::icon_frame("FaChevronUp", CHEVRON_COLOR).ok();
        ui.chevron_down = nowui_icons::icon_frame("FaChevronDown", CHEVRON_COLOR).ok();
        ui.chevron_right = nowui_icons::icon_frame("FaChevronRight", CHEVRON_COLOR).ok();
        ui.icon_add_file = nowui_icons::icon_frame("FaFileCirclePlus", CHEVRON_COLOR).ok();
        ui.icon_add_folder = nowui_icons::icon_frame("FaFolderPlus", CHEVRON_COLOR).ok();
        self.layout_names = layout_names(&ast);
        self.layout_hierarchy = self.layout_names.first().map(|root| layout_hierarchy(&ast, root)).unwrap_or_default();
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

    /// The source byte range `id` was expanded from, if any — built by
    /// `Semantic::expand` during the most recent `reload_with_overrides`
    /// (see its own `node_spans` doc comment). The inspector's click-to-
    /// select uses this to map a clicked preview node back to a byte range
    /// in the editor's own buffer. `None` for a node from a dynamically
    /// re-expanded `if`/`for` region rebuilt outside a full reload, or one
    /// with no traceable single-file span (e.g. the entry file mixed with
    /// `#`-imported content — see `Semantic::node_spans`'s own caveat about
    /// multi-file span disambiguation being future work).
    pub fn node_span(&self, id: nowui_core::NodeId) -> Option<nowui_syntax::ast::Span> {
        self.semantic.node_spans.get(&id).copied()
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
    fn node_span_maps_a_widget_back_to_its_own_source_range() {
        let dir = scratch_dir("node_span");
        let path = dir.join("main.nowui");
        let src = "layout: App { Text `hi` Button `Go` }";
        fs::write(&path, src).unwrap();

        let doc = PreviewDoc::load(&path, "App").unwrap();
        let root = doc.ui.get(doc.ui.layers[0].root);
        assert_eq!(root.children.len(), 2);

        let text_span = doc.node_span(root.children[0]).expect("Text should have a recorded span");
        assert_eq!(&src[text_span.start..text_span.end], "Text `hi`");

        let button_span = doc.node_span(root.children[1]).expect("Button should have a recorded span");
        assert_eq!(&src[button_span.start..button_span.end], "Button `Go`");
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
    fn layout_names_lists_every_top_level_layout_in_source_order() {
        let dir = scratch_dir("layout_names");
        let path = dir.join("main.nowui");
        fs::write(&path, "layout: App { Text `a` }\nlayout: Card(title) { Text `${title}` }\nlayout: Footer { Text `f` }").unwrap();

        let doc = PreviewDoc::load(&path, "App").unwrap();
        assert_eq!(doc.layout_names, vec!["App".to_string(), "Card".to_string(), "Footer".to_string()]);
    }

    #[test]
    fn layout_hierarchy_lists_every_reachable_layout_with_its_own_full_path() {
        let dir = scratch_dir("layout_hierarchy");
        let path = dir.join("main.nowui");
        fs::write(&path, "layout: App { PageLogin } layout: PageLogin { ResultPopUp } layout: ResultPopUp { Text `hi` } layout: Orphan { Text `never used` }").unwrap();

        let doc = PreviewDoc::load(&path, "App").unwrap();
        assert_eq!(
            doc.layout_hierarchy,
            vec![
                ("App".to_string(), "App".to_string()),
                ("App > PageLogin".to_string(), "PageLogin".to_string()),
                ("App > PageLogin > ResultPopUp".to_string(), "ResultPopUp".to_string()),
            ],
            "Orphan is never used anywhere, so it isn't reachable from App"
        );
    }

    #[test]
    fn layout_hierarchy_breaks_a_cycle_instead_of_recursing_forever() {
        let dir = scratch_dir("layout_hierarchy_cycle");
        let path = dir.join("main.nowui");
        fs::write(&path, "layout: App { for x in state.items { App } }").unwrap();

        let doc = PreviewDoc::load(&path, "App").unwrap();
        // One extra "App > App" level shows the actual recursive use, then
        // the cycle guard refuses to recurse into that repeated `App` a
        // second time — bounded, not an infinite/unbounded walk.
        assert_eq!(doc.layout_hierarchy, vec![("App".to_string(), "App".to_string()), ("App > App".to_string(), "App".to_string())]);
    }

    #[test]
    fn layout_hierarchy_finds_a_layout_used_only_inside_an_if_branch() {
        let dir = scratch_dir("layout_hierarchy_if");
        let path = dir.join("main.nowui");
        fs::write(&path, "layout: App { if state.loggedIn { Dashboard } else { PageLogin } } layout: Dashboard { Text `d` } layout: PageLogin { Text `l` }").unwrap();

        let doc = PreviewDoc::load(&path, "App").unwrap();
        let ids: Vec<&str> = doc.layout_hierarchy.iter().map(|(_, id)| id.as_str()).collect();
        assert!(ids.contains(&"Dashboard"));
        assert!(ids.contains(&"PageLogin"));
    }

    #[test]
    fn layout_names_stays_at_its_last_good_value_after_a_failed_reload() {
        let dir = scratch_dir("layout_names_failed");
        let path = dir.join("main.nowui");
        fs::write(&path, "layout: App { Text `a` }\nlayout: Extra { Text `e` }").unwrap();
        let mut doc = PreviewDoc::load(&path, "App").unwrap();
        assert_eq!(doc.layout_names.len(), 2);

        let canonical = path.canonicalize().unwrap();
        let mut overrides = HashMap::new();
        overrides.insert(canonical, "layout: App { Text `unterminated".to_string());
        assert!(doc.reload_with_overrides(&overrides).is_err());
        // Untouched, same all-or-nothing contract `ui` already has (see
        // `a_reload_that_fails_to_parse_leaves_the_last_good_ui_in_place`).
        assert_eq!(doc.layout_names.len(), 2);
    }

    #[test]
    fn set_entry_layout_switches_which_layout_renders_in_the_same_file() {
        let dir = scratch_dir("set_entry_layout");
        let path = dir.join("main.nowui");
        fs::write(&path, "layout: App { Text `from app` }\nlayout: Alt { Text `from alt` }").unwrap();

        let mut doc = PreviewDoc::load(&path, "App").unwrap();
        assert_eq!(doc.entry_layout(), "App");
        let root = doc.ui.get(doc.ui.layers[0].root);
        let nowui_core::NodeKind::Text { content } = &doc.ui.get(root.children[0]).kind else { panic!() };
        assert_eq!(content, "from app");

        doc.set_entry_layout("Alt", &HashMap::new()).unwrap();
        assert_eq!(doc.entry_layout(), "Alt");
        let root = doc.ui.get(doc.ui.layers[0].root);
        let nowui_core::NodeKind::Text { content } = &doc.ui.get(root.children[0]).kind else { panic!() };
        assert_eq!(content, "from alt");
    }

    #[test]
    fn switch_entry_points_the_document_at_a_different_file() {
        let dir = scratch_dir("switch_entry");
        let a = dir.join("a.nowui");
        let b = dir.join("b.nowui");
        fs::write(&a, "layout: App { Text `file a` }").unwrap();
        fs::write(&b, "layout: App { Text `file b` }").unwrap();

        let mut doc = PreviewDoc::load(&a, "App").unwrap();
        doc.switch_entry(&b, "App", &HashMap::new()).unwrap();
        assert_eq!(doc.entry_path, b);
        let root = doc.ui.get(doc.ui.layers[0].root);
        let nowui_core::NodeKind::Text { content } = &doc.ui.get(root.children[0]).kind else { panic!() };
        assert_eq!(content, "file b");
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
