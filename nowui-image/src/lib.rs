//! Decodes PNG/GIF/JPEG/BMP/WebP bytes into plain RGBA8 pixel buffers —
//! shared by every `Painter` backend (see `nowui-core`'s `NodeKind::Image`
//! and the `Painter::draw_image` contract) so the actual decode logic
//! exists exactly once, the same "renderer-agnostic shared preprocessing"
//! shape `nowui-text` already uses for text shaping. A static image decodes
//! to one `Frame`; an animated GIF decodes to one `Frame` per animation
//! frame, each carrying its own display delay — `nowui-runtime`'s redraw
//! loop is what actually advances `Node::Image`'s current frame index
//! against real elapsed time (see `NodeKind::Image::frame_elapsed_ms`).

use std::io::Cursor;
use std::path::Path;

pub mod nowdat;
pub use nowdat::NowdatArchive;

/// One decoded, straight (non-premultiplied) RGBA8 frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, row-major, RGBA8.
    pub rgba: Vec<u8>,
    /// How long this frame stays on screen before advancing to the next —
    /// `0` for a single-frame (non-animated) image, meaningless there.
    pub delay_ms: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    /// Always at least one frame. More than one only for an animated GIF.
    pub frames: Vec<Frame>,
}

impl DecodedImage {
    pub fn is_animated(&self) -> bool {
        self.frames.len() > 1
    }
}

/// Decode `bytes` (already read into memory — the caller decides whether
/// that came from disk, a bundled asset, or a network response) into a
/// `DecodedImage`. Detects an animated GIF specifically to decode every
/// frame with its own delay; everything else (including a single-frame
/// GIF) decodes as one `Frame`.
pub fn decode_bytes(bytes: &[u8]) -> Result<DecodedImage, String> {
    let format = image::guess_format(bytes).map_err(|e| format!("could not detect image format: {e}"))?;

    if format == image::ImageFormat::Gif {
        return decode_gif(bytes);
    }

    let img = image::load_from_memory_with_format(bytes, format).map_err(|e| format!("could not decode image: {e}"))?;
    let rgba = img.to_rgba8();
    let (width, height) = (rgba.width(), rgba.height());
    Ok(DecodedImage { width, height, frames: vec![Frame { width, height, rgba: rgba.into_raw(), delay_ms: 0 }] })
}

fn decode_gif(bytes: &[u8]) -> Result<DecodedImage, String> {
    use image::codecs::gif::GifDecoder;
    use image::AnimationDecoder;

    let decoder = GifDecoder::new(Cursor::new(bytes)).map_err(|e| format!("could not open GIF: {e}"))?;
    let frames = decoder.into_frames().collect_frames().map_err(|e| format!("could not decode GIF frames: {e}"))?;
    if frames.is_empty() {
        return Err("GIF has no frames".to_string());
    }

    let mut out = Vec::with_capacity(frames.len());
    let (mut width, mut height) = (0u32, 0u32);
    for f in frames {
        let (numer, denom) = f.delay().numer_denom_ms();
        let delay_ms = if denom == 0 { 0 } else { numer / denom.max(1) };
        let buf = f.into_buffer();
        width = buf.width();
        height = buf.height();
        out.push(Frame { width, height, rgba: buf.into_raw(), delay_ms });
    }
    Ok(DecodedImage { width, height, frames: out })
}

/// Read and decode a local file. Relative-path resolution (against the
/// `.nowui` file that referenced it) is the caller's job — see
/// `nowui-runtime`'s image-loading code, which mirrors `#`-import path
/// resolution for exactly this reason.
pub fn decode_file(path: &Path) -> Result<DecodedImage, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("could not read `{}`: {e}", path.display()))?;
    decode_bytes(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x2 red PNG, base64-decoded at test time rather than checked in as
    /// a binary fixture file — keeps the crate dependency-free of any test
    /// asset directory.
    fn tiny_png_bytes() -> Vec<u8> {
        // Generated with the `image` crate itself in a throwaway script;
        // pixel-exact isn't the point here, only that decoding round-trips.
        let mut img = image::RgbaImage::new(2, 2);
        for p in img.pixels_mut() {
            *p = image::Rgba([255, 0, 0, 255]);
        }
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(img).write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
        bytes
    }

    #[test]
    fn decode_bytes_reads_a_static_png_as_one_frame() {
        let decoded = decode_bytes(&tiny_png_bytes()).expect("should decode");
        assert_eq!((decoded.width, decoded.height), (2, 2));
        assert_eq!(decoded.frames.len(), 1);
        assert!(!decoded.is_animated());
        assert_eq!(decoded.frames[0].rgba.len(), 2 * 2 * 4);
        assert_eq!(&decoded.frames[0].rgba[0..4], &[255, 0, 0, 255]);
    }

    /// A 2-frame animated GIF (red, then blue), each held 100ms.
    fn tiny_animated_gif_bytes() -> Vec<u8> {
        use image::codecs::gif::GifEncoder;
        use image::{Delay, Frame as EncFrame, Rgba, RgbaImage};

        let mut red = RgbaImage::new(2, 2);
        for p in red.pixels_mut() {
            *p = Rgba([255, 0, 0, 255]);
        }
        let mut blue = RgbaImage::new(2, 2);
        for p in blue.pixels_mut() {
            *p = Rgba([0, 0, 255, 255]);
        }

        let mut bytes = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut bytes);
            let delay = Delay::from_numer_denom_ms(100, 1);
            encoder.encode_frame(EncFrame::from_parts(red, 0, 0, delay)).unwrap();
            encoder.encode_frame(EncFrame::from_parts(blue, 0, 0, delay)).unwrap();
        }
        bytes
    }

    #[test]
    fn decode_bytes_reads_every_frame_of_an_animated_gif_with_its_own_delay() {
        let decoded = decode_bytes(&tiny_animated_gif_bytes()).expect("should decode");
        assert!(decoded.is_animated());
        assert_eq!(decoded.frames.len(), 2);
        assert_eq!(&decoded.frames[0].rgba[0..4], &[255, 0, 0, 255], "first frame is red");
        assert_eq!(&decoded.frames[1].rgba[0..4], &[0, 0, 255, 255], "second frame is blue");
        assert_eq!(decoded.frames[0].delay_ms, 100);
        assert_eq!(decoded.frames[1].delay_ms, 100);
    }

    #[test]
    fn decode_bytes_rejects_garbage_input() {
        assert!(decode_bytes(b"not an image").is_err());
    }

    #[test]
    fn decode_file_reads_and_decodes_from_disk() {
        let dir = std::env::temp_dir().join("nowui_image_test_decode_file");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tiny.png");
        std::fs::write(&path, tiny_png_bytes()).unwrap();

        let decoded = decode_file(&path).expect("should decode from disk");
        assert_eq!((decoded.width, decoded.height), (2, 2));
    }
}
