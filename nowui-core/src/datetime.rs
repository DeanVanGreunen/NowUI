//! Shared date/time math, string formatting, and popup geometry for the
//! `Date`/`Time`/`DateTime` widgets. Centralized here (like `dropdown_metrics`
//! in `style.rs`) so `paint.rs`'s drawing and `nowui-runtime`'s click
//! hit-testing always agree on where every nav arrow/day cell/spinner arrow
//! actually is.
//!
//! No external date/time crate: only one conversion (Unix days <-> Y/M/D) is
//! needed, so it's implemented directly via Howard Hinnant's well-known
//! `days_from_civil`/`civil_from_days` (public-domain, proleptic Gregorian).
//! `now()` reads the system clock as **UTC**, not the OS's local timezone —
//! no timezone database is linked into this crate, matching the "don't
//! half-implement it" convention in CLAUDE.md for other out-of-scope
//! features.

use crate::geometry::Rect;

// ---------------------------------------------------------------------
// Calendar math
// ---------------------------------------------------------------------

pub fn is_leap_year(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

pub fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(y) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// Days since 1970-01-01 for a proleptic-Gregorian `y`/`m`/`d`.
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y as i64 - 1 } else { y as i64 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m as i64 + 9) % 12; // [0, 11], Mar = 0
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Inverse of `days_from_civil`.
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

/// Day of week for `y`/`m`/`d`: `0` = Sunday, ..., `6` = Saturday.
pub fn weekday(y: i32, m: u32, d: u32) -> u32 {
    let days = days_from_civil(y, m, d);
    // 1970-01-01 was a Thursday (weekday 4); `rem_euclid` keeps this correct
    // for dates before the epoch too.
    (days.rem_euclid(7) + 4).rem_euclid(7) as u32
}

/// The system clock's current date/time (UTC) — `(year, month, day, hour,
/// minute, second)`.
pub fn now() -> (i32, u32, u32, u32, u32, u32) {
    let total_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = total_secs.div_euclid(86400);
    let secs_of_day = total_secs.rem_euclid(86400) as u32;
    let (y, m, d) = civil_from_days(days);
    (y, m, d, secs_of_day / 3600, (secs_of_day % 3600) / 60, secs_of_day % 60)
}

// ---------------------------------------------------------------------
// Formatting / parsing — `Date` is `DD/MM/YYYY`, `Time` is `HH:MM[:SS]`,
// `DateTime` is the two joined by one space.
// ---------------------------------------------------------------------

pub fn format_date(y: i32, m: u32, d: u32) -> String {
    format!("{d:02}/{m:02}/{y:04}")
}

pub fn parse_date(s: &str) -> Option<(i32, u32, u32)> {
    let mut it = s.trim().split('/');
    let d: u32 = it.next()?.trim().parse().ok()?;
    let m: u32 = it.next()?.trim().parse().ok()?;
    let y: i32 = it.next()?.trim().parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
        return None;
    }
    Some((y, m, d))
}

pub fn format_time(h: u32, m: u32, s: u32, with_seconds: bool) -> String {
    if with_seconds {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{h:02}:{m:02}")
    }
}

pub fn parse_time(s: &str) -> Option<(u32, u32, u32)> {
    let mut it = s.trim().split(':');
    let h: u32 = it.next()?.trim().parse().ok()?;
    let m: u32 = it.next()?.trim().parse().ok()?;
    let sec: u32 = match it.next() {
        Some(v) => v.trim().parse().ok()?,
        None => 0,
    };
    if it.next().is_some() || h > 23 || m > 59 || sec > 59 {
        return None;
    }
    Some((h, m, sec))
}

pub fn format_datetime(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32, with_seconds: bool) -> String {
    format!("{} {}", format_date(y, mo, d), format_time(h, mi, s, with_seconds))
}

/// Split a `DateTime`'s combined value on its single separating space into
/// `(date_part, time_part)` — either half is `""` if not yet picked (an
/// empty overall value, or one that only ever got one half filled in).
pub fn split_datetime(value: &str) -> (&str, &str) {
    if value.is_empty() {
        return ("", "");
    }
    if let Some((d, t)) = value.split_once(' ') {
        (d, t)
    } else if value.contains('/') {
        (value, "")
    } else {
        ("", value)
    }
}

pub fn join_datetime(date_part: &str, time_part: &str) -> String {
    match (date_part.is_empty(), time_part.is_empty()) {
        (true, true) => String::new(),
        (false, true) => date_part.to_string(),
        (true, false) => time_part.to_string(),
        (false, false) => format!("{date_part} {time_part}"),
    }
}

