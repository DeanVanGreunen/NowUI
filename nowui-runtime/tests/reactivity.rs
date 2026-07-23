//! Regression test for a real bug: `#[derive(NowUiState)]`'s generated
//! `call()` only matched names listed in the struct's own `#[nowui(methods(
//! ...))]` attribute, with no fallback for nested `NowUiState` fields (unlike
//! `get`/`set`, which always delegated). A `{onClick: state.counter.increment}`
//! binding, where `increment` lives on a *nested* `Counter` field rather than
//! directly on the top-level app state, silently no-op'd — `call` returned
//! `false` for every path whose first segment named a field instead of a
//! locally-declared method. Fixed in `nowui-macros` by also emitting a
//! delegating `call` arm for every non-scalar field, exactly parallel to
//! `get`/`set`.

use nowui_core::{Event, EventKind, Node, NodeKind, NowUiState, Point, Style};

#[derive(Default, Clone, NowUiState)]
struct AppState {
    counter: Counter,
}

#[derive(Default, Clone, NowUiState)]
#[nowui(methods(increment, decrement))]
struct Counter {
    count: i64,
}

impl Counter {
    fn increment(&mut self, _event: &mut Event) {
        self.count += 1;
    }

    fn decrement(&mut self, _event: &mut Event) {
        self.count -= 1;
    }
}

fn click(node: &mut Node) -> Event<'_> {
    Event { kind: EventKind::Click, cursor: Point::default(), key: None, node }
}

#[test]
fn call_delegates_through_a_nested_state_field() {
    let mut state = AppState::default();
    let mut node = Node::new(NodeKind::Container, Style::default());

    assert!(state.call(&["counter", "increment"], &mut click(&mut node)));
    assert!(state.call(&["counter", "increment"], &mut click(&mut node)));
    assert!(state.call(&["counter", "decrement"], &mut click(&mut node)));

    assert_eq!(state.counter.count, 1);
}

#[test]
fn call_returns_false_for_an_unknown_path() {
    let mut state = AppState::default();
    let mut node = Node::new(NodeKind::Container, Style::default());
    assert!(!state.call(&["counter", "nope"], &mut click(&mut node)));
    assert!(!state.call(&["nope"], &mut click(&mut node)));
}

#[derive(Default, Clone, NowUiState)]
#[nowui(methods(handle_me))]
struct Row {
    label: String,
}

impl Row {
    fn handle_me(&mut self, event: &mut Event) {
        event.node.style.opacity = 0.0;
    }
}

#[derive(Default, Clone, NowUiState)]
struct RowsState {
    rows: Vec<Row>,
}

#[test]
fn call_dispatches_into_a_vec_item_by_index() {
    // The path shape `nowui-runtime`'s `dynamic::substitute_loop_var`
    // rewrites a `for row in state.rows { ... {onClick: row.handle_me} }`
    // binding onto: the field name, then the item's numeric index, then the
    // rest of the path delegated into that one `Row`'s own `call`.
    let mut state = RowsState { rows: vec![Row::default(), Row::default()] };
    let mut node = Node::new(NodeKind::Container, Style::default());

    assert!(state.call(&["rows", "1", "handle_me"], &mut click(&mut node)));
    assert_eq!(node.style.opacity, 0.0);

    // Out of range, or a scalar-index segment that isn't even a number —
    // both fail closed rather than panicking.
    let mut node2 = Node::new(NodeKind::Container, Style::default());
    assert!(!state.call(&["rows", "5", "handle_me"], &mut click(&mut node2)));
    assert!(!state.call(&["rows", "nope", "handle_me"], &mut click(&mut node2)));
}

#[test]
fn handler_can_mutate_the_originating_node() {
    // The event carries a live handle to the node it fired on — a handler
    // can reach into its style/kind directly, not just `self`.
    #[derive(Default, Clone, NowUiState)]
    #[nowui(methods(hide))]
    struct Hider {
        _unused: bool,
    }
    impl Hider {
        fn hide(&mut self, event: &mut Event) {
            event.node.style.opacity = 0.0;
        }
    }

    let mut state = Hider::default();
    let mut node = Node::new(NodeKind::Container, Style::default());
    assert_ne!(node.style.opacity, 0.0);
    assert!(state.call(&["hide"], &mut click(&mut node)));
    assert_eq!(node.style.opacity, 0.0);
}
