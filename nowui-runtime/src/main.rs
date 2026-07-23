//! NowUI runtime entry point.
//!
//! Usage:
//!   cargo run -p nowui-runtime -- examples/login.nowui [EntryLayout]
//!
//! Defaults to `examples/login.nowui` and entry layout `App`.

mod app;
mod loader;
mod semantic;
mod transitions;

use std::path::Path;
use std::process::ExitCode;

use app::App;
use semantic::Semantic;
use winit::event_loop::{ControlFlow, EventLoop};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "examples/login.nowui".to_string());
    let entry = args.next().unwrap_or_else(|| "App".to_string());

    let ast = match loader::load_and_resolve(Path::new(&path)) {
        Ok(ast) => ast,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut sem = Semantic::new(&ast);
    let ui = match sem.build(&entry) {
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

    // Event-driven redraw: sleep between events, render on demand.
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App::new(ui);
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
