//! The winit application harness: window + softbuffer surface, event-driven
//! redraw (ControlFlow::Wait), and the solve -> paint -> present cycle guarded
//! by a dirty flag.
//!
//! winit API note: this targets winit 0.30 (`ApplicationHandler` + `run_app`,
//! `resumed(&ActiveEventLoop)`). These names were introduced in 0.30 and do not
//! exist on 0.29 or earlier. If a future winit reshapes these callbacks
//! (e.g. `can_create_surfaces`, `&dyn ActiveEventLoop`), align the method
//! signatures with the docs for that version — the logic is unchanged.

use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::Instant;

use nowui_core::{compute_effective, dropdown_metrics, AnimatableStyle, Color, NodeId, NodeKind, Point, Rect, Size, Ui};
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

pub struct App {
    ui: Ui,
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
    transitions: Transitions,
}

impl App {
    pub fn new(ui: Ui) -> Self {
        App {
            ui,
            window: None,
            surface: None,
            cursor: Point::default(),
            text: TextContext::new(),
            hovered: None,
            pressed: None,
            transitions: Transitions::new(),
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
    /// self-contained state that a click can drive without an external
    /// callback/state-binding system (that's step 6 of the roadmap;
    /// `onClick` bindings are still just parsed and stored, not invoked).
    /// Selecting an *option* from an open dropdown is handled separately by
    /// `select_dropdown_option`, since the option list is a floating popup
    /// that lives outside the node's own `computed` rect (see paint.rs) and
    /// so isn't reachable through the normal rect-based `hit_test`.
    fn handle_click(&mut self, id: NodeId) {
        match &mut self.ui.get_mut(id).kind {
            NodeKind::Checkbox { checked, .. } => *checked = !*checked,
            NodeKind::Dropdown { open, .. } => *open = !*open,
            _ => {}
        }
        // Clicking anywhere closes every *other* open dropdown — there's no
        // outside-click-detection system built in, so without this an open
        // dropdown would just sit there floating forever.
        self.close_other_dropdowns(Some(id));
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
        if let NodeKind::Dropdown { options, selected, open, .. } = &mut node.kind {
            let idx = (local_y / option_h).max(0.0) as usize;
            if idx < options.len() {
                *selected = Some(idx);
            }
            *open = false;
        }
        self.close_other_dropdowns(Some(id));
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

impl ApplicationHandler for App {
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
                self.ui.dirty = true;
                self.request_redraw();
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Point::new(position.x as f32, position.y as f32);
                let hit = self.ui.hit_test(self.cursor);
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
                                    self.handle_click(hit);
                                }
                                None => self.close_other_dropdowns(None),
                            }
                        }
                        self.ui.dirty = true;
                        self.request_redraw();
                    }
                    ElementState::Released => {
                        if self.pressed.is_some() {
                            self.pressed = None;
                            self.ui.dirty = true;
                            self.request_redraw();
                        }
                    }
                }
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