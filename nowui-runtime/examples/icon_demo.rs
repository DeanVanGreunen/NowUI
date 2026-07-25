//! End-to-end demo of the `Icon` widget — see `examples/icon_demo.nowui`.
//!
//! Proves: the embedded react-icons library (fa/fa6/md/bs/io5 — see
//! `nowui-icons-gen`) resolves real icon names, `line-color`/`fill-color`/
//! `text-color` recoloring actually changes the rasterized pixels
//! (including a `hover:fill-color-[...]` variant — both a literal hex value
//! and a `${state.path}`-driven one), and `Icon` accepts the same ordinary
//! event bindings (`onClick` here) as every other widget — no
//! `Icon`-specific dispatch code exists in `nowui-runtime`'s `App`, this
//! exercises the fully generic path.
//!
//! Run:  cargo run -p nowui-runtime --example icon_demo

use std::process::ExitCode;

use nowui_core::{Event, NowUiState};

#[derive(Default, Clone, NowUiState)]
#[nowui(methods(increment, toggle_hover_color))]
struct AppState {
    clicks: i64,
    hover_color: String,
}

impl AppState {
    fn increment(&mut self, app: &mut AppState, _event: &Event) {
        app.clicks += 1;
    }

    fn toggle_hover_color(&mut self, app: &mut AppState, _event: &Event) {
        app.hover_color = if app.hover_color == "#dc2626" { "#16a34a".to_string() } else { "#dc2626".to_string() };
    }
}

fn main() -> ExitCode {
    let nowui_file = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/icon_demo.nowui");
    nowui_runtime::run_path(
        "Icon Demo",
        nowui_file,
        "App",
        AppState { clicks: 0, hover_color: "#dc2626".to_string() },
    )
}
