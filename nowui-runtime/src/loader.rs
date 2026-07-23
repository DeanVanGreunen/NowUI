//! Resolves `# relative/path.nowui` import directives into a single flat AST.
//!
//! `nowui-syntax::parse` is pure (no I/O); file resolution — reading a file,
//! joining a relative path against the *importing* file's own directory, and
//! guarding against import cycles — lives here instead, since `nowui-runtime`
//! is already where file I/O happens (see `main.rs`).

use std::collections::HashSet;
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
    load_into(entry_path, &mut out, &mut visited)?;
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

fn load_into(path: &Path, out: &mut Vec<Node>, visited: &mut HashSet<PathBuf>) -> Result<(), String> {
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("could not read `{}`: {e}", path.display()))?;
    if !visited.insert(canonical) {
        return Ok(());
    }

    let src = std::fs::read_to_string(path).map_err(|e| format!("could not read `{}`: {e}", path.display()))?;
    let ast = nowui_syntax::parse(&src)
        .map_err(|errors| format!("parse error(s) in `{}`:\n{errors:?}", path.display()))?;

    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    for node in ast {
        match node {
            Node::Import { path: rel } => load_into(&dir.join(&rel), out, visited)?,
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
