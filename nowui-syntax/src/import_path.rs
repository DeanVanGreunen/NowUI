//! Shared `#`-import path-joining logic — used both at compile time
//! (`nowui-macros`, embedding a `#[nowui(view(...))]` view's whole `#`-import
//! graph into the binary) and at runtime (`nowui-runtime`'s loader,
//! resolving those same imports against the embedded map instead of the
//! filesystem). Both sides must compute *identical* keys for a given
//! import — one produces them, the other looks them up — so this lives once,
//! in the one crate both already depend on, rather than duplicated.
//!
//! Purely lexical, no filesystem access, so it behaves identically whether
//! the files in question exist on disk (compile time, `nowui-macros`) or
//! only as embedded strings (runtime, no disk at all). Unlike
//! `Path::canonicalize`, `..`/`.` segments are resolved lexically rather than
//! by consulting the filesystem — the only option once the files aren't
//! really there anymore, and consistent as long as both sides agree (which,
//! being the same function, they do).

/// Join `dir` (a `/`-separated relative directory, or `""` for the root)
/// with a `#`-import's own `rel` path, producing a normalized `/`-separated
/// key.
pub fn join_import_path(dir: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = if dir.is_empty() { Vec::new() } else { dir.split('/').collect() };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(seg),
        }
    }
    parts.join("/")
}

/// The `/`-separated "directory" portion of a normalized import key, for
/// resolving *that* file's own further imports relative to it.
pub fn import_dirname(path: &str) -> &str {
    path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_a_root_relative_import() {
        assert_eq!(join_import_path("", "widgets/BillingCard.nowui"), "widgets/BillingCard.nowui");
    }

    #[test]
    fn joins_relative_to_a_nested_directory() {
        assert_eq!(join_import_path("widgets", "shared/Icon.nowui"), "widgets/shared/Icon.nowui");
    }

    #[test]
    fn resolves_dot_dot_lexically() {
        assert_eq!(join_import_path("widgets/shared", "../Icon.nowui"), "widgets/Icon.nowui");
    }

    #[test]
    fn import_dirname_of_a_root_file_is_empty() {
        assert_eq!(import_dirname("login.nowui"), "");
    }

    #[test]
    fn import_dirname_of_a_nested_file_is_its_parent() {
        assert_eq!(import_dirname("widgets/BillingCard.nowui"), "widgets");
    }
}
