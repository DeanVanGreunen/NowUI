//! The winit application harness: window + softbuffer surface, event-driven
//! redraw (ControlFlow::Wait), and the solve -> paint -> present cycle guarded
//! by a dirty flag.
//!
//! Reactivity lives here too: each redraw, every node's `value_path` is
//! resolved against the live `S: NowUiState` app state and written into the
//! widget (`resolve_values`); every dispatched DOM-ish event (`onClick`,
//! `onMouseDown`, ...) calls back into it (`dispatch_event`). See CLAUDE.md's
//! "Reactivity" section for the full read/write data flow and exactly which
//! widgets/events are wired.
//!
//! winit API note: this targets winit 0.30 (`ApplicationHandler` + `run_app`,
//! `resumed(&ActiveEventLoop)`). These names were introduced in 0.30 and do not
//! exist on 0.29 or earlier. If a future winit reshapes these callbacks
//! (e.g. `can_create_surfaces`, `&dyn ActiveEventLoop`), align the method
//! signatures with the docs for that version — the logic is unchanged.

use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::Instant;

use nowui_core::{
    compute_effective, dropdown_metrics, AnimatableStyle, Color, Event, EventKind, NodeId,
    NodeKind, NowUiState, Point, Rect, Size, StateValue, TemplatePart, Ui,
};
use nowui_render::{present_to_softbuffer, SkiaPainter, TextContext};
use tiny_skia::Pixmap;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::window::{Window, WindowId};

use crate::transitions::Transitions;

/// Background color painted before the tree each frame (opaque so premultiplied
/// == straight for the softbuffer bridge).
const CLEAR: Color = Color { r: 0x26, g: 0x80, b: 0xd4, a: 255 };

pub struct App<S: NowUiState> {
    ui: Ui,
    /// The live app state `value`/event bindings read from and dispatch to —
    /// usually a `#[derive(NowUiState)]` struct; `nowui_core::NoState` for
    /// the plain CLI binary, which has no Rust-side state at all.
    state: S,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    cursor: Point,
    /// Font database + glyph cache. Built once (loading system fonts is slow)
    /// and reused across every redraw.
    text: TextContext,
    /// The node the cursor is currently over (`hover:` variant trigger).
    hovered: Option<NodeId>,
    /// The node the mouse button is currently held down on (`active:` trigger).
    pressed: Option<NodeId>,
    /// Set while a `Slider`'s thumb is being dragged — real, intrinsic
    /// interaction, independent of the generic `onMouseDown`/`onMouseMove`/
    /// `onMouseUp` bindings (which now *are* dispatched, same as everything
    /// else — see `dispatch_event`).
    dragging_slider: Option<NodeId>,
    transitions: Transitions,
}

