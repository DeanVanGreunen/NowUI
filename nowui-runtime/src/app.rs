//! The winit application harness: window + softbuffer surface, event-driven
//! redraw (ControlFlow::Wait), and the solve -> paint -> present cycle guarded
//! by a dirty flag.
//!
//! Reactivity lives here too: each redraw, every node's `value_path`
//! (`resolve_values`), backtick `${state.path}` templates (`resolve_templates`),
//! and style-bracket `${state.path}` interpolation (`resolve_dynamic_styles`)
//! are resolved against the live `S: NowUiState` app state and written into
//! the widget/style; every dispatched DOM-ish event (`onClick`,
//! `onMouseDown`, ...) calls back into it (`dispatch_event`), handing the
//! callback a live `&mut` handle to the node it fired on. See CLAUDE.md's
//! "Reactivity" section for the full read/write data flow and exactly which
//! widgets/events are wired.
//!
//! winit API note: this targets winit 0.30 (`ApplicationHandler` + `run_app`,
//! `resumed(&ActiveEventLoop)`). These names were introduced in 0.30 and do not
//! exist on 0.29 or earlier. If a future winit reshapes these callbacks
//! (e.g. `can_create_surfaces`, `&dyn ActiveEventLoop`), align the method
//! signatures with the docs for that version — the logic is unchanged.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nowui_core::{
    compute_effective, display_string, dropdown_metrics, AnimatableStyle, Color, CursorIcon, Event,
    EventKind, NodeId, NodeKind, NowUiState, Point, Rect, Size, StateValue, TemplatePart, Ui,
};
use nowui_render::{present_to_softbuffer, SkiaPainter, TextContext};
use nowui_render_gpu::{GpuFontCache, GpuPainter, GpuSurfaceState};
use tiny_skia::Pixmap;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use crate::transitions::Transitions;
use crate::Backend;

/// Background color painted before the tree each frame (opaque so premultiplied
/// == straight for the softbuffer bridge).
const CLEAR: Color = Color { r: 0x26, g: 0x80, b: 0xd4, a: 255 };

/// A fixed 60fps game loop, by explicit request — a deliberate departure
/// from this engine's original "event-driven, not a game loop" design (see
/// `ControlFlow::WaitUntil` usage in `about_to_wait`): every frame redraws
/// unconditionally, whether or not anything actually changed.
const FRAME_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / 60);

pub struct App<S: NowUiState + 'static> {
    /// The OS window's title bar text — set once at `App::new` (from
    /// `run`/`run_path`'s `window_title` argument) and applied in `resumed`
    /// when the winit `Window` is actually created.
    title: String,
    ui: Ui,
    /// The live app state `value`/event bindings read from and dispatch to —
    /// usually a `#[derive(NowUiState)]` struct; `nowui_core::NoState` for
    /// the plain CLI binary, which has no Rust-side state at all.
    state: S,
    /// `Arc`, not `Rc` — `nowui-render-gpu`'s `wgpu::Surface` requires a
    /// `Send + Sync` window handle (see `GpuSurfaceState`'s doc comment);
    /// `softbuffer::Surface` (the CPU backend) is equally happy with either,
    /// so `Arc` covers both backends uniformly.
    window: Option<Arc<Window>>,
    /// Only populated when `backend == Backend::Cpu`.
    surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    /// Only populated when `backend == Backend::Gpu`.
    gpu: Option<GpuSurfaceState>,
    /// Resolved font *data* cache for the GPU text path — see
    /// `GpuFontCache`'s own doc comment. Unused (but harmless to keep
    /// around) on `Backend::Cpu`.
    gpu_font_cache: GpuFontCache,
    backend: Backend,
    cursor: Point,
    /// Font database + glyph cache. Built once (loading system fonts is slow)
    /// and reused across every redraw.
    text: TextContext,
    /// The node the cursor is currently over (`hover:` variant trigger).
    hovered: Option<NodeId>,
    /// The OS cursor (visible?, icon) last actually set on the window —
    /// compared against each `CursorMoved`'s freshly-resolved value so
    /// `Window::set_cursor`/`set_cursor_visible` are only called on an actual
    /// change, not once per mouse-move event.
    current_cursor: (bool, winit::window::CursorIcon),
    /// The node the mouse button is currently held down on (`active:` trigger).
    pressed: Option<NodeId>,
    /// Set while a `Slider`'s thumb is being dragged — real, intrinsic
    /// interaction, independent of the generic `onMouseDown`/`onMouseMove`/
    /// `onMouseUp` bindings (which now *are* dispatched, same as everything
    /// else — see `dispatch_event`).
    dragging_slider: Option<NodeId>,
    /// Set while a `TextInput` is being click-dragged to select — mirrors
    /// `dragging_slider`'s shape. The drag's anchor point lives on the node
    /// itself (`selection_anchor`, set once at mouse-down); this only needs
    /// to remember *which* node is being dragged so `CursorMoved` knows to
    /// keep extending its `cursor`.
    dragging_text_input: Option<NodeId>,
    /// Tracked from `WindowEvent::ModifiersChanged` — needed for Shift
    /// (extend a `TextInput` selection) and Ctrl (select-all). Nothing else
    /// in this engine currently reads a modifier key, so this exists purely
    /// for `edit_text_input`.
    modifiers: winit::keyboard::ModifiersState,
    /// Kept alive for the app's whole lifetime (not dropped after the
    /// initial `build()`) purely for its registered dynamic regions — an
    /// `if`/`for`'s live re-expansion each redraw needs the AST it came
    /// from, via `Semantic::refresh_dynamic_regions`. See `dynamic.rs`.
    semantic: crate::semantic::Semantic,
    transitions: Transitions,
    /// Nodes whose `onLoad` is due later than "now" — from a nonzero
    /// `{onLoadDelay: ...}` — each paired with the `Instant` it should fire
    /// at. Checked every frame (the loop is unconditional/fixed-rate now —
    /// see `FRAME_INTERVAL`/`about_to_wait` — so this no longer needs to
    /// drive `ControlFlow` itself the way it used to).
    pending_on_load_timers: Vec<(NodeId, Instant)>,
    /// The `Instant` the next frame should fire at — `about_to_wait` compares
    /// against this and, once reached, both requests the redraw and advances
    /// it by `FRAME_INTERVAL` (not just `now + FRAME_INTERVAL`, to avoid
    /// drift accumulating frame over frame), then reschedules
    /// `ControlFlow::WaitUntil` for the new deadline.
    next_frame: Instant,
}

