//! The designer's own `winit::application::ApplicationHandler` — built
//! directly on `nowui_core`/`nowui_render_gpu`'s lower-level pieces (not
//! `nowui_runtime::App`/`run_path`, which each own a whole `EventLoop` and
//! exactly one window — see `preview.rs`'s module doc for why). Currently a
//! single undetachable read-only preview window (this crate's first
//! build-order stage); the multi-window chrome+preview split, live reload,
//! and detach all land in later stages without changing this shape much —
//! just what gets rendered into which window.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use nowui_core::{Edges, NodeId, NodeKind, Painter, Point, Size};
use nowui_render_gpu::{GpuFontCache, GpuPainter, GpuSurfaceState};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

use crate::chrome::Chrome;
use crate::preview::PreviewDoc;
use crate::state::{DesignerState, LayoutOption, TabInfo, VfsNode};
use crate::tabs::Tabs;
use crate::watcher::FileWatcher;

/// Same fixed-60fps-loop convention `nowui-runtime`'s own `App` uses (see
/// its module doc / CLAUDE.md's "Runtime gotchas") — not on-demand redraw.
const FRAME_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / 60);
const CLEAR: nowui_core::Color = nowui_core::Color { r: 0x1e, g: 0x1e, b: 0x1e, a: 255 };

/// The position of `id` among every *live* (reachable from a layer root)
/// node in `ui`, walked depth-first in the same order `designer.nowui`'s
/// own recursive expansion produces, whose `kind` matches `pred` — `None`
/// if `id` itself doesn't match `pred` (or isn't reachable at all). Shared
/// by `handle_tree_click`/`handle_button_click` to map a hit-tested node
/// back to "the Nth thing of this kind", since neither `TreeViewItem`/
/// `Button` carries any other identifying data a loop-variable-rooted
/// binding could thread through yet (see `VfsNode::path`'s own doc comment
/// on the underlying engine gap).
///
/// Deliberately walks the *live tree* rather than scanning `ui.nodes` in
/// raw `NodeId` order — a `for`/`if` region rebuild (e.g. `state.tree`'s own
/// `bg_color`/`text_color` changing on every tab switch, or `state.tabs`
/// changing on every keystroke) never frees its old arena nodes (see
/// CLAUDE.md's own "no node-removal/GC" limitation), so orphaned duplicates
/// from earlier rebuilds pile up interleaved with the current generation.
/// Counting raw `NodeId` order would count those dead duplicates too,
/// inflating a live node's own index past what `flatten_tree_paths`/
/// `state.tabs.len()` actually expect. A depth-first walk from each layer's
/// own root only ever visits what's actually still referenced.
fn node_index_among(ui: &nowui_core::Ui, id: NodeId, pred: impl Fn(&NodeKind) -> bool) -> Option<usize> {
    fn walk(ui: &nowui_core::Ui, id: NodeId, pred: &impl Fn(&NodeKind) -> bool, out: &mut Vec<NodeId>) {
        let node = ui.get(id);
        if pred(&node.kind) {
            out.push(id);
        }
        for &child in &node.children {
            walk(ui, child, pred, out);
        }
    }
    let mut live = Vec::new();
    for layer in &ui.layers {
        walk(ui, layer.root, &pred, &mut live);
    }
    live.iter().position(|&n| n == id)
}

/// Every `VfsNode` in `nodes`, flattened via the exact same pre-order
/// depth-first walk `designer.nowui`'s own recursive `RenderVfsNode` layout
/// expands into `TreeViewItem` arena nodes — the Nth entry pushed here is
/// guaranteed to correspond to the Nth `TreeViewItem` `nowui-core` builds,
/// in `NodeId` order (both walk the identical `Vec<VfsNode>` structure the
/// identical way). `(path, is_dir)`, or `None` for a `truncated` placeholder
/// row — not a real file or folder, so clicking it is a no-op either way.
fn flatten_tree_paths(nodes: &[VfsNode], out: &mut Vec<Option<(PathBuf, bool)>>) {
    for n in nodes {
        out.push(if n.truncated { None } else { Some((PathBuf::from(&n.path), n.is_dir)) });
        flatten_tree_paths(&n.children, out);
    }
}

/// The first top-level `layout:` name in `src`, or `None` if it doesn't
/// even parse — a last-resort fallback for `load_active_tab_into_editor_
/// and_preview` when a tab's remembered/default `entry_layout` doesn't
/// actually exist in the file (e.g. a file with no `layout: App` at all).
/// Deliberately parses `src` directly rather than going through `#`-import
/// resolution (`PreviewDoc`'s own `layout_names` field does that, but only
/// *after* a successful build — this exists specifically for the case
/// where that hasn't happened yet) — a reasonable simplification for a
/// last-resort fallback, not a general substitute for `PreviewDoc::
/// layout_names`.
fn first_layout_name_in_source(src: &str) -> Option<String> {
    let ast = nowui_syntax::parse(src).ok()?;
    ast.into_iter().find_map(|n| match n {
        nowui_syntax::ast::Node::LayoutDef { name, .. } => Some(name),
        _ => None,
    })
}

/// The new-item prompt's own placeholder while nothing is being created —
/// `DesignerState::creating_hint`'s starting/resting value.
pub const IDLE_HINT: &str = "Right-click a folder (or the explorer) to create a file or folder";

/// `DesignerState::popup_left`/`popup_top`/`context_menu_left`/
/// `context_menu_top` while idle — parked off-screen (see `DesignerState::
/// popup_left`'s own doc comment for why off-screen rather than
/// `opacity-0`).
const POPUP_HIDDEN: &str = "-9999px";
/// A fixed, reasonable center-ish placement for the new-item popup while a
/// creation is in progress — not measured against the real window size (the
/// window is resizable and this crate doesn't track its current size), same
/// "good enough, not pixel-perfect" scope this build stage already accepts
/// elsewhere (see `Chrome::preview_slot_rect`'s own structural-recognition
/// comment).
const POPUP_LEFT: &str = "430px";
const POPUP_TOP: &str = "280px";

/// Re-scans `vfs`'s own project root into the `Vec<VfsNode>` shape
/// `designer.nowui`'s explorer renders — shared by `main.rs` (the initial
/// scan) and `DesignerApp` (re-scanning after a file/folder is created), so
/// both stay in exact sync. Returns exactly **one** top-level entry: the
/// project root folder itself (the directory the entry `.nowui` file lives
/// in), with everything else nested under it as its own `children` — so the
/// explorer always shows a real, visible root folder row, not a flattened
/// list of its contents.
pub fn scan_tree(vfs: &crate::virtual_fs::VirtualFs) -> Vec<VfsNode> {
    match vfs.scan(crate::virtual_fs::DEFAULT_MAX_DEPTH) {
        Ok(entry) => vec![VfsNode::from_entry(&entry)],
        Err(e) => {
            eprintln!("nowui-designer: failed to scan the project folder: {e}");
            Vec::new()
        }
    }
}

/// Which kind of entry the new-item prompt is currently naming.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum NewItemKind {
    File,
    Folder,
}

impl NewItemKind {
    fn noun(self) -> &'static str {
        match self {
            NewItemKind::File => "file",
            NewItemKind::Folder => "folder",
        }
    }
}

pub struct DesignerApp {
    pub chrome: Chrome,
    pub doc: PreviewDoc,
    pub state: DesignerState,
    /// Every open editor tab plus which one is active — see `tabs::Tabs`'s
    /// own doc comment. `self.doc`/`self.chrome`'s editor buffer always
    /// mirror the *active* tab; switching tabs saves the outgoing one's
    /// buffer/cursor back into its own `OpenTab` first
    /// (`sync_editor_into_active_tab`) so nothing is lost.
    tabs: Tabs,
    /// `None` in an environment that can't create a real filesystem watcher
    /// (see `watcher::try_new_watcher`'s own doc comment) — the designer
    /// still runs, just without reload-on-external-edit.
    watcher: Option<FileWatcher>,
    window: Option<Arc<Window>>,
    gpu: Option<GpuSurfaceState>,
    /// The preview's own second OS window, only while detached (`Ctrl+D`
    /// toggles) — `None` while docked, when the preview instead composites
    /// into the main window's own scene (`redraw`). Same `PreviewDoc`/`Ui`
    /// either way; only where its paint output goes changes — see this
    /// module's own doc comment and `preview.rs`'s `render_into`.
    preview_window: Option<Arc<Window>>,
    preview_gpu: Option<GpuSurfaceState>,
    text: nowui_text::TextContext,
    font_cache: GpuFontCache,
    next_frame: Instant,
    cursor: Point,
    modifiers: ModifiersState,
    /// The preview node last clicked — drawn with a highlight outline each
    /// redraw, and what a click looks up in `PreviewDoc::node_span` to
    /// select the matching source range in the editor. Cleared whenever a
    /// reload replaces the preview's whole `Ui` (a stale `NodeId` could
    /// otherwise alias an unrelated node in the new tree).
    selected_node: Option<NodeId>,
    /// Owns the project's real-and-pending file/folder tree — `state.tree`
    /// is just this crate's own last `scan_tree` snapshot, rebuilt after
    /// every create.
    vfs: crate::virtual_fs::VirtualFs,
    /// Which directory a new file/folder from the "+ File"/"+ Folder"
    /// buttons is created inside — the project root until a directory row
    /// in the tree is clicked (`handle_tree_click`), which selects it.
    selected_dir: PathBuf,
    /// `Some` exactly while the new-item prompt (`Chrome::new_item_node`)
    /// is actively accepting a name — set from the context menu's own "New
    /// File.../New Folder..." items, cleared on Enter (confirm) or Escape
    /// (cancel). Gates whether keyboard input routes to the prompt instead
    /// of the main editor (see `window_event`'s own `KeyboardInput` arm).
    creating: Option<NewItemKind>,
    /// `Some((dir, anchor))` exactly while the right-click context menu is
    /// open — `dir` is where "New File.../New Folder..." would create
    /// something, and what "Reveal in File Explorer" reveals (the
    /// right-clicked folder itself, or its parent for a right-clicked
    /// file, or the project root for a right-click on empty space — see
    /// `open_context_menu`); `anchor` is the screen position it opened at,
    /// re-applied to `DesignerState::context_menu_left`/`top` every
    /// `sync_reactive_state` (so it doesn't drift if the mouse moves while
    /// the menu is still open). `None` while closed.
    context_menu: Option<(PathBuf, Point)>,
}

