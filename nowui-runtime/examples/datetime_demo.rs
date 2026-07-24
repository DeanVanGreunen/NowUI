//! End-to-end demo of the `Date`/`Time`/`DateTime` widgets: staged-vs-
//! committed picker popups (Confirm/Cancel), a draggable analog clock with
//! an AM/PM toggle, a calendar with independent month/year navigation and a
//! `{minYear:/maxYear:}`-bounded year dropdown, and the combined `DateTime`
//! Calendar/Clock tab toggle — see `examples/datetime_demo.nowui`.
//!
//! Run:  cargo run -p nowui-runtime --example datetime_demo

use std::process::ExitCode;

use nowui_core::{Event, NowUiState};

#[derive(Default, Clone, NowUiState)]
#[nowui(methods(on_birthday_picked, on_alarm_picked, on_meeting_picked))]
struct AppState {
    birthday: String,
    alarm: String,
    meeting: String,
    min_year: i64,
    max_year: i64,
}

impl AppState {
    fn on_birthday_picked(&mut self, _app: &mut AppState, _event: &Event) {
        println!("birthday confirmed: {}", self.birthday);
    }

    fn on_alarm_picked(&mut self, _app: &mut AppState, _event: &Event) {
        println!("alarm confirmed: {}", self.alarm);
    }

    fn on_meeting_picked(&mut self, _app: &mut AppState, _event: &Event) {
        println!("meeting confirmed: {}", self.meeting);
    }
}

fn main() -> ExitCode {
    let nowui_file = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/datetime_demo.nowui");
    nowui_runtime::run_path(
        "Date / Time / DateTime",
        nowui_file,
        "App",
        AppState { birthday: String::new(), alarm: String::new(), meeting: String::new(), min_year: 1950, max_year: 2075 },
    )
}
