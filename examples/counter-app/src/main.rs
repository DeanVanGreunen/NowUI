use std::process::ExitCode;
use nowui_core::{Event, NowUiState};

#[derive(Default, Clone, NowUiState)]
#[nowui(view("/login.nowui"))]
#[nowui(methods(sign_in))]
pub struct App {
    username: String,
    password: String,
    rows: Vec<Row>,
}

impl App {
  pub fn sign_in(&self, _event: &Event) {
        println!("username: {}, password: {}", self.username, self.password);
    }
}

#[derive(Default, Clone, NowUiState)]
#[nowui(methods(handle_me))]
pub struct Row {
    id:String,
    label:String,
}

impl Row {
    pub fn handle_me(&mut self, _event:&Event){
    }
}

fn main() -> ExitCode {
    nowui_runtime::run( "App", App {
        username: "".to_string(),
        password: "".to_string(),
        rows: vec![Row { id: "x".to_string(), label:"x".to_string()}],
    })
}
