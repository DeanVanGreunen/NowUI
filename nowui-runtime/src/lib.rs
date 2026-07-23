//! `nowui-runtime` as a library: loads a `.nowui` source (from disk, or
//! bundled straight into the binary), resolves `#` imports, builds the
//! retained UI tree, and runs the winit app loop against a live app-state
//! object.
//!
//! Three ways to use this:
//!   * the `nowui` CLI binary (`main.rs`) — arbitrary `.nowui` files, no Rust
//!     state, via `run_path(window_title, path, entry, NoState)`;
//!   * your own binary, loading a `.nowui` file from disk at runtime — call
//!     `nowui_runtime::run_path(window_title, path, entry, my_state)`. See
//!     `nowui-runtime/examples/counter.rs`.
//!   * your own binary, with the `.nowui` file **bundled into the
//!     executable** at compile time — `#[derive(NowUiState)]
//!     #[nowui(view("/login.nowui"))]` on your state struct (path relative
//!     to that crate's own `src/` directory), then call
//!     `nowui_runtime::run(window_title, entry, my_state)` with no path at
//!     all. See `examples/counter-app/src/main.rs`.

pub mod app;
pub mod dynamic;
pub mod loader;
pub mod semantic;
pub mod transitions;

use std::path::Path;
use std::process::ExitCode;

use nowui_core::NowUiState;
use nowui_syntax::ast::Node;
use winit::event_loop::{ControlFlow, EventLoop};

pub use app::App;

/// Which rendering backend `App` uses. `Gpu` (`vello`/`wgpu`, via
/// `nowui-render-gpu`) is the default for `run`/`run_path`. `Cpu` (tiny-skia/
/// softbuffer, via `nowui-render`) remains fully supported and is reachable
/// via `run_with_backend`/`run_path_with_backend` — useful on a headless/
/// no-GPU machine, or as a visual regression reference against the GPU path.
/// See README.md's architecture section for the fidelity differences between
/// the two (e.g. text rotates with its node's transform on `Gpu`, not `Cpu`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Cpu,
    Gpu,
}

/// Build the `entry` layout from `ast` and run the winit event loop against
/// `state` until the window closes — the shared tail end of `run`/
/// `run_path`, once each has produced an AST by whatever means (bundled
/// string vs. on-disk file + import resolution). `window_title` becomes the
/// OS window's title bar text (`Window::with_title`).
fn run_ast<S: NowUiState + 'static>(window_title: &str, ast: Vec<Node>, entry: &str, state: S, backend: Backend) -> ExitCode {
    let mut sem = semantic::Semantic::new(&ast);
    let ui = match sem.build(entry, &state) {
        Some(ui) => ui,
        None => {
            eprintln!("error: entry layout `{entry}` not found");
            eprintln!("available layouts: {}", available(&ast).join(", "));
            return ExitCode::FAILURE;
        }
    };

    for w in &sem.warnings {
        eprintln!("warning: {w}");
    }

    // A fixed 60fps loop, not event-driven — `App::about_to_wait` owns the
    // actual `ControlFlow::WaitUntil` pacing and overrides this immediately
    // once the window exists (`App::resumed`); this initial value only
    // covers the brief window before that.
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    // `sem` (specifically its registered dynamic regions) stays alive for
    // the app's whole lifetime — an `if`/`for`'s live re-expansion each
    // redraw needs the AST it came from, not just the one-time `Ui` it
    // originally produced.
    let mut app = App::new(window_title.to_string(), ui, state, sem, backend);
    app.dispatch_pending_on_load();
    match event_loop.run_app(&mut app) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("event loop error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Build the `entry` layout from `state`'s **bundled** `.nowui` view
/// (`#[nowui(view("/path.nowui"))]` on its `#[derive(NowUiState)]`) and run
/// the winit event loop against `state` until the window closes — no path
/// argument, no filesystem access at runtime: the entry file *and* its whole
/// `#`-import graph were embedded into the binary at compile time (see
/// `nowui-macros`'s `build_embedded_view`), so a bundled view is free to use
/// `#` imports same as any on-disk file. `state`'s `NowUiState` impl is what
/// `{value: state.foo.bar}` and `{onClick: state.foo.bar}` bindings resolve
/// against and dispatch to each frame — see CLAUDE.md's "Reactivity" section
/// for the full read/write data flow.
///
/// Fails with a clear error (rather than compiling a working binary that
/// panics) if `S` has no `#[nowui(view(...))]` — use `run_path` instead for
/// a state type that isn't backed by a bundled view.
///
/// `window_title` becomes the OS window's title bar text. Renders via
/// `Backend::Gpu` — use `run_with_backend` to pick `Backend::Cpu` instead
/// (e.g. on a headless/no-GPU machine).
pub fn run<S: NowUiState + 'static>(window_title: &str, entry: &str, state: S) -> ExitCode {
    run_with_backend(window_title, entry, state, Backend::Gpu)
}

