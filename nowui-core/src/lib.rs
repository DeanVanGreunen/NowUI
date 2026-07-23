//! NowUI runtime model: the retained node arena, resolved styles, the layout
//! solver, and the paint walk. Free of both the parser and the renderer.

pub mod arena;
pub mod geometry;
pub mod layout;
pub mod paint;
pub mod painter;
pub mod state;
pub mod style;
pub mod tailwind;
pub mod text_input;

pub use arena::{Layer, Node, NodeId, NodeKind, Template, TemplatePart, Ui, EVENT_BINDING_KEYS};
pub use geometry::{Color, Edges, Point, Rect, Size};
// `nowui_macros::NowUiState` (a derive macro) and `state::NowUiState` (the
// trait) share a name but live in separate namespaces (macro vs. type), so
// this re-export is not a conflict — `#[derive(nowui_core::NowUiState)]` and
// `impl nowui_core::NowUiState for Foo` both resolve unambiguously.
pub use nowui_macros::NowUiState;
pub use painter::{Painter, TextStyle};
pub use state::{display_string, Event, EventKind, NoState, NowUiState, StateValue};
pub use style::{
    compute_effective, dropdown_metrics, slider_metrics, Align, AnimatableStyle, Direction,
    Display, GridTrack, Position, Sizing, Style, StyleVariants, TextAlign, Transform2D,
    Transition, DEFAULT_CONTROL_WIDTH,
};
pub use tailwind::Easing;

#[cfg(test)]
mod tests {
    use super::*;

    /// A painter that records nothing but supports measurement — lets us test
    /// the solver with no renderer.
    struct NullPainter;
    impl Painter for NullPainter {
        fn fill_rect(&mut self, _: Rect, _: Color, _: Edges) {}
        fn stroke_rect(&mut self, _: Rect, _: Color, _: f32, _: Edges) {}
        fn draw_text(&mut self, _: &str, _: Rect, _: &TextStyle) {}
        fn push_clip(&mut self, _: Rect) {}
        fn pop_clip(&mut self) {}
    }

    #[test]
    fn column_stacks_children_vertically() {
        let mut ui = Ui::new();

        let a = ui.push(Node::new(
            NodeKind::Container,
            Style { height: Sizing::Fixed(30.0), width: Sizing::Fill(1.0), ..Default::default() },
        ));
        let b = ui.push(Node::new(
            NodeKind::Container,
            Style { height: Sizing::Fixed(50.0), width: Sizing::Fill(1.0), ..Default::default() },
        ));
        let root = ui.push(Node::new(
            NodeKind::Container,
            Style { direction: Direction::Column, ..Default::default() },
        ));
        ui.get_mut(root).children = vec![a, b];
        ui.add_layer(root, "main");

        layout::solve(&mut ui, Size::new(200.0, 200.0), &mut NullPainter);

        let ra = ui.get(a).computed;
        let rb = ui.get(b).computed;
        assert_eq!(ra.y, 0.0);
        assert_eq!(ra.h, 30.0);
        assert_eq!(rb.y, 30.0, "second child stacks below the first");
        assert_eq!(rb.h, 50.0);
    }

    #[test]
    fn fill_child_expands_to_viewport() {
        let mut ui = Ui::new();
        let child = ui.push(Node::new(
            NodeKind::Container,
            Style { width: Sizing::Fill(1.0), height: Sizing::Fill(1.0), ..Default::default() },
        ));
        let root = ui.push(Node::new(NodeKind::Container, Style::default()));
        ui.get_mut(root).children = vec![child];
        ui.add_layer(root, "main");

        layout::solve(&mut ui, Size::new(300.0, 400.0), &mut NullPainter);

        let rc = ui.get(child).computed;
        assert_eq!(rc.w, 300.0);
        assert_eq!(rc.h, 400.0);
    }

    #[test]
    fn hex_color_parses() {
        assert_eq!(Color::from_hex("#2680d4"), Some(Color::rgb(0x26, 0x80, 0xd4)));
        assert_eq!(Color::from_hex("#fff"), Some(Color::WHITE));
        assert_eq!(Color::from_hex("nope"), None);
    }

    #[test]
    fn percent_sizing_resolves_against_parent() {
        let mut ui = Ui::new();
        let child = ui.push(Node::new(
            NodeKind::Container,
            Style { width: Sizing::Percent(0.5), height: Sizing::Fixed(10.0), ..Default::default() },
        ));
        let root = ui.push(Node::new(NodeKind::Container, Style::default()));
        ui.get_mut(root).children = vec![child];
        ui.add_layer(root, "main");

        layout::solve(&mut ui, Size::new(300.0, 400.0), &mut NullPainter);

        assert_eq!(ui.get(child).computed.w, 150.0);
    }