impl DesignerApp {
    /// `chrome`/`doc` are already loaded/pointed at `doc.entry_path` by the
    /// caller (`main.rs`) — this just seeds a single open tab from that
    /// existing state, rather than reloading anything a second time.
    pub fn new(chrome: Chrome, doc: PreviewDoc, state: DesignerState, vfs: crate::virtual_fs::VirtualFs) -> Self {
        let mut watcher = crate::watcher::try_new_watcher();
        if let (Some(w), Ok(files)) = (&mut watcher, doc.imported_files()) {
            w.set_watched(&files);
        }
        let mut tabs = Tabs::default();
        let initial_buffer = chrome.editor_text().to_string();
        tabs.open_or_focus(&doc.entry_path.clone(), || initial_buffer);
        if let Some(tab) = tabs.active_mut() {
            tab.selected_layout = Some(doc.entry_layout().to_string());
        }
        let selected_dir = vfs.root.clone();
        let mut app = DesignerApp {
            chrome,
            doc,
            state,
            vfs,
            selected_dir,
            creating: None,
            context_menu: None,
            tabs,
            watcher,
            window: None,
            gpu: None,
            preview_window: None,
            preview_gpu: None,
            text: nowui_text::TextContext::new(),
            font_cache: GpuFontCache::new(),
            next_frame: Instant::now(),
            cursor: Point::default(),
            modifiers: ModifiersState::empty(),
            selected_node: None,
        };
        app.sync_reactive_state();
        app
    }

