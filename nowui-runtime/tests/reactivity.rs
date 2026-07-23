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

use nowui_core::{Event, EventKind, NowUiState, Point};

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
    fn increment(&mut self, _event: &Event) {
        self.count += 1;
    }

    fn decrement(&mut self, _event: &Event) {
        self.count -= 1;
    }
}

fn click() -> Event {
    Event { kind: EventKind::Click, cursor: Point::default(), key: None }
}

#[test]
fn call_delegates_through_a_nested_state_field() {
    let mut state = AppState::default();

    assert!(state.call(&["counter", "increment"], &click()));
    assert!(state.call(&["counter", "increment"], &click()));
    assert!(state.call(&["counter", "decrement"], &click()));

    assert_eq!(state.counter.count, 1);
}

#[test]
fn call_returns_false_for_an_unknown_path() {
    let mut state = AppState::default();
    assert!(!state.call(&["counter", "nope"], &click()));
    assert!(!state.call(&["nope"], &click()));
}
