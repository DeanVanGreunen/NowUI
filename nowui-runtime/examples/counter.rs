//! End-to-end reactivity example: a real `#[derive(NowUiState)]` app-state
//! struct backing `examples/counter.nowui`'s `{value: state.counter.count}`
//! and `{onClick: state.counter.increment}` bindings.
//!
//! Run:  cargo run -p nowui-runtime --example counter

use std::process::ExitCode;

use nowui_core::{Event, NowUiState};

#[derive(Default, Clone, NowUiState)]
struct AppState {
    counter: Counter,
}

// Callable methods aren't discovered from the `impl Counter` block below —
// derive macros never see it — so they're listed explicitly here.
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

fn main() -> ExitCode {
    let nowui_file = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/counter.nowui");
    nowui_runtime::run_path(nowui_file, "App", AppState::default())
}
