//! Pure open-tab bookkeeping — no `Ui`/winit dependency, so opening,
//! switching, and closing tabs is unit-testable without a real window or
//! `Chrome`/`PreviewDoc` in play. `app::DesignerApp` is what wires this to
//! the real editor buffer/live preview each time the active tab changes
//! (`sync_editor_into_active_tab`/`load_active_tab_into_editor_and_preview`).

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct OpenTab {
    pub path: PathBuf,
    /// The editor's own buffer for this tab — kept here (not just live in
    /// the one shared editor `TextInput`) so switching away and back
    /// doesn't lose unsaved edits; only the *active* tab's buffer is ever
    /// actually loaded into the real editor node at a given moment.
    pub buffer: String,
    pub dirty: bool,
    pub cursor: usize,
    pub selection_anchor: Option<usize>,
    /// The `layout:` name currently selected for this tab's own preview —
    /// `None` until this file has actually been loaded at least once.
    pub selected_layout: Option<String>,
}

/// An ordered list of open tabs plus which one (if any) is active — deliberately
/// not just a `Vec<OpenTab>` with an `Option<usize>` field on some larger
/// struct, so `open_or_focus`/`close`'s own index bookkeeping can't drift
/// out of sync with the list it indexes into.
#[derive(Default)]
pub struct Tabs {
    open: Vec<OpenTab>,
    active: Option<usize>,
}

impl Tabs {
    /// If `path` is already open, just switches to it (returning its index
    /// and `false`); otherwise opens a new tab at the end of the list via
    /// `initial_buffer` (called only in the "actually new" case, so a
    /// caller can pass e.g. `|| fs::read_to_string(path).unwrap_or_default()`
    /// without paying for a disk read on every click of an already-open
    /// file) and returns its index and `true`.
    pub fn open_or_focus(&mut self, path: &Path, initial_buffer: impl FnOnce() -> String) -> (usize, bool) {
        if let Some(i) = self.open.iter().position(|t| t.path == path) {
            self.active = Some(i);
            return (i, false);
        }
        self.open.push(OpenTab {
            path: path.to_path_buf(),
            buffer: initial_buffer(),
            dirty: false,
            cursor: 0,
            selection_anchor: None,
            selected_layout: None,
        });
        let i = self.open.len() - 1;
        self.active = Some(i);
        (i, true)
    }

    pub fn active_index(&self) -> Option<usize> {
        self.active
    }

    pub fn active(&self) -> Option<&OpenTab> {
        self.active.map(|i| &self.open[i])
    }

    pub fn active_mut(&mut self) -> Option<&mut OpenTab> {
        self.active.and_then(move |i| self.open.get_mut(i))
    }

    pub fn len(&self) -> usize {
        self.open.len()
    }

    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &OpenTab> {
        self.open.iter()
    }

    /// Switches to the tab at `index`. Returns `false` (no-op) if out of
    /// range — a stale click-mapped index (e.g. the tab list changed
    /// between hit-test and dispatch) shouldn't panic.
    pub fn switch_to(&mut self, index: usize) -> bool {
        if index < self.open.len() {
            self.active = Some(index);
            true
        } else {
            false
        }
    }

    /// Closes the tab at `index`. If `index` *was* the active tab, the new
    /// active tab (if any remain) is whichever one now sits at the same
    /// index — the next tab to the right, or the new last tab if the closed
    /// one was rightmost — the same convention most tabbed editors use.
    /// Closing a *different* tab (e.g. a background tab's own close button)
    /// leaves whichever tab was already active still active — its own
    /// index just shifts left by one if it sat to the right of the closed
    /// tab. No-op if `index` is out of range.
    pub fn close(&mut self, index: usize) {
        if index >= self.open.len() {
            return;
        }
        self.open.remove(index);
        self.active = match self.active {
            None => None,
            Some(_) if self.open.is_empty() => None,
            Some(active) if active == index => Some(index.min(self.open.len() - 1)),
            Some(active) if active > index => Some(active - 1),
            Some(active) => Some(active),
        };
    }