/// Same as `run`, with an explicit `backend` choice instead of the `Gpu` default.
pub fn run_with_backend<S: NowUiState + 'static>(window_title: &str, entry: &str, state: S, backend: Backend) -> ExitCode {
    let Some(source) = S::nowui_view() else {
        eprintln!(
            "error: `{}` has no `#[nowui(view(\"/path.nowui\"))]` — add one, or call \
             `nowui_runtime::run_path(window_title, path, entry, state)` to load from disk instead",
            std::any::type_name::<S>()
        );
        return ExitCode::FAILURE;
    };
    // `nowui_view_path`/`nowui_view_imports` are always populated together
    // with `nowui_view` by the derive (see `nowui-macros`) whenever
    // `view(...)` is present, `nowui_view_imports` as `Some(&[])` even for
    // an import-free file — so these `unwrap_or` fallbacks only matter for a
    // hand-written `NowUiState` impl that overrides `nowui_view` directly
    // without the other two (uncommon, but not disallowed).
    let entry_dir = S::nowui_view_path().map(|p| nowui_syntax::import_dirname(p.trim_start_matches('/'))).unwrap_or("");
    let imports = S::nowui_view_imports().unwrap_or(&[]);
    let ast = match loader::load_and_resolve_bundled(source, entry_dir, imports) {
        Ok(ast) => ast,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    run_ast(window_title, ast, entry, state, backend)
}

/// Load `path` from disk, resolve its `#` imports, build the `entry` layout,
/// and run the winit event loop against `state` until the window closes —
/// the original (pre-bundling) entry point; still the right choice for an
/// arbitrary/dev-time `.nowui` file (e.g. the `nowui` CLI binary), or for
/// iterating on a file without a rebuild.
///
/// `window_title` becomes the OS window's title bar text. Renders via
/// `Backend::Gpu` — use `run_path_with_backend` to pick `Backend::Cpu`
/// instead (e.g. on a headless/no-GPU machine).
pub fn run_path<S: NowUiState + 'static>(window_title: &str, path: &str, entry: &str, state: S) -> ExitCode {
    run_path_with_backend(window_title, path, entry, state, Backend::Gpu)
}

/// Same as `run_path`, with an explicit `backend` choice instead of the `Gpu` default.
pub fn run_path_with_backend<S: NowUiState + 'static>(
    window_title: &str,
    path: &str,
    entry: &str,
    state: S,
    backend: Backend,
) -> ExitCode {
    let ast = match loader::load_and_resolve(Path::new(path)) {
        Ok(ast) => ast,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    run_ast(window_title, ast, entry, state, backend)
}

fn available(ast: &[nowui_syntax::ast::Node]) -> Vec<String> {
    ast.iter()
        .filter_map(|n| match n {
            nowui_syntax::ast::Node::LayoutDef { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect()
}
