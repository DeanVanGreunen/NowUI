//! `nowui-designer` — a visual designer for `.nowui` files: a VS Code-style
//! project explorer + tabbed editor + live preview + inspector, with the
//! designer's own chrome dogfooded in `.nowui` itself. See the crate's own
//! design plan for the full architecture; this binary is being built up in
//! stages (see `mod` list below — each is runnable/testable independently
//! before the next is wired in).
//!
//! Stage reached so far: `virtual_fs` (the project tree model) is real and
//! tested. The winit multi-window harness (`app`/`preview`) that actually
//! opens a window and renders a live document is the next stage.

mod virtual_fs;

fn main() {
    // Placeholder entry point until the winit harness (next build-order
    // stage) lands — `virtual_fs` is exercised by its own test suite
    // (`cargo test -p nowui-designer`) in the meantime.
    eprintln!("nowui-designer: preview window not wired up yet — see nowui-designer/src/main.rs");
}