impl<S: NowUiState + 'static> App<S> {
    pub fn new(title: String, ui: Ui, state: S, semantic: crate::semantic::Semantic, backend: Backend) -> Self {
        App {
            title,
            ui,
            state,
            window: None,
            surface: None,
            gpu: None,
            gpu_font_cache: GpuFontCache::new(),
            backend,
            cursor: Point::default(),
            text: TextContext::new(),
            hovered: None,
            current_cursor: (true, winit::window::CursorIcon::default()),
            pressed: None,
            dragging_slider: None,
            dragging_text_input: None,
            modifiers: winit::keyboard::ModifiersState::empty(),
            semantic,
            transitions: Transitions::new(),
            pending_on_load_timers: Vec::new(),
            next_frame: Instant::now(),
        }
    }

    /// Resolve every node's `value_path` (if any) against the live state and
    /// write the result into the widget it belongs to — the read half of
    /// reactivity. Widgets with no `value_path`, or whose path doesn't
    /// resolve (wrong type, unknown field, `NoState`), are left exactly as
    /// the `.nowui` file authored them; this never *clears* a value, only
    /// overrides it when the state genuinely has one.
    fn resolve_values(&mut self) {
        for i in 0..self.ui.nodes.len() {
            let id = NodeId(i as u32);
            let path = self.ui.get(id).value_path.clone();
            if path.is_empty() {
                continue;
            }
            let sub = state_subpath(&path);
            let Some(value) = self.state.get(&sub) else { continue };

            // A slider mid-drag is the source of truth for its own value
            // this frame — don't let a stale read fight the live gesture.
            let dragging = self.dragging_slider == Some(id);

            let node = self.ui.get_mut(id);
            match &mut node.kind {
                NodeKind::Text { content } => *content = display_string(&value),
                NodeKind::Checkbox { checked, .. } => {
                    if let Some(b) = value.as_bool() {
                        *checked = b;
                    }
                }
                NodeKind::Dropdown { options, selected, .. } => {
                    if let Some(s) = value.as_str() {
                        *selected = options.iter().position(|o| o == s);
                    }
                }
                NodeKind::TextInput { label, .. } => {
                    if let Some(s) = value.as_str() {
                        *label = s.to_string();
                    }
                }
                NodeKind::Date { value: v, .. } | NodeKind::Time { value: v, .. } | NodeKind::DateTime { value: v, .. } => {
                    if let Some(s) = value.as_str() {
                        *v = s.to_string();
                    }
                }
                NodeKind::Slider { value: v } if !dragging => {
                    if let Some(n) = value.as_f64() {
                        *v = (n as f32 / 100.0).clamp(0.0, 1.0);
                    }
                }
                NodeKind::ProgressBar { value: v } => {
                    if let Some(n) = value.as_f64() {
                        *v = (n as f32 / 100.0).clamp(0.0, 1.0);
                    }
                }
                _ => {}
            }
        }
    }

    /// Re-render every node's `templates` (backticks containing `${state.path}`
    /// interpolation, e.g. `` `Count: ${state.counter.count}` ``) against the
    /// live state and write the result into the widget field(s) that backtick
    /// originally built — the same read-half-of-reactivity idea as
    /// `resolve_values`, just for inline text instead of a `{value: ...}`
    /// binding. A node with no dynamic backticks has empty `templates` and is
    /// skipped entirely.
    fn resolve_templates(&mut self) {
        for i in 0..self.ui.nodes.len() {
            let id = NodeId(i as u32);
            let templates = self.ui.get(id).templates.clone();
            if templates.is_empty() {
                continue;
            }
            let rendered: Vec<String> = templates.iter().map(|t| self.render_template(t)).collect();
            apply_resolved_templates(&mut self.ui.get_mut(id).kind, &rendered);
        }
    }

    /// Concatenate one backtick's literal/`${state.path}` parts into the
    /// string it should currently display.
    fn render_template(&self, parts: &[TemplatePart]) -> String {
        let mut out = String::new();
        for part in parts {
            match part {
                TemplatePart::Lit(s) => out.push_str(s),
                TemplatePart::Var(path) => {
                    if let Some(v) = self.state.get(&state_subpath(path)) {
                        out.push_str(&display_string(&v));
                    }
                }
            }
        }
        out
    }

    /// Resolve every node's `Style::dynamic` entries (a `key-[${state.path}]`
    /// bracket value, e.g. `w-[${state.myWidth}]`) against the live state and
    /// re-apply them onto `base_style` — the same read-half-of-reactivity
    /// idea as `resolve_values`/`resolve_templates`, but for style values
    /// instead of widget content. Runs before `apply_dynamic_styles` each
    /// redraw so hover/focus/responsive variants and transitions are computed
    /// from the resolved value, not the stale default `apply_style` left in
    /// place at parse time. Written into `base_style` (not the transient,
    /// recomputed-every-frame `style`) since that's the field
    /// `apply_dynamic_styles` treats as ground truth.
    ///
    /// Reuses `semantic::apply_exact`/`apply_prefixed` — the exact same
    /// key-dispatch `resolve_styles` uses for the static (parse-time) case —
    /// so a dynamic value is interpreted identically to a literal one; keep
    /// this in sync if that dispatch ever changes.
    fn resolve_dynamic_styles(&mut self) {
        for i in 0..self.ui.nodes.len() {
            let id = NodeId(i as u32);
            let dynamic = self.ui.get(id).base_style.dynamic.clone();
            if dynamic.is_empty() {
                continue;
            }
            for (key, path) in &dynamic {
                let Some(value) = self.state.get(&state_subpath(path)) else { continue };
                let v = display_string(&value);
                let style = &mut self.ui.get_mut(id).base_style;
                let _ = crate::semantic::apply_exact(style, key, &v) || crate::semantic::apply_prefixed(style, key, &v);
            }
        }
    }

    /// Write `value` back to whatever state path `id`'s `value_path` names,
    /// if any — the write half of reactivity, called after any interaction
    /// that changes a widget's own value (`Checkbox` toggle, `Dropdown`
    /// selection, `Slider` drag).
    fn write_back_value(&mut self, id: NodeId, value: StateValue) {
        let path = self.ui.get(id).value_path.clone();
        if path.is_empty() {
            return;
        }
        self.state.set(&state_subpath(&path), value);
    }

    /// Dispatch `event_name` (an `EVENT_BINDING_KEYS` entry, e.g. `"onClick"`)
    /// to `id`'s bound state method, if it declared one via `{onClick:
    /// state.foo.bar}`. The `Event` built here borrows `id`'s own arena node
    /// mutably (`event.node`), so the handler can read/mutate the node it
    /// fired on directly (every `Node` field is `pub`) in addition to `self`.
    /// Marks the UI dirty when the handler ran, since a callback mutating
    /// state (or the node) almost always needs a redraw to show it.
    fn dispatch_event(&mut self, id: NodeId, event_name: &str, kind: EventKind, key: Option<String>) {
        let Some(path) = self.ui.get(id).events.get(event_name).cloned() else { return };
        let cursor = self.cursor;
        let node = self.ui.get_mut(id);
        let mut event = Event { kind, cursor, key, node };
        // SAFETY: `root` is a second, independently-constructed `&mut S`
        // aliasing `self.state` — the same object `call` is about to be
        // invoked on as its receiver — so a handler can reach sibling state
        // through `root`/`state` even from a method declared on some nested
        // field several delegation-hops down (see `NowUiState::call`'s doc
        // comment for the full picture and the caveat this carries: don't
        // write to the exact same field through both `self` and `root` in
        // one handler).
        let root_ptr: *mut S = &mut self.state;
        if self.state.call(&state_subpath(&path), &mut event, unsafe { &mut *root_ptr }) {
            self.ui.dirty = true;
        }
    }

    /// Dispatch `event_name` to every node in the tree that bound it —
    /// for window-level events like `onResize` that aren't about any one
    /// widget's position.
    fn dispatch_event_broadcast(&mut self, event_name: &str, kind: EventKind) {
        for i in 0..self.ui.nodes.len() {
            self.dispatch_event(NodeId(i as u32), event_name, kind, None);
        }
    }

    /// Fire `"onLoad"` on every node created since the last call to this —
    /// called once right after the initial `build()` (see `lib.rs::run_ast`)
    /// and again after every `refresh_dynamic_regions` in `redraw`, so a
    /// `for`/`if` region's freshly-expanded nodes get it too, not just the
    /// static tree. `dispatch_event` already no-ops for a node that didn't
    /// bind `onLoad`, so this doesn't need to check first.
    ///
    /// A node with `on_load_delay_secs` above zero (`{onLoadDelay: 1.0}`)
    /// doesn't fire immediately — it's queued into `pending_on_load_timers`
    /// instead; `fire_due_on_load_timers` (called *before* this, not after —
    /// see its own doc comment) is what actually dispatches it once its
    /// deadline passes.
    pub(crate) fn dispatch_pending_on_load(&mut self) {
        let now = Instant::now();
        for id in self.semantic.take_pending_on_load() {
            let delay = self.ui.get(id).on_load_delay_secs;
            if delay <= 0.0 {
                self.dispatch_event(id, "onLoad", EventKind::Load, None);
            } else {
                self.pending_on_load_timers.push((id, now + Duration::from_secs_f32(delay)));
            }
        }
    }

    /// Dispatch `"onLoad"` for every queued delayed node (`{onLoadDelay:
    /// ...}`) whose deadline has passed. Called in `redraw` *before*
    /// `refresh_dynamic_regions`, deliberately the opposite order from
    /// `dispatch_pending_on_load` — a delayed `onLoad` handler often mutates
    /// state an `if`/`for` branches on (e.g. a splash screen navigating away
    /// after a delay), and that mutation needs to be visible to *this same
    /// frame's* region re-evaluation, not next frame's. Getting this
    /// backwards was a real bug: the mutation would land one frame too late,
    /// and since nothing else was dirty by then, `ControlFlow` had already
    /// dropped back to `Wait` with no redraw scheduled to pick it up — the
    /// UI would sit stale until an unrelated input event forced one.
    fn fire_due_on_load_timers(&mut self) {
        let now = Instant::now();
        let (due, still_pending): (Vec<_>, Vec<_>) =
            self.pending_on_load_timers.drain(..).partition(|&(_, deadline)| deadline <= now);
        self.pending_on_load_timers = still_pending;
        for (id, _) in due {
            self.dispatch_event(id, "onLoad", EventKind::Load, None);
        }
    }

    /// Recompute each node's per-frame effective style (`base_style` +
    /// responsive/hover/focus/active overlays), transition-smoothing the
    /// animatable subset (colors/opacity/radius/transform). Non-animatable
    /// fields (sizing, typography, grid tracks, ...) snap instantly — see
    /// CLAUDE.md for why only that subset is animated.
    fn apply_dynamic_styles(&mut self, viewport_w: f32) {
        let now = Instant::now();
        for i in 0..self.ui.nodes.len() {
            let id = NodeId(i as u32);
            let base = self.ui.get(id).base_style.clone();
            let hovered = self.hovered == Some(id);
            let focused = self.ui.focus == Some(id);
            let pressed = self.pressed == Some(id);

            let target = compute_effective(&base, viewport_w, hovered, focused, pressed);
            let mut effective = target.clone();
            let animated = self.transitions.step(id, AnimatableStyle::from_style(&target), base.transition, now);
            animated.write_into(&mut effective);
            self.ui.get_mut(id).style = effective;
        }
    }

    fn redraw(&mut self) {
        let Some(window) = self.window.clone() else { return };
        let ready = match self.backend {
            Backend::Cpu => self.surface.is_some(),
            Backend::Gpu => self.gpu.is_some(),
        };
        if !ready {
            return;
        }

        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));

        // Fire any due delayed `onLoad` *first* — its handler may mutate
        // state an `if`/`for` branches on, and that needs to be visible to
        // the region re-evaluation right below, not next frame (see
        // `fire_due_on_load_timers`'s doc comment).
        self.fire_due_on_load_timers();

        // Re-expand any `if`/`for` whose condition/list actually changed
        // *before* everything else — a newly-appeared node needs its own
        // `value_path`/`templates`/`Style::dynamic` resolved this same
        // frame, not one frame late.
        self.semantic.refresh_dynamic_regions(&mut self.ui, &self.state);
        self.dispatch_pending_on_load();

        self.resolve_values();
        self.resolve_templates();
        self.resolve_dynamic_styles();
        self.apply_dynamic_styles(w as f32);

        match self.backend {
            Backend::Cpu => self.redraw_cpu(&window, w, h),
            Backend::Gpu => self.redraw_gpu(&window, w, h),
        }

        self.ui.dirty = false;

        // Frame pacing (a fixed 60fps loop, not event-driven) is owned by
        // `about_to_wait` — it schedules the next `WaitUntil` deadline and
        // the next `request_redraw()` regardless of what happened this
        // frame. Nothing to decide here.
    }

    /// Only ask the OS for an IME composition window while a `TextInput` is
    /// actually focused — otherwise e.g. a focused `Button` would still pop
    /// up a candidate window on every keystroke. Idempotent, so just redoing
    /// this unconditionally every redraw is simplest; no need to hook every
    /// individual place focus can change. Shared by both backends — reads
    /// only `self.ui`, not pixels.
    fn update_ime_cursor_area(&self, window: &Window) {
        let focused_text_input = self
            .ui
            .focus
            .filter(|&id| matches!(self.ui.get(id).kind, NodeKind::TextInput { .. }));
        window.set_ime_allowed(focused_text_input.is_some());
        if let Some(id) = focused_text_input {
            let rect = self.ui.get(id).computed;
            window.set_ime_cursor_area(
                winit::dpi::PhysicalPosition::new(rect.x as i32, (rect.y + rect.h) as i32),
                winit::dpi::PhysicalSize::new(rect.w.max(1.0) as u32, 1u32),
            );
        }
    }

    fn redraw_cpu(&mut self, window: &Window, w: u32, h: u32) {
        let mut pixmap = Pixmap::new(w, h).expect("pixmap alloc");
        pixmap.fill(tiny_skia::Color::from_rgba8(CLEAR.r, CLEAR.g, CLEAR.b, 255));

        {
            let mut painter = SkiaPainter::new(&mut pixmap, &mut self.text);
            nowui_core::layout::solve(&mut self.ui, Size::new(w as f32, h as f32), &mut painter);
        }

        // Needs `computed` rects from the solve above (for each TextInput's
        // own box width) but can't run inside that same block — it uses
        // `self.measure_text_width`, which would conflict with the painter
        // already borrowing `self.text` above.
        self.update_text_input_scroll();

        {
            let mut painter = SkiaPainter::new(&mut pixmap, &mut self.text);
            nowui_core::paint::paint(&self.ui, &mut painter);
        }

        self.update_ime_cursor_area(window);

        let surface = self.surface.as_mut().expect("checked in redraw");
        surface
            .resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap())
            .expect("surface resize");

        let mut buffer = surface.buffer_mut().expect("buffer");
        present_to_softbuffer(&pixmap, &mut buffer);
        buffer.present().expect("present");
    }

    fn redraw_gpu(&mut self, window: &Window, w: u32, h: u32) {
        let mut scene = vello::Scene::new();

        {
            let mut painter = GpuPainter::new(&mut scene, &mut self.text, &mut self.gpu_font_cache);
            nowui_core::layout::solve(&mut self.ui, Size::new(w as f32, h as f32), &mut painter);
        }

        // See `redraw_cpu`'s matching comment — same borrow-conflict reason.
        self.update_text_input_scroll();

        {
            let mut painter = GpuPainter::new(&mut scene, &mut self.text, &mut self.gpu_font_cache);
            nowui_core::paint::paint(&self.ui, &mut painter);
        }

        self.update_ime_cursor_area(window);

        let gpu = self.gpu.as_mut().expect("checked in redraw");
        gpu.resize(w, h);
        gpu.render_and_present(&scene, CLEAR);
    }

    fn request_redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Toggle a `Checkbox`, or open a `Dropdown` — the only two widgets with
    /// self-contained state a click can drive directly. Selecting an
    /// *option* from an open dropdown is handled separately by
    /// `select_dropdown_option`, since the option list is a floating popup
    /// that lives outside the node's own `computed` rect (see paint.rs) and
    /// so isn't reachable through the normal rect-based `hit_test`.
    fn handle_click(&mut self, id: NodeId) {
        let mut new_value = None;
        match &mut self.ui.get_mut(id).kind {
            NodeKind::Checkbox { checked, .. } => {
                *checked = !*checked;
                new_value = Some(StateValue::Bool(*checked));
            }
            NodeKind::Dropdown { open, .. } => *open = !*open,
            // No value to write back — unlike `Dropdown`, there's no single
            // "selected" value a `Menu` has; it's one-way bound (`onClick`
            // only), not two-way.
            NodeKind::Menu { open, .. } => *open = !*open,
            // Re-sync the calendar's browsed month to whatever date is
            // already picked every time the popup opens, so reopening always
            // starts on the picked date rather than wherever it was last
            // left browsing.
            NodeKind::Date { open, value, view_year, view_month, .. } => {
                *open = !*open;
                if *open {
                    if let Some((y, m, _)) = nowui_core::datetime::parse_date(value) {
                        *view_year = y;
                        *view_month = m;
                    }
                }
            }
            NodeKind::Time { open, .. } => *open = !*open,
            NodeKind::DateTime { open, value, view_year, view_month, .. } => {
                *open = !*open;
                if *open {
                    let (date_part, _) = nowui_core::datetime::split_datetime(value);
                    if let Some((y, m, _)) = nowui_core::datetime::parse_date(date_part) {
                        *view_year = y;
                        *view_month = m;
                    }
                }
            }
            _ => {}
        }
        if let Some(v) = new_value {
            self.write_back_value(id, v);
        }
        // Clicking anywhere closes every *other* open dropdown/menu/picker —
        // there's no outside-click-detection system built in, so without
        // this an open popup would just sit there floating forever.
        self.close_other_dropdowns(Some(id));
        self.close_other_menus(Some(id));
        self.close_other_pickers(Some(id));
        self.dispatch_event(id, "onClick", EventKind::Click, None);
    }

    /// The screen-space rect an open dropdown's popup occupies — must match
    /// `paint::paint_dropdown_popup`'s placement exactly, or clicks and
    /// pixels disagree about where the list is.
    fn dropdown_popup_rect(&self, id: NodeId) -> Option<Rect> {
        let node = self.ui.get(id);
        let NodeKind::Dropdown { options, open, .. } = &node.kind else { return None };
        if !*open {
            return None;
        }
        let (_, option_h) = dropdown_metrics(node.style.font_size);
        let rect = node.computed;
        Some(Rect::new(rect.x, rect.y + rect.h, rect.w, option_h * options.len() as f32))
    }

    /// Find the open dropdown (if any) whose floating popup contains `p`.
    fn find_open_dropdown_popup_at(&self, p: Point) -> Option<NodeId> {
        (0..self.ui.nodes.len())
            .map(|i| NodeId(i as u32))
            .find(|&id| self.dropdown_popup_rect(id).is_some_and(|r| r.contains(p)))
    }

    fn select_dropdown_option(&mut self, id: NodeId, p: Point) {
        let node = self.ui.get_mut(id);
        let rect = node.computed;
        let font_size = node.style.font_size;
        let (_, option_h) = dropdown_metrics(font_size);
        let local_y = p.y - (rect.y + rect.h);
        let mut selected_str = None;
        if let NodeKind::Dropdown { options, selected, open, .. } = &mut node.kind {
            let idx = (local_y / option_h).max(0.0) as usize;
            if idx < options.len() {
                *selected = Some(idx);
                selected_str = Some(options[idx].clone());
            }
            *open = false;
        }
        if let Some(s) = selected_str {
            self.write_back_value(id, StateValue::Str(s));
        }
        self.close_other_dropdowns(Some(id));
    }

    /// The screen-space rect an open `Menu`'s popup occupies — must match
    /// `paint::paint_menu_popup`'s placement exactly (both read the same
    /// `Node::content_size` the solver's `arrange_menu_popups` stashed),
    /// or clicks and pixels disagree about where the popup is.
    fn menu_popup_rect(&self, id: NodeId) -> Option<Rect> {
        let node = self.ui.get(id);
        let NodeKind::Menu { open, .. } = &node.kind else { return None };
        if !*open || node.children.is_empty() {
            return None;
        }
        let rect = node.computed;
        let size = node.content_size;
        Some(Rect::new(rect.x, rect.y + rect.h, size.w, size.h))
    }

    /// Find the open menu (if any) whose floating popup contains `p`.
    fn find_open_menu_popup_at(&self, p: Point) -> Option<NodeId> {
        (0..self.ui.nodes.len()).map(|i| NodeId(i as u32)).find(|&id| self.menu_popup_rect(id).is_some_and(|r| r.contains(p)))
    }

    /// A click landed inside an open menu's popup: find which real child node
    /// (typically a `MenuItem`, but it could be anything nested) is under the
    /// cursor via `Ui::hit_test_within` (children have real rects from
    /// `arrange_menu_popups`, but the popup itself isn't `menu`'s own
    /// `computed` rect, so the normal root-down `hit_test` can't reach them),
    /// dispatch that child's own `onClick`, then close the menu — matching
    /// `select_dropdown_option`'s "picking closes the popup" convention.
    ///
    /// The arena has no parent pointers, so a `MenuItem` genuinely can't find
    /// its own `Menu` to close it — but that's not a blocker here, because
    /// this function never needs to walk *up* from the item at all. Both
    /// `find_open_menu_popup_at` (which already resolved `menu`'s id from the
    /// click point, before the item was ever identified) and this function
    /// are called by `App`, which holds the whole arena and just closes
    /// `menu` directly. The parent-pointer gap only matters for something
    /// that has *only* a child id and needs its ancestor from that — not the
    /// case here.
    fn select_menu_item(&mut self, menu: NodeId, p: Point) {
        if let Some(item) = self.ui.hit_test_within(menu, p) {
            self.dispatch_event(item, "onClick", EventKind::Click, None);
        }
        if let NodeKind::Menu { open, .. } = &mut self.ui.get_mut(menu).kind {
            *open = false;
        }
    }

    fn close_other_menus(&mut self, keep: Option<NodeId>) {
        for i in 0..self.ui.nodes.len() {
            let id = NodeId(i as u32);
            if Some(id) == keep {
                continue;
            }
            if let NodeKind::Menu { open, .. } = &mut self.ui.get_mut(id).kind {
                if *open {
                    *open = false;
                    self.ui.dirty = true;
                }
            }
        }
    }

    /// The screen-space rect an open `Date`/`Time`/`DateTime`'s popup
    /// occupies — must match `paint`'s `paint_date_popup`/`paint_time_popup`/
    /// `paint_datetime_popup` exactly (all built from the same
    /// `nowui_core::datetime` layout functions), or clicks and pixels
    /// disagree about where the popup is.
    fn picker_popup_rect(&self, id: NodeId) -> Option<Rect> {
        let node = self.ui.get(id);
        let font_size = node.style.font_size;
        let with_seconds = node.style.with_seconds;
        match &node.kind {
            NodeKind::Date { open, view_year, view_month, .. } if *open => {
                Some(nowui_core::datetime::layout_calendar(node.computed, font_size, *view_year, *view_month).popup_rect)
            }
            NodeKind::Time { open, .. } if *open => Some(nowui_core::datetime::layout_clock(node.computed, font_size, with_seconds).popup_rect),
            NodeKind::DateTime { open, view_year, view_month, .. } if *open => {
                Some(nowui_core::datetime::layout_datetime(node.computed, font_size, with_seconds, *view_year, *view_month).popup_rect)
            }
            _ => None,
        }
    }

    /// Find the open `Date`/`Time`/`DateTime` (if any) whose floating popup
    /// contains `p` — checked before the normal hit test, same reason as
    /// `find_open_dropdown_popup_at`.
    fn find_open_picker_popup_at(&self, p: Point) -> Option<NodeId> {
        (0..self.ui.nodes.len()).map(|i| NodeId(i as u32)).find(|&id| self.picker_popup_rect(id).is_some_and(|r| r.contains(p)))
    }

    /// A click landed inside an open `Date`'s calendar popup: nav
    /// arrow, or a day cell. Picking a day is a single, terminal action
    /// (like `Dropdown` selecting an option) — it writes the value back,
    /// dispatches `onSelect`, and closes the popup.
    fn select_date_popup(&mut self, id: NodeId, p: Point) {
        let node = self.ui.get_mut(id);
        let font_size = node.style.font_size;
        let rect = node.computed;
        let NodeKind::Date { value, open, view_year, view_month, .. } = &mut node.kind else { return };
        let layout = nowui_core::datetime::layout_calendar(rect, font_size, *view_year, *view_month);
        if layout.prev_rect.contains(p) {
            step_view_month(view_year, view_month, -1);
            return;
        }
        if layout.next_rect.contains(p) {
            step_view_month(view_year, view_month, 1);
            return;
        }
        let Some(day) = layout.day_cells.iter().find(|(r, _)| r.contains(p)).and_then(|(_, d)| *d) else { return };
        *value = nowui_core::datetime::format_date(*view_year, *view_month, day);
        *open = false;
        let new_value = value.clone();
        self.write_back_value(id, StateValue::Str(new_value));
        self.close_other_pickers(Some(id));
        self.dispatch_event(id, "onSelect", EventKind::Click, None);
    }

    /// A click landed inside an open `Time`'s spinner popup: a `+`/`-`
    /// column arrow. Unlike `Date`, this never closes the popup — dialing in
    /// a time takes more than one click.
    fn select_time_popup(&mut self, id: NodeId, p: Point) {
        let node = self.ui.get_mut(id);
        let font_size = node.style.font_size;
        let with_seconds = node.style.with_seconds;
        let rect = node.computed;
        let NodeKind::Time { value, .. } = &mut node.kind else { return };
        let (h, m, s) = nowui_core::datetime::parse_time(value).unwrap_or_else(|| {
            let (_, _, _, h, m, s) = nowui_core::datetime::now();
            (h, m, s)
        });
        let layout = nowui_core::datetime::layout_clock(rect, font_size, with_seconds);
        let Some((h, m, s)) = step_from_clock_columns(&layout, p, h, m, s) else { return };
        *value = nowui_core::datetime::format_time(h, m, s, with_seconds);
        let new_value = value.clone();
        self.write_back_value(id, StateValue::Str(new_value));
        self.dispatch_event(id, "onSelect", EventKind::Click, None);
    }

    /// A click landed inside an open `DateTime`'s combined popup: dispatch
    /// to the calendar half or the clock half depending on where `p` falls.
    /// Neither half closes the popup on its own (see `NodeKind::DateTime`'s
    /// doc comment) — only clicking the box again, or elsewhere, does.
    fn select_datetime_popup(&mut self, id: NodeId, p: Point) {
        let node = self.ui.get_mut(id);
        let font_size = node.style.font_size;
        let with_seconds = node.style.with_seconds;
        let rect = node.computed;
        let NodeKind::DateTime { value, view_year, view_month, .. } = &mut node.kind else { return };
        let layout = nowui_core::datetime::layout_datetime(rect, font_size, with_seconds, *view_year, *view_month);
        let (date_part, time_part) = nowui_core::datetime::split_datetime(value);
        let (date_part, time_part) = (date_part.to_string(), time_part.to_string());

        if layout.calendar.popup_rect.contains(p) {
            if layout.calendar.prev_rect.contains(p) {
                step_view_month(view_year, view_month, -1);
                return;
            }
            if layout.calendar.next_rect.contains(p) {
                step_view_month(view_year, view_month, 1);
                return;
            }
            let Some(day) = layout.calendar.day_cells.iter().find(|(r, _)| r.contains(p)).and_then(|(_, d)| *d) else {
                return;
            };
            let new_date = nowui_core::datetime::format_date(*view_year, *view_month, day);
            *value = nowui_core::datetime::join_datetime(&new_date, &time_part);
        } else if layout.clock.popup_rect.contains(p) {
            let (h, m, s) = nowui_core::datetime::parse_time(&time_part).unwrap_or_else(|| {
                let (_, _, _, h, m, s) = nowui_core::datetime::now();
                (h, m, s)
            });
            let Some((h, m, s)) = step_from_clock_columns(&layout.clock, p, h, m, s) else { return };
            let new_time = nowui_core::datetime::format_time(h, m, s, with_seconds);
            *value = nowui_core::datetime::join_datetime(&date_part, &new_time);
        } else {
            return;
        }
        let new_value = value.clone();
        self.write_back_value(id, StateValue::Str(new_value));
        self.dispatch_event(id, "onSelect", EventKind::Click, None);
    }

    fn close_other_pickers(&mut self, keep: Option<NodeId>) {
        for i in 0..self.ui.nodes.len() {
            let id = NodeId(i as u32);
            if Some(id) == keep {
                continue;
            }
            let open = match &mut self.ui.get_mut(id).kind {
                NodeKind::Date { open, .. } | NodeKind::Time { open, .. } | NodeKind::DateTime { open, .. } => open,
                _ => continue,
            };
            if *open {
                *open = false;
                self.ui.dirty = true;
            }
        }
    }

    /// Set a `Slider`'s value (0.0..=1.0) from a cursor x position within its
    /// own track rect — used both when a drag starts (clicking the track
    /// jumps the thumb there, standard slider UX) and on every subsequent
    /// `CursorMoved` while dragging. Writes the new value back to state too.
    fn set_slider_value_from_cursor(&mut self, id: NodeId, cursor: Point) {
        let rect = self.ui.get(id).computed;
        let mut new_value = None;
        if let NodeKind::Slider { value } = &mut self.ui.get_mut(id).kind {
            if rect.w > 0.0 {
                *value = ((cursor.x - rect.x) / rect.w).clamp(0.0, 1.0);
                new_value = Some(*value);
            }
        }
        if let Some(v) = new_value {
            self.write_back_value(id, StateValue::Float((v * 100.0) as f64));
        }
    }

    /// Measure `text`'s pixel width at `size` outside of an actual redraw —
    /// used for click-to-caret hit-testing, which happens on a mouse event,
    /// not inside the paint pass. Pure cosmic-text shaping (`nowui-text`) —
    /// no `Painter`/pixmap/GPU surface needed at all, unlike before this was
    /// extracted (a throwaway 1x1 `SkiaPainter` used to be the only way to
    /// reach `measure_text`).
    fn measure_text_width(&mut self, text: &str, size: f32) -> f32 {
        nowui_text::measure(&mut self.text.font_system, text, size).x
    }

    /// Keep every `TextInput`'s caret in view by adjusting its scroll offset
    /// (`Node::scroll_offset` — reused here for a TextInput's own internal
    /// text view; unrelated to its normal job of shifting a `scroll-h`/
    /// `scroll-v` *container's* children, which a `TextInput` has none of).
    /// Called once per redraw, after `layout::solve` (needs each box's
    /// `computed` width/height) and before painting (`paint_text_input`
    /// reads `scroll_offset` to shift the drawn text/caret/selection/
    /// underline together).
    ///
    /// Single-line: horizontal only, following the caret's *x* position.
    /// Multiline (`style.multiline`): vertical only (wrapping already
    /// handles horizontal overflow — see `paint_multiline_text_input`),
    /// following the caret's *hard line* — approximate in the same way
    /// painting is (a wrapped, not newline-broken, long line isn't counted
    /// as extra lines here either, see CLAUDE.md).
    ///
    /// Either way, the offset only ever moves just far enough to bring the
    /// caret back into view (clamped so it never scrolls past the point
    /// where the remaining content is shorter than the box) — not reset to
    /// 0 or recentered every frame, so editing in the middle of an already-
    /// scrolled value doesn't visibly jump.
    fn update_text_input_scroll(&mut self) {
        for i in 0..self.ui.nodes.len() {
            let id = NodeId(i as u32);
            let Some((shown, multiline, caret_char, font_size, box_w, box_h, offset)) = (|| {
                let node = self.ui.get(id);
                let NodeKind::TextInput { label, masked, cursor, ime_preview, .. } = &node.kind else { return None };
                let content_rect = node.computed.inset(node.style.border_width).inset(node.style.padding);
                let shown = nowui_core::text_input::display_string(label, *cursor, ime_preview, *masked);
                let caret_char = *cursor + nowui_core::text_input::char_len(ime_preview);
                Some((shown, node.style.multiline, caret_char, node.style.font_size, content_rect.w, content_rect.h, node.scroll_offset))
            })() else {
                continue;
            };

            if multiline {
                let line_h = nowui_core::text_input::line_height(font_size);
                let (caret_line, _) = nowui_core::text_input::line_and_col(&shown, caret_char);
                let total_h = nowui_core::text_input::hard_lines(&shown).len() as f32 * line_h;
                let caret_y = caret_line as f32 * line_h;

                let mut y = offset.y;
                if caret_y - y < 0.0 {
                    y = caret_y;
                } else if (caret_y + line_h) - y > box_h {
                    y = caret_y + line_h - box_h;
                }
                y = y.clamp(0.0, (total_h - box_h).max(0.0));

                let node = self.ui.get_mut(id);
                node.scroll_offset.x = 0.0;
                node.scroll_offset.y = y;
                continue;
            }

            let caret_x = self.measure_text_width(&shown.chars().take(caret_char).collect::<String>(), font_size);
            let total_w = self.measure_text_width(&shown, font_size);

            let mut x = offset.x;
            if caret_x - x < 0.0 {
                x = caret_x;
            } else if caret_x - x > box_w {
                x = caret_x - box_w;
            }
            x = x.clamp(0.0, (total_w - box_w).max(0.0));

            self.ui.get_mut(id).scroll_offset.x = x;
        }
    }

    /// Which char index in `id`'s `TextInput` a click at `cursor` (screen
    /// coordinates) landed nearest — measures the exact same
    /// `text_input::display_string` the painter draws (see `paint.rs`), so
    /// a click always lands on the character it visually points at. Left-
    /// aligned text only (see `paint_text_input`'s doc comment).
    fn char_index_for_click(&mut self, id: NodeId, cursor: Point) -> usize {
        let (shown, style, content_rect, scroll) = {
            let node = self.ui.get(id);
            let style = node.style.clone();
            let content_rect = node.computed.inset(style.border_width).inset(style.padding);
            let NodeKind::TextInput { label, masked, cursor: caret, ime_preview, .. } = &node.kind else {
                return 0;
            };
            let shown = nowui_core::text_input::display_string(label, *caret, ime_preview, *masked);
            (shown, style, content_rect, node.scroll_offset)
        };

        if style.multiline {
            let line_h = nowui_core::text_input::line_height(style.font_size);
            let lines = nowui_core::text_input::hard_lines(&shown);
            // No horizontal scroll in multiline mode (wrapping replaces the
            // need for it — see `paint_multiline_text_input`), only vertical.
            let target_y = (cursor.y - content_rect.y) + scroll.y;
            let line = ((target_y / line_h).floor().max(0.0) as usize).min(lines.len().saturating_sub(1));
            let target_x = cursor.x - content_rect.x;
            let col = self.nearest_char_boundary(lines[line], target_x, style.font_size);
            return nowui_core::text_input::char_index_at(&shown, line, col);
        }

        // The painter draws `shown` starting at `content_rect.x - scroll_offset.x`
        // (see `paint_text_input`) — a click's position relative to that same
        // shifted origin, not the box's own unshifted edge, is what actually
        // lines up with a given char's rendered position once the view has
        // scrolled.
        let target_x = (cursor.x - content_rect.x) + scroll.x;
        self.nearest_char_boundary(&shown, target_x, style.font_size)
    }

    /// Which char index in `text` sits nearest to `target_x` (0 = before the
    /// first char, `text.chars().count()` = after the last) — snaps to
    /// whichever side of each char's midpoint `target_x` is closer to, so
    /// clicking the right half of a glyph lands after it rather than always
    /// rounding down to before it. Shared by both single-line and per-line
    /// multiline click hit-testing.
    fn nearest_char_boundary(&mut self, text: &str, target_x: f32, font_size: f32) -> usize {
        let chars: Vec<char> = text.chars().collect();
        let mut prev_w = 0.0;
        for i in 0..chars.len() {
            let prefix: String = chars[..=i].iter().collect();
            let w = self.measure_text_width(&prefix, font_size);
            if target_x < (prev_w + w) / 2.0 {
                return i;
            }
            prev_w = w;
        }
        chars.len()
    }

    /// Start a click (and potential click-drag) selection in a `TextInput`:
    /// places the caret at the clicked char and remembers that position as
    /// the drag anchor. `CursorMoved` (while `dragging_text_input` stays
    /// `Some`) extends the selection by moving `cursor` further; `MouseUp`
    /// collapses `selection_anchor` back to `None` if the click never
    /// actually turned into a drag (anchor == cursor still).
    fn start_text_selection(&mut self, id: NodeId, cursor: Point) {
        let idx = self.char_index_for_click(id, cursor);
        if let NodeKind::TextInput { cursor: caret, selection_anchor, .. } = &mut self.ui.get_mut(id).kind {
            *caret = idx;
            *selection_anchor = Some(idx);
        }
        self.dragging_text_input = Some(id);
    }

    /// Full text editing for a focused `TextInput`: character insertion
    /// (replacing the selection, if any), Backspace/Delete, Left/Right/Home/
    /// End caret movement (Shift extends/starts a selection; Ctrl+A selects
    /// all), all delegated to the pure char-index math in
    /// `nowui_core::text_input` so it's shared with the painter and stays
    /// independently testable. Takes `logical_key`/`text` as plain values
    /// rather than a `winit::event::KeyEvent` (which has a private field and
    /// so can't be constructed in a test). IME composition is handled
    /// separately, in the `WindowEvent::Ime` arm below — this only ever
    /// touches `label` directly, never `ime_preview`.
    ///
    /// Returns the new value only when `label` itself actually changed, so
    /// the caller can skip a no-op state write for a pure cursor-move/no-op
    /// (e.g. Backspace on an already-empty field, or an arrow key).
    fn edit_text_input(&mut self, id: NodeId, logical_key: &Key, text: Option<&str>, shift: bool, ctrl: bool) -> Option<String> {
        use nowui_core::text_input::{char_len, delete_range, insert_str, move_left, move_right};

        let multiline = self.ui.get(id).style.multiline;
        let NodeKind::TextInput { label, cursor, selection_anchor, .. } = &mut self.ui.get_mut(id).kind else {
            return None;
        };
        let mut changed = false;

        match logical_key {
            // Single-line: Enter's `text` is `Some("\r")`, which the
            // catch-all arm below filters out as a control character — a
            // no-op, same as ignoring it explicitly. Multiline: a literal
            // newline, replacing the selection first like any other insert.
            Key::Named(NamedKey::Enter) if multiline => {
                if let Some(anchor) = selection_anchor.take() {
                    delete_range(label, cursor, anchor);
                }
                insert_str(label, cursor, "\n");
                changed = true;
            }
            Key::Named(NamedKey::Backspace) => {
                changed = match selection_anchor.take() {
                    Some(anchor) => delete_range(label, cursor, anchor),
                    None if *cursor > 0 => delete_range(label, cursor, *cursor - 1),
                    None => false,
                };
            }
            Key::Named(NamedKey::Delete) => {
                changed = match selection_anchor.take() {
                    Some(anchor) => delete_range(label, cursor, anchor),
                    None if *cursor < char_len(label) => delete_range(label, cursor, *cursor + 1),
                    None => false,
                };
            }
            Key::Named(NamedKey::ArrowLeft) => move_left(cursor, selection_anchor, shift),
            Key::Named(NamedKey::ArrowRight) => move_right(cursor, selection_anchor, shift, char_len(label)),
            Key::Named(NamedKey::Home) => {
                if shift {
                    selection_anchor.get_or_insert(*cursor);
                } else {
                    *selection_anchor = None;
                }
                *cursor = 0;
            }
            Key::Named(NamedKey::End) => {
                if shift {
                    selection_anchor.get_or_insert(*cursor);
                } else {
                    *selection_anchor = None;
                }
                *cursor = char_len(label);
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("a") => {
                *selection_anchor = Some(0);
                *cursor = char_len(label);
            }
            _ => {
                if let Some(text) = text {
                    let typed: String = text.chars().filter(|c| !c.is_control()).collect();
                    if !typed.is_empty() {
                        if let Some(anchor) = selection_anchor.take() {
                            delete_range(label, cursor, anchor);
                        }
                        insert_str(label, cursor, &typed);
                        changed = true;
                    }
                }
            }
        }

        changed.then(|| label.clone())
    }

    /// Set a focused `TextInput`'s in-progress IME composition text —
    /// called from `WindowEvent::Ime(Ime::Preedit(..))`. Pulled out into its
    /// own method (like `edit_text_input`) so it's testable without a real
    /// winit event loop (`WindowEvent`/`ActiveEventLoop` aren't constructible
    /// outside one).
    fn set_ime_preview(&mut self, id: NodeId, text: String) {
        if let NodeKind::TextInput { ime_preview, .. } = &mut self.ui.get_mut(id).kind {
            *ime_preview = text;
        }
    }

    /// Commit IME-composed `text` into a focused `TextInput`: clears
    /// `ime_preview` and splices `text` into `label` at the cursor, exactly
    /// like a regular keystroke — called from `WindowEvent::Ime(Ime::
    /// Commit(..))`. Returns the new value (for a state write-back) since,
    /// unlike a preview, a commit always changes `label`.
    fn commit_ime_text(&mut self, id: NodeId, text: &str) -> Option<String> {
        let NodeKind::TextInput { label, cursor, ime_preview, .. } = &mut self.ui.get_mut(id).kind else {
            return None;
        };
        ime_preview.clear();
        nowui_core::text_input::insert_str(label, cursor, text);
        Some(label.clone())
    }

    fn close_other_dropdowns(&mut self, keep: Option<NodeId>) {
        for i in 0..self.ui.nodes.len() {
            let id = NodeId(i as u32);
            if Some(id) == keep {
                continue;
            }
            if let NodeKind::Dropdown { open, .. } = &mut self.ui.get_mut(id).kind {
                if *open {
                    *open = false;
                    self.ui.dirty = true;
                }
            }
        }
    }
}

