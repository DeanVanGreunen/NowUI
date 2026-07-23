//! Shared cosmic-text shaping/measurement, used by every `nowui_core::Painter`
//! backend (the CPU `SkiaPainter` in `nowui-render`, and any GPU-backed
//! painter) so this logic exists exactly once regardless of how a backend
//! actually rasterizes the shaped glyphs it gets back.

use cosmic_text::{Attrs, Buffer, Metrics, Shaping};
use nowui_core::{Point, TextAlign};

/// The font database and glyph rasterization cache. Expensive to build
/// (loading system fonts can take up to ~1s), so create one and keep it alive
/// for the life of the app rather than per-frame. `swash_cache` is only
/// needed by a backend that rasterizes glyphs itself on the CPU (`SkiaPainter`);
/// a GPU backend that rasterizes glyphs via its own pipeline never touches it.
pub struct TextContext {
    pub font_system: cosmic_text::FontSystem,
    pub swash_cache: cosmic_text::SwashCache,
}

impl TextContext {
    /// Discovers and loads installed fonts from the OS's own font store —
    /// `fontdb`'s `load_system_fonts()` covers the platform's default paths:
    /// the Fonts registry/directory on Windows, `/System/Library/Fonts` +
    /// `/Library/Fonts` + `~/Library/Fonts` on macOS, and fontconfig/XDG font
    /// directories on Linux. No fonts are bundled with NowUI.
    pub fn new() -> Self {
        TextContext {
            font_system: cosmic_text::FontSystem::new(),
            swash_cache: cosmic_text::SwashCache::new(),
        }
    }
}

impl Default for TextContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Shape `text` at `size`, wrapping to `width` if given (`None` measures the
/// text's natural, unwrapped extent), aligned per `align`.
pub fn shape_text(font_system: &mut cosmic_text::FontSystem, text: &str, size: f32, width: Option<f32>, align: TextAlign) -> Buffer {
    let metrics = Metrics::new(size, size * 1.3);
    let mut buffer = Buffer::new(font_system, metrics);
    buffer.set_size(font_system, width, None);
    buffer.set_text(font_system, text, Attrs::new(), Shaping::Advanced);
    let align = match align {
        TextAlign::Left => cosmic_text::Align::Left,
        TextAlign::Center => cosmic_text::Align::Center,
        TextAlign::Right => cosmic_text::Align::Right,
    };
    for line in buffer.lines.iter_mut() {
        line.set_align(Some(align));
    }
    buffer.shape_until_scroll(font_system, false);
    buffer
}

/// Measure the pixel size of `text` at `size` (its natural, unwrapped
/// extent) — used by the layout solver for `Hug`-sized text nodes, and by
/// anything else (caret math, click hit-testing) that needs a text's size
/// outside of any actual paint pass. Pure cosmic-text shaping — no
/// rasterization, no GPU/CPU pixel buffer needed at all.
pub fn measure(font_system: &mut cosmic_text::FontSystem, text: &str, size: f32) -> Point {
    if text.is_empty() {
        return Point::new(0.0, size * 1.3);
    }
    let buffer = shape_text(font_system, text, size, None, TextAlign::Left);
    let mut width = 0.0f32;
    let mut lines = 0.0f32;
    for run in buffer.layout_runs() {
        width = width.max(run.line_w);
        lines += 1.0;
    }
    Point::new(width, lines * buffer.metrics().line_height)
}
