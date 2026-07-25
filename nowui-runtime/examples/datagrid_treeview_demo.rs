//! End-to-end demo of the `DataGrid` and `TreeView` widgets — see
//! `examples/datagrid_treeview_demo.nowui`.
//!
//! `DataGrid`: a real `<table>`-style auto-layout. The `Name` column's
//! content ("Ferdinand") is deliberately wider than its own header ("Name")
//! and every other row's own `Name` cell — try widening the window and
//! watch every row's `Name` column (header included) stay pinned to that
//! one cell's width, not just the row it came from.
//!
//! `TreeView`: a hand-nested, `hasCheckboxSelection`+`canSelectMultiple`
//! tree. Click a row's own disclosure triangle to expand/collapse it; click
//! its checkbox to select it. Drag-to-reorder (`onNodeMove`) isn't built yet
//! — see `NodeKind::TreeView`'s own doc comment in nowui-core.
//!
//! Run:  cargo run -p nowui-runtime --example datagrid_treeview_demo

use std::process::ExitCode;

use nowui_core::{Event, NowUiState};

#[derive(Default, Clone, NowUiState)]
#[nowui(root(AppState))]
#[nowui(methods(handle_select))]
struct Row {
    id: String,
    name: String,
}

impl Row {
    fn handle_select(&mut self, _app: &mut AppState, _event: &Event) {
        println!("selected row: {} ({})", self.id, self.name);
    }
}

#[derive(Default, Clone, NowUiState)]
#[nowui(methods(sort_by_id, sort_by_name))]
struct AppState {
    rows: Vec<Row>,
}

impl AppState {
    fn sort_by_id(&mut self, _app: &mut AppState, _event: &Event) {
        self.rows.sort_by(|a, b| a.id.cmp(&b.id));
    }

    fn sort_by_name(&mut self, _app: &mut AppState, _event: &Event) {
        self.rows.sort_by(|a, b| a.name.cmp(&b.name));
    }
}

fn main() -> ExitCode {
    let nowui_file = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/datagrid_treeview_demo.nowui");
    let rows = vec![
        Row { id: "3".to_string(), name: "Bo".to_string() },
        Row { id: "1".to_string(), name: "Ferdinand".to_string() },
        Row { id: "2".to_string(), name: "Amy".to_string() },
    ];
    nowui_runtime::run_path("DataGrid / TreeView", nowui_file, "App", AppState { rows })
}
