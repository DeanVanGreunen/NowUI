//! Watches a `PreviewDoc`'s own transitive `#`-import set on disk and
//! surfaces "something changed" to `app.rs`'s redraw loop — the trigger
//! half of live reload (`preview::PreviewDoc::reload_with_overrides` is the
//! half that actually re-resolves and rebuilds). Debounced only in the
//! loose sense that `poll_changed` drains every queued event into one
//! `bool` per call — a burst of saves (many editors write a file more than
//! once per save) collapses to a single reload on the next redraw, not one
//! per underlying OS event.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};

use notify::{RecursiveMode, Watcher};

pub struct FileWatcher {
    watcher: notify::RecommendedWatcher,
    rx: Receiver<()>,
    watched: Vec<PathBuf>,
}

impl FileWatcher {
    pub fn new() -> notify::Result<Self> {
        let (tx, rx) = channel();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if res.is_ok() {
                // The exact event kind doesn't matter here — any change to
                // a watched file is a reload trigger; `poll_changed`
                // collapses however many arrive into one redraw-time check.
                let _ = tx.send(());
            }
        })?;
        Ok(FileWatcher { watcher, rx, watched: Vec::new() })
    }

    /// Replace the watched set with exactly `paths` — unwatches anything no
    /// longer present, watches anything new. Called after every successful
    /// reload with the document's freshly-resolved import graph, so the
    /// watch set never drifts from what's actually `#`-imported (a file
    /// that stopped being imported stops triggering reloads; a newly added
    /// one starts).
    pub fn set_watched(&mut self, paths: &[PathBuf]) {
        for old in &self.watched {
            if !paths.contains(old) {
                let _ = self.watcher.unwatch(old);
            }
        }
        for new in paths {
            if !self.watched.contains(new) {
                let _ = self.watcher.watch(new, RecursiveMode::NonRecursive);
            }
        }
        self.watched = paths.to_vec();
    }

    /// Drain every queued change event; `true` if at least one arrived
    /// since the last call.
    pub fn poll_changed(&self) -> bool {
        let mut changed = false;
        loop {
            match self.rx.try_recv() {
                Ok(()) => changed = true,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        changed
    }
}

/// Convenience used by tests and (eventually) a "watcher unavailable"
/// fallback path in `app.rs` — not every environment can create a real
/// filesystem watcher (sandboxed CI, certain container setups), and a
/// designer that can't watch files should still run, just without live
/// reload-on-external-edit.
pub fn try_new_watcher() -> Option<FileWatcher> {
    FileWatcher::new().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, Instant};

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nowui_designer_watcher_test_{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// File-system watch delivery is asynchronous — poll for up to a few
    /// seconds instead of asserting immediately after the write.
    fn wait_for_change(watcher: &FileWatcher, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if watcher.poll_changed() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    #[test]
    fn detects_a_change_to_a_watched_file() {
        let dir = scratch_dir("detect");
        let path = dir.join("main.nowui");
        fs::write(&path, "layout: App {}").unwrap();

        let mut watcher = FileWatcher::new().expect("should create a watcher");
        watcher.set_watched(&[path.clone()]);
        // The watch registration itself is also async on some platforms —
        // give it a moment before writing, or the very first write can race
        // ahead of the watch actually being armed.
        std::thread::sleep(Duration::from_millis(100));

        fs::write(&path, "layout: App { Text `changed` }").unwrap();
        assert!(wait_for_change(&watcher, Duration::from_secs(5)), "a write to a watched file should be detected");
    }

    #[test]
    fn ignores_a_change_to_a_file_that_was_never_watched() {
        let dir = scratch_dir("ignore");
        let watched_path = dir.join("watched.nowui");
        let other_path = dir.join("other.nowui");
        fs::write(&watched_path, "layout: App {}").unwrap();
        fs::write(&other_path, "layout: Other {}").unwrap();

        let mut watcher = FileWatcher::new().expect("should create a watcher");
        watcher.set_watched(&[watched_path]);
        std::thread::sleep(Duration::from_millis(100));

        fs::write(&other_path, "layout: Other { Text `changed` }").unwrap();
        assert!(!wait_for_change(&watcher, Duration::from_secs(2)), "a change to an unwatched file must not trigger a reload");
    }

    #[test]
    fn set_watched_stops_watching_a_path_that_is_no_longer_in_the_new_set() {
        let dir = scratch_dir("unwatch");
        let path = dir.join("main.nowui");
        fs::write(&path, "layout: App {}").unwrap();

        let mut watcher = FileWatcher::new().expect("should create a watcher");
        watcher.set_watched(&[path.clone()]);
        std::thread::sleep(Duration::from_millis(100));
        watcher.set_watched(&[]); // no longer watching `path`
        std::thread::sleep(Duration::from_millis(100));

        fs::write(&path, "layout: App { Text `changed` }").unwrap();
        assert!(!wait_for_change(&watcher, Duration::from_secs(2)), "a path removed from the watch set must not still trigger reloads");
    }
}
