//! Converts a char-index (the unit both `nowui_syntax::parse`'s
//! `chumsky::Simple<char>` errors and this crate's own tokenizer use) into
//! an LSP `Position` — line number plus a **UTF-16 code-unit** column, per
//! the LSP spec (`textDocument/positionEncoding` defaults to UTF-16), which
//! is not the same as a char count whenever the source contains characters
//! outside the Basic Multilingual Plane.

pub struct LineIndex {
    /// Char-index (not byte-index) where each line begins; `line_starts[0]`
    /// is always `0`.
    line_starts: Vec<usize>,
    chars: Vec<char>,
}

impl LineIndex {
    pub fn new(source: &str) -> Self {
        let chars: Vec<char> = source.chars().collect();
        let mut line_starts = vec![0];
        for (i, &c) in chars.iter().enumerate() {
            if c == '\n' {
                line_starts.push(i + 1);
            }
        }
        Self { line_starts, chars }
    }

    /// `(line, utf16_character)` for the char at `char_idx` — clamped to the
    /// end of the source if `char_idx` overruns it (a parse error's span can
    /// legitimately point at EOF).
    pub fn position(&self, char_idx: usize) -> (u32, u32) {
        let char_idx = char_idx.min(self.chars.len());
        let line = match self.line_starts.binary_search(&char_idx) {
            Ok(l) => l,
            Err(l) => l - 1,
        };
        let line_start = self.line_starts[line];
        let utf16_col: usize = self.chars[line_start..char_idx].iter().map(|c| c.len_utf16()).sum();
        (line as u32, utf16_col as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_char_is_line_zero_col_zero() {
        let idx = LineIndex::new("abc\ndef");
        assert_eq!(idx.position(0), (0, 0));
    }

    #[test]
    fn char_after_a_newline_starts_the_next_line() {
        let idx = LineIndex::new("abc\ndef");
        assert_eq!(idx.position(4), (1, 0));
        assert_eq!(idx.position(5), (1, 1));
    }

    #[test]
    fn a_char_outside_the_bmp_counts_as_two_utf16_units() {
        // U+1F600 (an emoji) is one `char` but two UTF-16 code units.
        let idx = LineIndex::new("😀x\ny");
        assert_eq!(idx.position(1), (0, 2), "the `x` after the emoji is 2 UTF-16 units in, not 1");
        assert_eq!(idx.position(3), (1, 0));
    }

    #[test]
    fn clamps_to_the_end_of_the_source() {
        let idx = LineIndex::new("abc");
        assert_eq!(idx.position(100), (0, 3));
    }
}
