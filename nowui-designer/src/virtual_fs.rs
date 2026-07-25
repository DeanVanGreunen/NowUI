//! A VS Code-Explorer-style project tree: real files on disk, plus
//! newly-created files/folders that only exist in memory until `flush`ed —
//! so a user can build out a folder structure (and see it in the explorer
//! immediately) before committing anything to disk.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::{fs, io};

/// A depth cap on `scan` (see its own doc comment) — a *defensive* limit,
/// not a design goal: real project trees are essentially never this deep,
/// so this exists purely to turn "silently truncate an absurdly deep or
/// cyclic-symlink tree" into a visible, honest `Truncated` leaf instead of
/// either hanging or just dropping content with no indication anything was
/// cut.
pub const DEFAULT_MAX_DEPTH: usize = 32;

#[derive(Debug, Clone, PartialEq)]
pub enum VfsEntry {
    File { name: String, path: PathBuf },
    Dir { name: String, path: PathBuf, children: Vec<VfsEntry> },
    /// `scan` hit `max_depth` with more content still beneath `path` — shown
    /// in the explorer as an explicit "N more levels" leaf rather than
    /// silently vanishing. See `DEFAULT_MAX_DEPTH`'s own doc comment.
    Truncated { path: PathBuf },
}

impl VfsEntry {
    pub fn name(&self) -> &str {
        match self {
            VfsEntry::File { name, .. } | VfsEntry::Dir { name, .. } => name,
            VfsEntry::Truncated { .. } => "…",
        }
    }
}

#[derive(Debug, Clone)]
enum PendingEntry {
    File { path: PathBuf, initial_content: String },
    Dir { path: PathBuf },
}

/// Owns a project root plus any not-yet-saved files/folders created within
/// it. `scan` merges both into one tree; `flush` writes every pending entry
/// to disk and clears the pending list (so a second `flush` with nothing new
/// is a no-op, not a re-write).
pub struct VirtualFs {
    pub root: PathBuf,
    pending: Vec<PendingEntry>,
}

impl VirtualFs {
    pub fn new(root: PathBuf) -> Self {
        VirtualFs { root, pending: Vec::new() }
    }

    /// Every path this project's entry `.nowui` file transitively imports
    /// (see `nowui_runtime::loader::load_and_resolve_tagged`) — the
    /// explorer uses this to distinguish "part of the live app" files from
    /// merely-present-on-disk ones.
    pub fn imported_files(entry_path: &Path) -> Result<HashSet<PathBuf>, String> {
        let (_, files) = nowui_runtime::loader::load_and_resolve_tagged(entry_path)?;
        Ok(files.into_iter().collect())
    }

    /// Queue a new file for creation under `parent` (a real or
    /// already-pending directory) with `initial_content`, returning its
    /// path — visible in the next `scan` immediately, written to disk only
    /// once `flush` runs.
    pub fn new_file(&mut self, parent: &Path, name: &str, initial_content: impl Into<String>) -> PathBuf {
        let path = parent.join(name);
        self.pending.push(PendingEntry::File { path: path.clone(), initial_content: initial_content.into() });
        path
    }

    /// Queue a new folder for creation under `parent`, returning its path.
    pub fn new_folder(&mut self, parent: &Path, name: &str) -> PathBuf {
        let path = parent.join(name);
        self.pending.push(PendingEntry::Dir { path: path.clone() });
        path
    }

    /// Write every pending entry to disk, parent directories included, and
    /// clear the pending list. A pending file whose own parent is also
    /// still pending is written correctly regardless of queue order (each
    /// file creation itself calls `create_dir_all` on its own parent first).
    pub fn flush(&mut self) -> io::Result<()> {
        for entry in self.pending.drain(..) {
            match entry {
                PendingEntry::Dir { path } => fs::create_dir_all(&path)?,
                PendingEntry::File { path, initial_content } => {
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::write(&path, initial_content)?;
                }
            }
        }
        Ok(())
    }

    /// Rename an already-on-disk file or folder — a still-pending (unsaved)
    /// entry should just be re-queued with the new name instead, since
    /// there's nothing on disk yet to rename.
    pub fn rename(&self, path: &Path, new_name: &str) -> io::Result<PathBuf> {
        let new_path = path.with_file_name(new_name);
        fs::rename(path, &new_path)?;
        Ok(new_path)
    }

