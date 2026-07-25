//! Resolves `# relative/path.nowui` import directives into a single flat AST.
//!
//! `nowui-syntax::parse` is pure (no I/O); file resolution — reading a file,
//! joining a relative path against the *importing* file's own directory, and
//! guarding against import cycles — lives here instead, since `nowui-runtime`
//! is already where file I/O happens (see `main.rs`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use nowui_syntax::ast::Node;

/// Parse `entry_path` and recursively inline every `#`-imported file's
/// top-level nodes in place, in import order, dropping the `Import` markers
/// themselves. A file already loaded earlier in the walk (by canonical path)
/// is skipped rather than re-parsed — this both dedupes diamond imports
/// (`A` and `B` both import `C`) and breaks cycles (`A` imports `B` imports
/// `A`) without needing a separate "currently visiting" stack.
pub fn load_and_resolve(entry_path: &Path) -> Result<Vec<Node>, String> {
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    load_into(entry_path, &mut out, &mut visited, None, &mut None)?;
    Ok(out)
}

/// Like `load_and_resolve`, but also returns every file the walk actually
/// visited (in load order, canonicalized, deduped the same way — a diamond
/// import appears once), for a caller that needs the whole transitive file
/// set — nowui-designer's virtual file explorer, and its watcher (which
/// paths to watch for a reload). Node-level parsing/import-resolution
/// semantics are otherwise identical to `load_and_resolve`.
pub fn load_and_resolve_tagged(entry_path: &Path) -> Result<(Vec<Node>, Vec<PathBuf>), String> {
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    let mut order = Vec::new();
    load_into(entry_path, &mut out, &mut visited, None, &mut Some(&mut order))?;
    Ok((out, order))
}

/// Like `load_and_resolve`, but any path present in `overrides` (matched by
/// canonical path, same identity `load_into`'s own cycle/dedup guard uses)
/// is resolved from that in-memory string instead of the filesystem —
/// letting a caller preview a document with unsaved editor buffers applied,
/// without writing them to disk first. A path not present in `overrides`
/// falls back to reading it from disk as usual, so this degrades to exactly
/// `load_and_resolve` when `overrides` is empty.
pub fn load_and_resolve_with_overrides(entry_path: &Path, overrides: &HashMap<PathBuf, String>) -> Result<Vec<Node>, String> {
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    load_into(entry_path, &mut out, &mut visited, Some(overrides), &mut None)?;
    Ok(out)
}

/// Parse an in-memory `.nowui` source with no filesystem access at all, and
/// no `#`-import resolution — for a bundled source (see
/// `nowui_core::NowUiState::nowui_view`) known to have no `#` imports at
/// all. Prefer `load_and_resolve_bundled` in general (it degrades to
/// exactly this when `imports` is empty); this is kept as the simple case
/// for direct/synthetic sources (tests, `NoState`-style ad hoc use) that
/// don't go through the `NowUiState` bundled-view machinery.
pub fn load_and_resolve_str(source: &str) -> Result<Vec<Node>, String> {
    nowui_syntax::parse(source).map_err(|errors| format!("parse error(s) in bundled view:\n{errors:?}"))
}

/// Like `load_and_resolve`, but for a `#[nowui(view(...))]`-bundled source
/// whose whole `#`-import graph was *also* embedded into the binary at
/// compile time (see `nowui-macros`'s `build_embedded_view`) — resolves
/// imports against `imports` (a `(key, source)` list, keyed exactly the way
/// the derive macro computed them: `nowui_syntax::join_import_path`/
/// `import_dirname`, starting from `entry_dir`, the bundled entry file's own
/// `#`-import base directory) instead of the filesystem. No disk access at
/// all — correct for a source that has genuinely been fully embedded.
pub fn load_and_resolve_bundled(entry_source: &str, entry_dir: &str, imports: &[(&str, &str)]) -> Result<Vec<Node>, String> {
    let map: std::collections::HashMap<&str, &str> = imports.iter().copied().collect();
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    load_bundled_into(entry_source, entry_dir, &map, &mut out, &mut visited)?;
    Ok(out)
}

fn load_bundled_into(
    source: &str,
    dir: &str,
    map: &std::collections::HashMap<&str, &str>,
    out: &mut Vec<Node>,
    visited: &mut HashSet<String>,
) -> Result<(), String> {
    let ast = nowui_syntax::parse(source).map_err(|errors| format!("parse error(s) in bundled view:\n{errors:?}"))?;

    for node in ast {
        match node {
            Node::Import { path: rel } => {
                let key = nowui_syntax::join_import_path(dir, &rel);
                if !visited.insert(key.clone()) {
                    continue;
                }
                let child_source = map.get(key.as_str()).ok_or_else(|| {
                    format!(
                        "bundled import `{rel}` (resolved to `{key}`) was not embedded — this indicates a mismatch \
                         between the derive macro's compile-time import-graph walk and this resolution, or the \
                         `.nowui` source changing since the last build"
                    )
                })?;
                let child_dir = nowui_syntax::import_dirname(&key);
                load_bundled_into(child_source, child_dir, map, out, visited)?;
            }
            other => out.push(other),
        }
    }
    Ok(())
}

