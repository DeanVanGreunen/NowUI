//! `nowui-bundle` — packs every image file under a directory (recursively)
//! into a single `.nowdat` sidecar archive (see `nowui_image::nowdat` for
//! the format itself), so a NowUI app can ship its local `Image` assets as
//! one compact file next to the executable instead of either scattering raw
//! image files around the install directory or bloating the executable
//! itself via `include_bytes!` for every asset.
//!
//! **Bundle key convention**: each entry is keyed by its file's own
//! **basename** (`logo.png`, not `assets/icons/logo.png`) — matching
//! exactly what `nowui-runtime`'s bundled-asset lookup does with an
//! `Image` widget's own `source` string (`Path::new(source).file_name()`),
//! regardless of how deep that source's own relative path is. This keeps
//! the common case (a flat or shallowly-nested assets folder) simple with
//! zero extra bookkeeping in `.nowui` source itself — an `Image` widget's
//! `source` text doesn't change at all between "load from disk" and "load
//! from the bundle," the runtime just tries the bundle first.
//!
//! **Known limitation, openly disclosed rather than silently wrong**: two
//! source files with the same basename in different subdirectories collide
//! in the bundle. This tool detects that and refuses to write a bundle
//! rather than silently dropping one of them — rename one of the files, or
//! don't ship both if this happens.
//!
//! Usage: `nowui-bundle <assets-dir> <output.nowdat>`

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let [_, assets_dir, output] = args.as_slice() else {
        eprintln!("usage: nowui-bundle <assets-dir> <output.nowdat>");
        return ExitCode::FAILURE;
    };

    let base = PathBuf::from(assets_dir);
    if !base.is_dir() {
        eprintln!("error: `{assets_dir}` is not a directory");
        return ExitCode::FAILURE;
    }

    let mut files = Vec::new();
    collect_files(&base, &mut files);
    files.sort();

    // Group by basename first so a collision is caught and reported
    // clearly, rather than silently letting the last-read file win.
    let mut by_key: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for path in &files {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
        by_key.entry(name).or_default().push(path.clone());
    }

    let mut had_collision = false;
    for (key, paths) in &by_key {
        if paths.len() > 1 {
            had_collision = true;
            eprintln!("error: `{key}` is ambiguous — matched by {} files:", paths.len());
            for p in paths {
                eprintln!("    {}", p.display());
            }
        }
    }
    if had_collision {
        eprintln!("refusing to write a bundle with ambiguous basenames — rename one of the files above.");
        return ExitCode::FAILURE;
    }

    let mut entries = Vec::with_capacity(by_key.len());
    for (key, mut paths) in by_key {
        let path = paths.remove(0);
        match std::fs::read(&path) {
            Ok(data) => entries.push((key, data)),
            Err(e) => eprintln!("warning: skipping {} ({e})", path.display()),
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let bytes = nowui_image::nowdat::build(&entries);
    let total_bytes = bytes.len();
    if let Err(e) = std::fs::write(output, bytes) {
        eprintln!("error writing `{output}`: {e}");
        return ExitCode::FAILURE;
    }

    println!("wrote `{output}`: {} entries, {total_bytes} bytes", entries.len());
    ExitCode::SUCCESS
}
