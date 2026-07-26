//! The designer chrome's own reactive state — the project's file tree, the
//! open-tab strip, and (when the active tab's file defines more than one)
//! the `layout:` picker. All three are mapped from Rust-owned source-of-
//! truth data (`virtual_fs::VfsEntry`, `app::OpenTab`, `PreviewDoc::
//! layout_names`) into a `NowUiState`-deriving shape so `designer.nowui`
//! can render them declaratively (`for`/recursive `layout:` — see
//! `VfsNode`'s own comment for why that's the right technique for the tree,
//! and `nowui-runtime`'s `dynamic::substitute_named_arg` for the engine-
//! level fix that makes it actually resolve live state instead of a bare
//! loop-variable name).

use crate::virtual_fs::VfsEntry;

#[derive(Default, Clone, nowui_core::NowUiState)]
pub struct VfsNode {
    pub name: String,
    /// The real (or still-pending, unsaved) filesystem path this entry
    /// corresponds to, as a plain string (`NowUiState` templates only
    /// render displayable scalars — see `display_string`). Not currently
    /// read by `designer.nowui` itself (`TreeViewItem`'s own `id` binding
    /// doesn't yet resolve a loop-variable-rooted path — a documented
    /// engine gap, see CLAUDE.md's `Variable`/`for` section) — clicking a
    /// tree row instead maps back to a path via `app::tree_item_paths`,
    /// which walks the same `Vec<VfsNode>` this type holds in the exact
    /// order `RenderVfsNode` below expands it into `TreeViewItem`s. Kept
    /// here anyway since it costs nothing and is the obviously-right home
    /// for it once that engine gap closes.
    pub path: String,
    pub is_dir: bool,
    /// See `VfsEntry::Truncated` — a depth-capped placeholder leaf, not a
    /// real file/folder.
    pub truncated: bool,
    pub children: Vec<VfsNode>,
    // Deliberately *no* per-entry `bg_color`/`text_color` fields — an
    // earlier revision drove the active-row highlight through exactly
    // that (a `bg-[${entry.bg_color}]` bracket resolved via `RenderVfsNode`'s
    // own layout param), but changing even one entry's color anywhere in
    // the tree flips the *whole* `for entry in state.tree` region's own
    // rebuild signature (`nowui-runtime`'s `signature_string` hashes every
    // field of every item), forcing the entire explorer subtree to
    // rebuild from scratch on every single tab switch. Fine for a handful
    // of files; for a real project tree it meant `Ui::gc`'s own orphan
    // count — and so per-redraw work — grew without bound over a session,
    // to the point of visibly hanging. `app::DesignerApp::
    // apply_active_row_highlight` now does this by mutating the *live*
    // `TreeViewItem` arena node's own `base_style` directly instead —
    // bypassing the reactive/region-rebuild system entirely for a purely
    // cosmetic, non-structural change.
}

impl VfsNode {
    pub fn from_entry(entry: &VfsEntry) -> Self {
        match entry {
            VfsEntry::File { name, path } => {
                VfsNode { name: name.clone(), path: path.display().to_string(), is_dir: false, truncated: false, children: Vec::new() }
            }
            VfsEntry::Dir { name, path, children } => VfsNode {
                name: name.clone(),
                path: path.display().to_string(),
                is_dir: true,
                truncated: false,
                children: children.iter().map(VfsNode::from_entry).collect(),
            },
            VfsEntry::Truncated { path } => {
                VfsNode { name: "… more".to_string(), path: path.display().to_string(), is_dir: false, truncated: true, children: Vec::new() }
            }
        }
    }
}

/// One entry in the open-tab strip — see `app::OpenTab` (the Rust-owned
/// source of truth this is rebuilt from every time the tab list changes,
/// not authored/edited through this reactive copy).
#[derive(Default, Clone, nowui_core::NowUiState)]
pub struct TabInfo {
    /// The file's own basename, prefixed with `"● "` while dirty (baked in
    /// here rather than left to a `.nowui`-side conditional — simpler than
    /// threading a second `dirty`-driven style/text decision through the
    /// chrome for what's fundamentally still just "the tab's own label").
    pub label: String,
    pub active: bool,
}

/// One entry in the layout picker's own `Dropdown` (`{values: state.
/// layout_options}`) — `label` is the full hierarchy path (`"App >
/// PageLogin > ResultPopUp"`, see `preview::layout_hierarchy`), `id` the
/// bare layout name to switch to (`Dropdown`'s own two-field-struct
/// convention for a `values` binding — see CLAUDE.md's `Dropdown` section).
/// Shown only while the active tab's file defines more than one `layout:`
/// (see `app::DesignerApp::sync_reactive_state`) — empty otherwise, so the
/// dropdown naturally has nothing to pick from rather than needing a
/// separate visibility guard.
#[derive(Default, Clone, nowui_core::NowUiState)]
pub struct LayoutOption {
    pub label: String,
    pub id: String,
}