    #[test]
    fn row_reverse_places_children_from_the_end() {
        let mut ui = Ui::new();
        let a = ui.push(Node::new(
            NodeKind::Container,
            Style { width: Sizing::Fixed(30.0), height: Sizing::Fixed(10.0), ..Default::default() },
        ));
        let b = ui.push(Node::new(
            NodeKind::Container,
            Style { width: Sizing::Fixed(50.0), height: Sizing::Fixed(10.0), ..Default::default() },
        ));
        let root = ui.push(Node::new(
            NodeKind::Container,
            Style { direction: Direction::RowReverse, ..Default::default() },
        ));
        ui.get_mut(root).children = vec![a, b];
        ui.add_layer(root, "main");

        layout::solve(&mut ui, Size::new(200.0, 200.0), &mut NullPainter);

        // `a` is first in source order but RowReverse lays out from the right.
        assert_eq!(ui.get(b).computed.x, 0.0, "b (second child) starts at the left edge");
        assert_eq!(ui.get(a).computed.x, 50.0, "a (first child) follows b, laid out in reverse");
    }

    #[test]
    fn grid_places_children_into_tracks() {
        let mut ui = Ui::new();
        let cells: Vec<_> = (0..4)
            .map(|_| ui.push(Node::new(NodeKind::Container, Style::default())))
            .collect();
        let root = ui.push(Node::new(
            NodeKind::Container,
            Style {
                display: Display::Grid,
                grid_template_columns: vec![GridTrack::Fr(1.0), GridTrack::Fr(1.0)],
                grid_template_rows: vec![GridTrack::Fr(1.0), GridTrack::Fr(1.0)],
                width: Sizing::Fixed(200.0),
                height: Sizing::Fixed(100.0),
                ..Default::default()
            },
        ));
        ui.get_mut(root).children = cells.clone();
        ui.add_layer(root, "main");

        layout::solve(&mut ui, Size::new(200.0, 100.0), &mut NullPainter);

        // 2x2 grid of a 200x100 box: each cell is 100x50.
        assert_eq!(ui.get(cells[0]).computed, Rect::new(0.0, 0.0, 100.0, 50.0));
        assert_eq!(ui.get(cells[1]).computed, Rect::new(100.0, 0.0, 100.0, 50.0));
        assert_eq!(ui.get(cells[2]).computed, Rect::new(0.0, 50.0, 100.0, 50.0));
        assert_eq!(ui.get(cells[3]).computed, Rect::new(100.0, 50.0, 100.0, 50.0));
    }

    #[test]
    fn absolute_child_is_out_of_flow_and_positioned_by_offsets() {
        let mut ui = Ui::new();
        let normal = ui.push(Node::new(
            NodeKind::Container,
            Style { height: Sizing::Fixed(20.0), width: Sizing::Fill(1.0), ..Default::default() },
        ));
        let absolute = ui.push(Node::new(
            NodeKind::Container,
            Style {
                position: Position::Absolute,
                width: Sizing::Fixed(30.0),
                height: Sizing::Fixed(10.0),
                top: Some(5.0),
                right: Some(5.0),
                ..Default::default()
            },
        ));
        let root = ui.push(Node::new(NodeKind::Container, Style::default()));
        ui.get_mut(root).children = vec![normal, absolute];
        ui.add_layer(root, "main");

        layout::solve(&mut ui, Size::new(200.0, 100.0), &mut NullPainter);

        // The absolute child doesn't push `normal` down or consume flow space.
        assert_eq!(ui.get(normal).computed, Rect::new(0.0, 0.0, 200.0, 20.0));
        // Positioned via top/right against the root's content box.
        assert_eq!(ui.get(absolute).computed, Rect::new(200.0 - 5.0 - 30.0, 5.0, 30.0, 10.0));
    }

    #[test]
    fn scroll_offset_shifts_children_and_clamps_via_content_size() {
        let mut ui = Ui::new();
        let a = ui.push(Node::new(
            NodeKind::Container,
            Style { height: Sizing::Fixed(50.0), width: Sizing::Fill(1.0), ..Default::default() },
        ));
        let b = ui.push(Node::new(
            NodeKind::Container,
            Style { height: Sizing::Fixed(50.0), width: Sizing::Fill(1.0), ..Default::default() },
        ));
        let root = ui.push(Node::new(
            NodeKind::Container,
            Style { scroll_y: true, height: Sizing::Fixed(60.0), width: Sizing::Fill(1.0), ..Default::default() },
        ));
        ui.get_mut(root).children = vec![a, b];
        ui.get_mut(root).scroll_offset = Point::new(0.0, 20.0);
        ui.add_layer(root, "main");

        layout::solve(&mut ui, Size::new(100.0, 60.0), &mut NullPainter);

        assert_eq!(ui.get(root).content_size, Size::new(100.0, 100.0));
        assert_eq!(ui.get(a).computed.y, -20.0, "scrolled up by the offset");
        assert_eq!(ui.get(b).computed.y, 30.0);
    }

