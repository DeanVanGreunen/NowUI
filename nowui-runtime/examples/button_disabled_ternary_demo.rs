//! End-to-end demo of `Button`'s `disabled:` style variant (+ `{disabled:
//! state.path}` binding) and a ternary `${cond ? "a" : "b"}` template
//! interpolation — see `examples/button_disabled_ternary_demo.nowui`.
//!
//! Clicking "Save" flips `is_saving` true, which simultaneously: disables
//! the button (`{disabled: state.is_saving}`, blocking further clicks —
//! `onClick` no longer dispatches once disabled), shows its
//! `disabled:text-[#FF0000] disabled:bg-[#FFFF00]` styling, and switches
//! its own label via `${state.is_saving == true ? "Saving..." : "Save"}`.
//! There's no built-in timer to flip it back — this widget-level demo is
//! only about proving `disabled:`/the ternary actually react live to state,
//! not modeling a real save flow.
//!
//! Run:  cargo run -p nowui-runtime --example button_disabled_ternary_demo

use std::process::ExitCode;

use nowui_core::{Event, NowUiState};

#[derive(Default, Clone, NowUiState)]
#[nowui(methods(save))]
struct AppState {
    is_saving: bool,
}

impl AppState {
    fn save(&mut self, app: &mut AppState, _event: &Event) {
        app.is_saving = true;
    }
}

fn main() -> ExitCode {
    let nowui_file = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/button_disabled_ternary_demo.nowui");
    nowui_runtime::run_path("Button disabled: + ternary demo", nowui_file, "App", AppState::default())
}
