//! NowUI CLI: runs an arbitrary `.nowui` file with no Rust-side state (every
//! `value`/event binding resolves to nothing/no-op — see `nowui_core::NoState`).
//! For a real app with live state and callbacks, depend on `nowui-runtime` as
//! a library and call `nowui_runtime::run(path, entry, my_state)` with your
//! own `#[derive(NowUiState)]` struct instead — see `examples/counter.rs`.
//!
//! Usage:
//!   cargo run -p nowui-runtime -- examples/login.nowui [EntryLayout]
//!
//! Defaults to `examples/login.nowui` and entry layout `App`.

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "examples/login.nowui".to_string());
    let entry = args.next().unwrap_or_else(|| "App".to_string());
    nowui_runtime::run(&path, &entry, nowui_core::NoState)
}
