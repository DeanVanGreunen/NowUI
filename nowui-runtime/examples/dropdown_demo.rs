//! End-to-end demo of `Dropdown`'s real `DropdownItem` children, live
//! `{values: state.path}` binding, `default-selected`/`disabled` (explicit
//! or blank-id-implied), `onSelect` (`Event::child_id`/`child_label`),
//! `Node::select_dropdown_by_id`, and the popup's own 300px max height +
//! scrollbar — see `examples/dropdown_demo.nowui`.
//!
//! Run:  cargo run -p nowui-runtime --example dropdown_demo

use std::process::ExitCode;

use nowui_core::{Event, NowUiState};

#[derive(Default, Clone, NowUiState)]
struct DropdownItem {
    label: String,
    id: String,
}

#[derive(Default, Clone, NowUiState)]
#[nowui(methods(on_select_person, add_person, force_select_bob))]
struct AppState {
    people: Vec<DropdownItem>,
    last_selected_id: String,
    last_selected_label: String,
    next_person: i64,
}

impl AppState {
    fn on_select_person(&mut self, app: &mut AppState, event: &Event) {
        app.last_selected_id = event.child_id.clone().unwrap_or_default();
        app.last_selected_label = event.child_label.clone().unwrap_or_default();
    }

    fn add_person(&mut self, app: &mut AppState, _event: &Event) {
        app.next_person += 1;
        let n = app.next_person;
        app.people.push(DropdownItem { label: format!("Person {n}"), id: format!("person-{n}") });
    }

    /// Demonstrates `Node::select_dropdown_by_id` — a handler can change a
    /// `Dropdown`'s own selection programmatically, not just read it.
    /// `"bob"` is one of the initially-seeded `values`-bound people below,
    /// so it's present from the very first frame.
    fn force_select_bob(&mut self, _app: &mut AppState, event: &mut Event) {
        event.node.select_dropdown_by_id("bob");
    }
}

fn main() -> ExitCode {
    let nowui_file = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/dropdown_demo.nowui");
    // 12 people (plus the 2 pinned static items) comfortably exceeds the
    // popup's 300px max height at the default font size (~33px/row), so
    // the demo starts already showing the scrollbar.
    let people = (1..=12)
        .map(|n| DropdownItem { label: format!("Person {n}"), id: if n == 2 { "bob".to_string() } else { format!("person-{n}") } })
        .collect();
    nowui_runtime::run_path("Dropdown Demo", nowui_file, "App", AppState { people, next_person: 12, ..Default::default() })
}
