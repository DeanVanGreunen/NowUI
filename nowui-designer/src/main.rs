//! `nowui-designer` — a visual designer for `.nowui` files: a VS Code-style
//! project explorer + tabbed editor + live preview + inspector, with the
//! designer's own chrome dogfooded in `.nowui` itself. See the crate's own
//! design plan for the full architecture; this binary is being built up in
//! stages — each is runnable/testable independently before the next is
//! wired in.
//!
//! Stage reached so far: pick a `.nowui` file via a native open dialog (the
//! one point this crate legitimately steps outside `.nowui` itself — see
//! `virtual_fs.rs`'s module doc) and render it, read-only, in a single
//! undetachable window — proves the `preview`/`app` harness end to end
//! before the dogfooded chrome, tabs, and inspector are layered on.

mod app;
mod chrome;
mod editor;
mod preview;
mod state;
mod tabs;
mod virtual_fs;
mod watcher;

use std::process::ExitCode;

use app::DesignerApp;
use preview::PreviewDoc;
use state::DesignerState;
use virtual_fs::VirtualFs;
use winit::event_loop::{ControlFlow, EventLoop};

fn main() -> ExitCode {
    // `NOWUI_DESIGNER_FILE` bypasses the native dialog — for automated
    // smoke-testing (a modal OS dialog can't be driven non-interactively)
    // and later CI, not a normal way to use the app.
    let entry_path = if let Ok(path) = std::env::var("NOWUI_DESIGNER_FILE") {
        std::path::PathBuf::from(path)
    } else {
        let Some(path) = rfd::FileDialog::new().add_filter("NowUI", &["nowui"]).set_title("Open a .nowui file").pick_file() else {
            eprintln!("nowui-designer: no file selected, exiting");
            return ExitCode::SUCCESS;
        };
        path
    };

    let doc = match PreviewDoc::load(&entry_path, "App") {
        Ok(doc) => doc,
        Err(e) => {
            eprintln!("nowui-designer: failed to load `{}`: {e}", entry_path.display());
            return ExitCode::FAILURE;
        }
    };

    // Project root: the entry file's own parent directory — a real
    // "workspace root" picker (project.json, .git, etc.) is future work;
    // this is enough to browse the files right around the file that was
    // opened.
    let project_root = entry_path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| std::path::PathBuf::from("."));
    let vfs = VirtualFs::new(project_root);
    let tree = app::scan_tree(&vfs);
    let state = DesignerState { tree, creating_hint: app::IDLE_HINT.to_string(), ..Default::default() };

    let mut chrome = match chrome::Chrome::load(&state) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("nowui-designer: failed to load the designer's own chrome: {e}");
            return ExitCode::FAILURE;
        }
    };
    match std::fs::read_to_string(&entry_path) {
        Ok(src) => chrome.set_editor_text(&src),
        Err(e) => eprintln!("nowui-designer: could not read `{}` into the editor: {e}", entry_path.display()),
    }

    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = DesignerApp::new(chrome, doc, state, vfs);
    match event_loop.run_app(&mut app) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("nowui-designer: event loop error: {e}");
            ExitCode::FAILURE
        }
    }
}