    /// Delete an already-on-disk file or folder (recursively, for a
    /// folder). A still-pending entry should be dropped from the pending
    /// list by the caller instead (nothing on disk to delete yet).
    pub fn delete(&self, path: &Path) -> io::Result<()> {
        if path.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        }
    }

    /// Drop a queued-but-not-yet-flushed entry (and, for a folder, every
    /// pending entry nested under it) without touching disk at all.
    pub fn discard_pending(&mut self, path: &Path) {
        self.pending.retain(|e| {
            let p = match e {
                PendingEntry::File { path, .. } | PendingEntry::Dir { path } => path,
            };
            p != path && !p.starts_with(path)
        });
    }

    /// Build the merged (real + pending) tree rooted at `self.root`, capped
    /// at `max_depth` levels (see `DEFAULT_MAX_DEPTH`).
    pub fn scan(&self, max_depth: usize) -> io::Result<VfsEntry> {
        scan_merged(&self.root, &self.pending, max_depth)
    }
}

fn scan_merged(root: &Path, pending: &[PendingEntry], max_depth: usize) -> io::Result<VfsEntry> {
    let name = root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| root.display().to_string());
    scan_dir_node(root, &name, pending, max_depth)
}

fn scan_dir_node(dir: &Path, name: &str, pending: &[PendingEntry], depth_left: usize) -> io::Result<VfsEntry> {
    if depth_left == 0 {
        let has_more = fs::read_dir(dir).map(|mut it| it.next().is_some()).unwrap_or(false)
            || pending.iter().any(|e| entry_path(e).starts_with(dir) && entry_path(e) != dir);
        return Ok(if has_more { VfsEntry::Truncated { path: dir.to_path_buf() } } else { VfsEntry::Dir { name: name.to_string(), path: dir.to_path_buf(), children: Vec::new() } });
    }

    let mut children = Vec::new();
    let mut seen = HashSet::new();

    if let Ok(read) = fs::read_dir(dir) {
        let mut real: Vec<_> = read.filter_map(|e| e.ok()).collect();
        real.sort_by_key(|e| e.file_name());
        for entry in real {
            let path = entry.path();
            let entry_name = entry.file_name().to_string_lossy().to_string();
            seen.insert(path.clone());
            if path.is_dir() {
                children.push(scan_dir_node(&path, &entry_name, pending, depth_left - 1)?);
            } else {
                children.push(VfsEntry::File { name: entry_name, path });
            }
        }
    }

    // Pending entries directly under `dir` that don't exist on disk yet.
    for e in pending {
        let p = entry_path(e);
        if p.parent() == Some(dir) && !seen.contains(&p) {
            let entry_name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            match e {
                PendingEntry::Dir { .. } => children.push(scan_dir_node(&p, &entry_name, pending, depth_left - 1)?),
                PendingEntry::File { .. } => children.push(VfsEntry::File { name: entry_name, path: p }),
            }
        }
    }

    children.sort_by(|a, b| a.name().cmp(b.name()));
    Ok(VfsEntry::Dir { name: name.to_string(), path: dir.to_path_buf(), children })
}

fn entry_path(e: &PendingEntry) -> PathBuf {
    match e {
        PendingEntry::File { path, .. } | PendingEntry::Dir { path } => path.clone(),
    }
}

