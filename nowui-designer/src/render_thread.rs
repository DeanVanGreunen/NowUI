//! Decouples the main window's GPU rendering (chrome + docked preview,
//! composited into one scene) from the winit event/input thread — same
//! architecture, and the same platform gotcha, as `nowui-runtime`'s own
//! `render_thread` module (see its doc comment for the full picture):
//! `App::redraw`/`DesignerApp::redraw` still run `layout::solve` on the
//! main thread every frame (`Node::computed`/scroll state must be correct
//! *this* frame for hit-testing, with zero lag — only the thread that owns
//! the `Ui` can update that before the next input event needs it), using a
//! throwaway CPU `SkiaPainter` purely for text measurement, then hand the
//! already-solved `Ui`(s) off here. This thread only ever builds the paint
//! scene and submits/presents it — never re-solves, never touches app
//! state.
//!
//! `GpuSurfaceState` is built on the main thread (`DesignerApp::resumed`)
//! and *moved* into this one at `spawn`, not constructed here from a raw
//! window handle — acquiring a `winit::window::Window`'s raw handle from a
//! thread other than the one that created it fails outright on this
//! platform (a real, reproduced failure during `nowui-runtime`'s own
//! version of this change), even though the resulting `wgpu`/`vello`
//! objects, once built, are not thread-affine.
//!
//! Scoped to the *main* window only. The optional detached preview window
//! (`Ctrl+D` — `DesignerApp::preview_window`/`redraw_preview_window`) keeps
//! rendering synchronously on the main thread: a second independent,
//! less-frequently-used GPU pipeline wasn't judged worth threading too for
//! this pass — the main window is what "clicking on UI elements" actually
//! interacts with.

use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use nowui_core::{Color, Edges, NodeId, Painter, Rect, Size, Ui};
use nowui_render_gpu::{GpuFontCache, GpuPainter, GpuSurfaceState};

/// Matches `app.rs`'s own `CLEAR` — kept separate since this module never
/// touches `DesignerApp` directly.
const CLEAR: Color = Color { r: 0x1e, g: 0x1e, b: 0x1e, a: 255 };

/// Selection-outline color — matches `app.rs`'s own inline
/// `Color::rgb(0x60, 0xa5, 0xfa)` used before this was extracted.
const SELECTION_OUTLINE: Color = Color { r: 0x60, g: 0xa5, b: 0xfa, a: 255 };

struct PreviewSnapshot {
    ui: Ui,
    /// Where to composite it — already includes the slot's own inset
    /// padding (see `Chrome::preview_slot_rect`).
    slot_rect: Rect,
    /// The inspector's own currently-selected preview node, if any — drawn
    /// as an outline directly (not part of the previewed document itself).
    selected: Option<NodeId>,
}

/// One frame's worth of already-solved state — `preview` is `None` exactly
/// while the preview is detached into its own window (nothing to composite
/// into the main window's own now-empty slot).
struct Snapshot {
    chrome_ui: Ui,
    size: Size,
    preview: Option<PreviewSnapshot>,
}

/// The single-slot channel the main thread publishes into and the render
/// thread waits on — see `nowui_runtime::render_thread`'s own `Shared` for
/// why a slot, not a queue: only the freshest frame is ever worth keeping.
struct Shared {
    slot: Mutex<Option<Snapshot>>,
    condvar: Condvar,
    stop: Mutex<bool>,
}

pub struct RenderThread {
    shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
}

impl RenderThread {
    /// Spawns the render thread, taking ownership of an already-built
    /// `gpu` (see this module's own doc comment for why it can't be built
    /// here instead) — its own `GpuFontCache`/`TextContext` are constructed
    /// fresh inside the thread.
    pub fn spawn(mut gpu: GpuSurfaceState) -> Self {
        let shared = Arc::new(Shared { slot: Mutex::new(None), condvar: Condvar::new(), stop: Mutex::new(false) });
        let shared_thread = shared.clone();
        let handle = std::thread::Builder::new()
            .name("nowui-designer-render".to_string())
            .spawn(move || {
                let mut font_cache = GpuFontCache::new();
                let mut text = nowui_text::TextContext::new();

                loop {
                    let snapshot = {
                        let mut slot = shared_thread.slot.lock().unwrap();
                        loop {
                            if *shared_thread.stop.lock().unwrap() {
                                return;
                            }
                            if let Some(s) = slot.take() {
                                break s;
                            }
                            slot = shared_thread.condvar.wait(slot).unwrap();
                        }
                    };

                    let mut scene = vello::Scene::new();
                    {
                        let mut painter = GpuPainter::new(&mut scene, &mut text, &mut font_cache);
                        nowui_core::paint::paint(&snapshot.chrome_ui, &mut painter);
                        if let Some(preview) = &snapshot.preview {
                            painter.push_clip(preview.slot_rect);
                            nowui_core::paint::paint(&preview.ui, &mut painter);
                            painter.pop_clip();
                            if let Some(id) = preview.selected {
                                let rect = preview.ui.get(id).computed;
                                painter.stroke_rect(rect, SELECTION_OUTLINE, 2.0, Edges::default());
                            }
                        }
                    }
                    gpu.resize(snapshot.size.w as u32, snapshot.size.h as u32);
                    gpu.render_and_present(&scene, CLEAR);
                }
            })
            .expect("failed to spawn nowui-designer-render thread");
        RenderThread { shared, handle: Some(handle) }
    }

    /// Publishes a new frame — `chrome_ui` and (while docked) the preview's
    /// own `ui` must already be solved (`layout::solve`/`solve_into`) by
    /// the caller. Never blocks on the render thread actually consuming
    /// it — see `Shared`'s own doc comment.
    pub fn publish(&self, chrome_ui: Ui, size: Size, preview: Option<(Ui, Rect, Option<NodeId>)>) {
        let mut slot = self.shared.slot.lock().unwrap();
        *slot = Some(Snapshot {
            chrome_ui,
            size,
            preview: preview.map(|(ui, slot_rect, selected)| PreviewSnapshot { ui, slot_rect, selected }),
        });
        self.shared.condvar.notify_one();
    }
}

impl Drop for RenderThread {
    /// Signals the render thread to stop and joins it — called (via
    /// dropping `DesignerApp::render_thread`) before the main window is
    /// torn down, so no in-flight `render_and_present` races its
    /// destruction.
    fn drop(&mut self) {
        *self.shared.stop.lock().unwrap() = true;
        self.shared.condvar.notify_one();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
