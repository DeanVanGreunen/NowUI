//! The designer chrome's own reactive state — currently just the project's
//! file tree, mapped from `virtual_fs::VfsEntry` into a `NowUiState`-
//! deriving shape so `designer.nowui` can walk it with a recursive
//! `layout:` + `for` (see its own comment for why that's the right
//! technique, and `nowui-runtime`'s `dynamic::substitute_named_arg` for the
//! engine-level fix that makes it actually resolve live state instead of a
//! bare loop-variable name).

use crate::virtual_fs::VfsEntry;

#[derive(Default, Clone, nowui_core::NowUiState)]
pub struct VfsNode {
    pub name: String,
    pub is_dir: bool,
    /// See `VfsEntry::Truncated` — a depth-capped placeholder leaf, not a
    /// real file/folder.
    pub truncated: bool,
    pub children: Vec<VfsNode>,
}

impl VfsNode {
    pub fn from_entry(entry: &VfsEntry) -> Self {
        match entry {
            VfsEntry::File { name, .. } => VfsNode { name: name.clone(), is_dir: false, truncated: false, children: Vec::new() },
            VfsEntry::Dir { name, children, .. } => VfsNode {
                name: name.clone(),
                is_dir: true,
                truncated: false,
                children: children.iter().map(VfsNode::from_entry).collect(),
            },
            VfsEntry::Truncated { .. } => VfsNode { name: "… more".to_string(), is_dir: false, truncated: true, children: Vec::new() },
        }
    }
}

#[derive(Default, Clone, nowui_core::NowUiState)]
pub struct DesignerState {
    /// The scanned project tree's own top-level entries — `designer.nowui`
    /// iterates this directly (`for entry in state.tree`), then recurses
    /// into each entry's own `children` via a self-referential `layout:`.
    pub tree: Vec<VfsNode>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn from_entry_maps_a_dir_and_its_children_recursively() {
        let entry = VfsEntry::Dir {
            name: "widgets".to_string(),
            path: PathBuf::from("widgets"),
            children: vec![
                VfsEntry::File { name: "Card.nowui".to_string(), path: PathBuf::from("widgets/Card.nowui") },
                VfsEntry::Truncated { path: PathBuf::from("widgets/deep") },
            ],
        };
        let node = VfsNode::from_entry(&entry);
        assert_eq!(node.name, "widgets");
        assert!(node.is_dir);
        assert_eq!(node.children.len(), 2);
        assert_eq!(node.children[0].name, "Card.nowui");
        assert!(!node.children[0].is_dir);
        assert!(node.children[1].truncated);
    }
}
