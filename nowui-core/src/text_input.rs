//! Pure char-index/selection math for `NodeKind::TextInput` — no winit, no
//! rendering. `cursor`/`selection_anchor` (see `arena.rs`) are stored in
//! *chars*, not bytes, so they can never land mid-UTF-8-codepoint; these
//! helpers are the only place that converts between the two. Shared by the
//! painter (caret/selection/placeholder rendering — see `paint.rs`) and the
//! runtime (editing, click-to-position hit-testing — see `nowui-runtime`'s
//! `App::edit_text_input`/`char_index_for_click`), so both always agree on
//! exactly what string is on screen and what a given char index means.

/// Convert a char index into `s` to its byte offset. A char index equal to
/// `s.chars().count()` (one past the last char) is a valid "end of string"
/// byte offset — the only way `cursor`/`selection_anchor` are ever allowed
/// to exceed the actual content.
pub fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(s.len())
}

pub fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// The string actually shown in a `TextInput` box: `label` with
/// `ime_preview` (if any) spliced in at `cursor`, then every character
/// replaced with `*` if `masked`. The painter and the runtime's click
/// hit-testing both build this exact string, so a click always lands on the
/// character it visually points at.
pub fn display_string(label: &str, cursor: usize, ime_preview: &str, masked: bool) -> String {
    let at = char_to_byte(label, cursor);
    let mut shown = String::with_capacity(label.len() + ime_preview.len());
    shown.push_str(&label[..at]);
    shown.push_str(ime_preview);
    shown.push_str(&label[at..]);
    if masked {
        "*".repeat(shown.chars().count())
    } else {
        shown
    }
}

/// Height, in pixels, of one line of text at `font_size` — matches
/// `nowui-render`'s own `Metrics::new(size, size * 1.3)` convention, so a
/// multiline `TextInput`'s hard-line caret placement stays in lockstep with
/// what cosmic-text actually renders each line at.
pub fn line_height(font_size: f32) -> f32 {
    font_size * 1.3
}

/// The 0-based *hard*-line index `cursor` (a char index into `shown`) falls
/// on, and its char offset within that line — split on `\n` only. A hard
/// line that itself word-wraps into multiple *visual* lines isn't accounted
/// for here (see `Style::multiline`'s doc comment): this is a hard-line
/// model, not a full wrapped-glyph-layout one, so caret/selection placement
/// on a wrapped (but not newline-broken) long line is approximate.
pub fn line_and_col(shown: &str, cursor: usize) -> (usize, usize) {
    let prefix_byte = char_to_byte(shown, cursor);
    let prefix = &shown[..prefix_byte];
    let line = prefix.matches('\n').count();
    let col = prefix.rsplit('\n').next().unwrap_or(prefix).chars().count();
    (line, col)
}

/// `shown`'s individual hard lines, split on `\n` (see `line_and_col`).
pub fn hard_lines(shown: &str) -> Vec<&str> {
    shown.split('\n').collect()
}

/// Inverse of `line_and_col`: the char index into `shown` for a given hard
/// line + column within it — used to turn a multiline click's (line, col)
/// hit-test result back into the flat char index `cursor`/`selection_anchor`
/// are stored as. An out-of-range `line` clamps to the last line; an
/// out-of-range `col` clamps to that line's length.
pub fn char_index_at(shown: &str, line: usize, col: usize) -> usize {
    let lines = hard_lines(shown);
    let line = line.min(lines.len().saturating_sub(1));
    let idx: usize = lines[..line].iter().map(|l| l.chars().count() + 1).sum();
    idx + col.min(lines[line].chars().count())
}

/// Normalize `(cursor, anchor)` into an ordered `(start, end)` char range.
pub fn ordered_range(cursor: usize, anchor: usize) -> (usize, usize) {
    if anchor <= cursor {
        (anchor, cursor)
    } else {
        (cursor, anchor)
    }
}

/// Delete the selected range `anchor..cursor` (order-independent) from
/// `label`, moving `cursor` to the start of the deleted range. Returns
/// `true` if anything was actually removed (a zero-width selection removes
/// nothing).
pub fn delete_range(label: &mut String, cursor: &mut usize, anchor: usize) -> bool {
    let (lo, hi) = ordered_range(*cursor, anchor);
    if lo == hi {
        return false;
    }
    let byte_lo = char_to_byte(label, lo);
    let byte_hi = char_to_byte(label, hi);
    label.replace_range(byte_lo..byte_hi, "");
    *cursor = lo;
    true
}

/// Insert `text` (already filtered of control characters by the caller) at
/// `cursor`, advancing it past the inserted text.
pub fn insert_str(label: &mut String, cursor: &mut usize, text: &str) {
    for ch in text.chars() {
        let byte = char_to_byte(label, *cursor);
        label.insert(byte, ch);
        *cursor += 1;
    }
}

/// Move the caret one char left. With `shift`, extends/starts a selection
/// instead of moving the bare caret (starting it at the caret's current
/// position — standard shift-select behavior). Without `shift`, an existing
/// selection is consumed by collapsing the caret to its start rather than
/// moving it an additional char further (matches every mainstream editor).
pub fn move_left(cursor: &mut usize, selection_anchor: &mut Option<usize>, shift: bool) {
    if shift {
        selection_anchor.get_or_insert(*cursor);
        *cursor = cursor.saturating_sub(1);
    } else if let Some(anchor) = selection_anchor.take() {
        *cursor = ordered_range(*cursor, anchor).0;
    } else {
        *cursor = cursor.saturating_sub(1);
    }
}