/// One row in the inspector panel — a selected preview node's own style
/// token (`label` bare, e.g. `"bg"`) or binding (`label` wrapped in braces,
/// e.g. `"{onClick}"`, so the two read distinctly at a glance without a
/// second reactive field). See `app::DesignerApp::refresh_inspector` (the
/// only writer) and `inspector::InspectorField` (the Rust-side source of
/// truth this is copied from).
#[derive(Default, Clone, nowui_core::NowUiState)]
pub struct InspectorFieldRow {
    pub label: String,
    pub value: String,
}

#[derive(Default, Clone, nowui_core::NowUiState)]
pub struct DesignerState {
    /// The scanned project tree's own top-level entries — `designer.nowui`
    /// iterates this directly (`for entry in state.tree`), then recurses
    /// into each entry's own `children` via a self-referential `layout:`.
    pub tree: Vec<VfsNode>,
    pub tabs: Vec<TabInfo>,
    pub layout_options: Vec<LayoutOption>,
    /// The new-file/-folder name prompt's own reactive subtitle text —
    /// e.g. `"New file in widgets/ — Enter to create, Esc to cancel"` while
    /// a creation is in progress (see `app::DesignerApp::sync_reactive_
    /// state`/`NewItemKind`).
    pub creating_hint: String,
    /// The popup's own header — `"New File"`/`"New Folder"` while a
    /// creation is in progress, `""` while idle (meaningless then, since
    /// the popup sits off-screen — see `popup_left`/`popup_top`).
    pub popup_title: String,
    /// The popup's own `position-absolute` placement, in pixel strings
    /// (`"430px"`/`"-9999px"`) — the prompt `TextInput` (see `chrome.rs`'s
    /// `new_item_node`) and its Confirm/Cancel buttons live inside this
    /// popup, always structurally present in `designer.nowui` (same
    /// "always present, only its reactive fields change" simplification
    /// the old inline sidebar prompt already used — re-finding a
    /// dynamically-appearing node's fresh `NodeId` every redraw would be
    /// more complex for no real benefit here) but parked off-screen while
    /// idle rather than centered over the app, so it never intercepts a
    /// click meant for whatever's underneath.
    pub popup_left: String,
    pub popup_top: String,
    /// The right-click context menu's own `position-absolute` placement —
    /// same off-screen-while-idle convention as `popup_left`/`popup_top`
    /// above, just anchored at the cursor's own right-click position
    /// instead of a fixed center-ish spot.
    pub context_menu_left: String,
    pub context_menu_top: String,
    /// The context menu always has four `Button` rows — Add Folder, Add
    /// File, Rename, Delete — structurally present in `designer.nowui` (no
    /// true `display: none` exists, so a row this frame's target doesn't
    /// support collapses to `0px` tall instead of being absent, same "fixed
    /// slot count, dynamic visibility" shape `popup_left`/`popup_top`
    /// already use for the whole popup). `context_menu_add_h` gates the
    /// Add Folder/Add File rows together (hidden for a file target);
    /// `context_menu_edit_h` gates Rename/Delete together (hidden for
    /// empty-space/project-root target). See `app::ContextMenuTarget`.
    pub context_menu_add_h: String,
    pub context_menu_edit_h: String,
    /// `"Rename Folder"`/`"Rename File"` and `"Delete Folder"`/`"Delete
    /// File"` — empty while the context menu is closed or its own row is
    /// hidden (`context_menu_edit_h == "0px"`), meaningless then anyway.
    pub context_menu_rename_label: String,
    pub context_menu_delete_label: String,
    /// The currently-selected preview node's own widget kind (`"Button"`,
    /// `"Text"`, ...), or `""` while nothing is selected — see
    /// `app::DesignerApp::refresh_inspector`.
    pub inspector_kind: String,
    pub inspector_fields: Vec<InspectorFieldRow>,
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

    #[test]
    fn from_entry_carries_the_real_path_through() {
        let entry = VfsEntry::File { name: "main.nowui".to_string(), path: PathBuf::from("/project/main.nowui") };
        let node = VfsNode::from_entry(&entry);
        assert_eq!(node.path, PathBuf::from("/project/main.nowui").display().to_string());
    }
}
