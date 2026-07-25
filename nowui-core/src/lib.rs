//! NowUI runtime model: the retained node arena, resolved styles, the layout
//! solver, and the paint walk. Free of both the parser and the renderer.

pub mod arena;
pub mod datetime;
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
    compute_effective, dropdown_metrics, slider_metrics, Align, AnimatableStyle, CursorIcon,
    Direction, Display, GridTrack, Position, Sizing, Style, StyleVariants, TextAlign, Transform2D,
    Transition, DEFAULT_CONTROL_WIDTH, DROPDOWN_POPUP_MAX_H,
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

    fn image_kind(width: u32, height: u32) -> NodeKind {
        NodeKind::Image {
            source: "test.png".to_string(),
            decoded: Some(nowui_image::DecodedImage {
                width,
                height,
                frames: vec![nowui_image::Frame { width, height, rgba: vec![0; (width * height * 4) as usize], delay_ms: 0 }],
            }),
            current_frame: 0,
            frame_elapsed_ms: 0.0,
            error: None,
        }
    }

    /// Wraps `child` in a default-style root container — a layer's own
    /// root always fills the given viewport regardless of its own style
    /// (see `layout::solve`'s doc comment), so a node whose *own* intrinsic
    /// sizing is under test needs to sit one level below the root, same as
    /// every other sizing test in this module.
    fn solved_child_rect(child_kind: NodeKind, child_style: Style, viewport: Size) -> Rect {
        let mut ui = Ui::new();
        let child = ui.push(Node::new(child_kind, child_style));
        let root = ui.push(Node::new(NodeKind::Container, Style::default()));
        ui.get_mut(root).children = vec![child];
        ui.add_layer(root, "main");
        layout::solve(&mut ui, viewport, &mut NullPainter);
        ui.get(child).computed
    }

    #[test]
    fn image_w_auto_scales_from_a_fixed_height_by_the_natural_aspect_ratio() {
        // 400x200 natural (2:1) image, h fixed at 100px -> w should scale to 200px.
        let style = Style { width: Sizing::Hug, height: Sizing::Fixed(100.0), ..Default::default() };
        let rect = solved_child_rect(image_kind(400, 200), style, Size::new(800.0, 600.0));
        assert_eq!(rect.h, 100.0);
        assert_eq!(rect.w, 200.0, "2:1 aspect ratio preserved: 100px tall -> 200px wide");
    }

    #[test]
    fn image_h_auto_scales_from_a_fixed_width_by_the_natural_aspect_ratio() {
        // 400x200 natural (2:1) image, w fixed at 100px -> h should scale to 50px.
        let style = Style { width: Sizing::Fixed(100.0), height: Sizing::Hug, ..Default::default() };
        let rect = solved_child_rect(image_kind(400, 200), style, Size::new(800.0, 600.0));
        assert_eq!(rect.w, 100.0);
        assert_eq!(rect.h, 50.0, "2:1 aspect ratio preserved: 100px wide -> 50px tall");
    }

    #[test]
    fn image_with_both_axes_auto_uses_its_raw_natural_size() {
        let style = Style { width: Sizing::Hug, height: Sizing::Hug, ..Default::default() };
        let rect = solved_child_rect(image_kind(64, 32), style, Size::new(800.0, 600.0));
        assert_eq!((rect.w, rect.h), (64.0, 32.0));
    }

    #[test]
    fn an_undecoded_image_takes_up_no_space() {
        let kind = NodeKind::Image { source: "still-loading.png".to_string(), decoded: None, current_frame: 0, frame_elapsed_ms: 0.0, error: None };
        let style = Style { width: Sizing::Hug, height: Sizing::Hug, ..Default::default() };
        let rect = solved_child_rect(kind, style, Size::new(800.0, 600.0));
        assert_eq!((rect.w, rect.h), (0.0, 0.0));
    }

    #[test]
    fn solve_into_forces_the_root_to_the_given_rect_instead_of_the_full_viewport() {
        let mut ui = Ui::new();
        let child = ui.push(Node::new(NodeKind::Container, Style { width: Sizing::Fill(1.0), height: Sizing::Fill(1.0), ..Default::default() }));
        let root = ui.push(Node::new(NodeKind::Container, Style::default()));
        ui.get_mut(root).children = vec![child];
        ui.add_layer(root, "main");

        layout::solve_into(&mut ui, Rect::new(50.0, 30.0, 200.0, 100.0), &mut NullPainter);

        assert_eq!(ui.get(root).computed, Rect::new(50.0, 30.0, 200.0, 100.0), "root pinned to the given rect, not (0,0)+viewport");
        assert_eq!(ui.get(child).computed, Rect::new(50.0, 30.0, 200.0, 100.0), "a Fill child fills that same rect");
        assert_eq!(ui.viewport, Size::default(), "solve_into leaves Ui::viewport untouched — this Ui doesn't own a window");
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
    fn data_grid_column_width_follows_the_widest_cell_including_the_header() {
        let mut ui = Ui::new();

        // Two header cells: 50px and 60px.
        let h0 = ui.push(Node::new(NodeKind::Container, Style { width: Sizing::Fixed(50.0), height: Sizing::Fixed(20.0), ..Default::default() }));
        let h1 = ui.push(Node::new(NodeKind::Container, Style { width: Sizing::Fixed(60.0), height: Sizing::Fixed(20.0), ..Default::default() }));
        let headers = ui.push(Node::new(NodeKind::Container, Style::default()));
        ui.get_mut(headers).children = vec![h0, h1];

        // Two rows. Row 0, col 0 is 120px wide — wider than its own header
        // (50px) — so the *whole* column 0 (header included) must widen to
        // 120px, not just that one cell's own row.
        let r0c0 = ui.push(Node::new(NodeKind::Container, Style { width: Sizing::Fixed(120.0), height: Sizing::Fixed(10.0), ..Default::default() }));
        let r0c1 = ui.push(Node::new(NodeKind::Container, Style { width: Sizing::Fixed(10.0), height: Sizing::Fixed(10.0), ..Default::default() }));
        let r1c0 = ui.push(Node::new(NodeKind::Container, Style { width: Sizing::Fixed(10.0), height: Sizing::Fixed(10.0), ..Default::default() }));
        let r1c1 = ui.push(Node::new(NodeKind::Container, Style { width: Sizing::Fixed(10.0), height: Sizing::Fixed(10.0), ..Default::default() }));
        let rows = ui.push(Node::new(NodeKind::Container, Style::default()));
        ui.get_mut(rows).children = vec![r0c0, r0c1, r1c0, r1c1];

        let grid = ui.push(Node::new(NodeKind::DataGrid, Style { width: Sizing::Fixed(400.0), ..Default::default() }));
        ui.get_mut(grid).children = vec![headers, rows];
        ui.add_layer(grid, "main");

        layout::solve(&mut ui, Size::new(400.0, 200.0), &mut NullPainter);

        // Column 0 is 120px everywhere: the header, row 0's cell (its own
        // natural width), and row 1's cell (which only ever asked for 10px).
        assert_eq!(ui.get(h0).computed.w, 120.0, "header widened to match the widest cell in its column");
        assert_eq!(ui.get(r0c0).computed.w, 120.0);
        assert_eq!(ui.get(r1c0).computed.w, 120.0, "row 1's cell widened too, even though it never asked for 120px itself");

        // Column 1 stays at its header's own 60px (no cell exceeds it).
        assert_eq!(ui.get(h1).computed.w, 60.0);
        assert_eq!(ui.get(r0c1).computed.w, 60.0);
        assert_eq!(ui.get(r1c1).computed.w, 60.0);

        // Column x-offsets follow column 0's resolved (widened) width.
        assert_eq!(ui.get(h1).computed.x, 120.0);
        assert_eq!(ui.get(r1c1).computed.x, 120.0);

        // Rows start below the header row, stacked in row order.
        assert_eq!(ui.get(h0).computed.y, 0.0);
        assert_eq!(ui.get(r0c0).computed.y, 20.0, "row 0 starts right after the 20px-tall header row");
        assert_eq!(ui.get(r1c0).computed.y, 30.0, "row 1 starts after row 0's own 10px height");
    }

    #[test]
    fn tree_view_item_indents_children_and_excludes_a_collapsed_subtree_from_layout() {
        let mut ui = Ui::new();

        let leaf = ui.push(Node::new(
            NodeKind::TreeViewItem { id: "leaf".to_string(), label: "Leaf".to_string(), collapsed: false, selected: false, checkbox: false, show_folder_actions: false, icon: None },
            Style::default(),
        ));
        let child = ui.push(Node::new(
            NodeKind::TreeViewItem { id: "child".to_string(), label: "Child".to_string(), collapsed: false, selected: false, checkbox: false, show_folder_actions: false, icon: None },
            Style::default(),
        ));
        ui.get_mut(child).children = vec![leaf];

        let parent = ui.push(Node::new(
            NodeKind::TreeViewItem { id: "parent".to_string(), label: "Parent".to_string(), collapsed: false, selected: false, checkbox: false, show_folder_actions: false, icon: None },
            Style::default(),
        ));
        ui.get_mut(parent).children = vec![child];

        let tree = ui.push(Node::new(NodeKind::TreeView { has_checkbox_selection: false, can_select_multiple: false }, Style { width: Sizing::Fixed(300.0), ..Default::default() }));
        ui.get_mut(tree).children = vec![parent];
        ui.add_layer(tree, "main");

        layout::solve(&mut ui, Size::new(300.0, 300.0), &mut NullPainter);

        let parent_rect = ui.get(parent).computed;
        let child_rect = ui.get(child).computed;
        let leaf_rect = ui.get(leaf).computed;

        assert_eq!(parent_rect.x, 0.0);
        assert!(child_rect.x > parent_rect.x, "child indents right of its parent");
        assert_eq!(child_rect.x - parent_rect.x, leaf_rect.x - child_rect.x, "every nesting level indents by the same amount");
        assert!(child_rect.y > parent_rect.y, "child stacks below its parent's own row");
        assert!(leaf_rect.y > child_rect.y, "leaf stacks below its parent's own row too");

        // Now collapse the middle node and re-solve: `leaf` must be excluded
        // from layout entirely — parent's own height shrinks back to just
        // its own row, since `child`'s whole (indented) subtree disappears.
        let NodeKind::TreeViewItem { collapsed, .. } = &mut ui.get_mut(child).kind else { panic!() };
        *collapsed = true;
        let expanded_parent_h = parent_rect.h;
        layout::solve(&mut ui, Size::new(300.0, 300.0), &mut NullPainter);
        let collapsed_parent_h = ui.get(parent).computed.h;
        assert!(collapsed_parent_h < expanded_parent_h, "collapsing a middle node shrinks its ancestor's own height");
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
    fn absolute_child_skips_an_unpositioned_parent_to_reach_the_nearest_positioned_ancestor() {
        // root (position-relative, padding 10)
        //   `-- middle (plain Container, NOT positioned, padding 20)
        //         `-- absolute (position-absolute, top/right: 5)
        //
        // Real CSS: `absolute`'s containing block is `root`'s content box
        // (the nearest positioned ancestor), not `middle`'s — `middle` is
        // just a plain box in between and must be skipped over.
        let mut ui = Ui::new();
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
        let middle = ui.push(Node::new(
            NodeKind::Container,
            Style { padding: Edges::all(20.0), width: Sizing::Fill(1.0), height: Sizing::Fill(1.0), ..Default::default() },
        ));
        ui.get_mut(middle).children = vec![absolute];
        let root = ui.push(Node::new(
            NodeKind::Container,
            Style { position: Position::Relative, padding: Edges::all(10.0), ..Default::default() },
        ));
        ui.get_mut(root).children = vec![middle];
        ui.add_layer(root, "main");

        layout::solve(&mut ui, Size::new(200.0, 100.0), &mut NullPainter);

        // Root's content box (after its own 10px padding): x in [10, 190], y in [10, 90].
        // Positioned via top/right against THAT box, not `middle`'s (which
        // would additionally subtract its own 20px padding).
        assert_eq!(ui.get(absolute).computed, Rect::new(190.0 - 5.0 - 30.0, 10.0 + 5.0, 30.0, 10.0));
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

    #[test]
    fn gc_frees_an_orphaned_subtree_but_leaves_the_live_tree_untouched() {
        let mut ui = Ui::new();
        // A small live tree: root -> child -> grandchild.
        let grandchild = ui.push(Node::new(NodeKind::Text { content: "gc".to_string() }, Style::default()));
        let child = ui.push(Node::new(NodeKind::Text { content: "live".to_string() }, Style::default()));
        ui.get_mut(child).children = vec![grandchild];
        let root = ui.push(Node::new(NodeKind::Container, Style::default()));
        ui.get_mut(root).children = vec![child];
        ui.add_layer(root, "main");

        // An orphaned subtree — never referenced by anything reachable from
        // `root` (simulating what a `for`/`if` region rebuild leaves behind
        // once it splices in a fresh replacement without freeing the old one).
        let orphan_child = ui.push(Node::new(NodeKind::Text { content: "orphaned leaf".to_string() }, Style::default()));
        let orphan_root = ui.push(Node::new(NodeKind::Text { content: "orphaned root".to_string() }, Style::default()));
        ui.get_mut(orphan_root).children = vec![orphan_child];

        ui.gc();

        let NodeKind::Text { content } = &ui.get(child).kind else { panic!("live child must survive gc untouched") };
        assert_eq!(content, "live");
        let NodeKind::Text { content } = &ui.get(grandchild).kind else { panic!("live grandchild must survive gc untouched") };
        assert_eq!(content, "gc");

        assert_eq!(ui.get(orphan_root).kind, NodeKind::Container, "the orphaned subtree's root is swept to an empty tombstone");
        assert!(ui.get(orphan_root).children.is_empty());
        assert_eq!(ui.get(orphan_child).kind, NodeKind::Container, "the orphan's own child is swept too");
    }

    #[test]
    fn gc_keeps_the_currently_focused_node_alive_even_if_nothing_else_points_at_it() {
        let mut ui = Ui::new();
        let detached_but_focused = ui.push(Node::new(NodeKind::TextInput {
            label: "still editing".to_string(),
            placeholder: String::new(),
            masked: false,
            cursor: 0,
            selection_anchor: None,
            ime_preview: String::new(),
            highlight_spans: Vec::new(),
        }, Style::default()));
        let root = ui.push(Node::new(NodeKind::Container, Style::default()));
        ui.add_layer(root, "main");
        ui.focus = Some(detached_but_focused);

        ui.gc();

        let NodeKind::TextInput { label, .. } = &ui.get(detached_but_focused).kind else { panic!("focus must survive gc") };
        assert_eq!(label, "still editing");
    }

    #[test]
    fn gc_never_reuses_a_swept_nodes_id_for_a_later_push() {
        let mut ui = Ui::new();
        let orphan = ui.push(Node::new(NodeKind::Text { content: "gone".to_string() }, Style::default()));
        let root = ui.push(Node::new(NodeKind::Container, Style::default()));
        ui.add_layer(root, "main");

        ui.gc();
        let new_id = ui.push(Node::new(NodeKind::Text { content: "new".to_string() }, Style::default()));

        assert_ne!(orphan, new_id, "a fresh push never lands on a swept slot — every NodeId stays permanently distinct");
    }
}