/// Opens the host OS's own file manager with `path` pre-selected — the
/// context menu's own "Reveal in File Explorer" (`app.rs`'s
/// `reveal_in_file_explorer`). One of the very few points this crate
/// legitimately steps outside `.nowui`/pure-Rust territory into a raw OS
/// command, same category `main.rs`'s own native open-file dialog already
/// is. `path` must exist on disk — a still-pending (unflushed) virtual
/// entry has nothing real to reveal yet.
pub fn reveal_in_file_explorer(path: &Path) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg("-R").arg(path).spawn()?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // No universal "select this file" convention on Linux file
        // managers — opening the containing folder is the reasonable,
        // widely-supported fallback.
        let dir = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
        std::process::Command::new("xdg-open").arg(dir).spawn()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nowui_designer_vfs_test_{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn scan_reflects_real_files_and_folders() {
        let dir = scratch_dir("scan_real");
        fs::write(dir.join("main.nowui"), "layout: App {}").unwrap();
        fs::create_dir_all(dir.join("widgets")).unwrap();
        fs::write(dir.join("widgets/Card.nowui"), "layout: Card {}").unwrap();

        let vfs = VirtualFs::new(dir.clone());
        let tree = vfs.scan(DEFAULT_MAX_DEPTH).unwrap();
        let VfsEntry::Dir { children, .. } = &tree else { panic!() };
        assert_eq!(children.len(), 2, "main.nowui and widgets/");

        let widgets = children.iter().find(|c| c.name() == "widgets").unwrap();
        let VfsEntry::Dir { children, .. } = widgets else { panic!() };
        assert_eq!(children[0].name(), "Card.nowui");
    }

    #[test]
    fn pending_entries_appear_in_scan_before_flush_and_on_disk_after() {
        let dir = scratch_dir("pending");
        let mut vfs = VirtualFs::new(dir.clone());
        vfs.new_folder(&dir, "widgets");
        vfs.new_file(&dir.join("widgets"), "Card.nowui", "layout: Card {}");

        let tree = vfs.scan(DEFAULT_MAX_DEPTH).unwrap();
        let VfsEntry::Dir { children, .. } = &tree else { panic!() };
        let widgets = children.iter().find(|c| c.name() == "widgets").expect("pending folder visible before flush");
        let VfsEntry::Dir { children, .. } = widgets else { panic!() };
        assert_eq!(children[0].name(), "Card.nowui", "pending file visible before flush too");
        assert!(!dir.join("widgets/Card.nowui").exists(), "not written to disk yet");

        vfs.flush().unwrap();
        assert_eq!(fs::read_to_string(dir.join("widgets/Card.nowui")).unwrap(), "layout: Card {}");

        // A second flush with nothing new queued is a harmless no-op.
        vfs.flush().unwrap();
    }

    #[test]
    fn discard_pending_drops_a_queued_folder_and_everything_nested_under_it() {
        let dir = scratch_dir("discard");
        let mut vfs = VirtualFs::new(dir.clone());
        let widgets = vfs.new_folder(&dir, "widgets");
        vfs.new_file(&widgets, "Card.nowui", "");

        vfs.discard_pending(&widgets);
        let tree = vfs.scan(DEFAULT_MAX_DEPTH).unwrap();
        let VfsEntry::Dir { children, .. } = &tree else { panic!() };
        assert!(children.is_empty(), "both the folder and its nested pending file are gone");

        vfs.flush().unwrap();
        assert!(!widgets.exists(), "discarded before flush, so never written to disk");
    }

    #[test]
    fn scan_caps_depth_and_marks_truncation_instead_of_silently_dropping_content() {
        let dir = scratch_dir("deep");
        let mut nested = dir.clone();
        for i in 0..5 {
            nested = nested.join(format!("level{i}"));
        }
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("leaf.nowui"), "layout: Leaf {}").unwrap();

        let vfs = VirtualFs::new(dir.clone());
        let tree = vfs.scan(2).unwrap();

        // level0 exists (depth 1), level0/level1 exists (depth 2), but
        // level1's own children are beyond the cap — a Truncated leaf, not
        // silently missing.
        let VfsEntry::Dir { children, .. } = &tree else { panic!() };
        let level0 = children.iter().find(|c| c.name() == "level0").unwrap();
        let VfsEntry::Dir { children, .. } = level0 else { panic!() };
        let level1 = &children[0];
        assert!(matches!(level1, VfsEntry::Truncated { .. }), "depth cap reached with real content beneath it");
    }

    #[test]
    fn imported_files_matches_the_loaders_own_transitively_resolved_set() {
        let dir = scratch_dir("imported");
        fs::write(dir.join("shared.nowui"), "layout: Shared { Text `s` }").unwrap();
        fs::write(dir.join("main.nowui"), "# shared.nowui\nlayout: App { Shared }").unwrap();
        // A file that exists on disk but isn't imported by anything.
        fs::write(dir.join("unused.nowui"), "layout: Unused {}").unwrap();

        let imported = VirtualFs::imported_files(&dir.join("main.nowui")).unwrap();
        assert_eq!(imported.len(), 2, "main.nowui and shared.nowui, not unused.nowui");
        assert!(imported.iter().any(|p| p.file_name().unwrap() == "main.nowui"));
        assert!(imported.iter().any(|p| p.file_name().unwrap() == "shared.nowui"));
        assert!(!imported.iter().any(|p| p.file_name().unwrap() == "unused.nowui"));
    }
}
