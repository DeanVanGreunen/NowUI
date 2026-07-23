use std::process::ExitCode;
use nowui_core::NowUiState;

#[derive(Default, Clone, NowUiState)]
#[nowui(view("/login.nowui"))]
pub struct App {
    username: String,
    password: String,
    rows: Vec<Row>,
}

#[derive(Default, Clone, NowUiState)]
pub struct Row {
    id:String,
    label:String,
}

fn main() -> ExitCode {
    nowui_runtime::run( "App", App {
        username: "".to_string(),
        password: "".to_string(),
        rows: vec![Row { id: "x".to_string(), label:"x".to_string()}],
    })
}
