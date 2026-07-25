//! Embedded [react-icons](https://github.com/react-icons/react-icons) SVG
//! library + SVG-to-RGBA rasterization for NowUI's `Icon` widget.
//!
//! Icons ship as one `.nowdat` archive (`assets/icons.nowdat`, see
//! `nowui_image::nowdat`) embedded straight into this crate via
//! `include_bytes!` — every consumer of `nowui-icons` gets the whole icon
//! library baked in at compile time, no runtime file lookup, the same
//! "compile-time embed" precedent `#[nowui(view(...))]` already sets for
//! `.nowui` source (see `CLAUDE.md`). Each entry is keyed by the icon's own
//! react-icons export name (`"FaUser"`, `"MdSettings"`, ...) and holds raw
//! UTF-8 SVG source text — see `nowui-icons-gen` (a separate, kept dev tool,
//! not part of any shipped binary) for how that archive is built from the
//! upstream `react-icons` npm package.
//!
//! `icon_frame` is the actual widget-facing entry point: look up an icon's
//! SVG by name, recolor it (react-icons' own generated SVGs use
//! `fill="currentColor"`/`stroke="currentColor"` at the root — see
//! `recolor`), rasterize it via `resvg`/`usvg` (built on `tiny-skia`, same
//! rendering foundation `nowui-render`'s CPU backend already uses) at a
//! fixed base resolution, and hand back a `nowui_image::Frame` — the exact
//! same shape `Painter::draw_image` already consumes for the `Image`
//! widget, so `nowui-core`'s `NodeKind::Icon` paints through the identical
//! path with zero new `Painter` methods.
//!
//! Rasterizing once at a fixed base size (not vector-perfect at every
//! screen size) is a deliberate, documented scope limit — same tradeoff
//! `NodeKind::Image` already accepts for a raster PNG/GIF asset scaled to
//! an arbitrary `w-[...]`/`h-[...]`. `DEFAULT_RASTER_SIZE` is chosen large
//! enough to stay crisp at the icon sizes this widget is actually used at.

use std::sync::{Mutex, OnceLock};

/// Baked in at compile time — see this module's own doc comment.
static ICON_ARCHIVE_BYTES: &[u8] = include_bytes!("../assets/icons.nowdat");

static ICON_ARCHIVE: OnceLock<Option<nowui_image::NowdatArchive>> = OnceLock::new();

fn archive() -> Option<&'static nowui_image::NowdatArchive> {
    ICON_ARCHIVE.get_or_init(|| nowui_image::NowdatArchive::open(ICON_ARCHIVE_BYTES.to_vec()).ok()).as_ref()
}

/// The pixel size an icon is rasterized at before `Painter::draw_image`
/// scales it (nearest-neighbor, same as `Image`) to its node's actual
/// `w-[...]`/`h-[...]` — chosen to stay crisp at typical icon sizes without
/// wastefully oversizing every rasterized icon in the (small, in-memory)
/// per-process cache below.
pub const DEFAULT_RASTER_SIZE: u32 = 128;

/// Raw SVG source bytes for `name` (e.g. `"FaUser"`), if it exists in the
/// embedded library.
pub fn icon_svg(name: &str) -> Option<&'static [u8]> {
    archive()?.get(name)
}

/// `[r, g, b, a]`, straight (non-premultiplied) — matches
/// `nowui_image::Frame`'s own convention.
pub type Rgba8 = [u8; 4];

/// Substitutes every `currentColor` occurrence (react-icons' own SVGs set
/// `fill="currentColor" stroke="currentColor"` at the root — see
/// `nowui-icons-gen`'s doc comment for why) with `color` as a `#rrggbb`
/// hex string. `usvg` doesn't resolve CSS's `currentColor` keyword on its
/// own (there's no surrounding document to inherit a `color:` property
/// from), so this is done textually before parsing rather than via a CSS
/// stylesheet override — simpler and just as correct for these
/// single-color icon SVGs (see `nowui-icons-gen` — no icon in the bundled
/// sets uses `currentColor` anywhere but the root `fill`/`stroke`).
fn recolor(svg: &str, color: Rgba8) -> String {
    let hex = format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2]);
    svg.replace("currentColor", &hex)
}

/// Rasterizes `svg` (already recolored) to a square `size`x`size` RGBA8
/// buffer, uniformly scaled to fit and centered — an icon's own `viewBox`
/// is usually already square, but this holds up for a non-square one too.
fn rasterize(svg: &str, size: u32) -> Result<nowui_image::Frame, String> {
    let opt = usvg::Options::default();
    let tree = usvg::Tree::from_str(svg, &opt).map_err(|e| format!("parsing icon svg: {e}"))?;
    let doc_size = tree.size();
    let (w, h) = (doc_size.width(), doc_size.height());
    if w <= 0.0 || h <= 0.0 {
        return Err("icon svg has a zero-area viewBox".to_string());
    }

    let scale = size as f32 / w.max(h);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).ok_or_else(|| "failed to allocate icon pixmap".to_string())?;
    let offset_x = (size as f32 - w * scale) / 2.0;
    let offset_y = (size as f32 - h * scale) / 2.0;
    let transform = resvg::tiny_skia::Transform::from_translate(offset_x, offset_y).pre_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for pixel in pixmap.pixels() {
        let straight = pixel.demultiply();
        rgba.extend_from_slice(&[straight.red(), straight.green(), straight.blue(), straight.alpha()]);
    }
    Ok(nowui_image::Frame { width: size, height: size, rgba, delay_ms: 0 })
}