/// `["state", "counter", "count"]` -> `["counter", "count"]` — every
/// `.nowui` binding path is rooted at the literal `state` segment (see
/// `nowui-syntax`'s dotted-path grammar), but `NowUiState` impls are rooted
/// at their own struct's fields, so that leading segment is stripped before
/// crossing the reflection boundary.
fn state_subpath(path: &[String]) -> Vec<&str> {
    let skip = usize::from(path.first().is_some_and(|s| s == "state"));
    path.iter().skip(skip).map(String::as_str).collect()
}

/// Map a resolved `cursor-*` style value to what actually has to be told to
/// the OS window: `(visible, icon)` — `icon` is meaningless whenever
/// `visible` is `false` (`cursor-none`), since the cursor isn't drawn at all.
fn cursor_icon_for(icon: CursorIcon) -> (bool, winit::window::CursorIcon) {
    match icon {
        CursorIcon::Auto => (true, winit::window::CursorIcon::default()),
        CursorIcon::Pointer => (true, winit::window::CursorIcon::Pointer),
        // No single generic "resize" cursor exists in `winit`/`cursor-icon`;
        // `AllResize` (an omnidirectional 4-way arrow) is the closest match
        // to "this is resizable" without committing to one axis/direction.
        CursorIcon::Resize => (true, winit::window::CursorIcon::AllResize),
        CursorIcon::NotAllowed => (true, winit::window::CursorIcon::NotAllowed),
        CursorIcon::Hidden => (false, winit::window::CursorIcon::default()),
    }
}

