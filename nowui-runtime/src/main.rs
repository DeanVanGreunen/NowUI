//! NowUI CLI: runs an arbitrary `.nowui` file with no Rust-side state (every
//! `value`/event binding resolves to nothing/no-op — see `nowui_core::NoState`).
//! For a real app with live state and callbacks, depend on `nowui-runtime` as
//! a library and either call `nowui_runtime::run_path(window_title, path, entry, my_state)`
//! (loads from disk at runtime — see `nowui-runtime/examples/counter.rs`) or
//! bundle the `.nowui` file into the binary with `#[nowui(view("/path.nowui"))]`
//! and call `nowui_runtime::run(window_title, entry, my_state)` instead (see
//! `examples/counter-app/src/main.rs`).
//!
//! Usage:
//!   cargo run -p nowui-runtime -- examples/counter-app/src/login.nowui [EntryLayout]
//!
//! Defaults to `examples/counter-app/src/login.nowui` and entry layout `App`.

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "examples/counter-app/src/login.nowui".to_string());
    let entry = args.next().unwrap_or_else(|| "App".to_string());
    nowui_runtime::run_path("NowUI", &path, &entry, nowui_core::NoState)
}
