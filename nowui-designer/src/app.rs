//! The designer's own `winit::application::ApplicationHandler` — built
//! directly on `nowui_core`/`nowui_render_gpu`'s lower-level pieces (not
//! `nowui_runtime::App`/`run_path`, which each own a whole `EventLoop` and
//! exactly one window — see `preview.rs`'s module doc for why). Currently a
//! single undetachable read-only preview window (this crate's first
//! build-order stage); the multi-window chrome+preview split, live reload,
//! and detach all land in later stages without changing this shape much —
//! just what gets rendered into which window.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nowui_core::{NodeKind, Point, Size};
use nowui_render_gpu::{GpuFontCache, GpuPainter, GpuSurfaceState};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

use crate::chrome::Chrome;
use crate::preview::PreviewDoc;
use crate::state::DesignerState;
use crate::watcher::FileWatcher;

/// Same fixed-60fps-loop convention `nowui-runtime`'s own `App` uses (see
/// its module doc / CLAUDE.md's "Runtime gotchas") — not on-demand redraw.
const FRAME_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / 60);
const CLEAR: nowui_core::Color = nowui_core::Color { r: 0x1e, g: 0x1e, b: 0x1e, a: 255 };

pub struct DesignerApp {
    pub chrome: Chrome,
    pub doc: PreviewDoc,
    pub state: DesignerState,
    /// `None` in an environment that can't create a real filesystem watcher
    /// (see `watcher::try_new_watcher`'s own doc comment) — the designer
    /// still runs, just without reload-on-external-edit.
    watcher: Option<FileWatcher>,
    window: Option<Arc<Window>>,
    gpu: Option<GpuSurfaceState>,
    text: nowui_text::TextContext,
    font_cache: GpuFontCache,
    next_frame: Instant,
    cursor: Point,
    modifiers: ModifiersState,
}

impl DesignerApp {
    pub fn new(chrome: Chrome, doc: PreviewDoc, state: DesignerState) -> Self {
        let mut watcher = crate::watcher::try_new_watcher();
        if let (Some(w), Ok(files)) = (&mut watcher, doc.imported_files()) {
            w.set_watched(&files);
        }
        DesignerApp {
            chrome,
            doc,
            state,
            watcher,
            window: None,
            gpu: None,
            text: nowui_text::TextContext::new(),
            font_cache: GpuFontCache::new(),
            next_frame: Instant::now(),
            cursor: Point::default(),
            modifiers: ModifiersState::empty(),
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
        let mut overrides = HashMap::new();
        overrides.insert(self.doc.entry_path.clone(), self.chrome.editor_text().to_string());
        if let Err(e) = self.doc.reload_with_overrides(&overrides) {
            // A mid-edit syntax error is expected and common — not logged
            // as an error, the same "leave the last good Ui in place"
            // behavior `PreviewDoc::reload_with_overrides` already gives
            // the caller for free.
            let _ = e;
        }
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
        }
    }

    /// Re-resolves the live document straight from disk (no unsaved-buffer
    /// overrides yet — those arrive with the editor) and re-arms the
    /// watcher with whatever it imports *now*, since a reload can change
    /// the import graph itself (an added/removed `#` import). A failed
    /// reload (a syntax error mid-edit in an external editor) is logged and
    /// otherwise ignored — `PreviewDoc::reload_with_overrides` already
    /// leaves the last good `Ui` in place rather than blanking the preview.
    fn reload_from_disk(&mut self) {
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

        let slot_rect = self.chrome.preview_slot_rect();
        {
            let mut painter = GpuPainter::new(&mut scene, &mut self.text, &mut self.font_cache);
            self.doc.render_into(slot_rect, &mut painter);
        }

        gpu.resize(w, h);
        gpu.render_and_present(&scene, CLEAR);
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
            self.next_frame = if now.saturating_duration_since(self.next_frame) > FRAME_INTERVAL {
                now + FRAME_INTERVAL
            } else {
                self.next_frame + FRAME_INTERVAL
            };
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
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
                // puts the caret at the end; clicking anything else clears
                // focus, so keystrokes don't land in the editor by
                // accident once the user's attention has moved elsewhere.
                let hit = self.chrome.ui.hit_test(self.cursor);
                if hit == Some(self.chrome.editor_node) {
                    self.chrome.ui.focus = Some(self.chrome.editor_node);
                    if let NodeKind::TextInput { label, cursor, selection_anchor, .. } = &mut self.chrome.ui.get_mut(self.chrome.editor_node).kind {
                        *cursor = nowui_core::text_input::char_len(label);
                        *selection_anchor = None;
                    }
                } else {
                    self.chrome.ui.focus = None;
                }
            }
            WindowEvent::KeyboardInput { event: key_event, .. } => {
                if key_event.state != ElementState::Pressed {
                    return;
                }
                // Ctrl+S saves regardless of focus (a global shortcut, same
                // convention most editors use) — everything else only
                // applies to the editor while it's actually focused.
                let ctrl = self.modifiers.control_key();
                if ctrl && matches!(&key_event.logical_key, winit::keyboard::Key::Character(c) if c.eq_ignore_ascii_case("s")) {
                    self.save_editor_buffer();
                    return;
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
                    self.reload_from_editor_buffer();
                }
            }
            _ => {}
        }
    }
}