/// Step a calendar popup's browsed month by `delta` (+/-1), wrapping the year
/// at the Dec/Jan boundary — pure UI-navigation state, never touches `value`.
fn step_view_month(view_year: &mut i32, view_month: &mut u32, delta: i32) {
    let zero_based = *view_month as i32 - 1 + delta;
    *view_year += zero_based.div_euclid(12);
    *view_month = (zero_based.rem_euclid(12) + 1) as u32;
}

/// Find which clock-popup column (if any) `p` landed an up/down arrow of,
/// and return the `(h, m, s)` that arrow click steps toward — `None` if `p`
/// missed every arrow (e.g. landed on a value display or blank popup area).
fn step_from_clock_columns(layout: &nowui_core::datetime::ClockLayout, p: Point, h: u32, m: u32, s: u32) -> Option<(u32, u32, u32)> {
    for (col, (up, _value, down)) in layout.columns.iter().enumerate() {
        if up.contains(p) {
            return Some(nowui_core::datetime::step_hms(h, m, s, col, 1));
        }
        if down.contains(p) {
            return Some(nowui_core::datetime::step_hms(h, m, s, col, -1));
        }
    }
    None
}

/// Write `values` (one per original backtick, same order/count as
/// `nowui-runtime/src/semantic.rs`'s `primitive()` built the node's string
/// fields from) into whichever `NodeKind` fields came from those backticks.
/// Keep this index mapping in sync with `primitive()` if either changes.
fn apply_resolved_templates(kind: &mut NodeKind, values: &[String]) {
    match kind {
        NodeKind::Text { content } => {
            if let Some(v) = values.first() {
                *content = v.clone();
            }
        }
        NodeKind::Button { label } => {
            if let Some(v) = values.first() {
                *label = v.clone();
            }
        }
        NodeKind::Checkbox { label, .. } => {
            if let Some(v) = values.first() {
                *label = v.clone();
            }
        }
        NodeKind::TextInput { label, placeholder, .. } => {
            if let Some(v) = values.first() {
                *label = v.clone();
            }
            if let Some(v) = values.get(1) {
                *placeholder = v.clone();
            }
        }
        NodeKind::Dropdown { placeholder, options, .. } => {
            if let Some(v) = values.first() {
                *placeholder = v.clone();
            }
            for (opt, v) in options.iter_mut().zip(values.iter().skip(1)) {
                *opt = v.clone();
            }
        }
        NodeKind::Menu { label, .. } => {
            if let Some(v) = values.first() {
                *label = v.clone();
            }
        }
        NodeKind::MenuItem { label } => {
            if let Some(v) = values.first() {
                *label = v.clone();
            }
        }
        NodeKind::Date { placeholder, .. } | NodeKind::Time { placeholder, .. } | NodeKind::DateTime { placeholder, .. } => {
            if let Some(v) = values.first() {
                *placeholder = v.clone();
            }
        }
        NodeKind::Slider { .. } | NodeKind::ProgressBar { .. } | NodeKind::Container => {}
    }
}

