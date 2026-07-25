//! Loads the optional `bundled.nowdat` sidecar archive (see
//! `nowui_image::nowdat`, built by the `nowui-bundle` CLI) sitting next to
//! the running executable, and resolves a local `Image` widget's `source`
//! against it before falling back to a plain disk read.
//!
//! `nowui-bundle` keys every entry by a file's own basename (see its own
//! doc comment for why); `resolve_local_image` mirrors that exact
//! convention on lookup, so an `Image`'s `source` text — whatever path it
//! resolved to on disk (`loader.rs`'s `resolve_image_paths`) — needs no
//! special-casing to also work as a bundle lookup.
//!
//! The archive is read at most once per process (`OnceLock`), not once per
//! `Image` node — the common case (no bundle shipped, or dozens of `Image`
//! nodes sharing one bundle) shouldn't re-read/re-parse the sidecar file
//! repeatedly.

use std::path::Path;
use std::sync::OnceLock;

use nowui_image::NowdatArchive;

static BUNDLE: OnceLock<Option<NowdatArchive>> = OnceLock::new();

fn bundle() -> Option<&'static NowdatArchive> {
    BUNDLE
        .get_or_init(|| {
            let exe = std::env::current_exe().ok()?;
            let path = exe.parent()?.join("bundled.nowdat");
            let bytes = std::fs::read(&path).ok()?;
            NowdatArchive::open(bytes).ok()
        })
        .as_ref()
}

/// Decode a local (non-network) `Image` source: try the bundled archive
/// first (by basename), then fall back to reading `source` straight off
/// disk — see `resolve_local_image_against` for the actual decision logic,
/// kept as a pure function so it's testable without touching the real
/// process-wide bundle/filesystem.
pub fn decode_local(source: &str) -> Result<nowui_image::DecodedImage, String> {
    resolve_local_image_against(bundle(), source)
}

fn resolve_local_image_against(archive: Option<&NowdatArchive>, source: &str) -> Result<nowui_image::DecodedImage, String> {
    if let Some(archive) = archive {
        if let Some(key) = Path::new(source).file_name().and_then(|n| n.to_str()) {
            if let Some(bytes) = archive.get(key) {
                return nowui_image::decode_bytes(bytes);
            }
        }
    }
    nowui_image::decode_file(Path::new(source))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_the_bundle_when_the_basename_matches() {
        let mut png_bytes = Vec::new();
        let img = image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 255]));
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut png_bytes), image::ImageFormat::Png)
            .unwrap();

        let entries = vec![("logo.png".to_string(), png_bytes)];
        let archive = NowdatArchive::open(nowui_image::nowdat::build(&entries)).unwrap();

        let decoded = resolve_local_image_against(Some(&archive), "assets/logo.png").expect("should decode from the bundle");
        assert_eq!((decoded.width, decoded.height), (2, 2));
    }

    #[test]
    fn falls_back_to_disk_when_the_bundle_has_no_matching_basename() {
        let entries: Vec<(String, Vec<u8>)> = vec![("other.png".to_string(), vec![1, 2, 3])];
        let archive = NowdatArchive::open(nowui_image::nowdat::build(&entries)).unwrap();

        let err = resolve_local_image_against(Some(&archive), "definitely-missing-file.png").unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn falls_back_to_disk_when_there_is_no_bundle_at_all() {
        let err = resolve_local_image_against(None, "definitely-missing-file.png").unwrap_err();
        assert!(!err.is_empty());
    }
}