// ---------------------------------------------------------------------
// Popup geometry — shared by `paint.rs` (drawing) and `nowui-runtime`
// (click hit-testing). Both must call the exact same function or clicks
// and pixels disagree about where the popup is.
// ---------------------------------------------------------------------

/// `(header_row_h, cell_h)` — the calendar's nav header and every weekday-
/// label/day cell share `cell_h`, so the grid is perfectly uniform.
pub fn calendar_metrics(font_size: f32) -> (f32, f32) {
    (font_size * 1.6 + 12.0, font_size * 1.8 + 10.0)
}

pub struct CalendarLayout {
    pub popup_rect: Rect,
    pub prev_rect: Rect,
    pub next_rect: Rect,
    /// Fixed 6-row x 7-col grid (42 cells, row-major) so switching months
    /// never changes the popup's overall height. `None` for a blank
    /// leading/trailing cell outside `month`'s real days.
    pub day_cells: Vec<(Rect, Option<u32>)>,
}

/// Lay out a full month-grid calendar popup anchored directly below `box_rect`
/// (a widget's own closed-box `computed` rect), showing `year`/`month`.
pub fn layout_calendar(box_rect: Rect, font_size: f32, year: i32, month: u32) -> CalendarLayout {
    const COLS: u32 = 7;
    const ROWS: u32 = 6;
    let (header_h, cell_h) = calendar_metrics(font_size);
    let cell_w = box_rect.w / COLS as f32;
    let popup_h = header_h + cell_h * (ROWS + 1) as f32; // +1: weekday-label row
    let popup_rect = Rect::new(box_rect.x, box_rect.y + box_rect.h, box_rect.w, popup_h);

    let nav_w = header_h;
    let prev_rect = Rect::new(popup_rect.x, popup_rect.y, nav_w, header_h);
    let next_rect = Rect::new(popup_rect.x + popup_rect.w - nav_w, popup_rect.y, nav_w, header_h);

    let grid_top = popup_rect.y + header_h + cell_h; // skip the weekday-label row
    let first_weekday = weekday(year, month, 1);
    let ndays = days_in_month(year, month);

    let mut day_cells = Vec::with_capacity((ROWS * COLS) as usize);
    for i in 0..(ROWS * COLS) {
        let row = i / COLS;
        let col = i % COLS;
        let rect = Rect::new(popup_rect.x + col as f32 * cell_w, grid_top + row as f32 * cell_h, cell_w, cell_h);
        let day = if i >= first_weekday && (i - first_weekday) < ndays { Some(i - first_weekday + 1) } else { None };
        day_cells.push((rect, day));
    }

    CalendarLayout { popup_rect, prev_rect, next_rect, day_cells }
}

/// Row height for each of a clock popup's up-arrow/value/down-arrow rows.
pub fn clock_metrics(font_size: f32) -> f32 {
    font_size * 1.6 + 10.0
}

pub struct ClockLayout {
    pub popup_rect: Rect,
    /// One `(up_rect, value_rect, down_rect)` triple per visible unit column
    /// — 2 columns (hour, minute) if `!with_seconds`, else 3.
    pub columns: Vec<(Rect, Rect, Rect)>,
}

/// Lay out a spinner-style time-picker popup anchored directly below
/// `box_rect`. A compact "click up/down to dial in a value" control rather
/// than a full scrolling dial — keeps the popup's size fixed and its hit
/// geometry trivial, matching this engine's "compact, correct-enough"
/// solver philosophy (see CLAUDE.md).
pub fn layout_clock(box_rect: Rect, font_size: f32, with_seconds: bool) -> ClockLayout {
    let row_h = clock_metrics(font_size);
    let ncols = if with_seconds { 3 } else { 2 };
    let col_w = box_rect.w / ncols as f32;
    let popup_rect = Rect::new(box_rect.x, box_rect.y + box_rect.h, box_rect.w, row_h * 3.0);

    let mut columns = Vec::with_capacity(ncols);
    for i in 0..ncols {
        let x = popup_rect.x + i as f32 * col_w;
        let up = Rect::new(x, popup_rect.y, col_w, row_h);
        let val = Rect::new(x, popup_rect.y + row_h, col_w, row_h);
        let down = Rect::new(x, popup_rect.y + row_h * 2.0, col_w, row_h);
        columns.push((up, val, down));
    }
    ClockLayout { popup_rect, columns }
}

pub struct DateTimeLayout {
    pub popup_rect: Rect,
    pub calendar: CalendarLayout,
    pub clock: ClockLayout,
}

