//! `#[nowui(view("/path.nowui"))]` embeds a `.nowui` file into the binary at
//! compile time via `include_str!` (path resolved relative to *this crate's*
//! own `src/` directory), so `nowui_runtime::run` can build straight from
//! `S::nowui_view()` with no filesystem access at runtime — see CLAUDE.md's
//! "Rust sample app" section and `examples/counter-app/src/main.rs` for the
//! real-world shape of this.

use nowui_core::NowUiState;

#[derive(Default, Clone, NowUiState)]
#[nowui(view("/test_view_fixture.nowui"))]
struct BundledApp {
    username: String,
}

#[derive(Default, Clone, NowUiState)]
struct PlainApp {
    username: String,
}

#[test]
fn nowui_view_returns_the_bundled_source_when_the_attribute_is_present() {
    assert_eq!(BundledApp::nowui_view(), Some("layout: App { Text `bundled view fixture` }\n"));
}

#[test]
fn nowui_view_defaults_to_none_without_the_attribute() {
    assert_eq!(PlainApp::nowui_view(), None);
}

#[test]
fn a_bundled_source_parses_and_builds_the_named_entry_layout() {
    let source = BundledApp::nowui_view().expect("view attribute set");
    let ast = nowui_runtime::loader::load_and_resolve_str(source).expect("bundled view should parse");
    let mut sem = nowui_runtime::semantic::Semantic::new(&ast);
    let state = BundledApp::default();
    assert!(sem.build("App", &state).is_some(), "the bundled view's `App` layout should build");
}

// `view_fixtures/main.nowui` imports `view_fixtures/widgets/Badge.nowui` via
// `# widgets/Badge.nowui` — covers the whole-import-graph embedding
// (`nowui-macros`'s `build_embedded_view`) end to end: the derive macro
// walks and embeds `Badge.nowui` too, and `nowui_runtime::run`'s bundled
// path resolves the import against the embedded map with no disk access.
#[derive(Default, Clone, NowUiState)]
#[nowui(view("/view_fixtures/main.nowui"))]
struct BundledAppWithImports {
    username: String,
}

#[test]
fn nowui_view_imports_embeds_every_transitively_imported_file() {
    let imports = BundledAppWithImports::nowui_view_imports().expect("view attribute set");
    assert_eq!(imports.len(), 1, "exactly the one imported file should be embedded");
    let (key, source) = imports[0];
    assert_eq!(key, "view_fixtures/widgets/Badge.nowui");
    assert!(source.contains("layout: Badge"));
}

#[test]
fn nowui_view_path_returns_the_literal_attribute_argument() {
    assert_eq!(BundledAppWithImports::nowui_view_path(), Some("/view_fixtures/main.nowui"));
}

#[test]
fn a_bundled_view_with_imports_resolves_them_and_builds_the_entry_layout() {
    let source = BundledAppWithImports::nowui_view().expect("view attribute set");
    let entry_dir = nowui_syntax::import_dirname(
        BundledAppWithImports::nowui_view_path().unwrap().trim_start_matches('/'),
    );
    let imports = BundledAppWithImports::nowui_view_imports().unwrap();
    let ast = nowui_runtime::loader::load_and_resolve_bundled(source, entry_dir, imports)
        .expect("bundled view + imports should resolve with no disk access");

    // The imported `Badge` layout def must have been inlined alongside `App`.
    let names: Vec<_> = ast
        .iter()
        .filter_map(|n| match n {
            nowui_syntax::ast::Node::LayoutDef { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(names, vec!["Badge", "App"]);

    let mut sem = nowui_runtime::semantic::Semantic::new(&ast);
    let state = BundledAppWithImports::default();
    assert!(sem.build("App", &state).is_some(), "the bundled view's `App` layout should build, `Badge` included");
}