    /// Rewrites every open tab's own path that equals `old_prefix` or sits
    /// nested under it (a folder rename affecting the files inside it) to
    /// the same path rooted at `new_prefix` instead — called after
    /// `VirtualFs::rename` succeeds, so an open tab keeps editing the same
    /// file/folder under its new name/location instead of pointing at a
    /// now-stale path. Returns whether the *active* tab's own path changed
    /// (the caller needs to know whether to reload the editor/preview).
    pub fn retarget_paths(&mut self, old_prefix: &Path, new_prefix: &Path) -> bool {
        let mut active_changed = false;
        for (i, tab) in self.open.iter_mut().enumerate() {
            if let Ok(rel) = tab.path.strip_prefix(old_prefix) {
                tab.path = new_prefix.join(rel);
                if Some(i) == self.active {
                    active_changed = true;
                }
            }
        }
        active_changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_or_focus_opens_a_new_tab_and_activates_it() {
        let mut tabs = Tabs::default();
        let (i, is_new) = tabs.open_or_focus(Path::new("/a.nowui"), || "content a".to_string());
        assert_eq!(i, 0);
        assert!(is_new);
        assert_eq!(tabs.active_index(), Some(0));
        assert_eq!(tabs.active().unwrap().buffer, "content a");
        assert_eq!(tabs.len(), 1);
    }

    #[test]
    fn open_or_focus_switches_to_an_already_open_tab_instead_of_duplicating() {
        let mut tabs = Tabs::default();
        tabs.open_or_focus(Path::new("/a.nowui"), || "a".to_string());
        tabs.open_or_focus(Path::new("/b.nowui"), || "b".to_string());
        let (i, is_new) = tabs.open_or_focus(Path::new("/a.nowui"), || panic!("must not re-read an already-open file"));
        assert_eq!(i, 0);
        assert!(!is_new);
        assert_eq!(tabs.len(), 2, "still only two tabs, not a duplicate");
        assert_eq!(tabs.active_index(), Some(0));
    }

    #[test]
    fn switch_to_changes_the_active_index_and_rejects_out_of_range() {
        let mut tabs = Tabs::default();
        tabs.open_or_focus(Path::new("/a.nowui"), || "a".to_string());
        tabs.open_or_focus(Path::new("/b.nowui"), || "b".to_string());
        assert!(tabs.switch_to(0));
        assert_eq!(tabs.active_index(), Some(0));
        assert!(!tabs.switch_to(5));
        assert_eq!(tabs.active_index(), Some(0), "an out-of-range switch is a no-op, not a panic or a silent wrap");
    }

    #[test]
    fn close_picks_the_tab_now_at_the_same_index_as_the_new_active_one() {
        let mut tabs = Tabs::default();
        tabs.open_or_focus(Path::new("/a.nowui"), || "a".to_string());
        tabs.open_or_focus(Path::new("/b.nowui"), || "b".to_string());
        tabs.open_or_focus(Path::new("/c.nowui"), || "c".to_string());
        tabs.switch_to(1); // "b" active

        tabs.close(1); // closes "b" — "c" (now at index 1) becomes active
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs.active().unwrap().path, PathBuf::from("/c.nowui"));
    }

    #[test]
    fn closing_a_background_tab_leaves_the_active_one_active() {
        // Regression: closing a tab via its own close button (not
        // necessarily the active one) must not steal activeness — the
        // previous implementation always recomputed `active` from the
        // *closed* index, so closing an unrelated background tab could
        // silently switch which tab was active.
        let mut tabs = Tabs::default();
        tabs.open_or_focus(Path::new("/a.nowui"), || "a".to_string());
        tabs.open_or_focus(Path::new("/b.nowui"), || "b".to_string());
        tabs.open_or_focus(Path::new("/c.nowui"), || "c".to_string());
        tabs.switch_to(0); // "a" active

        tabs.close(2); // closes "c", a background tab to the right of "a"
        assert_eq!(tabs.active().unwrap().path, PathBuf::from("/a.nowui"), "the active tab must not change");

        tabs.switch_to(1); // "b" active (now at index 1: [a, b])
        tabs.close(0); // closes "a", a background tab to the *left* of "b"
        assert_eq!(tabs.active().unwrap().path, PathBuf::from("/b.nowui"), "still active, just shifted to index 0");
    }

    #[test]
    fn close_the_last_remaining_tab_leaves_no_active_tab() {
        let mut tabs = Tabs::default();
        tabs.open_or_focus(Path::new("/a.nowui"), || "a".to_string());
        tabs.close(0);
        assert!(tabs.is_empty());
        assert_eq!(tabs.active_index(), None);
    }

    #[test]
    fn close_the_rightmost_tab_activates_the_new_rightmost() {
        let mut tabs = Tabs::default();
        tabs.open_or_focus(Path::new("/a.nowui"), || "a".to_string());
        tabs.open_or_focus(Path::new("/b.nowui"), || "b".to_string());
        tabs.close(1); // closes "b", the rightmost — "a" should become active
        assert_eq!(tabs.active().unwrap().path, PathBuf::from("/a.nowui"));
    }

    #[test]
    fn retarget_paths_rewrites_a_renamed_folders_own_nested_tabs() {
        let mut tabs = Tabs::default();
        tabs.open_or_focus(Path::new("/proj/widgets/Card.nowui"), || "card".to_string());
        tabs.open_or_focus(Path::new("/proj/other.nowui"), || "other".to_string());
        tabs.switch_to(0);

        let active_changed = tabs.retarget_paths(Path::new("/proj/widgets"), Path::new("/proj/controls"));

        assert!(active_changed, "the active tab (index 0) was nested under the renamed folder");
        assert_eq!(tabs.active().unwrap().path, PathBuf::from("/proj/controls/Card.nowui"));
        // The sibling tab, not nested under the renamed folder, is untouched.
        assert_eq!(tabs.iter().nth(1).unwrap().path, PathBuf::from("/proj/other.nowui"));
    }

    #[test]
    fn retarget_paths_handles_a_plain_file_rename_not_just_a_folder() {
        let mut tabs = Tabs::default();
        tabs.open_or_focus(Path::new("/proj/a.nowui"), || "a".to_string());
        tabs.switch_to(0);

        let active_changed = tabs.retarget_paths(Path::new("/proj/a.nowui"), Path::new("/proj/renamed.nowui"));

        assert!(active_changed);
        assert_eq!(tabs.active().unwrap().path, PathBuf::from("/proj/renamed.nowui"));
    }
}