fn load_into(
    path: &Path,
    out: &mut Vec<Node>,
    visited: &mut HashSet<PathBuf>,
    overrides: Option<&HashMap<PathBuf, String>>,
    order: &mut Option<&mut Vec<PathBuf>>,
) -> Result<(), String> {
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("could not read `{}`: {e}", path.display()))?;
    if !visited.insert(canonical.clone()) {
        return Ok(());
    }
    if let Some(order) = order {
        order.push(canonical.clone());
    }

    let src = match overrides.and_then(|o| o.get(&canonical)) {
        Some(text) => text.clone(),
        None => std::fs::read_to_string(path).map_err(|e| format!("could not read `{}`: {e}", path.display()))?,
    };
    let ast = nowui_syntax::parse(&src)
        .map_err(|errors| format!("parse error(s) in `{}`:\n{errors:?}", path.display()))?;

    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    for node in ast {
        match node {
            Node::Import { path: rel } => load_into(&dir.join(&rel), out, visited, overrides, order)?,
            other => out.push(other),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nowui_loader_test_{name}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn inlines_an_imported_layout_def() {
        let dir = scratch_dir("basic");
        fs::create_dir_all(dir.join("widgets")).unwrap();
        fs::write(
            dir.join("widgets/BillingCard.nowui"),
            "layout: BillingCard { Text `Billing` }",
        )
        .unwrap();
        fs::write(
            dir.join("main.nowui"),
            "# widgets/BillingCard.nowui\nlayout: App { BillingCard }",
        )
        .unwrap();

        let ast = load_and_resolve(&dir.join("main.nowui")).expect("should resolve");
        let names: Vec<_> = ast
            .iter()
            .filter_map(|n| match n {
                Node::LayoutDef { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["BillingCard", "App"]);
    }

    #[test]
    fn diamond_import_is_only_loaded_once() {
        let dir = scratch_dir("diamond");
        fs::write(dir.join("shared.nowui"), "layout: Shared { Text `s` }").unwrap();
        fs::write(dir.join("a.nowui"), "# shared.nowui\nlayout: A { Shared }").unwrap();
        fs::write(dir.join("b.nowui"), "# shared.nowui\nlayout: B { Shared }").unwrap();
        fs::write(dir.join("main.nowui"), "# a.nowui\n# b.nowui\nlayout: App { A }").unwrap();

        let ast = load_and_resolve(&dir.join("main.nowui")).expect("should resolve");
        let shared_count = ast
            .iter()
            .filter(|n| matches!(n, Node::LayoutDef { name, .. } if name == "Shared"))
            .count();
        assert_eq!(shared_count, 1, "shared.nowui imported via both a and b must only be loaded once");
    }

    #[test]
    fn tagged_reports_every_visited_file_once_even_through_a_diamond_import() {
        let dir = scratch_dir("tagged_diamond");
        fs::write(dir.join("shared.nowui"), "layout: Shared { Text `s` }").unwrap();
        fs::write(dir.join("a.nowui"), "# shared.nowui\nlayout: A { Shared }").unwrap();
        fs::write(dir.join("b.nowui"), "# shared.nowui\nlayout: B { Shared }").unwrap();
        fs::write(dir.join("main.nowui"), "# a.nowui\n# b.nowui\nlayout: App { A }").unwrap();

        let (ast, files) = load_and_resolve_tagged(&dir.join("main.nowui")).expect("should resolve");
        assert_eq!(files.len(), 4, "main, a, b, shared — shared counted once despite the diamond");
        let names: HashSet<_> = files.iter().filter_map(|p| p.file_name()).collect();
        assert!(names.contains(std::ffi::OsStr::new("main.nowui")));
        assert!(names.contains(std::ffi::OsStr::new("shared.nowui")));
        assert!(!ast.is_empty());
    }

    #[test]
    fn overrides_take_precedence_over_disk_content() {
        let dir = scratch_dir("overrides");
        fs::write(dir.join("main.nowui"), "layout: App { Text `on disk` }").unwrap();

        let canonical = dir.join("main.nowui").canonicalize().unwrap();
        let mut overrides = HashMap::new();
        overrides.insert(canonical, "layout: App { Text `unsaved edit` }".to_string());

        let ast = load_and_resolve_with_overrides(&dir.join("main.nowui"), &overrides).expect("should resolve");
        let Node::LayoutDef { children, .. } = &ast[0] else { panic!() };
        let Node::Widget { string_args, .. } = &children[0] else { panic!() };
        assert_eq!(string_args[0].render_flat(), "unsaved edit");
    }

    #[test]
    fn a_path_absent_from_overrides_still_falls_back_to_disk() {
        let dir = scratch_dir("overrides_fallback");
        fs::write(dir.join("main.nowui"), "layout: App { Text `on disk` }").unwrap();

        let ast = load_and_resolve_with_overrides(&dir.join("main.nowui"), &HashMap::new()).expect("should resolve");
        let Node::LayoutDef { children, .. } = &ast[0] else { panic!() };
        let Node::Widget { string_args, .. } = &children[0] else { panic!() };
        assert_eq!(string_args[0].render_flat(), "on disk");
    }

    #[test]
    fn circular_import_does_not_infinite_loop() {
        let dir = scratch_dir("cycle");
        fs::write(dir.join("a.nowui"), "# b.nowui\nlayout: A { Text `a` }").unwrap();
        fs::write(dir.join("b.nowui"), "# a.nowui\nlayout: B { Text `b` }").unwrap();

        let ast = load_and_resolve(&dir.join("a.nowui")).expect("should resolve without hanging");
        let names: Vec<_> = ast
            .iter()
            .filter_map(|n| match n {
                Node::LayoutDef { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["B", "A"], "b.nowui loads first (a's import), then a's own def");
    }
}