    #[test]
    fn closed_menu_ignores_its_children_entirely() {
        let mut ui = Ui::new();
        let item = ui.push(Node::new(
            NodeKind::MenuItem { label: "Open Preferences".to_string() },
            Style { height: Sizing::Fixed(40.0), width: Sizing::Fill(1.0), ..Default::default() },
        ));
        let menu = ui.push(Node::new(NodeKind::Menu { label: "Preferences".to_string(), open: false }, Style::default()));
        ui.get_mut(menu).children = vec![item];
        // `Menu` as the layer *root* would always fill the viewport
        // (`solve`'s special-casing for roots), masking its own Hug height
        // — nest it under a plain wrapper, like every other Hug-sizing test
        // here, so its measured height is actually observable.
        let root = ui.push(Node::new(NodeKind::Container, Style::default()));
        ui.get_mut(root).children = vec![menu];
        ui.add_layer(root, "main");

        layout::solve(&mut ui, Size::new(200.0, 100.0), &mut NullPainter);

        // Hug height is just the header label's own text height — the
        // 40px-tall item contributes nothing while closed.
        let header_h = ui.get(menu).computed.h;
        assert!(header_h < 40.0, "closed Menu's height ({header_h}) must not include its item's 40px");
    }

    #[test]
    fn open_menu_never_grows_its_own_size_from_children() {
        // Unlike an accordion, an open Menu's own box never changes size —
        // its children float in a popup below it instead (same principle as
        // Dropdown's open option list never affecting its own box size).
        let mut ui = Ui::new();
        let item = ui.push(Node::new(
            NodeKind::MenuItem { label: "Open Preferences".to_string() },
            Style { height: Sizing::Fixed(40.0), width: Sizing::Fill(1.0), ..Default::default() },
        ));
        let closed_menu =
            ui.push(Node::new(NodeKind::Menu { label: "Preferences".to_string(), open: false }, Style::default()));
        let open_menu = ui.push(Node::new(NodeKind::Menu { label: "Preferences".to_string(), open: true }, Style::default()));
        ui.get_mut(open_menu).children = vec![item];
        let root = ui.push(Node::new(NodeKind::Container, Style::default()));
        ui.get_mut(root).children = vec![closed_menu, open_menu];
        ui.add_layer(root, "main");

        layout::solve(&mut ui, Size::new(200.0, 100.0), &mut NullPainter);

        assert_eq!(
            ui.get(closed_menu).computed.h,
            ui.get(open_menu).computed.h,
            "open or closed, a Menu's own height is just its header text — never its children's"
        );
    }

    #[test]
    fn open_menu_popup_positions_its_children_floating_below_the_header() {
        let mut ui = Ui::new();
        let item = ui.push(Node::new(
            NodeKind::MenuItem { label: "Open Preferences".to_string() },
            Style { height: Sizing::Fixed(40.0), width: Sizing::Fill(1.0), ..Default::default() },
        ));
        let menu = ui.push(Node::new(NodeKind::Menu { label: "Preferences".to_string(), open: true }, Style::default()));
        ui.get_mut(menu).children = vec![item];
        let root = ui.push(Node::new(NodeKind::Container, Style::default()));
        ui.get_mut(root).children = vec![menu];
        ui.add_layer(root, "main");

        layout::solve(&mut ui, Size::new(200.0, 100.0), &mut NullPainter);

        let menu_rect = ui.get(menu).computed;
        let item_rect = ui.get(item).computed;
        assert_eq!(item_rect.y, menu_rect.y + menu_rect.h, "item floats directly below the header, not inside it");
        assert_eq!(item_rect.h, 40.0);
        assert_eq!(ui.get(menu).content_size, Size::new(menu_rect.w, 40.0), "popup size recorded for paint/hit-testing");
    }

    #[test]
    fn closed_or_childless_menu_gets_no_popup_size() {
        let mut ui = Ui::new();
        let closed_menu =
            ui.push(Node::new(NodeKind::Menu { label: "Preferences".to_string(), open: false }, Style::default()));
        let no_children_menu =
            ui.push(Node::new(NodeKind::Menu { label: "Preferences".to_string(), open: true }, Style::default()));
        let root = ui.push(Node::new(NodeKind::Container, Style::default()));
        ui.get_mut(root).children = vec![closed_menu, no_children_menu];
        ui.add_layer(root, "main");

        layout::solve(&mut ui, Size::new(200.0, 100.0), &mut NullPainter);

        assert_eq!(ui.get(closed_menu).content_size, Size::default());
        assert_eq!(ui.get(no_children_menu).content_size, Size::default());
    }
}
