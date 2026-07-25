//! End-to-end demo of the `Image` widget — see `examples/image_demo.nowui`.
//!
//! Built and verified: local file loading (relative to the `.nowui` file
//! that references it), `w-[auto]`/`h-[auto]` aspect-ratio-preserving
//! sizing, animated-GIF frame playback with the `loop` bare-flag wraparound
//! behavior, and rendering on both the CPU (`SkiaPainter`) and GPU
//! (`GpuPainter`) backends.
//!
//! **Not yet built** (staged for later, see the session's own plan): network
//! image loading (`http://`/`https://` sources currently report a disclosed
//! "not yet implemented" error rather than silently showing nothing) and the
//! `.nowdat` bundle format for shipping local images inside a compiled
//! binary — this demo loads `assets/test-image.png`/`test-image.gif`
//! straight off disk, same as every other `run_path`-based example.
//!
//! Run:  cargo run -p nowui-runtime --example image_demo

use std::process::ExitCode;

fn main() -> ExitCode {
    let nowui_file = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/image_demo.nowui");
    nowui_runtime::run_path("Image Demo", nowui_file, "App", nowui_core::NoState)
}
