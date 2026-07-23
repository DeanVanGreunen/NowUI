//! Standalone reactivity example, shaped after the original App/Counter
//! design sketch:
//!
//! ```ignore
//! #[NowUI(instance)]
//! pub struct App { counter: Counter }
//! #[NowUI(Widget)]
//! pub struct Counter { counter: i64 }
//! impl Counter {
//!     pub fn increament(mut self, e: Event) { self.counter += 1; }
//! }
//! ```
//!
//! Two deliberate deviations from that sketch, both load-bearing:
//!   * `Clone`, not `Copy` — a `Copy` app-state struct can't hold a `String`
//!     field (e.g. a future `username`), so `Clone` is the derive that scales.
//!   * `fn increment(&mut self, event: &Event)`, not `fn increment(mut self,
//!     event: Event)` — by-value `self` can't persist a mutation back into
//!     the live app state; `App`'s `resolve_values`/`dispatch_event` need a
//!     `&mut` handle each frame, which only `&mut self` gives them.
//!
//! `#[NowUI(instance)]`/`#[NowUI(Widget)]` become one derive,
//! `#[derive(nowui_core::NowUiState)]`, for both: a plain Rust derive macro
//! can't see a struct's separate `impl` block, so callable methods are
//! listed explicitly via `#[nowui(methods(...))]` rather than discovered.
//!
//! Run:  cargo run -p nowui-counter-app

use std::process::ExitCode;

use nowui_core::{Event, NowUiState};

#[derive(Default, Clone, NowUiState)]
pub struct App {
    counter: Counter,
}

#[derive(Default, Clone, NowUiState)]
#[nowui(methods(increment, decrement))]
pub struct Counter {
    counter: f64,
}

impl Counter {
    pub fn increment(&mut self, _event: &Event) {
        self.counter *= 10.0;
    }

    pub fn decrement(&mut self, _event: &Event) {
        self.counter /= 10.0;
    }
}

fn main() -> ExitCode {
    let nowui_file = concat!(env!("CARGO_MANIFEST_DIR"), "/counter.nowui");
    nowui_runtime::run(nowui_file, "App", App { 
        counter: Counter { 
            counter: 1.0
        }
    })
}