/// Mirror of [`move_left`] for the right arrow key; `len` is the char count
/// currently in the field (the caret's upper bound).
pub fn move_right(cursor: &mut usize, selection_anchor: &mut Option<usize>, shift: bool, len: usize) {
    if shift {
        selection_anchor.get_or_insert(*cursor);
        *cursor = (*cursor + 1).min(len);
    } else if let Some(anchor) = selection_anchor.take() {
        *cursor = ordered_range(*cursor, anchor).1;
    } else {
        *cursor = (*cursor + 1).min(len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_to_byte_handles_multibyte_chars() {
        let s = "héllo"; // é is 2 bytes
        assert_eq!(char_to_byte(s, 0), 0);
        assert_eq!(char_to_byte(s, 1), 1);
        assert_eq!(char_to_byte(s, 2), 3, "past the 2-byte é");
        assert_eq!(char_to_byte(s, 5), s.len());
        assert_eq!(char_to_byte(s, 99), s.len(), "out of range clamps to end");
    }

    #[test]
    fn display_string_splices_preview_at_cursor_and_masks() {
        assert_eq!(display_string("ab", 1, "XY", false), "aXYb");
        assert_eq!(display_string("ab", 1, "XY", true), "****");
        assert_eq!(display_string("ab", 2, "", false), "ab");
    }

    #[test]
    fn delete_range_removes_selection_and_moves_cursor_to_start() {
        let mut label = "hello".to_string();
        let mut cursor = 4;
        assert!(delete_range(&mut label, &mut cursor, 1));
        assert_eq!(label, "ho");
        assert_eq!(cursor, 1);
    }

    #[test]
    fn delete_range_is_a_noop_for_a_zero_width_selection() {
        let mut label = "hello".to_string();
        let mut cursor = 2;
        assert!(!delete_range(&mut label, &mut cursor, 2));
        assert_eq!(label, "hello");
    }

    #[test]
    fn insert_str_advances_cursor_past_inserted_text() {
        let mut label = "ac".to_string();
        let mut cursor = 1;
        insert_str(&mut label, &mut cursor, "b");
        assert_eq!(label, "abc");
        assert_eq!(cursor, 2);
    }

    #[test]
    fn move_left_without_shift_collapses_selection_to_its_start() {
        let mut cursor = 4;
        let mut anchor = Some(1);
        move_left(&mut cursor, &mut anchor, false);
        assert_eq!(cursor, 1);
        assert_eq!(anchor, None);
    }

    #[test]
    fn move_left_with_shift_extends_selection() {
        let mut cursor = 4;
        let mut anchor = None;
        move_left(&mut cursor, &mut anchor, true);
        assert_eq!(cursor, 3);
        assert_eq!(anchor, Some(4));
    }

    #[test]
    fn move_right_clamps_to_len() {
        let mut cursor = 4;
        let mut anchor = None;
        move_right(&mut cursor, &mut anchor, false, 4);
        assert_eq!(cursor, 4);
    }

    #[test]
    fn move_right_without_shift_collapses_selection_to_its_end() {
        let mut cursor = 1;
        let mut anchor = Some(4);
        move_right(&mut cursor, &mut anchor, false, 10);
        assert_eq!(cursor, 4);
        assert_eq!(anchor, None);
    }

    #[test]
    fn hard_lines_splits_on_newline() {
        assert_eq!(hard_lines("a\nbb\nccc"), vec!["a", "bb", "ccc"]);
        assert_eq!(hard_lines("no newlines"), vec!["no newlines"]);
    }

    #[test]
    fn line_and_col_finds_the_hard_line_and_offset_within_it() {
        let shown = "abc\nde\nfghij";
        assert_eq!(line_and_col(shown, 0), (0, 0), "start of first line");
        assert_eq!(line_and_col(shown, 2), (0, 2), "mid first line");
        assert_eq!(line_and_col(shown, 4), (1, 0), "just after the first \\n, start of second line");
        assert_eq!(line_and_col(shown, 6), (1, 2), "end of second line");
        assert_eq!(line_and_col(shown, 7), (2, 0), "start of third line");
        assert_eq!(line_and_col(shown, 12), (2, 5), "end of third (last) line");
    }

    #[test]
    fn line_and_col_on_a_single_line_string_is_always_line_zero() {
        assert_eq!(line_and_col("hello", 0), (0, 0));
        assert_eq!(line_and_col("hello", 5), (0, 5));
    }

    #[test]
    fn char_index_at_is_the_inverse_of_line_and_col() {
        let shown = "abc\nde\nfghij";
        for cursor in 0..=char_len(shown) {
            let (line, col) = line_and_col(shown, cursor);
            assert_eq!(char_index_at(shown, line, col), cursor, "round-trips for cursor {cursor}");
        }
    }

    #[test]
    fn char_index_at_clamps_out_of_range_line_and_col() {
        let shown = "ab\ncd";
        assert_eq!(char_index_at(shown, 99, 0), 3, "out-of-range line clamps to the last line");
        assert_eq!(char_index_at(shown, 0, 99), 2, "out-of-range col clamps to that line's length");
    }
}
