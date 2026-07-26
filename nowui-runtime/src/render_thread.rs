//! Decouples GPU rendering from the winit event/input thread.
//!
//! `App::redraw` (GPU backend) still runs `layout::solve` on the main
//! thread every frame — needed regardless, since `Node::computed`/
//! `scroll_offset`/`Ui::auto_scroll` must be correct *this* frame for
//! hit-testing/scroll-follow-caret with zero lag (see `App::redraw_gpu`'s
//! own doc comment). What moves off the main thread is everything after
//! that: building the paint `Scene` and submitting/presenting it to the
//! GPU. The main thread publishes an already-solved `Ui` snapshot here;
//! this thread only ever paints it and presents — never re-solves, never
//! touches app state `S`, never touches the `Window` beyond what was baked
//! into `GpuSurfaceState` at construction.
//!
//! Scoped to `Backend::Gpu` only. `Backend::Cpu`'s `softbuffer::Surface`
//! wraps the raw platform window handle directly (an `HWND` on Windows via
//! GDI) — presenting from a thread other than the one that owns the window
//! isn't documented as safe for that path, so the CPU backend keeps
//! rendering on the main thread, unchanged.
//!
//! `GpuSurfaceState::render_and_present`/`resize` never take a `&Window`
//! argument — only `GpuSurfaceState::new` does, at construction (see its
//! own doc comment on why the window handle just needs to be
//! `Send + Sync`, not thread-affine) — so once built, this thread needs no
//! window access at all, only the `Ui` snapshots handed to it.
//!
//! `Ui` derives `Clone` (`nowui-core/src/arena.rs`) with no thread-affine
//! handles of its own — fonts/painter resources live on `GpuFontCache`/
//! `TextContext`, never on `Ui` — so publishing a snapshot is a plain (if
//! non-trivial: a decoded image/icon's raw pixel buffer is deep-copied, not
//! `Arc`-shared) clone.
//!
//! `GpuSurfaceState` itself is **constructed on the main thread**
//! (`App::resumed`, as before threading) and *moved* into this one at
//! `spawn` — not built here from a raw `window: Arc<Window>` handle. An
//! earlier version of this module tried the latter and hit a real,
//! reproducible failure: querying a `winit::window::Window`'s raw window
//! handle from a thread other than the one that created it fails outright
//! on this platform (`WgpuCreateSurfaceError(RawHandle(Unavailable))`) —
//! window-handle *acquisition* is thread-affine even though the resulting
//! `wgpu::Surface`/`Device`/`Queue` (once built) are not. `GpuFontCache`/
//! `TextContext` have no such constraint and are still constructed fresh
//! inside the spawned thread (never moved across threads at all — the
//! simplest way to sidestep asking whether they're `Send`).

use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use nowui_core::{Size, Ui};
use nowui_render_gpu::{GpuFontCache, GpuPainter, GpuSurfaceState};

/// Matches `app.rs`'s own `CLEAR` — kept as a separate constant since this
/// module never touches `App` directly.
const CLEAR: nowui_core::Color = nowui_core::Color { r: 0x26, g: 0x80, b: 0xd4, a: 255 };

struct Snapshot {
    ui: Ui,
    size: Size,
}

/// The single-slot channel the main thread publishes into and the render
/// thread waits on — deliberately a slot, not a queue: the render thread
/// only ever wants the *freshest* state. If it's still painting/presenting
/// frame N when the main thread finishes solving frame N+1, publishing
/// just overwrites the slot — frame N+1 supersedes N outright rather than
/// queuing behind it, so a render thread that's fallen behind catches up
/// by skipping straight to the newest frame instead of working through a
/// backlog of now-stale ones.
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
    /// Spawns the render thread, taking ownership of an *already-built*
    /// `gpu` (see this module's own doc comment for why it can't be
    /// constructed here from a raw window handle instead) — this thread's
    /// own `GpuFontCache`/`TextContext` are still built fresh inside it.
    pub fn spawn(mut gpu: GpuSurfaceState) -> Self {
        let shared = Arc::new(Shared { slot: Mutex::new(None), condvar: Condvar::new(), stop: Mutex::new(false) });
        let shared_thread = shared.clone();
        let handle = std::thread::Builder::new()
            .name("nowui-render".to_string())
            .spawn(move || {
                let mut font_cache = GpuFontCache::new();
                let mut text = nowui_render::TextContext::new();

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
                        nowui_core::paint::paint(&snapshot.ui, &mut painter);
                    }
                    gpu.resize(snapshot.size.w as u32, snapshot.size.h as u32);
                    gpu.render_and_present(&scene, CLEAR);
                }
            })
            .expect("failed to spawn nowui-render thread");
        RenderThread { shared, handle: Some(handle) }
    }

    /// Publishes a new, already-solved `Ui` snapshot to render — never
    /// blocks on the render thread actually consuming it (see `Shared`'s
    /// own doc comment). `size` is the window's current physical size,
    /// re-applied via `GpuSurfaceState::resize` every frame on the render
    /// thread's own side, same as the pre-threading code path did.
    pub fn publish(&self, ui: Ui, size: Size) {
        let mut slot = self.shared.slot.lock().unwrap();
        *slot = Some(Snapshot { ui, size });
        self.shared.condvar.notify_one();
    }
}

impl Drop for RenderThread {
    /// Signals the render thread to stop and joins it before returning —
    /// called (via dropping `App::render_thread`) before the window itself
    /// is torn down, so no in-flight `render_and_present` races the
    /// window's own destruction. Blocks the calling (main) thread for at
    /// most however long the render thread's current in-flight
    /// `render_and_present` takes to finish — bounded, and only ever paid
    /// once, on shutdown.
    fn drop(&mut self) {
        *self.shared.stop.lock().unwrap() = true;
        self.shared.condvar.notify_one();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