impl<S: NowUiState> App<S> {
    pub fn new(ui: Ui, state: S) -> Self {
        App {
            ui,
            state,
            window: None,
            surface: None,
            cursor: Point::default(),
            text: TextContext::new(),
            hovered: None,
            pressed: None,
            dragging_slider: None,
            transitions: Transitions::new(),
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
    /// state.foo.bar}`. Marks the UI dirty when the handler ran, since a
    /// callback mutating state almost always needs a redraw to show it.
    fn dispatch_event(&mut self, id: NodeId, event_name: &str, kind: EventKind, key: Option<String>) {
        let Some(path) = self.ui.get(id).events.get(event_name).cloned() else { return };
        let event = Event { kind, cursor: self.cursor, key };
        if self.state.call(&state_subpath(&path), &event) {
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

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        let Some(window) = self.window.clone() else { return };
        if self.surface.is_none() {
            return;
        }

        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));

        self.resolve_values();
        self.resolve_templates();
        self.apply_dynamic_styles(w as f32);

        let surface = self.surface.as_mut().expect("checked above");
        surface
            .resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap())
            .expect("surface resize");

        let mut pixmap = Pixmap::new(w, h).expect("pixmap alloc");
        pixmap.fill(tiny_skia::Color::from_rgba8(CLEAR.r, CLEAR.g, CLEAR.b, 255));

        {
            let mut painter = SkiaPainter::new(&mut pixmap, &mut self.text);
            nowui_core::layout::solve(&mut self.ui, Size::new(w as f32, h as f32), &mut painter);
            nowui_core::paint::paint(&self.ui, &mut painter);
        }

        let mut buffer = surface.buffer_mut().expect("buffer");
        present_to_softbuffer(&pixmap, &mut buffer);
        buffer.present().expect("present");
        self.ui.dirty = false;

        // Keep pumping frames only while a transition is actually in-flight —
        // event-driven otherwise (ControlFlow::Wait), never a free-running loop.
        //
        // `request_redraw()` alone isn't reliable here: called from inside the
        // `RedrawRequested` handler it can get coalesced with the in-flight
        // redraw instead of scheduling a genuinely new one, silently stalling
        // an animation partway. Driving the control flow directly guarantees
        // the next tick actually happens.
        if self.transitions.any_active(Instant::now()) {
            event_loop.set_control_flow(ControlFlow::Poll);
            self.request_redraw();
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
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
            _ => {}
        }
        if let Some(v) = new_value {
            self.write_back_value(id, v);
        }
        // Clicking anywhere closes every *other* open dropdown — there's no
        // outside-click-detection system built in, so without this an open
        // dropdown would just sit there floating forever.
        self.close_other_dropdowns(Some(id));
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
            self.write_back_value(id, StateValue::Number((v * 100.0) as f64));
        }
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
        NodeKind::Slider { .. } | NodeKind::ProgressBar { .. } | NodeKind::Container => {}
    }
}

/// Render a `StateValue` for display in a `Text` node — whichever variant
/// it is, without needing the caller to know the field's original type.
fn display_string(value: &StateValue) -> String {
    match value {
        StateValue::Str(s) => s.clone(),
        StateValue::Bool(b) => b.to_string(),
        StateValue::Number(n) => {
            if n.fract() == 0.0 {
                format!("{}", *n as i64)
            } else {
                format!("{n}")
            }
        }
    }
}

impl<S: NowUiState> ApplicationHandler for App<S> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title("NowUI")
            .with_inner_size(winit::dpi::LogicalSize::new(1024.0, 640.0));
        let window = Rc::new(event_loop.create_window(attrs).expect("create window"));

        let context = softbuffer::Context::new(window.clone()).expect("softbuffer context");
        let surface = softbuffer::Surface::new(&context, window.clone()).expect("surface");

        self.window = Some(window);
        self.surface = Some(surface);
        self.ui.dirty = true;
        self.request_redraw();
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

                let hit = self.ui.hit_test(self.cursor);
                if let Some(id) = hit {
                    self.dispatch_event(id, "onMouseMove", EventKind::MouseMove, None);
                }
                if hit != self.hovered {
                    self.hovered = hit;
                    self.ui.dirty = true;
                    self.request_redraw();
                }
            }

            WindowEvent::MouseInput { state, button, .. } => {
                if button != MouseButton::Left {
                    return;
                }
                match state {
                    ElementState::Pressed => {
                        // A floating dropdown popup sits on top of everything
                        // pixel-wise but outside every node's own `computed`
                        // rect, so it's checked before falling back to the
                        // normal rect-based hit test.
                        if let Some(dropdown) = self.find_open_dropdown_popup_at(self.cursor) {
                            self.select_dropdown_option(dropdown, self.cursor);
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
                                    } else {
                                        self.handle_click(hit);
                                    }
                                }
                                None => self.close_other_dropdowns(None),
                            }
                        }
                        self.ui.dirty = true;
                        self.request_redraw();
                    }
                    ElementState::Released => {
                        self.dragging_slider = None;
                        if let Some(id) = self.pressed.take() {
                            self.dispatch_event(id, "onMouseUp", EventKind::MouseUp, None);
                            self.ui.dirty = true;
                            self.request_redraw();
                        }
                    }
                }
            }

            WindowEvent::KeyboardInput { event, .. } => {
                let Some(id) = self.ui.focus else { return };
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

            WindowEvent::RedrawRequested => {
                if self.ui.dirty || self.transitions.any_active(Instant::now()) {
                    self.redraw(event_loop);
                }
            }

            _ => {}
        }
    }
}