impl<S: NowUiState + 'static> ApplicationHandler for App<S> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title(self.title.clone())
            .with_inner_size(winit::dpi::LogicalSize::new(1024.0, 640.0));
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        let size = window.inner_size();

        match self.backend {
            Backend::Cpu => {
                let context = softbuffer::Context::new(window.clone()).expect("softbuffer context");
                let surface = softbuffer::Surface::new(&context, window.clone()).expect("surface");
                self.surface = Some(surface);
            }
            Backend::Gpu => {
                self.gpu = Some(GpuSurfaceState::new(window.clone(), size.width.max(1), size.height.max(1)));
            }
        }

        self.window = Some(window);
        self.ui.dirty = true;
        self.next_frame = Instant::now();
        self.request_redraw();
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }

    /// Fires once winit has delivered every event queued for this iteration
    /// and is about to go idle — the standard place to schedule the next
    /// tick of a fixed-rate loop. Requests the next redraw once `next_frame`
    /// is reached (advancing it by exactly `FRAME_INTERVAL`, not `now +
    /// FRAME_INTERVAL`, so occasional scheduling jitter doesn't accumulate
    /// into drift over a long-running session — except when the app actually
    /// fell behind, e.g. the window was minimized/stalled, in which case
    /// trying to "catch up" would fire a burst of redraws all at once; that
    /// case resyncs to `now + FRAME_INTERVAL` instead), then reschedules
    /// `ControlFlow::WaitUntil` for whatever the new deadline is.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if now >= self.next_frame {
            self.next_frame += FRAME_INTERVAL;
            if self.next_frame < now {
                self.next_frame = now + FRAME_INTERVAL;
            }
            self.request_redraw();
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(_) => {
                self.dispatch_event_broadcast("onResize", EventKind::Resize);
                self.ui.dirty = true;
                self.request_redraw();
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Point::new(position.x as f32, position.y as f32);

                if let Some(id) = self.dragging_slider {
                    self.set_slider_value_from_cursor(id, self.cursor);
                    self.ui.dirty = true;
                    self.request_redraw();
                }

                if let Some(id) = self.dragging_text_input {
                    let idx = self.char_index_for_click(id, self.cursor);
                    if let NodeKind::TextInput { cursor, .. } = &mut self.ui.get_mut(id).kind {
                        *cursor = idx;
                    }
                    self.ui.dirty = true;
                    self.request_redraw();
                }

                let hit = self.ui.hit_test(self.cursor);
                if let Some(id) = hit {
                    self.dispatch_event(id, "onMouseMove", EventKind::MouseMove, None);
                }
                if hit != self.hovered {
                    self.hovered = hit;
                    self.ui.dirty = true;
                    self.request_redraw();
                }

                // `cursor-*` cascades like real CSS `cursor`: an `Auto`
                // (unset) node defers to its nearest ancestor that actually
                // declared one, rather than only ever looking at the single
                // deepest hit node — otherwise `Button cursor-pointer { Text
                // ... }` wouldn't show a hand while over the `Text` child,
                // since `hit_test` returns that innermost leaf, not `Button`.
                let resolved_icon = self
                    .ui
                    .hit_test_chain(self.cursor)
                    .iter()
                    .rev()
                    .map(|&id| self.ui.get(id).style.cursor)
                    .find(|c| *c != CursorIcon::Auto)
                    .unwrap_or(CursorIcon::Auto);
                let wanted = cursor_icon_for(resolved_icon);
                if wanted != self.current_cursor {
                    self.current_cursor = wanted;
                    if let Some(window) = &self.window {
                        let (visible, icon) = wanted;
                        window.set_cursor_visible(visible);
                        if visible {
                            window.set_cursor(icon);
                        }
                    }
                }
            }

            WindowEvent::MouseInput { state, button, .. } => {
                if button != MouseButton::Left {
                    return;
                }
                match state {
                    ElementState::Pressed => {
                        // Floating dropdown/menu popups sit on top of
                        // everything pixel-wise but outside every node's own
                        // `computed` rect, so they're checked before falling
                        // back to the normal rect-based hit test.
                        if let Some(dropdown) = self.find_open_dropdown_popup_at(self.cursor) {
                            self.select_dropdown_option(dropdown, self.cursor);
                        } else if let Some(menu) = self.find_open_menu_popup_at(self.cursor) {
                            self.select_menu_item(menu, self.cursor);
                        } else if let Some(picker) = self.find_open_picker_popup_at(self.cursor) {
                            match &self.ui.get(picker).kind {
                                NodeKind::Date { .. } => self.select_date_popup(picker, self.cursor),
                                NodeKind::Time { .. } => self.select_time_popup(picker, self.cursor),
                                NodeKind::DateTime { .. } => self.select_datetime_popup(picker, self.cursor),
                                _ => {}
                            }
                        } else {
                            match self.ui.hit_test(self.cursor) {
                                Some(hit) => {
                                    self.ui.focus = Some(hit);
                                    self.pressed = Some(hit);
                                    self.dispatch_event(hit, "onMouseDown", EventKind::MouseDown, None);
                                    if matches!(self.ui.get(hit).kind, NodeKind::Slider { .. }) {
                                        // Clicking anywhere on the track jumps
                                        // the thumb there, then drags from it.
                                        self.dragging_slider = Some(hit);
                                        self.set_slider_value_from_cursor(hit, self.cursor);
                                    } else if matches!(self.ui.get(hit).kind, NodeKind::TextInput { .. }) {
                                        self.start_text_selection(hit, self.cursor);
                                        self.handle_click(hit);
                                    } else {
                                        self.handle_click(hit);
                                    }
                                }
                                None => {
                                    self.close_other_dropdowns(None);
                                    self.close_other_menus(None);
                                    self.close_other_pickers(None);
                                }
                            }
                        }
                        self.ui.dirty = true;
                        self.request_redraw();
                    }
                    ElementState::Released => {
                        self.dragging_slider = None;
                        if let Some(id) = self.dragging_text_input.take() {
                            // A plain click (never dragged) leaves a zero-
                            // width "selection" sitting at the caret —
                            // collapse it to `None` so it behaves exactly
                            // like no selection at all (nothing to render,
                            // nothing for Backspace/typing to replace).
                            if let NodeKind::TextInput { cursor, selection_anchor, .. } = &mut self.ui.get_mut(id).kind {
                                if *selection_anchor == Some(*cursor) {
                                    *selection_anchor = None;
                                }
                            }
                        }
                        if let Some(id) = self.pressed.take() {
                            self.dispatch_event(id, "onMouseUp", EventKind::MouseUp, None);
                            self.ui.dirty = true;
                            self.request_redraw();
                        }
                    }
                }
            }

            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
            }

            WindowEvent::Ime(ime) => {
                let Some(id) = self.ui.focus else { return };
                if !matches!(self.ui.get(id).kind, NodeKind::TextInput { .. }) {
                    return;
                }
                match ime {
                    winit::event::Ime::Enabled | winit::event::Ime::Disabled => {}
                    winit::event::Ime::Preedit(text, _cursor_range) => {
                        self.set_ime_preview(id, text);
                        self.ui.dirty = true;
                        self.request_redraw();
                    }
                    winit::event::Ime::Commit(text) => {
                        if let Some(new_value) = self.commit_ime_text(id, &text) {
                            self.write_back_value(id, StateValue::Str(new_value));
                        }
                        self.ui.dirty = true;
                        self.request_redraw();
                    }
                }
            }

            WindowEvent::KeyboardInput { event, .. } => {
                let Some(id) = self.ui.focus else { return };

                if event.state == ElementState::Pressed && matches!(self.ui.get(id).kind, NodeKind::TextInput { .. }) {
                    let shift = self.modifiers.shift_key();
                    let ctrl = self.modifiers.control_key();
                    if let Some(new_value) = self.edit_text_input(id, &event.logical_key, event.text.as_deref(), shift, ctrl) {
                        self.write_back_value(id, StateValue::Str(new_value));
                    }
                }

                let key = event.logical_key.to_text().map(str::to_string).unwrap_or_else(|| format!("{:?}", event.logical_key));
                match event.state {
                    ElementState::Pressed => {
                        self.dispatch_event(id, "onKeyDown", EventKind::KeyDown, Some(key.clone()));
                        self.dispatch_event(id, "onKeyPress", EventKind::KeyPress, Some(key));
                    }
                    ElementState::Released => {
                        self.dispatch_event(id, "onKeyUp", EventKind::KeyUp, Some(key));
                    }
                }
                self.ui.dirty = true;
                self.request_redraw();
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x * 40.0, y * 40.0),
                    MouseScrollDelta::PixelDelta(p) => (p.x as f32, p.y as f32),
                };
                // Nearest-to-cursor (deepest) scrollable ancestor wins.
                let chain = self.ui.hit_test_chain(self.cursor);
                for &id in chain.iter().rev() {
                    let style = &self.ui.get(id).style;
                    let (scroll_x, scroll_y) = (style.scroll_x, style.scroll_y);
                    if !scroll_x && !scroll_y {
                        continue;
                    }
                    let content = self.ui.get(id).content_size;
                    let rect = self.ui.get(id).computed;
                    let node = self.ui.get_mut(id);
                    // Inverted from the naive `+= delta`: wheel-away-from-user
                    // (positive delta) pans the *view* down, i.e. decreases
                    // the offset, matching "natural"/trackpad-style scrolling.
                    if scroll_y {
                        let max_y = (content.h - rect.h).max(0.0);
                        node.scroll_offset.y = (node.scroll_offset.y - dy).clamp(0.0, max_y);
                    }
                    if scroll_x {
                        let max_x = (content.w - rect.w).max(0.0);
                        node.scroll_offset.x = (node.scroll_offset.x - dx).clamp(0.0, max_x);
                    }
                    self.ui.dirty = true;
                    self.request_redraw();
                    break;
                }
            }

            // Unconditional — this is a fixed 60fps loop (see
            // `about_to_wait`), not an on-demand repaint gated on whether
            // anything actually changed.
            WindowEvent::RedrawRequested => self.redraw(),

            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nowui_core::{Node, Sizing, Style};

    #[derive(Default, Clone, NowUiState)]
    struct DemoState {
        width: f64,
    }

    #[test]
    fn resolve_dynamic_styles_applies_live_state_to_a_style_field() {
        // `w-[${state.width}]` — recorded on `Style::dynamic` by the semantic
        // pass at parse time, left unresolved until now.
        let mut style = Style::default();
        style.dynamic.insert("w".to_string(), vec!["state".to_string(), "width".to_string()]);
        let mut ui = Ui::new();
        let id = ui.push(Node::new(NodeKind::Container, style));
        ui.add_layer(id, "main");

        let mut app = App::new("test".to_string(), ui, DemoState { width: 250.0 }, crate::semantic::Semantic::new(&[]), Backend::Cpu);
        app.resolve_dynamic_styles();

        assert_eq!(app.ui.get(id).base_style.width, Sizing::Fixed(250.0));
    }

    #[test]
    fn resolve_dynamic_styles_is_a_noop_when_the_path_does_not_resolve() {
        let mut style = Style::default();
        style.dynamic.insert("w".to_string(), vec!["state".to_string(), "nope".to_string()]);
        let mut ui = Ui::new();
        let id = ui.push(Node::new(NodeKind::Container, style));
        ui.add_layer(id, "main");

        let mut app = App::new("test".to_string(), ui, DemoState::default(), crate::semantic::Semantic::new(&[]), Backend::Cpu);
        app.resolve_dynamic_styles();

        assert_eq!(app.ui.get(id).base_style.width, Sizing::Hug, "left at its default");
    }

    #[test]
    fn on_load_delay_defers_dispatch_until_its_deadline_passes() {
        #[derive(Default, Clone, nowui_core::NowUiState)]
        #[nowui(methods(loaded))]
        struct S {
            load_count: i64,
        }
        impl S {
            fn loaded(&mut self, _app: &mut S, _event: &mut nowui_core::Event) {
                self.load_count += 1;
            }
        }

        let src = "layout: T { Text `a` {onLoad: state.loaded, onLoadDelay: 0.02} }";
        let ast = nowui_syntax::parse(src).unwrap();
        let mut sem = crate::semantic::Semantic::new(&ast);
        let ui = sem.build("T", &S::default()).unwrap();

        let mut app = App::new("test".to_string(), ui, S::default(), sem, Backend::Cpu);
        app.dispatch_pending_on_load();
        assert_eq!(app.state.load_count, 0, "queued, not fired yet — the delay hasn't elapsed");
        assert_eq!(app.pending_on_load_timers.len(), 1);

        std::thread::sleep(std::time::Duration::from_millis(40));
        app.fire_due_on_load_timers();
        assert_eq!(app.state.load_count, 1, "deadline passed — fires on the next check");
        assert!(app.pending_on_load_timers.is_empty());
    }

    #[test]
    fn a_delayed_on_loads_state_mutation_is_visible_to_the_same_frames_region_refresh() {
        // Regression test for the real bug this split guarded against: a
        // splash screen's `{onLoad: state.go, onLoadDelay: ...}` flips
        // `state.page`, which an `if` branches the visible content on. If
        // `fire_due_on_load_timers` ran *after* `refresh_dynamic_regions`
        // (as a single combined `dispatch_pending_on_load` used to), the
        // flip would land one frame too late — and since nothing else was
        // dirty by then, the app would go idle (`ControlFlow::Wait`) with
        // the stale screen showing until an unrelated click/keypress forced
        // another redraw.
        #[derive(Default, Clone, nowui_core::NowUiState)]
        #[nowui(methods(go))]
        struct S {
            page: String,
        }
        impl S {
            fn go(&mut self, _app: &mut S, _event: &mut nowui_core::Event) {
                self.page = "b".to_string();
            }
        }

        let src = "layout: T { if state.page == \"a\" { \
            Text `A` {onLoad: state.go, onLoadDelay: 0.02} \
        } else { Text `B` } }";
        let ast = nowui_syntax::parse(src).unwrap();
        let mut sem = crate::semantic::Semantic::new(&ast);
        let state = S { page: "a".to_string() };
        let ui = sem.build("T", &state).unwrap();

        let mut app = App::new("test".to_string(), ui, state, sem, Backend::Cpu);
        app.dispatch_pending_on_load();
        assert_eq!(app.pending_on_load_timers.len(), 1, "queued, not fired — delay hasn't elapsed yet");

        std::thread::sleep(std::time::Duration::from_millis(40));
        // Exactly what `redraw` does, in the same order: fire due timers
        // *first*, then refresh regions.
        app.fire_due_on_load_timers();
        app.semantic.refresh_dynamic_regions(&mut app.ui, &app.state);

        let root = app.ui.get(app.ui.layers[0].root);
        let NodeKind::Text { content } = &app.ui.get(root.children[0]).kind else { panic!("expected Text") };
        assert_eq!(content, "B", "the region refresh saw state.page == \"b\" this same frame");
    }

    #[test]
    fn zero_on_load_delay_fires_immediately_same_as_no_binding_at_all() {
        #[derive(Default, Clone, nowui_core::NowUiState)]
        #[nowui(methods(loaded))]
        struct S {
            load_count: i64,
        }
        impl S {
            fn loaded(&mut self, _app: &mut S, _event: &mut nowui_core::Event) {
                self.load_count += 1;
            }
        }

        let src = "layout: T { Text `a` {onLoad: state.loaded, onLoadDelay: 0.0} }";
        let ast = nowui_syntax::parse(src).unwrap();
        let mut sem = crate::semantic::Semantic::new(&ast);
        let ui = sem.build("T", &S::default()).unwrap();

        let mut app = App::new("test".to_string(), ui, S::default(), sem, Backend::Cpu);
        app.dispatch_pending_on_load();
        assert_eq!(app.state.load_count, 1);
        assert!(app.pending_on_load_timers.is_empty(), "never queued at all — no delay to wait out");
    }

    #[test]
    fn dispatch_pending_on_load_fires_once_for_the_initial_tree_and_again_for_a_new_for_row() {
        #[derive(Default, Clone, nowui_core::NowUiState)]
        #[nowui(methods(loaded))]
        struct S {
            load_count: i64,
            rows: Vec<i64>,
        }
        impl S {
            fn loaded(&mut self, _app: &mut S, _event: &mut nowui_core::Event) {
                self.load_count += 1;
            }
        }

        let src = "layout: T { Container {onLoad: state.loaded} for x in state.rows { Text `${x}` {onLoad: state.loaded} } }";
        let ast = nowui_syntax::parse(src).unwrap();
        let mut sem = crate::semantic::Semantic::new(&ast);
        let state = S { load_count: 0, rows: vec![1] };
        let ui = sem.build("T", &state).unwrap();

        let mut app = App::new("test".to_string(), ui, state, sem, Backend::Cpu);
        app.dispatch_pending_on_load();
        assert_eq!(app.state.load_count, 2, "the static Container plus the one initial row");

        app.state.rows.push(2);
        app.semantic.refresh_dynamic_regions(&mut app.ui, &app.state);
        app.dispatch_pending_on_load();
        assert_eq!(app.state.load_count, 4, "for rebuilds both rows fresh (no per-item keying) — onLoad refires for each");
    }

    #[test]
    fn handle_click_toggles_a_menus_open_state_and_dispatches_onclick() {
        let mut ui = Ui::new();
        let item = ui.push(Node::new(NodeKind::MenuItem { label: "Open Preferences".to_string() }, Style::default()));
        let menu = ui.push(Node::new(NodeKind::Menu { label: "Preferences".to_string(), open: false }, Style::default()));
        ui.get_mut(menu).children = vec![item];
        ui.add_layer(menu, "main");
        let mut app = App::new("test".to_string(), ui, nowui_core::NoState, crate::semantic::Semantic::new(&[]), Backend::Cpu);

        app.handle_click(menu);
        let NodeKind::Menu { open, .. } = &app.ui.get(menu).kind else { panic!() };
        assert!(open, "first click opens it");

        app.handle_click(menu);
        let NodeKind::Menu { open, .. } = &app.ui.get(menu).kind else { panic!() };
        assert!(!open, "second click closes it again");
    }

    #[test]
    fn clicking_a_menu_item_dispatches_its_own_onclick_independent_of_the_menu() {
        let mut ui = Ui::new();
        let item = ui.push(Node::new(NodeKind::MenuItem { label: "Open Preferences".to_string() }, Style::default()));
        ui.get_mut(item).events.insert("onClick".to_string(), vec!["state".to_string(), "item_click".to_string()]);
        let menu = ui.push(Node::new(NodeKind::Menu { label: "Preferences".to_string(), open: true }, Style::default()));
        ui.get_mut(menu).events.insert("onClick".to_string(), vec!["state".to_string(), "menuClick".to_string()]);
        ui.get_mut(menu).children = vec![item];
        ui.add_layer(menu, "main");

        #[derive(Default, Clone, nowui_core::NowUiState)]
        #[nowui(methods(item_click))]
        struct S {
            item_clicked: bool,
        }
        impl S {
            fn item_click(&mut self, _app: &mut S, _event: &mut nowui_core::Event) {
                self.item_clicked = true;
            }
        }

        // The item lives in the menu's floating popup, not reachable through
        // normal in-flow hit-testing — clicking it goes through
        // `select_menu_item`, mirroring `select_dropdown_option`.
        ui.get_mut(item).computed = Rect::new(0.0, 40.0, 100.0, 20.0);
        let mut app = App::new("test".to_string(), ui, S::default(), crate::semantic::Semantic::new(&[]), Backend::Cpu);
        app.select_menu_item(menu, Point::new(10.0, 50.0));

        assert!(app.state.item_clicked, "MenuItem's own onClick fired, not the parent Menu's");
        // Picking an item closes the popup, matching Dropdown's
        // select-closes-the-list convention.
        let NodeKind::Menu { open, .. } = &app.ui.get(menu).kind else { panic!() };
        assert!(!open, "selecting an item closes the menu popup");
    }

    fn date_app(value: &str) -> (App<nowui_core::NoState>, NodeId) {
        let mut ui = Ui::new();
        let id = ui.push(Node::new(
            NodeKind::Date { value: value.to_string(), placeholder: String::new(), open: true, view_year: 2024, view_month: 3 },
            Style { font_size: 16.0, ..Style::default() },
        ));
        ui.get_mut(id).computed = Rect::new(0.0, 0.0, 280.0, 41.0); // matches dropdown_metrics(16.0)'s box_h
        ui.add_layer(id, "main");
        let app = App::new("test".to_string(), ui, nowui_core::NoState, crate::semantic::Semantic::new(&[]), Backend::Cpu);
        (app, id)
    }

    #[test]
    fn clicking_a_day_cell_writes_the_date_back_and_closes_the_popup() {
        let (mut app, id) = date_app("");
        let layout = nowui_core::datetime::layout_calendar(app.ui.get(id).computed, 16.0, 2024, 3);
        // March 2024: day 1 is the 6th cell (index 5) — see datetime.rs's own test.
        let (day_one_rect, day) = layout.day_cells[5];
        assert_eq!(day, Some(1));
        let p = Point::new(day_one_rect.x + 1.0, day_one_rect.y + 1.0);

        app.select_date_popup(id, p);

        let NodeKind::Date { value, open, .. } = &app.ui.get(id).kind else { panic!() };
        assert_eq!(value, "01/03/2024");
        assert!(!open, "picking a day is terminal — closes the popup");
    }

    #[test]
    fn clicking_the_next_arrow_advances_the_browsed_month_without_touching_the_value() {
        let (mut app, id) = date_app("");
        let layout = nowui_core::datetime::layout_calendar(app.ui.get(id).computed, 16.0, 2024, 3);
        let next_click = Point::new(layout.next_rect.x + 1.0, layout.next_rect.y + 1.0);

        app.select_date_popup(id, next_click);

        let NodeKind::Date { value, open, view_year, view_month, .. } = &app.ui.get(id).kind else { panic!() };
        assert_eq!((*view_year, *view_month), (2024, 4), "March -> April");
        assert_eq!(value, "", "browsing months never touches the picked value");
        assert!(open, "browsing doesn't close the popup");
    }

    #[test]
    fn clicking_the_prev_arrow_in_january_wraps_back_to_december_of_the_prior_year() {
        let mut ui = Ui::new();
        let id = ui.push(Node::new(
            NodeKind::Date { value: String::new(), placeholder: String::new(), open: true, view_year: 2024, view_month: 1 },
            Style { font_size: 16.0, ..Style::default() },
        ));
        ui.get_mut(id).computed = Rect::new(0.0, 0.0, 280.0, 41.0);
        ui.add_layer(id, "main");
        let mut app = App::new("test".to_string(), ui, nowui_core::NoState, crate::semantic::Semantic::new(&[]), Backend::Cpu);
        let layout = nowui_core::datetime::layout_calendar(app.ui.get(id).computed, 16.0, 2024, 1);
        let prev_click = Point::new(layout.prev_rect.x + 1.0, layout.prev_rect.y + 1.0);

        app.select_date_popup(id, prev_click);

        let NodeKind::Date { view_year, view_month, .. } = &app.ui.get(id).kind else { panic!() };
        assert_eq!((*view_year, *view_month), (2023, 12));
    }

    #[test]
    fn clicking_a_spinner_up_arrow_increments_that_unit_and_keeps_the_popup_open() {
        let mut ui = Ui::new();
        let id = ui.push(Node::new(
            NodeKind::Time { value: "09:30".to_string(), placeholder: String::new(), open: true },
            Style { font_size: 16.0, ..Style::default() },
        ));
        ui.get_mut(id).computed = Rect::new(0.0, 0.0, 280.0, 41.0);
        ui.add_layer(id, "main");
        let mut app = App::new("test".to_string(), ui, nowui_core::NoState, crate::semantic::Semantic::new(&[]), Backend::Cpu);
        let layout = nowui_core::datetime::layout_clock(app.ui.get(id).computed, 16.0, false);
        let (hour_up, ..) = layout.columns[0];
        let p = Point::new(hour_up.x + 1.0, hour_up.y + 1.0);

        app.select_time_popup(id, p);

        let NodeKind::Time { value, open, .. } = &app.ui.get(id).kind else { panic!() };
        assert_eq!(value, "10:30");
        assert!(open, "dialing in a time takes more than one click — never auto-closes");
    }

    #[test]
    fn datetime_popup_updates_only_the_half_that_was_clicked() {
        let mut ui = Ui::new();
        let id = ui.push(Node::new(
            NodeKind::DateTime {
                value: "15/03/2024 09:30".to_string(),
                placeholder: String::new(),
                open: true,
                view_year: 2024,
                view_month: 3,
            },
            Style { font_size: 16.0, ..Style::default() },
        ));
        ui.get_mut(id).computed = Rect::new(0.0, 0.0, 280.0, 41.0);
        ui.add_layer(id, "main");
        let mut app = App::new("test".to_string(), ui, nowui_core::NoState, crate::semantic::Semantic::new(&[]), Backend::Cpu);
        let layout = nowui_core::datetime::layout_datetime(app.ui.get(id).computed, 16.0, false, 2024, 3);

        // Click day 20 in the calendar half — only the date part changes.
        let (day_20_rect, day) = layout.calendar.day_cells.iter().find(|(_, d)| *d == Some(20)).copied().unwrap();
        assert_eq!(day, Some(20));
        app.select_datetime_popup(id, Point::new(day_20_rect.x + 1.0, day_20_rect.y + 1.0));
        {
            let NodeKind::DateTime { value, open, .. } = &app.ui.get(id).kind else { panic!() };
            assert_eq!(value, "20/03/2024 09:30");
            assert!(open, "picking the date half never closes a DateTime popup");
        }

        // Now click the minute column's down arrow — only the time part changes.
        let layout = nowui_core::datetime::layout_datetime(app.ui.get(id).computed, 16.0, false, 2024, 3);
        let (_, _, minute_down) = layout.clock.columns[1];
        app.select_datetime_popup(id, Point::new(minute_down.x + 1.0, minute_down.y + 1.0));
        let NodeKind::DateTime { value, open, .. } = &app.ui.get(id).kind else { panic!() };
        assert_eq!(value, "20/03/2024 09:29");
        assert!(open);
    }

    /// A one-node `Ui` (a `TextInput` seeded with `label`, cursor at the
    /// end, no selection/IME) plus the `App` wrapping it — the common setup
    /// every `edit_text_input`/click-hit-testing test starts from.
    fn text_input_app(label: &str) -> (App<nowui_core::NoState>, NodeId) {
        let mut ui = Ui::new();
        let id = ui.push(Node::new(
            NodeKind::TextInput {
                label: label.to_string(),
                placeholder: String::new(),
                masked: false,
                cursor: label.chars().count(),
                selection_anchor: None,
                ime_preview: String::new(),
            },
            Style::default(),
        ));
        ui.add_layer(id, "main");
        (App::new("test".to_string(), ui, nowui_core::NoState, crate::semantic::Semantic::new(&[]), Backend::Cpu), id)
    }

    fn text_input_state(app: &App<nowui_core::NoState>, id: NodeId) -> (String, usize, Option<usize>) {
        let NodeKind::TextInput { label, cursor, selection_anchor, .. } = &app.ui.get(id).kind else { panic!() };
        (label.clone(), *cursor, *selection_anchor)
    }

    fn multiline_text_input_app(label: &str) -> (App<nowui_core::NoState>, NodeId) {
        let (mut app, id) = text_input_app(label);
        app.ui.get_mut(id).style.multiline = true;
        app.ui.get_mut(id).base_style.multiline = true;
        (app, id)
    }

    #[test]
    fn edit_text_input_appends_text_and_backspace_deletes_before_cursor() {
        let (mut app, id) = text_input_app("");

        assert_eq!(app.edit_text_input(id, &Key::Character("d".into()), Some("d"), false, false), Some("d".to_string()));
        assert_eq!(app.edit_text_input(id, &Key::Character("e".into()), Some("e"), false, false), Some("de".to_string()));
        assert_eq!(
            app.edit_text_input(id, &Key::Named(NamedKey::Backspace), None, false, false),
            Some("d".to_string()),
            "backspace deletes the char immediately before the cursor"
        );

        assert_eq!(text_input_state(&app, id), ("d".to_string(), 1, None));
    }

    #[test]
    fn edit_text_input_returns_none_when_nothing_changed() {
        let (mut app, id) = text_input_app("");

        // Backspace on an already-empty field is a no-op.
        assert_eq!(app.edit_text_input(id, &Key::Named(NamedKey::Backspace), None, false, false), None);
        // A pure cursor move never changes `label` — no state write needed.
        assert_eq!(app.edit_text_input(id, &Key::Named(NamedKey::ArrowLeft), None, false, false), None);
    }

    #[test]
    fn edit_text_input_filters_control_characters() {
        let (mut app, id) = text_input_app("");

        // Enter's `text` is `Some("\r")` — must not land in the buffer.
        assert_eq!(app.edit_text_input(id, &Key::Named(NamedKey::Enter), Some("\r"), false, false), None);
    }

    #[test]
    fn arrow_keys_move_the_caret_and_delete_deletes_after_it() {
        let (mut app, id) = text_input_app("abc"); // cursor starts at 3 (the end)

        app.edit_text_input(id, &Key::Named(NamedKey::ArrowLeft), None, false, false);
        assert_eq!(text_input_state(&app, id), ("abc".to_string(), 2, None));

        assert_eq!(
            app.edit_text_input(id, &Key::Named(NamedKey::Delete), None, false, false),
            Some("ab".to_string()),
            "Delete removes the char after the cursor, not before"
        );
        assert_eq!(text_input_state(&app, id), ("ab".to_string(), 2, None));
    }

    #[test]
    fn shift_arrow_extends_a_selection_and_plain_arrow_collapses_it() {
        let (mut app, id) = text_input_app("hello"); // cursor at 5

        app.edit_text_input(id, &Key::Named(NamedKey::ArrowLeft), None, true, false);
        app.edit_text_input(id, &Key::Named(NamedKey::ArrowLeft), None, true, false);
        assert_eq!(text_input_state(&app, id), ("hello".to_string(), 3, Some(5)), "selection grows leftward from 5");

        // A plain (non-shift) arrow collapses the selection to one edge
        // instead of moving the caret one further char.
        app.edit_text_input(id, &Key::Named(NamedKey::ArrowLeft), None, false, false);
        assert_eq!(text_input_state(&app, id), ("hello".to_string(), 3, None));
    }

    #[test]
    fn typing_replaces_the_active_selection() {
        let (mut app, id) = text_input_app("hello");
        if let NodeKind::TextInput { cursor, selection_anchor, .. } = &mut app.ui.get_mut(id).kind {
            *cursor = 4; // selects "ell" (chars 1..4)
            *selection_anchor = Some(1);
        }

        assert_eq!(app.edit_text_input(id, &Key::Character("X".into()), Some("X"), false, false), Some("hXo".to_string()));
        assert_eq!(text_input_state(&app, id), ("hXo".to_string(), 2, None));
    }

    #[test]
    fn backspace_and_delete_remove_the_active_selection_instead_of_one_char() {
        let (mut app, id) = text_input_app("hello");
        if let NodeKind::TextInput { cursor, selection_anchor, .. } = &mut app.ui.get_mut(id).kind {
            *cursor = 4;
            *selection_anchor = Some(1);
        }

        assert_eq!(app.edit_text_input(id, &Key::Named(NamedKey::Backspace), None, false, false), Some("ho".to_string()));
        assert_eq!(text_input_state(&app, id), ("ho".to_string(), 1, None));
    }

    #[test]
    fn ctrl_a_selects_everything() {
        let (mut app, id) = text_input_app("hello");
        app.edit_text_input(id, &Key::Character("a".into()), Some("a"), false, true);
        assert_eq!(text_input_state(&app, id), ("hello".to_string(), 5, Some(0)));
    }

    #[test]
    fn home_and_end_move_the_caret_to_the_edges() {
        let (mut app, id) = text_input_app("hello"); // starts at 5
        app.edit_text_input(id, &Key::Named(NamedKey::Home), None, false, false);
        assert_eq!(text_input_state(&app, id).1, 0);
        app.edit_text_input(id, &Key::Named(NamedKey::End), None, false, false);
        assert_eq!(text_input_state(&app, id).1, 5);
    }

    #[test]
    fn char_index_for_click_finds_the_nearest_char_boundary() {
        let (mut app, id) = text_input_app("hello");
        app.ui.get_mut(id).computed = Rect::new(0.0, 0.0, 200.0, 30.0);

        // Clicking at the box's left edge (x == content start) must land
        // before the first character, not after it.
        let idx = app.char_index_for_click(id, Point::new(0.0, 15.0));
        assert_eq!(idx, 0);

        // Clicking far past the end of the text lands after the last char.
        let idx = app.char_index_for_click(id, Point::new(9999.0, 15.0));
        assert_eq!(idx, 5);
    }

    #[test]
    fn char_index_for_click_accounts_for_the_current_scroll_offset() {
        // Regression: the painter draws `shown` starting at `content_rect.x
        // - scroll_offset.x` (see `paint_text_input`), but this used to
        // measure clicks against the box's own unshifted edge — correct
        // only while unscrolled. Once scrolled, a click landed on whichever
        // character used to be under that screen position *before*
        // scrolling, not the one actually rendered there now.
        let (mut app, id) = text_input_app("hello world");
        app.ui.get_mut(id).computed = Rect::new(0.0, 0.0, 200.0, 30.0);

        let unscrolled_idx = app.char_index_for_click(id, Point::new(50.0, 15.0));

        app.ui.get_mut(id).scroll_offset.x = 30.0;
        let scrolled_idx = app.char_index_for_click(id, Point::new(50.0, 15.0));

        assert!(
            scrolled_idx > unscrolled_idx,
            "the same screen x now points at a character further into the (scrolled) text, not the same one as before"
        );
    }

    #[test]
    fn update_text_input_scroll_is_zero_when_the_text_fits_the_box() {
        let (mut app, id) = text_input_app("hi");
        app.ui.get_mut(id).computed = Rect::new(0.0, 0.0, 200.0, 30.0);
        app.update_text_input_scroll();
        assert_eq!(app.ui.get(id).scroll_offset.x, 0.0);
    }

    #[test]
    fn update_text_input_scroll_follows_the_caret_past_a_narrow_box() {
        let (mut app, id) = text_input_app("a very long value that overflows a narrow box");
        app.ui.get_mut(id).computed = Rect::new(0.0, 0.0, 50.0, 30.0);

        app.update_text_input_scroll();

        let offset = app.ui.get(id).scroll_offset.x;
        assert!(offset > 0.0, "caret (at the end) is past the box, so the view must have scrolled");

        // Moving the caret back to the very start must scroll back to 0 —
        // the offset only ever moves just far enough to show the caret.
        app.edit_text_input(id, &Key::Named(NamedKey::Home), None, false, false);
        app.update_text_input_scroll();
        assert_eq!(app.ui.get(id).scroll_offset.x, 0.0);
    }

    #[test]
    fn update_text_input_scroll_clamps_to_the_end_of_the_text() {
        let (mut app, id) = text_input_app("hello");
        app.ui.get_mut(id).computed = Rect::new(0.0, 0.0, 200.0, 30.0);
        // A stale offset from before some characters were deleted must not
        // leave a scrolled-away gap once the remaining text fits the box.
        app.ui.get_mut(id).scroll_offset.x = 500.0;

        app.update_text_input_scroll();

        assert_eq!(app.ui.get(id).scroll_offset.x, 0.0);
    }

    #[test]
    fn enter_inserts_a_newline_only_when_multiline() {
        let (mut app, id) = text_input_app("ab");
        assert_eq!(
            app.edit_text_input(id, &Key::Named(NamedKey::Enter), Some("\r"), false, false),
            None,
            "single-line: Enter is a no-op, same as any other filtered control character"
        );

        let (mut app, id) = multiline_text_input_app("ab");
        assert_eq!(
            app.edit_text_input(id, &Key::Named(NamedKey::Enter), Some("\r"), false, false),
            Some("ab\n".to_string())
        );
        assert_eq!(text_input_state(&app, id), ("ab\n".to_string(), 3, None));
    }

    #[test]
    fn enter_replaces_the_active_selection_when_multiline() {
        let (mut app, id) = multiline_text_input_app("hello");
        if let NodeKind::TextInput { cursor, selection_anchor, .. } = &mut app.ui.get_mut(id).kind {
            *cursor = 4;
            *selection_anchor = Some(1);
        }
        assert_eq!(
            app.edit_text_input(id, &Key::Named(NamedKey::Enter), Some("\r"), false, false),
            Some("h\no".to_string())
        );
    }

    #[test]
    fn char_index_for_click_on_a_multiline_input_finds_the_right_line() {
        let (mut app, id) = multiline_text_input_app("aa\nbb\ncc");
        app.ui.get_mut(id).computed = Rect::new(0.0, 0.0, 200.0, 90.0);
        let line_h = nowui_core::text_input::line_height(app.ui.get(id).style.font_size);

        // A click on the second line (y between one and two line-heights
        // down) must resolve to a char index on "bb", not "aa" or "cc".
        let idx = app.char_index_for_click(id, Point::new(0.0, line_h + 1.0));
        let (line, _) = nowui_core::text_input::line_and_col("aa\nbb\ncc", idx);
        assert_eq!(line, 1);
    }

    #[test]
    fn update_text_input_scroll_follows_the_caret_line_when_multiline() {
        let (mut app, id) = multiline_text_input_app("a\nb\nc\nd\ne");
        // A box only tall enough for ~2 lines — the caret (at the end, on
        // the last line) starts out of view.
        app.ui.get_mut(id).computed = Rect::new(0.0, 0.0, 200.0, 40.0);

        app.update_text_input_scroll();

        assert_eq!(app.ui.get(id).scroll_offset.x, 0.0, "no horizontal scroll in multiline mode");
        assert!(app.ui.get(id).scroll_offset.y > 0.0, "vertical scroll follows the caret's line");
    }

    #[test]
    fn ime_preedit_sets_the_preview_text() {
        let (mut app, id) = text_input_app("ac");
        app.set_ime_preview(id, "b".to_string());
        let NodeKind::TextInput { ime_preview, .. } = &app.ui.get(id).kind else { panic!() };
        assert_eq!(ime_preview, "b");
    }

    #[test]
    fn ime_commit_inserts_at_the_cursor_and_clears_the_preview() {
        let (mut app, id) = text_input_app("ac");
        if let NodeKind::TextInput { cursor, ime_preview, .. } = &mut app.ui.get_mut(id).kind {
            *cursor = 1;
            *ime_preview = "PREVIEW".to_string(); // mid-composition preview
        }

        assert_eq!(app.commit_ime_text(id, "b"), Some("abc".to_string()));
        assert_eq!(text_input_state(&app, id), ("abc".to_string(), 2, None));
        let NodeKind::TextInput { ime_preview, .. } = &app.ui.get(id).kind else { panic!() };
        assert!(ime_preview.is_empty());
    }

    #[test]
    fn display_string_formats_int_and_float_differently() {
        // Int never carries a decimal point; Float always does, even when
        // it lands on a whole number (`3.0`, not `3`) — Int/Float are kept
        // as separate StateValue variants specifically so this doesn't have
        // to guess an int-looking float apart from a real int.
        assert_eq!(display_string(&StateValue::Int(3)), "3");
        assert_eq!(display_string(&StateValue::Int(-7)), "-7");
        assert_eq!(display_string(&StateValue::Float(3.0)), "3.0");
        assert_eq!(display_string(&StateValue::Float(3.5)), "3.5");
        assert_eq!(display_string(&StateValue::Bool(true)), "true");
        assert_eq!(display_string(&StateValue::Str("hi".to_string())), "hi");
    }
}