    /// Rebuild `state.tabs`/`state.layouts` from `self.tabs`/`self.doc` —
    /// the Rust-owned source of truth — so `designer.nowui`'s own `for tab
    /// in state.tabs`/`for layout in state.layouts` render the current
    /// picture next redraw. Cheap (a couple of short `Vec`s), so called
    /// after every tab/layout-affecting action rather than diffed.
    fn sync_reactive_state(&mut self) {
        let active_index = self.tabs.active_index();
        self.state.tabs = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| {
                let name = tab.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| tab.path.display().to_string());
                let label = if tab.dirty { format!("\u{25cf} {name}") } else { name };
                TabInfo { label, active: Some(i) == active_index }
            })
            .collect();

        // Only worth showing at all once the active file actually defines
        // more than one `layout:` — see `LayoutOption`'s own doc comment.
        self.state.layout_options = if self.doc.layout_names.len() > 1 {
            self.doc.layout_hierarchy.iter().map(|(path, name)| LayoutOption { label: path.clone(), id: name.clone() }).collect()
        } else {
            Vec::new()
        };

        let dir_label = match self.selected_dir.strip_prefix(&self.vfs.root) {
            Ok(rel) if !rel.as_os_str().is_empty() => rel.display().to_string(),
            _ => "project root".to_string(),
        };
        self.state.creating_hint = match self.creating {
            Some(kind) => format!("New {} in {dir_label}", kind.noun()),
            None => IDLE_HINT.to_string(),
        };
        self.state.popup_title = match self.creating {
            Some(NewItemKind::File) => "New File".to_string(),
            Some(NewItemKind::Folder) => "New Folder".to_string(),
            None => String::new(),
        };
        let (left, top) = if self.creating.is_some() { (POPUP_LEFT, POPUP_TOP) } else { (POPUP_HIDDEN, POPUP_HIDDEN) };
        self.state.popup_left = left.to_string();
        self.state.popup_top = top.to_string();

        match &self.context_menu {
            Some((_, anchor)) => {
                self.state.context_menu_left = format!("{}px", anchor.x);
                self.state.context_menu_top = format!("{}px", anchor.y);
            }
            None => {
                self.state.context_menu_left = POPUP_HIDDEN.to_string();
                self.state.context_menu_top = POPUP_HIDDEN.to_string();
            }
        }

        let active_path = self.tabs.active().map(|t| t.path.as_path());
        crate::state::apply_active_highlight(&mut self.state.tree, active_path);
    }

    /// Save the currently-active tab's buffer/cursor/selection out of the
    /// live editor node — called right before switching away from it (tab
    /// click, file-tree open, close), so nothing typed is lost.
    fn sync_editor_into_active_tab(&mut self) {
        let buffer = self.chrome.editor_text().to_string();
        let (cursor, selection_anchor) = match &self.chrome.ui.get(self.chrome.editor_node).kind {
            NodeKind::TextInput { cursor, selection_anchor, .. } => (*cursor, *selection_anchor),
            _ => (0, None),
        };
        if let Some(tab) = self.tabs.active_mut() {
            if tab.buffer != buffer {
                tab.dirty = true;
            }
            tab.buffer = buffer;
            tab.cursor = cursor;
            tab.selection_anchor = selection_anchor;
        }
    }

    /// Loads the now-active tab's buffer into the real editor node (cursor/
    /// selection restored, clamped in case the buffer's own length changed
    /// since it was last active) and re-points the live preview at that
    /// tab's own file/layout. If the tab's remembered `selected_layout`
    /// doesn't actually exist in the file (most commonly: a tab that's
    /// never been successfully loaded yet, defaulting to `"App"`), falls
    /// back to the first `layout:` the file actually defines instead of
    /// leaving the preview on a stale error/empty state.
    fn load_active_tab_into_editor_and_preview(&mut self) {
        let Some(tab) = self.tabs.active() else { return };
        let path = tab.path.clone();
        let buffer = tab.buffer.clone();
        let cursor = tab.cursor;
        let selection_anchor = tab.selection_anchor;
        let mut entry_layout = tab.selected_layout.clone().unwrap_or_else(|| "App".to_string());

        self.chrome.set_editor_text(&buffer); // also resets cursor to end + re-highlights
        let len = nowui_core::text_input::char_len(&buffer);
        if let NodeKind::TextInput { cursor: c, selection_anchor: sel, .. } = &mut self.chrome.ui.get_mut(self.chrome.editor_node).kind {
            *c = cursor.min(len);
            *sel = selection_anchor.filter(|s| *s <= len);
        }

        self.selected_node = None;
        let mut overrides = HashMap::new();
        overrides.insert(path.clone(), buffer.clone());
        if self.doc.switch_entry(&path, &entry_layout, &overrides).is_err() {
            if let Some(name) = first_layout_name_in_source(&buffer) {
                entry_layout = name;
                let _ = self.doc.switch_entry(&path, &entry_layout, &overrides);
            }
        }
        if let Some(tab) = self.tabs.active_mut() {
            tab.selected_layout = Some(self.doc.entry_layout().to_string());
        }

        if let (Some(w), Ok(files)) = (&mut self.watcher, self.doc.imported_files()) {
            w.set_watched(&files);
        }
        if let Some(w) = &self.window {
            w.set_title(&format!("NowUI Designer — {}", path.display()));
        }
        self.sync_reactive_state();
    }

    /// Open `path` as a new tab (or switch to it if already open), saving
    /// the outgoing tab's buffer first. Called from the file-tree's own
    /// click handling (`handle_tree_click`).
    fn open_file(&mut self, path: PathBuf) {
        self.sync_editor_into_active_tab();
        self.tabs.open_or_focus(&path, || std::fs::read_to_string(&path).unwrap_or_default());
        self.load_active_tab_into_editor_and_preview();
    }

    /// Switch to the tab at `index` (a no-op if out of range — see `Tabs::
    /// switch_to`), saving the outgoing tab's buffer first.
    fn switch_tab(&mut self, index: usize) {
        self.sync_editor_into_active_tab();
        if self.tabs.switch_to(index) {
            self.load_active_tab_into_editor_and_preview();
        }
    }

    /// `Ctrl+W` — close the active tab. If another tab becomes active,
    /// loads it into the editor/preview; if that was the last tab, leaves
    /// the editor/preview showing whatever they last did (nothing left to
    /// switch to) but still refreshes `state.tabs`/`state.layouts` so the
    /// (now empty) tab strip actually reflects it.
    fn close_active_tab(&mut self) {
        let Some(index) = self.tabs.active_index() else { return };
        self.tabs.close(index);
        if self.tabs.active_index().is_some() {
            self.load_active_tab_into_editor_and_preview();
        } else {
            self.sync_reactive_state();
        }
    }

    /// Switch which of the active tab's own `layout:` definitions is being
    /// previewed — the layout picker's own click handling
    /// (`handle_button_click`).
    fn select_layout(&mut self, name: &str) {
        let mut overrides = HashMap::new();
        overrides.insert(self.doc.entry_path.clone(), self.chrome.editor_text().to_string());
        if self.doc.set_entry_layout(name, &overrides).is_ok() {
            if let Some(tab) = self.tabs.active_mut() {
                tab.selected_layout = Some(name.to_string());
            }
            self.selected_node = None;
            self.sync_reactive_state();
        }
    }

    /// Maps a clicked `TreeViewItem`'s `NodeId` back to the entry it
    /// represents — `TreeViewItem`'s own `id` binding doesn't yet resolve a
    /// loop-variable-rooted path (see `VfsNode::path`'s own doc comment),
    /// so this instead zips "the Nth `TreeViewItem` in arena order" against
    /// "the Nth entry `flatten_tree_paths` walks `state.tree` into," which
    /// `designer.nowui`'s own `RenderVfsNode` is guaranteed to produce in
    /// the exact same order (both are the same pre-order depth-first
    /// walk). A click within the row's own disclosure-triangle zone (same
    /// `nowui_core::layout::TREE_TRIANGLE_W` zone `nowui_runtime::App::
    /// handle_click` uses for a real app's `TreeView`) toggles `collapsed`
    /// instead of opening/selecting anything — only meaningful when the
    /// item actually has children. Otherwise: a file opens it as a tab; a
    /// folder selects it as the right-click context menu's own default
    /// creation target. A `truncated` placeholder row maps to `None` —
    /// neither, so the click is a no-op.
    fn handle_tree_click(&mut self, id: NodeId) {
        let node = self.chrome.ui.get(id);
        let has_children = !node.children.is_empty();
        let local_x = self.cursor.x - node.computed.x;

        if has_children && local_x < nowui_core::layout::TREE_TRIANGLE_W {
            if let NodeKind::TreeViewItem { collapsed, .. } = &mut self.chrome.ui.get_mut(id).kind {
                *collapsed = !*collapsed;
            }
            return;
        }

        let Some((path, is_dir)) = self.tree_click_path(id) else { return };
        if is_dir {
            self.selected_dir = path;
            self.sync_reactive_state();
        } else {
            self.open_file(path);
        }
    }

    /// Maps a clicked `TreeViewItem`'s `NodeId` back to the entry it
    /// represents — see `handle_tree_click`'s own doc comment on why this
    /// is positional (`node_index_among`/`flatten_tree_paths`) rather than
    /// reading a real `id` binding. `None` for a `truncated` placeholder row
    /// or an id with no arena match.
    fn tree_click_path(&self, id: NodeId) -> Option<(PathBuf, bool)> {
        let index = node_index_among(&self.chrome.ui, id, |k| matches!(k, NodeKind::TreeViewItem { .. }))?;
        let mut paths = Vec::new();
        flatten_tree_paths(&self.state.tree, &mut paths);
        paths.get(index).cloned().flatten()
    }

    /// Maps a clicked `Button`'s `NodeId` to the new-item popup's Cancel/
    /// Create buttons, the context menu's own three items, or a tab-strip
    /// switch (the layout picker is a `Dropdown` now — see `select_
    /// dropdown_option` — not a `Button` list). `designer.nowui` renders
    /// the new-item popup's own two buttons first (always present, see
    /// `DesignerState::popup_left`'s own doc comment), then the context
    /// menu's own three (also always present, see `DesignerState::
    /// context_menu_left`'s own doc comment), then every tab-strip button,
    /// strictly in that source order, so "the Nth `Button` in arena order"
    /// splits cleanly into those fixed-then-variable-length groups with no
    /// separate marker needed.
    fn handle_button_click(&mut self, id: NodeId) {
        let Some(index) = node_index_among(&self.chrome.ui, id, |k| matches!(k, NodeKind::Button { .. })) else { return };
        match index {
            0 => self.cancel_creating(),
            1 => self.confirm_new_item(),
            2 => self.start_creating_from_context_menu(NewItemKind::File),
            3 => self.start_creating_from_context_menu(NewItemKind::Folder),
            4 => self.reveal_in_file_explorer(),
            _ => self.switch_tab(index - 5),
        }
    }

    /// Right-click on a `TreeViewItem` (targets that row) or on empty
    /// explorer/preview space (targets the project root) opens the context
    /// menu at the cursor's current position. Re-opening while already open
    /// just retargets/repositions it — same "starting a new one abandons
    /// the previous, unconfirmed one" convention `start_creating` already
    /// has for the new-item popup.
    fn open_context_menu(&mut self, target_dir: PathBuf) {
        self.context_menu = Some((target_dir, self.cursor));
        self.selected_node = None;
        self.chrome.ui.focus = None;
        self.sync_reactive_state();
    }

    /// Closes the context menu without acting on it (Escape, or clicking
    /// anywhere else — see `close_open_dropdowns`'s own sibling role).
    fn close_context_menu(&mut self) {
        self.context_menu = None;
        self.sync_reactive_state();
    }

    /// "New File.../New Folder..." in the context menu — targets whichever
    /// directory it was opened against, same downstream flow (`start_
    /// creating`/the new-item popup) a tree-row click used to drive
    /// directly before this menu existed.
    fn start_creating_from_context_menu(&mut self, kind: NewItemKind) {
        if let Some((dir, _)) = self.context_menu.take() {
            self.selected_dir = dir;
        }
        self.start_creating(kind);
    }

    /// "Reveal in File Explorer" — opens the host OS's own file manager
    /// with the context menu's own target pre-selected. Best-effort: a
    /// failure (no target, OS command not found, ...) is logged and
    /// otherwise silently ignored, same as this crate's other
    /// filesystem-adjacent operations (`VirtualFs::flush`'s own callers).
    fn reveal_in_file_explorer(&mut self) {
        let Some((dir, _)) = self.context_menu.take() else { return };
        self.sync_reactive_state();
        if let Err(e) = crate::virtual_fs::reveal_in_file_explorer(&dir) {
            eprintln!("nowui-designer: couldn't reveal `{}`: {e}", dir.display());
        }
    }

    /// Opens the new-item popup (see `DesignerState::popup_left`'s own doc
    /// comment), focusing its name prompt with an empty buffer, targeting
    /// `self.selected_dir` — set by the caller just before this, either from
    /// clicking a folder row's own `folder-actions` icon (`handle_tree_
    /// click`) or, in tests, directly. Starting a *new* creation while one
    /// was already in progress just retargets/relabels it — the previous
    /// (unconfirmed, so nothing was ever created) attempt is simply
    /// abandoned.
    fn start_creating(&mut self, kind: NewItemKind) {
        self.creating = Some(kind);
        self.chrome.set_new_item_text("");
        self.chrome.ui.focus = Some(self.chrome.new_item_node);
        self.selected_node = None;
        self.sync_reactive_state();
    }

    /// Discards the in-progress creation (Escape, or nothing to confirm)
    /// without touching the filesystem.
    fn cancel_creating(&mut self) {
        self.creating = None;
        self.chrome.set_new_item_text("");
        self.chrome.ui.focus = None;
        self.sync_reactive_state();
    }

    /// Enter, while the new-item prompt is focused — creates the file/
    /// folder the prompt is currently naming under `self.selected_dir`
    /// (queued via `VirtualFs::new_file`/`new_folder`, then `flush`ed to
    /// disk immediately — simpler than also modeling an "unsaved new file"
    /// state on top of everything `OpenTab::dirty` already tracks, and
    /// matches a real file explorer's own "it exists the moment you name
    /// it" behavior), re-scans the tree, and — for a new *file* — opens it
    /// as a tab right away, same as VS Code's own explorer does. An empty
    /// name, or a `VirtualFs`/disk error, cancels without creating
    /// anything rather than silently doing nothing.
    fn confirm_new_item(&mut self) {
        let Some(kind) = self.creating else { return };
        let name = self.chrome.new_item_text().trim().to_string();
        self.cancel_creating();
        if name.is_empty() {
            return;
        }

        let created_path = match kind {
            NewItemKind::File => self.vfs.new_file(&self.selected_dir, &name, ""),
            NewItemKind::Folder => self.vfs.new_folder(&self.selected_dir, &name),
        };
        if let Err(e) = self.vfs.flush() {
            eprintln!("nowui-designer: failed to create `{}`: {e}", created_path.display());
            return;
        }

        self.state.tree = scan_tree(&self.vfs);
        if kind == NewItemKind::File {
            self.open_file(created_path);
        } else {
            self.sync_reactive_state();
        }
    }

    /// Looks up `id`'s own source span (`PreviewDoc::node_span`) and, if
    /// found, selects that byte range in the editor's buffer — converted to
    /// **char** indices first (`TextInput::cursor`/`selection_anchor`'s own
    /// convention; `nowui_syntax::ast::Span` is byte-based). Also focuses
    /// the editor, so the selection is actually visible and a follow-up
    /// keystroke replaces exactly the clicked token.
    fn select_in_source(&mut self, id: NodeId) {
        let Some(span) = self.doc.node_span(id) else { return };
        let text = self.chrome.editor_text();
        if span.start > text.len() || span.end > text.len() {
            return; // stale span against an out-of-sync buffer — ignore rather than panic on a bad byte index.
        }
        let start_char = text[..span.start].chars().count();
        let end_char = text[..span.end].chars().count();

        self.chrome.ui.focus = Some(self.chrome.editor_node);
        if let NodeKind::TextInput { cursor, selection_anchor, .. } = &mut self.chrome.ui.get_mut(self.chrome.editor_node).kind {
            *selection_anchor = Some(start_char);
            *cursor = end_char;
        }
    }

    /// Re-renders the live preview from the editor's *current in-memory
    /// buffer*, not disk — a `PreviewDoc::reload_with_overrides` override
    /// keyed on the entry path, same mechanism an unsaved editor buffer is
    /// designed for (see its own doc comment). Called after every keystroke
    /// that actually changed the buffer (`editor::edit_text_input` reports
    /// whether it did), so what's on screen always matches what's in the
    /// editor, saved or not.
    fn reload_from_editor_buffer(&mut self) {
        self.selected_node = None;
        let buffer = self.chrome.editor_text().to_string();
        if let Some(tab) = self.tabs.active_mut() {
            tab.dirty = tab.buffer != buffer;
            tab.buffer = buffer.clone();
        }
        let mut overrides = HashMap::new();
        overrides.insert(self.doc.entry_path.clone(), buffer);
        if let Err(e) = self.doc.reload_with_overrides(&overrides) {
            // A mid-edit syntax error is expected and common — not logged
            // as an error, the same "leave the last good Ui in place"
            // behavior `PreviewDoc::reload_with_overrides` already gives
            // the caller for free.
            let _ = e;
        }
        // The dirty-dot on the tab strip, and `layout_names` (an edit can
        // add/remove a `layout:`), both need to stay live.
        self.sync_reactive_state();
    }

    /// Saves the editor's current buffer to disk. The watcher then sees its
    /// own write and would otherwise immediately "reload from disk" right
    /// back over whatever's already showing — harmless (same content) but
    /// wasteful; not specifically suppressed here since a stray extra
    /// reload of identical content isn't a correctness issue, only a minor
    /// inefficiency.
    fn save_editor_buffer(&mut self) {
        if let Err(e) = std::fs::write(&self.doc.entry_path, self.chrome.editor_text()) {
            eprintln!("nowui-designer: failed to save `{}`: {e}", self.doc.entry_path.display());
            return;
        }
        if let Some(tab) = self.tabs.active_mut() {
            tab.dirty = false;
        }
        self.sync_reactive_state();
    }

    /// Re-resolves the live document straight from disk (no unsaved-buffer
    /// overrides yet — those arrive with the editor) and re-arms the
    /// watcher with whatever it imports *now*, since a reload can change
    /// the import graph itself (an added/removed `#` import). A failed
    /// reload (a syntax error mid-edit in an external editor) is logged and
    /// otherwise ignored — `PreviewDoc::reload_with_overrides` already
    /// leaves the last good `Ui` in place rather than blanking the preview.
    fn reload_from_disk(&mut self) {
        self.selected_node = None;
        if let Err(e) = self.doc.reload_with_overrides(&std::collections::HashMap::new()) {
            eprintln!("nowui-designer: reload of `{}` failed: {e}", self.doc.entry_path.display());
        }
        if let (Some(w), Ok(files)) = (&mut self.watcher, self.doc.imported_files()) {
            w.set_watched(&files);
        }
    }

    /// Solves and paints the chrome first (full window), then reads wherever
    /// its own layout placed the preview slot this frame and solves/paints
    /// the live document into exactly that rect (`layout::solve_into`) —
    /// the Gap-1 "second, independent `Ui`/`Layer` composited into a
    /// chrome-defined region" technique, reusing the *existing* multi-layer
    /// compositing model instead of true node-tree embedding. Both draws go
    /// into the same `Scene`/present call, so they composite as one frame;
    /// `push_clip`/`pop_clip` around the preview keeps overflowing content
    /// from spilling outside its slot, same as any other clipped container.
    fn redraw(&mut self) {
        let Some(window) = self.window.clone() else { return };
        let Some(gpu) = self.gpu.as_mut() else { return };

        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));
        let mut scene = vello::Scene::new();

        self.chrome.refresh(&self.state);
        {
            let mut painter = GpuPainter::new(&mut scene, &mut self.text, &mut self.font_cache);
            nowui_core::layout::solve(&mut self.chrome.ui, Size::new(w as f32, h as f32), &mut painter);
            nowui_core::paint::paint(&self.chrome.ui, &mut painter);
        }

        // While detached, the preview composites into its own window
        // instead (`redraw_preview_window`) — the slot in chrome just sits
        // empty (its own background shows through), same principle as any
        // other panel with nothing in it yet.
        if self.preview_window.is_none() {
            let slot_rect = self.chrome.preview_slot_rect();
            let mut painter = GpuPainter::new(&mut scene, &mut self.text, &mut self.font_cache);
            self.doc.render_into(slot_rect, &mut painter);
            // The inspector's own selection outline — drawn directly, not
            // through `paint::paint`'s tree walk, since it's chrome-level
            // UI *about* the preview, not part of the previewed document
            // itself. A stale id from before the last reload can't land
            // here: `reload_from_editor_buffer`/`reload_from_disk` both
            // clear `selected_node` first.
            if let Some(id) = self.selected_node {
                let rect = self.doc.ui.get(id).computed;
                painter.stroke_rect(rect, nowui_core::Color::rgb(0x60, 0xa5, 0xfa), 2.0, Edges::default());
            }
        }

        gpu.resize(w, h);
        gpu.render_and_present(&scene, CLEAR);
    }

    /// The detached preview's own redraw — same `PreviewDoc::render_into`
    /// call `redraw` makes while docked, just aimed at this window's own
    /// full size instead of a chrome-provided slot rect, and presented to
    /// its own `GpuSurfaceState` instead of the chrome's.
    fn redraw_preview_window(&mut self) {
        let Some(window) = self.preview_window.clone() else { return };
        let Some(gpu) = self.preview_gpu.as_mut() else { return };
        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));

        let mut scene = vello::Scene::new();
        {
            let mut painter = GpuPainter::new(&mut scene, &mut self.text, &mut self.font_cache);
            self.doc.render_into(nowui_core::Rect::new(0.0, 0.0, w as f32, h as f32), &mut painter);
        }
        gpu.resize(w, h);
        gpu.render_and_present(&scene, nowui_core::Color::rgb(0xf3, 0xf4, 0xf6));
    }

    /// `Ctrl+D` — create (dock → floating) or destroy (floating → dock) the
    /// preview's own second window. Creating/destroying a `winit::Window`
    /// needs `&ActiveEventLoop`, only available from an `ApplicationHandler`
    /// method, so this can't live on `PreviewDoc`/`Chrome` themselves.
    fn toggle_detach(&mut self, event_loop: &ActiveEventLoop) {
        if self.preview_window.is_some() {
            self.preview_window = None;
            self.preview_gpu = None;
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("NowUI Preview")
            .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0));
        let window = Arc::new(event_loop.create_window(attrs).expect("create preview window"));
        let size = window.inner_size();
        self.preview_gpu = Some(GpuSurfaceState::new(window.clone(), size.width.max(1), size.height.max(1)));
        self.preview_window = Some(window);
    }

    /// The layout picker's own live-values `Dropdown` needs the same
    /// open/close/pick-an-option interaction `nowui_runtime::App` gives a
    /// real app — reimplemented here directly since the chrome isn't
    /// dispatched through that runtime at all (see this module's own doc
    /// comment). Mirrors `nowui-runtime/src/app.rs`'s own `dropdown_popup_
    /// rect`/`select_dropdown_option` geometry exactly, so clicks and pixels
    /// agree about where the popup's rows are.
    fn dropdown_popup_rect(&self, id: NodeId) -> Option<nowui_core::Rect> {
        let node = self.chrome.ui.get(id);
        let NodeKind::Dropdown { options, open, .. } = &node.kind else { return None };
        if !*open {
            return None;
        }
        let (_, option_h) = nowui_core::dropdown_metrics(node.style.font_size);
        let rect = node.computed;
        let h = (option_h * options.len() as f32).min(nowui_core::DROPDOWN_POPUP_MAX_H);
        Some(nowui_core::Rect::new(rect.x, rect.y + rect.h, rect.w, h))
    }

    /// The open dropdown (if any) whose floating popup contains `p` —
    /// checked before ordinary hit-testing, since a popup floats outside
    /// its own box's `computed` rect (not reachable via `Ui::hit_test`).
    fn find_open_dropdown_popup_at(&self, p: Point) -> Option<NodeId> {
        (0..self.chrome.ui.nodes.len()).map(|i| NodeId(i as u32)).find(|&id| self.dropdown_popup_rect(id).is_some_and(|r| r.contains(p)))
    }

    /// A click inside `id`'s own open popup at `p` — picks the row under the
    /// cursor (a disabled option, or a click past the last row, still
    /// closes the popup without changing the selection) and, for the layout
    /// picker specifically, switches the previewed layout to the picked
    /// option's own `id` (the layout name — see `DesignerState::
    /// layout_options`).
    fn select_dropdown_option(&mut self, id: NodeId, p: Point) {
        let node = self.chrome.ui.get_mut(id);
        let rect = node.computed;
        let font_size = node.style.font_size;
        let (_, option_h) = nowui_core::dropdown_metrics(font_size);
        let local_y = p.y - (rect.y + rect.h) + node.scroll_offset.y;
        let mut picked_id = None;
        if let NodeKind::Dropdown { options, option_ids, option_disabled, selected, open, .. } = &mut node.kind {
            let idx = (local_y / option_h).max(0.0) as usize;
            if idx < options.len() {
                if !option_disabled[idx] {
                    *selected = Some(idx);
                    picked_id = Some(option_ids[idx].clone());
                }
            }
            *open = false;
        }
        if let Some(name) = picked_id {
            self.select_layout(&name);
        }
    }

    /// Clicking a closed `Dropdown`'s own box opens it (closing every other
    /// one first, same as a real app's own `close_other_dropdowns`); clicking
    /// an already-open one's box closes it again.
    fn toggle_dropdown(&mut self, id: NodeId) {
        let was_open = matches!(self.chrome.ui.get(id).kind, NodeKind::Dropdown { open: true, .. });
        self.close_open_dropdowns();
        if let NodeKind::Dropdown { open, .. } = &mut self.chrome.ui.get_mut(id).kind {
            *open = !was_open;
        }
    }

    /// Closes every open `Dropdown` — same "clicking anywhere closes
    /// whatever else was open" convention `nowui_runtime::App::
    /// close_other_dropdowns` already gives a real app, just applied
    /// unconditionally rather than "every *other* one" since the chrome
    /// never has two dropdowns open against each other's clicks in the same
    /// gesture.
    fn close_open_dropdowns(&mut self) {
        for node in &mut self.chrome.ui.nodes {
            if let NodeKind::Dropdown { open, .. } = &mut node.kind {
                *open = false;
            }
        }
    }
}