/// A `DateTime` popup: a calendar stacked directly above a clock, both
/// anchored below `box_rect`, sharing one continuous popup rect — neither
/// half auto-closes the popup on its own (see `nowui-runtime`'s
/// `select_datetime_popup`), since picking a full date-time takes more than
/// one click.
pub fn layout_datetime(box_rect: Rect, font_size: f32, with_seconds: bool, year: i32, month: u32) -> DateTimeLayout {
    let calendar = layout_calendar(box_rect, font_size, year, month);
    let clock_anchor = Rect::new(box_rect.x, calendar.popup_rect.y + calendar.popup_rect.h, box_rect.w, 0.0);
    let clock = layout_clock(clock_anchor, font_size, with_seconds);
    let popup_rect = Rect::new(box_rect.x, box_rect.y + box_rect.h, box_rect.w, calendar.popup_rect.h + clock.popup_rect.h);
    DateTimeLayout { popup_rect, calendar, clock }
}

/// Apply a +/-1 wraparound step to one column (`0` = hour, `1` = minute,
/// `2` = second) of an `(h, m, s)` triple.
pub fn step_hms(h: u32, m: u32, s: u32, column: usize, delta: i32) -> (u32, u32, u32) {
    let wrap = |v: u32, bound: u32| ((v as i32 + delta).rem_euclid(bound as i32)) as u32;
    match column {
        0 => (wrap(h, 24), m, s),
        1 => (h, wrap(m, 60), s),
        _ => (h, m, wrap(s, 60)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_in_month_accounts_for_leap_years() {
        assert_eq!(days_in_month(2024, 2), 29); // leap
        assert_eq!(days_in_month(2023, 2), 28); // not leap
        assert_eq!(days_in_month(1900, 2), 28); // divisible by 100, not 400
        assert_eq!(days_in_month(2000, 2), 29); // divisible by 400
    }

    #[test]
    fn civil_days_roundtrip_through_a_known_date() {
        // 2024-03-15 is a Friday.
        assert_eq!(weekday(2024, 3, 15), 5);
        // 1970-01-01 (the epoch) is a Thursday.
        assert_eq!(weekday(1970, 1, 1), 4);
    }

    #[test]
    fn date_format_and_parse_round_trip() {
        assert_eq!(format_date(2024, 3, 5), "05/03/2024");
        assert_eq!(parse_date("05/03/2024"), Some((2024, 3, 5)));
        assert_eq!(parse_date("31/02/2024"), None); // Feb never has 31 days
        assert_eq!(parse_date("not a date"), None);
    }

    #[test]
    fn time_format_respects_with_seconds() {
        assert_eq!(format_time(9, 5, 30, true), "09:05:30");
        assert_eq!(format_time(9, 5, 30, false), "09:05");
        assert_eq!(parse_time("09:05"), Some((9, 5, 0)));
        assert_eq!(parse_time("09:05:30"), Some((9, 5, 30)));
        assert_eq!(parse_time("25:00"), None);
    }

    #[test]
    fn split_and_join_datetime_round_trip() {
        assert_eq!(split_datetime("05/03/2024 09:05:30"), ("05/03/2024", "09:05:30"));
        assert_eq!(split_datetime("05/03/2024"), ("05/03/2024", ""));
        assert_eq!(split_datetime("09:05:30"), ("", "09:05:30"));
        assert_eq!(split_datetime(""), ("", ""));
        assert_eq!(join_datetime("05/03/2024", "09:05:30"), "05/03/2024 09:05:30");
        assert_eq!(join_datetime("05/03/2024", ""), "05/03/2024");
        assert_eq!(join_datetime("", "09:05:30"), "09:05:30");
    }

    #[test]
    fn calendar_layout_places_day_one_after_its_leading_blanks() {
        // March 2024 starts on a Friday (weekday 5), so the first 5 cells
        // (Sun..Thu) are blank and day 1 lands in the 6th cell.
        let layout = layout_calendar(Rect::new(0.0, 0.0, 280.0, 40.0), 16.0, 2024, 3);
        assert_eq!(layout.day_cells[4].1, None);
        assert_eq!(layout.day_cells[5].1, Some(1));
        assert_eq!(layout.day_cells[6].1, Some(2));
        // March has 31 days: day 31 is the 5+31-1 = 35th cell (0-indexed).
        assert_eq!(layout.day_cells[35].1, Some(31));
        assert_eq!(layout.day_cells[36].1, None);
    }

    #[test]
    fn step_hms_wraps_at_bounds() {
        assert_eq!(step_hms(23, 0, 0, 0, 1), (0, 0, 0));
        assert_eq!(step_hms(0, 0, 0, 0, -1), (23, 0, 0));
        assert_eq!(step_hms(0, 59, 0, 1, 1), (0, 0, 0));
        assert_eq!(step_hms(0, 0, 0, 1, -1), (0, 59, 0));
        assert_eq!(step_hms(0, 0, 59, 2, 1), (0, 0, 0));
    }
}
