//! `.nowdat` — a minimal sidecar archive format for shipping local `Image`
//! assets alongside a compiled NowUI binary, instead of baking raw bytes
//! into the executable itself via `include_bytes!` (which bloats the
//! `.exe`/`.rlib` for an app with a lot of large images — every asset stays
//! resident in the binary's own on-disk image even when most of them are
//! rarely shown). Packed ahead of time by the `nowui-bundle` CLI, read at
//! runtime by `nowui-runtime`'s image loading (see its `bundled_assets.rs`).
//!
//! No external serialization dependency — this project's crates keep their
//! own dependency footprint deliberately small (`nowui-image` itself has
//! exactly one dependency, `image`), and this format is simple enough not
//! to need one:
//!
//! ```text
//! magic:   b"NWDT"                4 bytes
//! version: u32 LE                 4 bytes  (currently 1)
//! count:   u32 LE                 4 bytes  (number of entries)
//! per entry, `count` times:
//!   key_len:     u32 LE           4 bytes
//!   key:         [u8; key_len]    UTF-8 — see `nowui-bundle`'s own doc
//!                                 comment for what a key actually is
//!   data_len:    u64 LE           8 bytes
//!   data_offset: u64 LE           8 bytes  (absolute byte offset into this
//!                                 same file)
//! ... then every entry's raw bytes, back to back, at their recorded offsets
//! ```
//!
//! The index is read once into a `HashMap` at `open()`; entry bytes are
//! sliced out of the single in-memory buffer on demand, no re-parsing.

use std::collections::HashMap;

const MAGIC: &[u8; 4] = b"NWDT";
const VERSION: u32 = 1;

/// A `.nowdat` archive, fully loaded into memory (these are meant to hold a
/// project's image assets, not arbitrarily large data sets — same scale
/// assumption the embedded `.nowui` view source already makes).
#[derive(Debug)]
pub struct NowdatArchive {
    bytes: Vec<u8>,
    index: HashMap<String, (u64, u64)>,
}

impl NowdatArchive {
    pub fn open(bytes: Vec<u8>) -> Result<Self, String> {
        if bytes.len() < 12 {
            return Err("not a .nowdat file (too short for a header)".to_string());
        }
        if &bytes[0..4] != MAGIC {
            return Err("not a .nowdat file (bad magic)".to_string());
        }
        let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        if version != VERSION {
            return Err(format!("unsupported .nowdat version {version} (expected {VERSION})"));
        }
        let count = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;

        let mut index = HashMap::with_capacity(count);
        let mut pos = 12usize;
        for _ in 0..count {
            let key_len = read_u32(&bytes, &mut pos)? as usize;
            let key_bytes = read_slice(&bytes, &mut pos, key_len)?;
            let key = std::str::from_utf8(key_bytes).map_err(|e| format!("invalid utf-8 key: {e}"))?.to_string();
            let data_len = read_u64(&bytes, &mut pos)?;
            let data_offset = read_u64(&bytes, &mut pos)?;
            if bytes.get(data_offset as usize..(data_offset + data_len) as usize).is_none() {
                return Err(format!("entry `{key}` points outside the archive (truncated file?)"));
            }
            index.insert(key, (data_offset, data_len));
        }
        Ok(NowdatArchive { bytes, index })
    }

    pub fn get(&self, key: &str) -> Option<&[u8]> {
        let (offset, len) = *self.index.get(key)?;
        self.bytes.get(offset as usize..(offset + len) as usize)
    }

    /// Every entry's key, in arbitrary order — for tooling that needs to
    /// walk the whole archive (e.g. `nowui-icons`' own "every embedded icon
    /// actually rasterizes" regression test), not a hot path.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.index.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }
}

fn read_u32(bytes: &[u8], pos: &mut usize) -> Result<u32, String> {
    let slice = read_slice(bytes, pos, 4)?;
    Ok(u32::from_le_bytes(slice.try_into().unwrap()))
}

fn read_u64(bytes: &[u8], pos: &mut usize) -> Result<u64, String> {
    let slice = read_slice(bytes, pos, 8)?;
    Ok(u64::from_le_bytes(slice.try_into().unwrap()))
}

fn read_slice<'a>(bytes: &'a [u8], pos: &mut usize, len: usize) -> Result<&'a [u8], String> {
    let slice = bytes.get(*pos..*pos + len).ok_or_else(|| "unexpected end of .nowdat header".to_string())?;
    *pos += len;
    Ok(slice)
}

/// Packs `entries` (key, raw bytes) into a `.nowdat` byte buffer, in the
/// order given. `nowui-bundle` is the intended caller; exposed here (rather
/// than kept private to that crate) so it stays testable against `open`
/// with a real round trip, in the same crate that owns the format.
pub fn build(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let header_len: usize = 12 + entries.iter().map(|(k, _)| 4 + k.len() + 8 + 8).sum::<usize>();

    let mut offset = header_len as u64;
    let mut headers = Vec::with_capacity(entries.len());
    for (key, data) in entries {
        headers.push((key, offset, data.len() as u64));
        offset += data.len() as u64;
    }

    let mut out = Vec::with_capacity(offset as usize);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (key, off, len) in &headers {
        out.extend_from_slice(&(key.len() as u32).to_le_bytes());
        out.extend_from_slice(key.as_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&off.to_le_bytes());
    }
    for (_, data) in entries {
        out.extend_from_slice(data);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_multiple_entries() {
        let entries = vec![
            ("logo.png".to_string(), vec![1, 2, 3, 4]),
            ("spinner.gif".to_string(), vec![9, 9, 9]),
            ("empty.bmp".to_string(), vec![]),
        ];
        let bytes = build(&entries);
        let archive = NowdatArchive::open(bytes).expect("valid archive");

        assert_eq!(archive.len(), 3);
        assert_eq!(archive.get("logo.png"), Some(&[1, 2, 3, 4][..]));
        assert_eq!(archive.get("spinner.gif"), Some(&[9, 9, 9][..]));
        assert_eq!(archive.get("empty.bmp"), Some(&[][..]));
        assert_eq!(archive.get("missing.png"), None);
    }

    #[test]
    fn open_rejects_bad_magic() {
        let err = NowdatArchive::open(vec![0u8; 20]).unwrap_err();
        assert!(err.contains("magic"));
    }

    #[test]
    fn open_rejects_a_truncated_header() {
        let bytes = build(&[("a.png".to_string(), vec![1, 2, 3])]);
        let truncated = bytes[..bytes.len() - 10].to_vec();
        assert!(NowdatArchive::open(truncated).is_err());
    }

    #[test]
    fn open_rejects_an_empty_buffer() {
        assert!(NowdatArchive::open(Vec::new()).is_err());
    }
}