/// Small process-wide cache keyed by `(name, color)` — a redraw loop that
/// rebuilds a dynamic region containing the same `Icon` repeatedly (e.g. a
/// `for`-generated list) would otherwise re-parse and re-rasterize
/// identical SVGs every time; `NodeKind::Icon` decodes once at node-build
/// time (see `CLAUDE.md`'s "no node-removal/GC" precedent for `Image`) but
/// a region rebuild still creates fresh nodes.
static CACHE: OnceLock<Mutex<std::collections::HashMap<(String, Rgba8), Result<nowui_image::Frame, String>>>> = OnceLock::new();

/// Look up `name`, recolor it to `color`, and rasterize it — the single
/// entry point `nowui-runtime`'s semantic pass calls for an `Icon` widget.
/// Returns a disclosed error (unknown icon name, or a malformed SVG) rather
/// than panicking, same convention `NodeKind::Image::error` already uses.
pub fn icon_frame(name: &str, color: Rgba8) -> Result<nowui_image::Frame, String> {
    let cache = CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let key = (name.to_string(), color);
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(result) = cache.get(&key) {
        return result.clone();
    }

    let result = (|| {
        let svg_bytes = icon_svg(name).ok_or_else(|| format!("unknown icon `{name}` (not in the embedded react-icons library)"))?;
        let svg = std::str::from_utf8(svg_bytes).map_err(|e| format!("icon `{name}` has invalid utf-8 svg source: {e}"))?;
        rasterize(&recolor(svg, color), DEFAULT_RASTER_SIZE)
    })();
    cache.insert(key, result.clone());
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_svg_finds_a_known_icon() {
        // Presence of this exact icon depends on `assets/icons.nowdat`
        // having been generated with the `fa` set included — see
        // `nowui-icons-gen`.
        assert!(icon_svg("FaUser").is_some(), "expected `FaUser` in the embedded icon archive");
    }

    #[test]
    fn icon_svg_returns_none_for_an_unknown_name() {
        assert!(icon_svg("NotARealIconName12345").is_none());
    }

    #[test]
    fn recolor_replaces_every_current_color_occurrence() {
        let svg = r#"<svg fill="currentColor" stroke="currentColor"><path d="M0 0"/></svg>"#;
        let out = recolor(svg, [255, 0, 0, 255]);
        assert!(!out.contains("currentColor"));
        assert_eq!(out.matches("#ff0000").count(), 2);
    }

    #[test]
    fn icon_frame_rasterizes_a_known_icon_to_the_default_size() {
        let frame = icon_frame("FaUser", [0, 0, 0, 255]).expect("FaUser should rasterize");
        assert_eq!(frame.width, DEFAULT_RASTER_SIZE);
        assert_eq!(frame.height, DEFAULT_RASTER_SIZE);
        assert_eq!(frame.rgba.len(), (DEFAULT_RASTER_SIZE * DEFAULT_RASTER_SIZE * 4) as usize);
        // At least one non-transparent pixel — the glyph actually painted
        // something, not just a blank canvas.
        assert!(frame.rgba.chunks_exact(4).any(|p| p[3] > 0));
    }

    #[test]
    fn icon_frame_reports_an_unknown_icon_as_an_error() {
        let err = icon_frame("TotallyMadeUpIcon", [0, 0, 0, 255]).unwrap_err();
        assert!(err.contains("unknown icon"));
    }

    /// Exhaustively rasterizes *every* icon currently embedded in
    /// `assets/icons.nowdat` — not just a couple of hand-picked names. This
    /// is what actually caught the real `usvg` "duplicate `fill`
    /// attribute" parse failure on `BsStar` during development (some
    /// react-icons SVGs set their own root `fill`, colliding with the
    /// default `nowui-icons-gen` injects — see its own `render_node` doc
    /// comment) — a couple of spot checks on `FaUser` alone would have
    /// missed it. Slow-ish (thousands of real rasterizations) but still
    /// well under a second in release-like perf; worth the coverage over
    /// every icon set this crate ships.
    #[test]
    fn every_embedded_icon_rasterizes_without_error() {
        // Deliberately bypasses `icon_frame`'s process-wide cache — caching
        // every icon in the library at once (10k+ entries x 512x512x4
        // bytes each) would hold several gigabytes for the length of this
        // one test, for no benefit (each icon is only rasterized once
        // here anyway).
        let archive = archive().expect("assets/icons.nowdat should be a valid archive");
        let mut failures = Vec::new();
        for name in archive.keys() {
            let svg_bytes = archive.get(name).expect("key came from this same archive's own iterator");
            let result = std::str::from_utf8(svg_bytes)
                .map_err(|e| e.to_string())
                .and_then(|svg| rasterize(&recolor(svg, [0, 0, 0, 255]), DEFAULT_RASTER_SIZE));
            if let Err(e) = result {
                failures.push(format!("{name}: {e}"));
            }
        }
        assert!(failures.is_empty(), "{} of {} icons failed to rasterize:\n{}", failures.len(), archive.len(), failures.join("\n"));
    }
}