impl ApplicationHandler for DesignerApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title(format!("NowUI Designer — {}", self.doc.entry_path.display()))
            .with_inner_size(winit::dpi::LogicalSize::new(1200.0, 800.0));
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        let size = window.inner_size();
        self.gpu = Some(GpuSurfaceState::new(window.clone(), size.width.max(1), size.height.max(1)));
        self.window = Some(window);
        self.next_frame = Instant::now();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if now >= self.next_frame {
            if self.watcher.as_ref().is_some_and(FileWatcher::poll_changed) {
                self.reload_from_disk();
            }
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            if let Some(w) = &self.preview_window {
                w.request_redraw();
            }
            self.next_frame = if now.saturating_duration_since(self.next_frame) > FRAME_INTERVAL {
                now + FRAME_INTERVAL
            } else {
                self.next_frame + FRAME_INTERVAL
            };
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        // The detached preview window is a passive mirror — it only ever
        // needs to redraw itself or hand control back to the docked view
        // when closed; every other event (mouse/keyboard/etc.) is the main
        // window's concern only.
        if Some(id) == self.preview_window.as_ref().map(|w| w.id()) {
            match event {
                WindowEvent::CloseRequested => self.toggle_detach(event_loop),
                WindowEvent::RedrawRequested => self.redraw_preview_window(),
                _ => {}
            }
            return;
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Point::new(position.x as f32, position.y as f32);
                self.chrome.ui.cursor = self.cursor;
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                // Click-to-position isn't wired up yet (see `editor.rs`'s
                // own doc comment) — clicking the editor focuses it and
                // puts the caret at the end. A `TreeViewItem`/`Button` hit
                // dispatches to the file tree / tab strip / layout picker
                // (`handle_tree_click`/`handle_button_click`). A click that
                // instead lands in the *preview* (a second, independent
                // `Ui` — see `PreviewDoc`) selects that node and highlights
                // its source span in the editor (`select_in_source`).
                // Anything else clears both, so neither stays highlighted
                // once the user's attention has moved elsewhere.
                // An open dropdown's own popup floats outside the normal
                // hit-test tree — checked first, before anything else, same
                // priority a real app's own click handling gives it.
                if let Some(dropdown_id) = self.find_open_dropdown_popup_at(self.cursor) {
                    self.select_dropdown_option(dropdown_id, self.cursor);
                    return;
                }

                let hit = self.chrome.ui.hit_test(self.cursor);
                if let Some(id) = hit.filter(|&id| matches!(self.chrome.ui.get(id).kind, NodeKind::Dropdown { .. })) {
                    self.toggle_dropdown(id);
                    return;
                }
                self.close_open_dropdowns();
                // Any left click closes the context menu — *except* one
                // landing on one of its own three buttons, which
                // `handle_button_click` (via the generic Button branch
                // below) already closes itself, correctly, only after
                // reading `self.context_menu`'s own target first.
                let is_button = hit.is_some_and(|id| matches!(self.chrome.ui.get(id).kind, NodeKind::Button { .. }));
                if !is_button {
                    self.close_context_menu();
                }
                if hit == Some(self.chrome.editor_node) {
                    self.selected_node = None;
                    self.chrome.ui.focus = Some(self.chrome.editor_node);
                    if let NodeKind::TextInput { label, cursor, selection_anchor, .. } = &mut self.chrome.ui.get_mut(self.chrome.editor_node).kind {
                        *cursor = nowui_core::text_input::char_len(label);
                        *selection_anchor = None;
                    }
                } else if hit == Some(self.chrome.new_item_node) {
                    // Clicking the prompt directly (rather than "+ File"/
                    // "+ Folder" first) just focuses it — typing only does
                    // anything once `self.creating` is actually `Some`
                    // (`confirm_new_item` no-ops otherwise), same as
                    // clicking into any other inert field.
                    self.selected_node = None;
                    self.chrome.ui.focus = Some(self.chrome.new_item_node);
                    let text = self.chrome.new_item_text().to_string();
                    if let NodeKind::TextInput { cursor, selection_anchor, .. } = &mut self.chrome.ui.get_mut(self.chrome.new_item_node).kind {
                        *cursor = nowui_core::text_input::char_len(&text);
                        *selection_anchor = None;
                    }
                } else if let Some(id) = hit.filter(|&id| matches!(self.chrome.ui.get(id).kind, NodeKind::TreeViewItem { .. })) {
                    self.handle_tree_click(id);
                } else if let Some(id) = hit.filter(|&id| matches!(self.chrome.ui.get(id).kind, NodeKind::Button { .. })) {
                    self.handle_button_click(id);
                } else if let Some(preview_hit) = self.doc.ui.hit_test(self.cursor) {
                    self.selected_node = Some(preview_hit);
                    self.select_in_source(preview_hit);
                } else {
                    self.selected_node = None;
                    self.chrome.ui.focus = None;
                }
            }
            // Right-click on a `TreeViewItem` targets that entry (a
            // directory itself, or a file's own parent directory); anywhere
            // else in the chrome targets the project root. Opens the
            // context menu at the cursor, same as a real file explorer.
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } => {
                self.close_open_dropdowns();
                let hit = self.chrome.ui.hit_test(self.cursor);
                let target = hit
                    .filter(|&id| matches!(self.chrome.ui.get(id).kind, NodeKind::TreeViewItem { .. }))
                    .and_then(|id| self.tree_click_path(id))
                    .map(|(path, is_dir)| if is_dir { path } else { path.parent().map(Path::to_path_buf).unwrap_or_else(|| self.vfs.root.clone()) })
                    .unwrap_or_else(|| self.vfs.root.clone());
                self.open_context_menu(target);
            }
            // Scrolls the nearest `scroll-x`/`scroll-y` ancestor of the
            // cursor within the *chrome* (the file-tree sidebar, the
            // scrollable tab strip) — same "nearest-to-cursor scrollable
            // ancestor wins" logic `nowui-runtime`'s own `App` uses for a
            // real app's `scroll-h`/`scroll-v` containers, reimplemented
            // here directly since the designer's chrome isn't driven
            // through `nowui_runtime::App` at all (see this module's own
            // doc comment). The live *preview* document's own scrollable
            // containers aren't reachable this way yet — a preview is
            // read-only content, not the chrome being designed, and isn't
            // in scope for this pass.
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x * 40.0, y * 40.0),
                    MouseScrollDelta::PixelDelta(p) => (p.x as f32, p.y as f32),
                };
                let chain = self.chrome.ui.hit_test_chain(self.cursor);
                for &id in chain.iter().rev() {
                    let style = &self.chrome.ui.get(id).style;
                    let (scroll_x, scroll_y) = (style.scroll_x, style.scroll_y);
                    if !scroll_x && !scroll_y {
                        continue;
                    }
                    let content = self.chrome.ui.get(id).content_size;
                    let rect = self.chrome.ui.get(id).computed;
                    let node = self.chrome.ui.get_mut(id);
                    if scroll_y {
                        let max_y = (content.h - rect.h).max(0.0);
                        node.scroll_offset.y = (node.scroll_offset.y - dy).clamp(0.0, max_y);
                    }
                    if scroll_x {
                        let max_x = (content.w - rect.w).max(0.0);
                        node.scroll_offset.x = (node.scroll_offset.x - dx).clamp(0.0, max_x);
                    }
                    break;
                }
            }
            WindowEvent::KeyboardInput { event: key_event, .. } => {
                if key_event.state != ElementState::Pressed {
                    return;
                }
                // Ctrl+S/Ctrl+D/Ctrl+W are global shortcuts (same convention
                // most editors use) — everything else only applies to the
                // editor while it's actually focused.
                let ctrl = self.modifiers.control_key();
                if ctrl && matches!(&key_event.logical_key, winit::keyboard::Key::Character(c) if c.eq_ignore_ascii_case("s")) {
                    self.save_editor_buffer();
                    return;
                }
                if ctrl && matches!(&key_event.logical_key, winit::keyboard::Key::Character(c) if c.eq_ignore_ascii_case("d")) {
                    self.toggle_detach(event_loop);
                    return;
                }
                if ctrl && matches!(&key_event.logical_key, winit::keyboard::Key::Character(c) if c.eq_ignore_ascii_case("w")) {
                    self.close_active_tab();
                    return;
                }
                if self.context_menu.is_some() && matches!(&key_event.logical_key, winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)) {
                    self.close_context_menu();
                    return;
                }

                // The new-item prompt, while focused, owns Enter (confirm)
                // and Escape (cancel) itself rather than falling through to
                // `edit_text_input` (which wouldn't do anything with either
                // on a non-`multi` field anyway) — checked before the
                // editor-focus gate below since the prompt is a *different*
                // node from the editor.
                if self.chrome.ui.focus == Some(self.chrome.new_item_node) {
                    match &key_event.logical_key {
                        winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter) => {
                            self.confirm_new_item();
                            return;
                        }
                        winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape) => {
                            self.cancel_creating();
                            return;
                        }
                        _ => {
                            let shift = self.modifiers.shift_key();
                            crate::editor::edit_text_input(
                                &mut self.chrome.ui,
                                self.chrome.new_item_node,
                                &key_event.logical_key,
                                key_event.text.as_deref(),
                                shift,
                            );
                            return;
                        }
                    }
                }

                if self.chrome.ui.focus != Some(self.chrome.editor_node) {
                    return;
                }
                let shift = self.modifiers.shift_key();
                let changed = crate::editor::edit_text_input(
                    &mut self.chrome.ui,
                    self.chrome.editor_node,
                    &key_event.logical_key,
                    key_event.text.as_deref(),
                    shift,
                );
                if changed {
                    self.chrome.update_editor_highlighting();
                    self.reload_from_editor_buffer();
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Recursively searches `nodes` (and their `children`) for one whose
    /// `name`/`is_dir` match `pred` — since `scan_tree` now always wraps the
    /// whole project under one visible root folder (see its own doc
    /// comment), a newly created top-level file/folder shows up one level
    /// deeper than `state.tree` itself.
    fn tree_contains(nodes: &[VfsNode], pred: impl Fn(&VfsNode) -> bool + Copy) -> bool {
        nodes.iter().any(|n| pred(n) || tree_contains(&n.children, pred))
    }

    /// Finds a `TreeViewItem` with the given `label` among `ui`'s own *live*
    /// nodes (reachable from a layer root) — a plain `ui.nodes.iter().
    /// position(...)` scan can return a stale orphan left behind by an
    /// earlier region rebuild (see `node_index_among`'s own doc comment on
    /// why `state.tree`'s dynamic `bg_color`/`text_color` brackets mean this
    /// now happens on every `chrome.refresh()`), which would have a
    /// meaningless `computed` rect and never actually receive a real click.
    fn find_live_tree_item(ui: &nowui_core::Ui, label: &str) -> NodeId {
        fn walk(ui: &nowui_core::Ui, id: NodeId, label: &str, out: &mut Option<NodeId>) {
            if matches!(&ui.get(id).kind, NodeKind::TreeViewItem { label: l, .. } if l == label) {
                *out = Some(id);
            }
            for &child in &ui.get(id).children {
                walk(ui, child, label, out);
            }
        }
        let mut found = None;
        for layer in &ui.layers {
            walk(ui, layer.root, label, &mut found);
        }
        found.unwrap_or_else(|| panic!("no live TreeViewItem labeled `{label}`"))
    }

    /// Every live `Button` node, depth-first from each layer's own root —
    /// same rationale as `find_live_tree_item`'s own doc comment.
    fn live_buttons(ui: &nowui_core::Ui) -> Vec<NodeId> {
        fn walk(ui: &nowui_core::Ui, id: NodeId, out: &mut Vec<NodeId>) {
            if matches!(ui.get(id).kind, NodeKind::Button { .. }) {
                out.push(id);
            }
            for &c in &ui.get(id).children {
                walk(ui, c, out);
            }
        }
        let mut out = Vec::new();
        for layer in &ui.layers {
            walk(ui, layer.root, &mut out);
        }
        out
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nowui_designer_app_test_{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Builds a `DesignerApp` the way `main.rs` does — load the entry file
    /// into `Chrome`/`PreviewDoc`, then hand both to `DesignerApp::new` —
    /// but with no real window (every method under test here works fine
    /// without one; `self.window` is only ever touched to update the OS
    /// window's title, which just silently no-ops while `None`).
    fn build_app(entry_path: &std::path::Path, tree: Vec<VfsNode>) -> DesignerApp {
        let state = DesignerState { tree, creating_hint: IDLE_HINT.to_string(), ..Default::default() };
        let doc = PreviewDoc::load(entry_path, "App").unwrap();
        let mut chrome = Chrome::load(&state).unwrap();
        chrome.set_editor_text(&fs::read_to_string(entry_path).unwrap());
        let vfs = crate::virtual_fs::VirtualFs::new(entry_path.parent().unwrap().to_path_buf());
        DesignerApp::new(chrome, doc, state, vfs)
    }

    #[test]
    fn new_seeds_a_single_tab_from_the_initially_opened_file() {
        let dir = scratch_dir("initial_tab");
        let a = dir.join("a.nowui");
        fs::write(&a, "layout: App { Text `a` }").unwrap();
        let app = build_app(&a, Vec::new());
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs.active().unwrap().path, a);
        assert_eq!(app.state.tabs.len(), 1);
        assert!(app.state.tabs[0].active);
    }

    #[test]
    fn open_file_adds_a_new_tab_and_switches_the_preview() {
        let dir = scratch_dir("open_file");
        let a = dir.join("a.nowui");
        let b = dir.join("b.nowui");
        fs::write(&a, "layout: App { Text `from a` }").unwrap();
        fs::write(&b, "layout: App { Text `from b` }").unwrap();
        let mut app = build_app(&a, Vec::new());

        app.open_file(b.clone());

        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.tabs.active().unwrap().path, b);
        assert_eq!(app.chrome.editor_text(), "layout: App { Text `from b` }");
        let root = app.doc.ui.get(app.doc.ui.layers[0].root);
        let NodeKind::Text { content } = &app.doc.ui.get(root.children[0]).kind else { panic!() };
        assert_eq!(content, "from b");
    }

    #[test]
    fn open_file_on_an_already_open_path_switches_instead_of_duplicating() {
        let dir = scratch_dir("open_file_dupe");
        let a = dir.join("a.nowui");
        let b = dir.join("b.nowui");
        fs::write(&a, "layout: App { Text `a` }").unwrap();
        fs::write(&b, "layout: App { Text `b` }").unwrap();
        let mut app = build_app(&a, Vec::new());
        app.open_file(b.clone());
        app.open_file(a.clone());

        assert_eq!(app.tabs.len(), 2, "still only two tabs");
        assert_eq!(app.tabs.active().unwrap().path, a);
    }

    #[test]
    fn switching_tabs_preserves_unsaved_edits_in_the_outgoing_tab() {
        let dir = scratch_dir("preserve_edits");
        let a = dir.join("a.nowui");
        let b = dir.join("b.nowui");
        fs::write(&a, "layout: App { Text `a` }").unwrap();
        fs::write(&b, "layout: App { Text `b` }").unwrap();
        let mut app = build_app(&a, Vec::new());
        app.open_file(b.clone());

        // Simulate an unsaved edit on "b" without touching disk.
        app.chrome.set_editor_text("layout: App { Text `b edited` }");
        app.reload_from_editor_buffer();

        app.open_file(a.clone());
        assert_eq!(app.chrome.editor_text(), "layout: App { Text `a` }");
        app.switch_tab(1); // back to "b"
        assert_eq!(app.chrome.editor_text(), "layout: App { Text `b edited` }", "the unsaved edit on b survived the round trip");
    }

    #[test]
    fn handle_tree_click_opens_the_clicked_files_own_path() {
        let dir = scratch_dir("tree_click");
        let a = dir.join("a.nowui");
        let b = dir.join("b.nowui");
        fs::write(&a, "layout: App { Text `a` }").unwrap();
        fs::write(&b, "layout: App { Text `b` }").unwrap();

        let tree = vec![
            VfsNode { name: "a.nowui".to_string(), path: a.display().to_string(), is_dir: false, ..Default::default() },
            VfsNode { name: "b.nowui".to_string(), path: b.display().to_string(), is_dir: false, ..Default::default() },
        ];
        let mut app = build_app(&a, tree);
        app.chrome.refresh(&app.state.clone());
        // Find the second TreeViewItem (b.nowui) the same live-tree way
        // `node_index_among` itself does — proves the *whole* click -> path
        // -> open_file pipeline, not just `flatten_tree_paths` in isolation.
        let b_item = find_live_tree_item(&app.chrome.ui, "b.nowui");

        app.handle_tree_click(b_item);

        assert_eq!(app.tabs.active().unwrap().path, b);
        assert_eq!(app.chrome.editor_text(), "layout: App { Text `b` }");
    }

    struct NullPainter;
    impl nowui_core::Painter for NullPainter {
        fn fill_rect(&mut self, _: nowui_core::Rect, _: nowui_core::Color, _: nowui_core::Edges) {}
        fn stroke_rect(&mut self, _: nowui_core::Rect, _: nowui_core::Color, _: f32, _: nowui_core::Edges) {}
        fn draw_text(&mut self, _: &str, _: nowui_core::Rect, _: &nowui_core::TextStyle) {}
        fn push_clip(&mut self, _: nowui_core::Rect) {}
        fn pop_clip(&mut self) {}
    }

    #[test]
    fn clicking_a_folders_disclosure_triangle_toggles_collapsed_without_opening_or_selecting_it() {
        let dir = scratch_dir("tree_collapse");
        let a = dir.join("a.nowui");
        fs::create_dir_all(dir.join("widgets")).unwrap();
        let nested = dir.join("widgets").join("Card.nowui");
        fs::write(&a, "layout: App { Text `a` }").unwrap();
        fs::write(&nested, "layout: Card { Text `c` }").unwrap();

        let tree = vec![VfsNode {
            name: "widgets".to_string(),
            path: dir.join("widgets").display().to_string(),
            is_dir: true,
            children: vec![VfsNode { name: "Card.nowui".to_string(), path: nested.display().to_string(), is_dir: false, ..Default::default() }],
            ..Default::default()
        }];
        let mut app = build_app(&a, tree);
        app.chrome.refresh(&app.state.clone());
        nowui_core::layout::solve(&mut app.chrome.ui, Size::new(1200.0, 800.0), &mut NullPainter);

        let widgets_item = find_live_tree_item(&app.chrome.ui, "widgets");

        let row_x = app.chrome.ui.get(widgets_item).computed.x;
        app.cursor.x = row_x; // inside the disclosure-triangle zone (TREE_TRIANGLE_W wide from the row's own x)

        app.handle_tree_click(widgets_item);
        let NodeKind::TreeViewItem { collapsed, .. } = &app.chrome.ui.get(widgets_item).kind else { panic!() };
        assert!(collapsed, "clicking the triangle collapses an expanded folder");
        assert_eq!(app.selected_dir, dir, "the triangle click must not also select the folder as the creation target");
        assert_eq!(app.tabs.len(), 1, "and must not open any tab");

        app.handle_tree_click(widgets_item);
        let NodeKind::TreeViewItem { collapsed, .. } = &app.chrome.ui.get(widgets_item).kind else { panic!() };
        assert!(!collapsed, "clicking it again re-expands it");
    }

    #[test]
    fn opening_a_multi_layout_file_populates_the_layout_picker_with_its_own_hierarchy_paths() {
        let dir = scratch_dir("multi_layout");
        let a = dir.join("a.nowui");
        fs::write(&a, "layout: App { PageLogin } layout: PageLogin { ResultPopUp } layout: ResultPopUp { Text `hi` }").unwrap();
        let app = build_app(&a, Vec::new());

        assert_eq!(app.state.layout_options.len(), 3);
        assert_eq!(app.state.layout_options[0].label, "App");
        assert_eq!(app.state.layout_options[0].id, "App");
        assert_eq!(app.state.layout_options[1].label, "App > PageLogin");
        assert_eq!(app.state.layout_options[1].id, "PageLogin");
        assert_eq!(app.state.layout_options[2].label, "App > PageLogin > ResultPopUp");
        assert_eq!(app.state.layout_options[2].id, "ResultPopUp");
    }

    #[test]
    fn a_layout_never_used_anywhere_is_not_reachable_and_so_never_appears_in_the_picker() {
        let dir = scratch_dir("unreachable_layout");
        let a = dir.join("a.nowui");
        fs::write(&a, "layout: App { PageLogin } layout: PageLogin { Text `hi` } layout: Orphan { Text `never used` }").unwrap();
        let app = build_app(&a, Vec::new());

        assert!(app.state.layout_options.iter().all(|o| o.id != "Orphan"), "Orphan isn't reachable from App, so it shouldn't show up in the picker");
    }

    #[test]
    fn opening_and_picking_a_layout_dropdown_option_switches_the_preview() {
        let dir = scratch_dir("layout_dropdown");
        let a = dir.join("a.nowui");
        fs::write(&a, "layout: App { PageLogin } layout: PageLogin { Text `login` }").unwrap();
        let mut app = build_app(&a, Vec::new());
        app.chrome.refresh(&app.state.clone());
        nowui_core::layout::solve(&mut app.chrome.ui, Size::new(1200.0, 800.0), &mut NullPainter);

        let dropdown_id = {
            fn walk(ui: &nowui_core::Ui, id: NodeId, out: &mut Option<NodeId>) {
                if matches!(ui.get(id).kind, NodeKind::Dropdown { .. }) {
                    *out = Some(id);
                }
                for &c in &ui.get(id).children {
                    walk(ui, c, out);
                }
            }
            let mut found = None;
            for layer in &app.chrome.ui.layers {
                walk(&app.chrome.ui, layer.root, &mut found);
            }
            found.expect("the layout picker's Dropdown should exist")
        };

        app.toggle_dropdown(dropdown_id);
        let NodeKind::Dropdown { open, .. } = &app.chrome.ui.get(dropdown_id).kind else { panic!() };
        assert!(open, "clicking the box opens it");

        let popup = app.dropdown_popup_rect(dropdown_id).expect("an open dropdown has a popup rect");
        let (_, option_h) = nowui_core::dropdown_metrics(app.chrome.ui.get(dropdown_id).style.font_size);
        // The second row — "App > PageLogin" — one option's height down from the top.
        let p = Point::new(popup.x + 5.0, popup.y + option_h + 1.0);
        app.select_dropdown_option(dropdown_id, p);

        let NodeKind::Dropdown { open, .. } = &app.chrome.ui.get(dropdown_id).kind else { panic!() };
        assert!(!open, "picking an option closes the popup");
        assert_eq!(app.doc.entry_layout(), "PageLogin", "picking it renders that layout only");
    }

    #[test]
    fn select_layout_switches_the_preview_without_touching_the_tab_list() {
        let dir = scratch_dir("select_layout");
        let a = dir.join("a.nowui");
        fs::write(&a, "layout: App { Text `main` }\nlayout: Alt { Text `alt` }").unwrap();
        let mut app = build_app(&a, Vec::new());

        app.select_layout("Alt");

        assert_eq!(app.doc.entry_layout(), "Alt");
        assert_eq!(app.tabs.len(), 1, "still the same one tab, just a different layout selected");
        assert_eq!(app.tabs.active().unwrap().selected_layout.as_deref(), Some("Alt"));
        let root = app.doc.ui.get(app.doc.ui.layers[0].root);
        let NodeKind::Text { content } = &app.doc.ui.get(root.children[0]).kind else { panic!() };
        assert_eq!(content, "alt");
    }

    #[test]
    fn a_single_layout_file_leaves_the_picker_empty() {
        let dir = scratch_dir("single_layout");
        let a = dir.join("a.nowui");
        fs::write(&a, "layout: App { Text `main` }").unwrap();
        let app = build_app(&a, Vec::new());
        assert!(app.state.layout_options.is_empty());
    }

    #[test]
    fn close_active_tab_activates_the_previous_tab_and_reloads_the_preview() {
        let dir = scratch_dir("close_tab");
        let a = dir.join("a.nowui");
        let b = dir.join("b.nowui");
        fs::write(&a, "layout: App { Text `a` }").unwrap();
        fs::write(&b, "layout: App { Text `b` }").unwrap();
        let mut app = build_app(&a, Vec::new());
        app.open_file(b.clone());

        app.close_active_tab();

        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs.active().unwrap().path, a);
        assert_eq!(app.chrome.editor_text(), "layout: App { Text `a` }");
    }

    #[test]
    fn confirm_new_item_creates_a_file_at_the_project_root_and_opens_it() {
        let dir = scratch_dir("new_file_root");
        let a = dir.join("a.nowui");
        fs::write(&a, "layout: App { Text `a` }").unwrap();
        let mut app = build_app(&a, Vec::new());

        app.start_creating(NewItemKind::File);
        assert_eq!(app.chrome.ui.focus, Some(app.chrome.new_item_node));
        app.chrome.set_new_item_text("new.nowui");
        app.confirm_new_item();

        assert!(app.creating.is_none(), "the prompt closes after confirming");
        let created = dir.join("new.nowui");
        assert!(created.exists(), "the file should be written to disk");
        assert_eq!(app.tabs.active().unwrap().path, created, "a new file opens as a tab, same as VS Code's own explorer");
        assert!(tree_contains(&app.state.tree, |n| n.name == "new.nowui"), "the tree should reflect the new file");
    }

    #[test]
    fn confirm_new_item_creates_a_folder_without_opening_a_tab() {
        let dir = scratch_dir("new_folder_root");
        let a = dir.join("a.nowui");
        fs::write(&a, "layout: App { Text `a` }").unwrap();
        let mut app = build_app(&a, Vec::new());
        let tab_count_before = app.tabs.len();

        app.start_creating(NewItemKind::Folder);
        app.chrome.set_new_item_text("widgets");
        app.confirm_new_item();

        assert!(dir.join("widgets").is_dir());
        assert_eq!(app.tabs.len(), tab_count_before, "creating a folder doesn't open any tab");
        assert!(tree_contains(&app.state.tree, |n| n.name == "widgets" && n.is_dir));
    }

    #[test]
    fn selecting_a_folder_in_the_tree_targets_new_items_at_it() {
        let dir = scratch_dir("select_folder_target");
        let a = dir.join("a.nowui");
        fs::create_dir_all(dir.join("widgets")).unwrap();
        fs::write(&a, "layout: App { Text `a` }").unwrap();

        let tree = vec![VfsNode { name: "widgets".to_string(), path: dir.join("widgets").display().to_string(), is_dir: true, ..Default::default() }];
        let mut app = build_app(&a, tree);
        app.chrome.refresh(&app.state.clone());
        let widgets_item = find_live_tree_item(&app.chrome.ui, "widgets");

        app.handle_tree_click(widgets_item);
        assert_eq!(app.selected_dir, dir.join("widgets"), "clicking a folder row selects it as the creation target");

        app.start_creating(NewItemKind::File);
        app.chrome.set_new_item_text("Card.nowui");
        app.confirm_new_item();

        assert!(dir.join("widgets/Card.nowui").exists(), "the new file should land inside the selected folder, not the project root");
    }

    #[test]
    fn cancel_creating_discards_the_prompt_without_touching_disk() {
        let dir = scratch_dir("cancel_creating");
        let a = dir.join("a.nowui");
        fs::write(&a, "layout: App { Text `a` }").unwrap();
        let mut app = build_app(&a, Vec::new());

        app.start_creating(NewItemKind::File);
        app.chrome.set_new_item_text("never-created.nowui");
        app.cancel_creating();

        assert!(app.creating.is_none());
        assert_eq!(app.chrome.new_item_text(), "");
        assert!(!dir.join("never-created.nowui").exists());
    }

    #[test]
    fn confirm_new_item_with_an_empty_name_creates_nothing() {
        let dir = scratch_dir("empty_name");
        let a = dir.join("a.nowui");
        fs::write(&a, "layout: App { Text `a` }").unwrap();
        let mut app = build_app(&a, Vec::new());
        let tab_count_before = app.tabs.len();

        app.start_creating(NewItemKind::File);
        app.chrome.set_new_item_text("   ");
        app.confirm_new_item();

        assert!(app.creating.is_none());
        assert_eq!(app.tabs.len(), tab_count_before);
    }

    #[test]
    fn open_context_menu_anchors_at_the_cursor_and_close_parks_it_off_screen() {
        let dir = scratch_dir("context_menu_open_close");
        let a = dir.join("a.nowui");
        fs::write(&a, "layout: App { Text `a` }").unwrap();
        let mut app = build_app(&a, Vec::new());
        app.cursor = Point::new(123.0, 456.0);

        app.open_context_menu(dir.clone());
        assert_eq!(app.context_menu, Some((dir.clone(), Point::new(123.0, 456.0))));
        assert_eq!(app.state.context_menu_left, "123px");
        assert_eq!(app.state.context_menu_top, "456px");

        app.close_context_menu();
        assert!(app.context_menu.is_none());
        assert_eq!(app.state.context_menu_left, POPUP_HIDDEN);
        assert_eq!(app.state.context_menu_top, POPUP_HIDDEN);
    }

    #[test]
    fn start_creating_from_context_menu_targets_the_menus_own_directory_and_closes_it() {
        let dir = scratch_dir("context_menu_new_file");
        let a = dir.join("a.nowui");
        fs::create_dir_all(dir.join("widgets")).unwrap();
        fs::write(&a, "layout: App { Text `a` }").unwrap();
        let mut app = build_app(&a, Vec::new());

        app.open_context_menu(dir.join("widgets"));
        app.start_creating_from_context_menu(NewItemKind::File);

        assert!(app.context_menu.is_none(), "starting a creation closes the context menu");
        assert_eq!(app.selected_dir, dir.join("widgets"));
        assert_eq!(app.creating, Some(NewItemKind::File));

        app.chrome.set_new_item_text("Card.nowui");
        app.confirm_new_item();
        assert!(dir.join("widgets/Card.nowui").exists(), "the file lands in the menu's own target directory");
    }

    #[test]
    fn handle_button_click_routes_context_menu_buttons_to_new_file_and_new_folder() {
        let dir = scratch_dir("context_menu_buttons");
        let a = dir.join("a.nowui");
        fs::write(&a, "layout: App { Text `a` }").unwrap();
        let mut app = build_app(&a, Vec::new());
        app.open_context_menu(dir.clone());
        app.chrome.refresh(&app.state.clone());

        let buttons = live_buttons(&app.chrome.ui);
        // index 0,1 = new-item popup Cancel/Create; 2,3,4 = context menu's
        // New File.../New Folder.../Reveal in File Explorer.
        app.handle_button_click(buttons[3]);

        assert!(app.context_menu.is_none());
        assert_eq!(app.creating, Some(NewItemKind::Folder));
        assert_eq!(app.selected_dir, dir);
    }

    #[test]
    fn handle_button_click_routes_the_first_two_buttons_to_the_popups_cancel_and_create() {
        let dir = scratch_dir("popup_buttons");
        let a = dir.join("a.nowui");
        fs::write(&a, "layout: App { Text `a` }").unwrap();
        let mut app = build_app(&a, Vec::new());
        app.start_creating(NewItemKind::File);
        app.chrome.set_new_item_text("new.nowui");
        app.chrome.refresh(&app.state.clone());

        let buttons = live_buttons(&app.chrome.ui);
        // The new-item popup's own Cancel/Create, the context menu's own
        // three (New File.../New Folder.../Reveal in File Explorer), plus
        // one tab-strip button for `a.nowui` (`build_app`/`DesignerApp::new`
        // always seed one open tab from the initially-opened file) — no
        // `layout:` picker buttons since `a.nowui` only defines one layout.
        assert_eq!(buttons.len(), 6);

        // Cancel (index 0) discards without creating anything.
        app.handle_button_click(buttons[0]);
        assert!(app.creating.is_none());
        assert!(!dir.join("new.nowui").exists());

        // Create (index 1) confirms the in-progress creation.
        app.start_creating(NewItemKind::File);
        app.chrome.set_new_item_text("new.nowui");
        app.chrome.refresh(&app.state.clone());
        let buttons = live_buttons(&app.chrome.ui);
        app.handle_button_click(buttons[1]);
        assert!(app.creating.is_none());
        assert!(dir.join("new.nowui").exists());
    }
}
