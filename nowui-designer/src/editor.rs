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
use nowui_core::{NodeId, NodeKind, Ui};
use winit::keyboard::{Key, NamedKey};

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
