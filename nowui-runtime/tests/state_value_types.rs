//! `StateValue::Int`/`Float` stay distinct rather than collapsing into one
//! `Number(f64)` (the derive knows a field's real Rust type, so it doesn't
//! need to guess int-vs-float back from the value the way display code
//! would have to with a collapsed `Number`). Covers: `#[derive(NowUiState)]`
//! emits `Int` for integer fields and `Float` for `f32`/`f64` fields; `set`
//! accepts the other numeric variant too (an `i64` field can be written from
//! a `Float`, e.g. a `Slider`'s value, and vice versa); `as_f64`/`as_i64`
//! widen/narrow across both variants.

use nowui_core::{NowUiState, StateValue};

#[derive(Default, Clone, NowUiState)]
struct Numbers {
    count: i64,
    ratio: f64,
}

#[test]
fn get_returns_int_for_integer_fields_and_float_for_float_fields() {
    let state = Numbers { count: 3, ratio: 0.5 };
    assert_eq!(state.get(&["count"]), Some(StateValue::Int(3)));
    assert_eq!(state.get(&["ratio"]), Some(StateValue::Float(0.5)));
}

#[test]
fn set_accepts_the_matching_variant() {
    let mut state = Numbers::default();
    assert!(state.set(&["count"], StateValue::Int(7)));
    assert!(state.set(&["ratio"], StateValue::Float(1.5)));
    assert_eq!(state.count, 7);
    assert_eq!(state.ratio, 1.5);
}

#[test]
fn set_accepts_the_cross_numeric_variant_via_as_i64_as_f64() {
    // A Slider's value is always `Float` (a 0..=100 percent) — it should
    // still be able to drive an integer-typed field, truncating.
    let mut state = Numbers::default();
    assert!(state.set(&["count"], StateValue::Float(9.9)));
    assert_eq!(state.count, 9);

    // And the reverse: an `Int` value widening into a `Float` field.
    assert!(state.set(&["ratio"], StateValue::Int(4)));
    assert_eq!(state.ratio, 4.0);
}

#[test]
fn set_rejects_non_numeric_values() {
    let mut state = Numbers::default();
    assert!(!state.set(&["count"], StateValue::Str("nope".to_string())));
    assert!(!state.set(&["ratio"], StateValue::Bool(true)));
}

#[test]
fn as_f64_and_as_i64_work_across_both_numeric_variants() {
    assert_eq!(StateValue::Int(5).as_f64(), Some(5.0));
    assert_eq!(StateValue::Float(5.9).as_f64(), Some(5.9));
    assert_eq!(StateValue::Int(5).as_i64(), Some(5));
    assert_eq!(StateValue::Float(5.9).as_i64(), Some(5), "truncates, same as `as i64`");
    assert_eq!(StateValue::Bool(true).as_f64(), None);
    assert_eq!(StateValue::Str("x".to_string()).as_i64(), None);
}

#[derive(Default, Clone, NowUiState)]
struct Rows {
    ints: Vec<i64>,
    names: Vec<String>,
}

#[test]
fn vec_fields_get_as_a_list_of_the_matching_scalar_variant() {
    let state = Rows { ints: vec![1, 2, 3], names: vec!["a".to_string(), "b".to_string()] };

    assert_eq!(
        state.get(&["ints"]),
        Some(StateValue::List(vec![StateValue::Int(1), StateValue::Int(2), StateValue::Int(3)]))
    );
    assert_eq!(
        state.get(&["names"]),
        Some(StateValue::List(vec![StateValue::Str("a".to_string()), StateValue::Str("b".to_string())]))
    );
}

#[test]
fn vec_fields_are_read_only_set_is_a_noop() {
    let mut state = Rows::default();
    assert!(!state.set(&["ints"], StateValue::List(vec![StateValue::Int(9)])));
    assert!(state.ints.is_empty(), "no `.nowui` syntax writes back into a whole list yet");
}

#[derive(Default, Clone, NowUiState)]
struct Row {
    id: String,
    label: String,
}

#[derive(Default, Clone, NowUiState)]
struct RowList {
    rows: Vec<Row>,
}

#[test]
fn vec_of_struct_fields_get_as_a_list_of_objects() {
    let state = RowList {
        rows: vec![
            Row { id: "1".to_string(), label: "one".to_string() },
            Row { id: "2".to_string(), label: "two".to_string() },
        ],
    };

    assert_eq!(
        state.get(&["rows"]),
        Some(StateValue::List(vec![
            StateValue::Object(vec![
                ("id".to_string(), StateValue::Str("1".to_string())),
                ("label".to_string(), StateValue::Str("one".to_string())),
            ]),
            StateValue::Object(vec![
                ("id".to_string(), StateValue::Str("2".to_string())),
                ("label".to_string(), StateValue::Str("two".to_string())),
            ]),
        ]))
    );
}

#[test]
fn to_state_value_snapshots_every_field_in_declaration_order() {
    let row = Row { id: "x".to_string(), label: "y".to_string() };
    assert_eq!(
        row.to_state_value(),
        StateValue::Object(vec![
            ("id".to_string(), StateValue::Str("x".to_string())),
            ("label".to_string(), StateValue::Str("y".to_string())),
        ])
    );
}

#[test]
fn get_field_looks_up_a_named_field_on_an_object() {
    let obj = StateValue::Object(vec![("id".to_string(), StateValue::Str("x".to_string()))]);
    assert_eq!(obj.get_field("id"), Some(&StateValue::Str("x".to_string())));
    assert_eq!(obj.get_field("nope"), None);
    assert_eq!(StateValue::Bool(true).get_field("id"), None, "get_field on a non-Object is always None");
}
