//! `nowui-runtime` as a library: loads a `.nowui` file, resolves `#` imports,
//! builds the retained UI tree, and runs the winit app loop against a live
//! app-state object.
//!
//! Two ways to use this:
//!   * the `nowui` CLI binary (`main.rs`) — arbitrary `.nowui` files, no Rust
//!     state, via `run(path, entry, NoState)`;
//!   * your own binary, with a `#[derive(nowui_core::NowUiState)]` struct —
//!     call `nowui_runtime::run(path, entry, my_state)` directly. See
//!     `examples/counter.rs` for a full end-to-end example.

pub mod app;
pub mod loader;
pub mod semantic;
pub mod transitions;

use std::path::Path;
use std::process::ExitCode;

use nowui_core::NowUiState;
use winit::event_loop::{ControlFlow, EventLoop};

pub use app::App;

/// Load `path`, build the `entry` layout, and run the winit event loop
/// against `state` until the window closes. `state`'s `NowUiState` impl
/// (usually via `#[derive(NowUiState)]`) is what `{value: state.foo.bar}`
/// and `{onClick: state.foo.bar}`-style bindings in the `.nowui` file
/// resolve against and dispatch to each frame — see CLAUDE.md's
/// "Reactivity" section for the full read/write data flow.
pub fn run<S: NowUiState + 'static>(path: &str, entry: &str, state: S) -> ExitCode {
    let ast = match loader::load_and_resolve(Path::new(path)) {
        Ok(ast) => ast,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut sem = semantic::Semantic::new(&ast);
    let ui = match sem.build(entry) {
        Some(ui) => ui,
        None => {
            eprintln!("error: entry layout `{entry}` not found in `{path}`");
            eprintln!("available layouts: {}", available(&ast).join(", "));
            return ExitCode::FAILURE;
        }
    };

    for w in &sem.warnings {
        eprintln!("warning: {w}");
    }

    // Event-driven redraw: sleep between events, render on demand. Kept as
    // winit's own `run_app` loop, not a user-owned poll loop — see CLAUDE.md
    // for why (this is a deliberate, discussed decision, not an oversight).
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App::new(ui, state);
    match event_loop.run_app(&mut app) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("event loop error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn available(ast: &[nowui_syntax::ast::Node]) -> Vec<String> {
    ast.iter()
        .filter_map(|n| match n {
            nowui_syntax::ast::Node::LayoutDef { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect()
}
