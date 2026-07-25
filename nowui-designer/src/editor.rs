//! Real keyboard editing for a single `NodeKind::TextInput` node, ported
//! from `nowui-runtime`'s own `App::edit_text_input` (see its doc comment —
//! this mirrors that logic, operating on a plain `&mut Ui` instead of
//! `&mut App<S>`, since nowui-designer's harness isn't an `App<S>` and this
//! logic doesn't touch application state at all — a free function, not
//! worth threading through `nowui_runtime::resolve`'s state-reading concern).
//!
//! **Known simplification**: click-to-position-the-caret isn't wired up yet
//! — clicking the editor always focuses it and places the caret at the end
//! of the text (see `app.rs`'s mouse handling). Real click positioning
//! needs the same `measure_text`-driven char-index math `nowui-runtime`'s
//! own `char_index_for_click` uses, which needs a `Painter` at click time —
//! a real, disclosed gap, not a silent one.

use nowui_core::text_input::{char_len, delete_range, insert_str, move_left, move_right};
use nowui_core::{Color, NodeId, NodeKind, Ui};
use winit::keyboard::{Key, NamedKey};

/// `.nowui` syntax colors, one per `nowui_lsp::tokenizer` kind constant —
/// a fixed, VS-Code-dark-theme-ish palette (this crate has no theme system
/// of its own yet). Reused in-process (see this crate's own module doc,
/// and `nowui-lsp`'s Cargo.toml comment, for why `nowui-lsp`'s tokenizer is
/// called directly here instead of over real LSP/JSON-RPC).
fn color_for_token_kind(kind: u32) -> Color {
    match kind {
        nowui_lsp::tokenizer::COMMENT => Color::rgb(0x6a, 0x99, 0x55),
        nowui_lsp::tokenizer::KEYWORD => Color::rgb(0xc5, 0x86, 0xc0),
        nowui_lsp::tokenizer::STRING => Color::rgb(0xce, 0x91, 0x78),
        nowui_lsp::tokenizer::NUMBER => Color::rgb(0xb5, 0xce, 0xa8),
        nowui_lsp::tokenizer::TYPE => Color::rgb(0x4e, 0xc9, 0xb0),
        nowui_lsp::tokenizer::VARIABLE => Color::rgb(0x9c, 0xdc, 0xfe),
        nowui_lsp::tokenizer::PROPERTY => Color::rgb(0x9c, 0xdc, 0xfe),
        nowui_lsp::tokenizer::NAMESPACE => Color::rgb(0xd7, 0xba, 0x7d),
        _ => Color::rgb(0xd4, 0xd4, 0xd4),
    }
}

/// Tokenizes `source` and maps each token into a `highlight_spans` entry —
/// `Token::start`/`len` are already **char** indices (see its own doc
/// comment), the same convention `highlight_spans` uses, so no byte/char
/// conversion is needed.
pub fn compute_highlight_spans(source: &str) -> Vec<(std::ops::Range<usize>, Color)> {
    nowui_lsp::tokenizer::tokenize(source)
        .into_iter()
        .map(|t| (t.start..t.start + t.len, color_for_token_kind(t.kind)))
        .collect()
}

/// Applies one keyboard event to `id`'s `TextInput` state (`label`/`cursor`/
/// `selection_anchor`). Returns `true` only when `label` itself actually
/// changed (a pure cursor move/no-op returns `false`), so a caller can skip
/// an unnecessary live-preview reload.
pub fn edit_text_input(ui: &mut Ui, id: NodeId, logical_key: &Key, text: Option<&str>, shift: bool) -> bool {
    let multiline = ui.get(id).style.multiline;
    let NodeKind::TextInput { label, cursor, selection_anchor, .. } = &mut ui.get_mut(id).kind else {
        return false;
    };
    let mut changed = false;

    match logical_key {
        Key::Named(NamedKey::Enter) if multiline => {
            if let Some(anchor) = selection_anchor.take() {
                delete_range(label, cursor, anchor);
            }
            insert_str(label, cursor, "\n");
            changed = true;
        }
        Key::Named(NamedKey::Backspace) => {
            changed = match selection_anchor.take() {
                Some(anchor) => delete_range(label, cursor, anchor),
                None if *cursor > 0 => delete_range(label, cursor, *cursor - 1),
                None => false,
            };
        }
        Key::Named(NamedKey::Delete) => {
            changed = match selection_anchor.take() {
                Some(anchor) => delete_range(label, cursor, anchor),
                None if *cursor < char_len(label) => delete_range(label, cursor, *cursor + 1),
                None => false,
            };
        }
        Key::Named(NamedKey::ArrowLeft) => move_left(cursor, selection_anchor, shift),
        Key::Named(NamedKey::ArrowRight) => move_right(cursor, selection_anchor, shift, char_len(label)),
        Key::Named(NamedKey::Home) => {
            if shift {
                selection_anchor.get_or_insert(*cursor);
            } else {
                *selection_anchor = None;
            }
            *cursor = 0;
        }
        Key::Named(NamedKey::End) => {
            if shift {
                selection_anchor.get_or_insert(*cursor);
            } else {
                *selection_anchor = None;
            }
            *cursor = char_len(label);
        }
        _ => {
            if let Some(text) = text {
                let typed: String = text.chars().filter(|c| !c.is_control()).collect();
                if !typed.is_empty() {
                    if let Some(anchor) = selection_anchor.take() {
                        delete_range(label, cursor, anchor);
                    }
                    insert_str(label, cursor, &typed);
                    changed = true;
                }
            }
        }
    }

    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use nowui_core::{Node, Style};

    fn editor_ui(text: &str) -> (Ui, NodeId) {
        let mut ui = Ui::new();
        let kind = NodeKind::TextInput {
            label: text.to_string(),
            placeholder: String::new(),
            masked: false,
            cursor: char_len(text),
            selection_anchor: None,
            ime_preview: String::new(),
            highlight_spans: Vec::new(),
        };
        let id = ui.push(Node::new(kind, Style { multiline: true, ..Default::default() }));
        (ui, id)
    }

    fn label_of(ui: &Ui, id: NodeId) -> &str {
        let NodeKind::TextInput { label, .. } = &ui.get(id).kind else { panic!() };
        label
    }

    #[test]
    fn compute_highlight_spans_colors_a_keyword_a_type_and_a_string_distinctly() {
        let src = "layout: T { Text `hi` }";
        let spans = compute_highlight_spans(src);

        let layout_start = src.find("layout").unwrap();
        let text_start = src.find("Text").unwrap();
        let string_start = src.find('`').unwrap();

        let color_at = |char_idx: usize| spans.iter().find(|(r, _)| r.contains(&char_idx)).map(|(_, c)| *c);

        let keyword_color = color_at(layout_start).expect("`layout` should be tokenized");
        let type_color = color_at(text_start).expect("`Text` should be tokenized");
        let string_color = color_at(string_start + 1).expect("the backtick string body should be tokenized");

        assert_ne!(keyword_color, type_color, "a keyword and a widget kind get different colors");
        assert_ne!(keyword_color, string_color);
        assert_ne!(type_color, string_color);
    }

    #[test]
    fn typing_inserts_at_the_cursor() {
        let (mut ui, id) = editor_ui("ac");
        let NodeKind::TextInput { cursor, .. } = &mut ui.get_mut(id).kind else { panic!() };
        *cursor = 1;
        assert!(edit_text_input(&mut ui, id, &Key::Character("b".into()), Some("b"), false));
        assert_eq!(label_of(&ui, id), "abc");
    }

    #[test]
    fn enter_inserts_a_newline_in_multiline_mode() {
        let (mut ui, id) = editor_ui("ab");
        let NodeKind::TextInput { cursor, .. } = &mut ui.get_mut(id).kind else { panic!() };
        *cursor = 1;
        assert!(edit_text_input(&mut ui, id, &Key::Named(NamedKey::Enter), Some("\r"), false));
        assert_eq!(label_of(&ui, id), "a\nb");
    }

    #[test]
    fn backspace_on_an_empty_field_is_a_no_op() {
        let (mut ui, id) = editor_ui("");
        assert!(!edit_text_input(&mut ui, id, &Key::Named(NamedKey::Backspace), None, false));
    }

    #[test]
    fn arrow_key_moves_the_cursor_without_reporting_a_change() {
        let (mut ui, id) = editor_ui("abc");
        assert!(!edit_text_input(&mut ui, id, &Key::Named(NamedKey::ArrowLeft), None, false));
        let NodeKind::TextInput { cursor, .. } = &ui.get(id).kind else { panic!() };
        assert_eq!(*cursor, 2);
    }
}
