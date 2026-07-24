//! Shared date/time math, string formatting, staged-picker state, and popup
//! geometry for the `Date`/`Time`/`DateTime` widgets. Centralized here (like
//! `dropdown_metrics` in `style.rs`) so `paint.rs`'s drawing and
//! `nowui-runtime`'s click/drag hit-testing always agree on where every
//! control actually is.
//!
//! No external date/time crate: only one conversion (Unix days <-> Y/M/D) is
//! needed, so it's implemented directly via Howard Hinnant's well-known
//! `days_from_civil`/`civil_from_days` (public-domain, proleptic Gregorian).
//! `now()` reads the system clock as **UTC**, not the OS's local timezone —
//! no timezone database is linked into this crate, matching the "don't
//! half-implement it" convention in CLAUDE.md for other out-of-scope
//! features.
//!
//! # Staged vs. committed value
//!
//! Every picker (`Date`/`Time`/`DateTime`) edits a *staged* copy
//! (`DatePickerState`/`TimePickerState`) while its popup is open — clicking a
//! day, dragging the clock hand, paging the year list, none of that touches
//! the widget's real `value`/`value_path` binding. Only **Confirm** commits
//! the staged state into `value` (and dispatches `onSelect`); **Cancel**, or
//! clicking outside the popup, discards it. See `nowui-runtime`'s
//! `select_date_popup`/`select_time_popup`/`select_datetime_popup` and
//! `confirm_picker`/`cancel_picker`.

use crate::geometry::{Point, Rect, Size};

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

/// Step a calendar's browsed month by `delta` (+/-1), wrapping the year at
/// the Dec/Jan boundary.
pub fn step_month(year: i32, month: u32, delta: i32) -> (i32, u32) {
    let zero_based = month as i32 - 1 + delta;
    (year + zero_based.div_euclid(12), (zero_based.rem_euclid(12) + 1) as u32)
}

// ---------------------------------------------------------------------
// Formatting / parsing — `Date` is `DD/MM/YYYY`, `Time` is `HH:MM[:SS]`,
// `DateTime` is the two joined by one space. Always 24-hour internally —
// the AM/PM toggle in the clock popup is purely a UI convenience for
// editing `TimePickerState`, converted at the edges (see `to_12_hour`/
// `from_12_hour`).
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
/// `(date_part, time_part)` — either half is `""` if not yet picked.
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

/// 24-hour -> `(1..=12, is_pm)`.
pub fn to_12_hour(h24: u32) -> (u32, bool) {
    let is_pm = h24 >= 12;
    let h12 = match h24 % 12 {
        0 => 12,
        h => h,
    };
    (h12, is_pm)
}

/// `(1..=12, is_pm)` -> 24-hour.
pub fn from_12_hour(h12: u32, is_pm: bool) -> u32 {
    let h = h12 % 12;
    if is_pm {
        h + 12
    } else {
        h
    }
}

// ---------------------------------------------------------------------
// Staged picker state — what a popup edits live; only written into the
// widget's real `value` on Confirm.
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DatePickerState {
    pub staged_year: i32,
    pub staged_month: u32,
    pub staged_day: u32,
    /// Whether the year-dropdown's paged year-grid overlay is currently
    /// showing (replacing the day grid) instead of the calendar body.
    pub year_list_open: bool,
    /// First year shown in the paged year-grid (12 years per page).
    pub year_list_page_start: i32,
    /// From `{minYear: state.path}`/`{maxYear: state.path}` — re-resolved
    /// every frame by `nowui-runtime`; these defaults only matter before
    /// that binding resolves (or when there is none).
    pub min_year: i32,
    pub max_year: i32,
}

impl DatePickerState {
    /// Seed staged state from the widget's committed `value`, or the system
    /// clock's current date if `value` is empty/unparseable — the popup
    /// always opens showing *some* concrete date, never a blank grid.
    pub fn from_value_or_now(value: &str) -> Self {
        let (y, m, d) = parse_date(value).unwrap_or_else(|| {
            let (y, m, d, ..) = now();
            (y, m, d)
        });
        DatePickerState { staged_year: y, staged_month: m, staged_day: d, year_list_open: false, year_list_page_start: y, min_year: y - 100, max_year: y + 100 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockMode {
    Hour,
    Minute,
    Second,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimePickerState {
    pub staged_hour: u32,
    pub staged_minute: u32,
    pub staged_second: u32,
    /// Which ring the dial currently edits — switched by tapping the
    /// hour/minute/second segment in the popup's header.
    pub mode: ClockMode,
}

impl TimePickerState {
    /// Seed staged state from the widget's committed `value`, or the system
    /// clock's current time if `value` is empty/unparseable.
    pub fn from_value_or_now(value: &str) -> Self {
        let (h, m, s) = parse_time(value).unwrap_or_else(|| {
            let (_, _, _, h, m, s) = now();
            (h, m, s)
        });
        TimePickerState { staged_hour: h, staged_minute: m, staged_second: s, mode: ClockMode::Hour }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateTimeTab {
    Calendar,
    Clock,
}

// ---------------------------------------------------------------------
// Analog-dial angle math (for the draggable clock hand)
// ---------------------------------------------------------------------

/// Angle in degrees from `center` to `p`, with `0` = straight up (12
/// o'clock) and increasing clockwise — matching a clock face, not standard
/// math convention.
pub fn angle_from_center(center: Point, p: Point) -> f32 {
    let dx = p.x - center.x;
    let dy = p.y - center.y;
    let raw = dx.atan2(-dy).to_degrees();
    if raw < 0.0 {
        raw + 360.0
    } else {
        raw
    }
}

/// Nearest hour (`1..=12`) for a dial angle — snapped to the 12 tick
/// positions (30 degrees apart), matching the real widget's hour ring.
pub fn angle_to_hour_12(angle_deg: f32) -> u32 {
    let step = (angle_deg / 30.0).round() as i32;
    let step = step.rem_euclid(12);
    if step == 0 {
        12
    } else {
        step as u32
    }
}

/// Continuous minute/second (`0..=59`) for a dial angle (6 degrees per
/// unit) — not snapped to the 12 labeled ticks, so dragging between two
/// numbers still lands on the exact minute/second, same as the real widget.
pub fn angle_to_60(angle_deg: f32) -> u32 {
    ((angle_deg / 6.0).round() as i32).rem_euclid(60) as u32
}

fn point_for_angle(center: Point, radius: f32, deg: f32) -> Point {
    let rad = deg.to_radians();
    Point::new(center.x + radius * rad.sin(), center.y - radius * rad.cos())
}

/// Where the `hour12` (`1..=12`) tick label sits on the dial.
pub fn point_for_hour_12(center: Point, radius: f32, hour12: u32) -> Point {
    point_for_angle(center, radius, (hour12 % 12) as f32 * 30.0)
}

/// Where a given minute/second (`0..=59`, in 5-unit ticks: 0, 5, .., 55)
/// label sits on the dial — the same 12 tick positions as the hour ring,
/// just relabeled, matching the real widget's minute ring.
pub fn point_for_60_tick(center: Point, radius: f32, tick: u32) -> Point {
    point_for_angle(center, radius, tick as f32 * 30.0)
}

/// The dial hand's own angle (degrees, clock convention) for the value
/// currently active under `mode`.
pub fn hand_angle(state: &TimePickerState, mode: ClockMode) -> f32 {
    match mode {
        ClockMode::Hour => (to_12_hour(state.staged_hour).0 % 12) as f32 * 30.0,
        ClockMode::Minute => state.staged_minute as f32 * 6.0,
        ClockMode::Second => state.staged_second as f32 * 6.0,
    }
}

// ---------------------------------------------------------------------
// Popup geometry — shared by `paint.rs` (drawing) and `nowui-runtime`
// (click/drag hit-testing). Both must call the exact same functions or
// clicks and pixels disagree about where the popup is.
// ---------------------------------------------------------------------

/// Height of the Cancel/Confirm footer row, present on every popup.
pub fn footer_h(font_size: f32) -> f32 {
    font_size * 1.6 + 16.0
}

/// Decide where a popup opens relative to `box_rect`, keeping it fully
/// inside `viewport` on both axes: below `box_rect` unless that would
/// overflow the bottom of the window (falling back to above, or back to
/// below again if there isn't room either way), and horizontally clamped so
/// it never runs off the left/right edge regardless of where `box_rect`
/// itself sits.
pub fn place_popup(box_rect: Rect, popup_h: f32, viewport: Size) -> Rect {
    let below_y = box_rect.y + box_rect.h;
    let fits_below = below_y + popup_h <= viewport.h;
    let fits_above = box_rect.y - popup_h >= 0.0;
    let y = if fits_below || !fits_above { below_y } else { box_rect.y - popup_h };
    let x = box_rect.x.clamp(0.0, (viewport.w - box_rect.w).max(0.0));
    Rect::new(x, y, box_rect.w, popup_h)
}

/// How much extra to pan the whole page (added to `Ui::auto_scroll`, same
/// sign convention: positive shifts content up/left) so `popup_rect` —
/// already placed by `place_popup`'s own flip/clamp — ends up fully within
/// the physical `viewport` window, with `padding` of breathing room past
/// whichever edge(s) it still overflows. `place_popup` alone can't always
/// guarantee full containment (e.g. a box positioned such that neither
/// "below" nor "above" has enough clear room for the popup's own height);
/// this is the page-panning fallback for exactly that residual case.
/// Returns `(0, 0)` if `popup_rect` already fits.
pub fn reveal_scroll_delta(popup_rect: Rect, viewport: Size, padding: f32) -> Point {
    let dx = if popup_rect.x < 0.0 {
        popup_rect.x - padding
    } else if popup_rect.x + popup_rect.w > viewport.w {
        (popup_rect.x + popup_rect.w) - viewport.w + padding
    } else {
        0.0
    };
    let dy = if popup_rect.y < 0.0 {
        popup_rect.y - padding
    } else if popup_rect.y + popup_rect.h > viewport.h {
        (popup_rect.y + popup_rect.h) - viewport.h + padding
    } else {
        0.0
    };
    Point::new(dx, dy)
}

pub struct YearListLayout {
    pub prev_page_rect: Rect,
    pub next_page_rect: Rect,
    pub range_label_rect: Rect,
    /// `(rect, year)` pairs actually in range (`min_year..=max_year`) for the
    /// current page — up to 12 per page (a 3x4 grid).
    pub year_cells: Vec<(Rect, i32)>,
}

pub struct CalendarLayout {
    pub popup_rect: Rect,
    pub month_prev_rect: Rect,
    pub month_next_rect: Rect,
    pub month_label_rect: Rect,
    pub year_dropdown_rect: Rect,
    pub year_prev_rect: Rect,
    pub year_next_rect: Rect,
    pub weekday_row: Rect,
    /// Fixed 6-row x 7-col grid (42 cells, row-major) so switching months
    /// never changes the popup's overall height. `None` for a blank
    /// leading/trailing cell outside `month`'s real days.
    pub day_cells: Vec<(Rect, Option<u32>)>,
    /// `Some` (replacing `day_cells`'s own area) while the year dropdown's
    /// paged year-grid overlay is open.
    pub year_list: Option<YearListLayout>,
    pub cancel_rect: Rect,
    pub confirm_rect: Rect,
}

const YEAR_LIST_COLS: u32 = 3;
const YEAR_LIST_ROWS: u32 = 4;
const YEAR_LIST_PAGE_SIZE: i32 = (YEAR_LIST_COLS * YEAR_LIST_ROWS) as i32;

fn calendar_popup_height(font_size: f32) -> f32 {
    let row_h = font_size * 1.6 + 12.0;
    let cell_h = font_size * 1.8 + 10.0;
    row_h * 2.0 + cell_h * 7.0 + footer_h(font_size)
}

fn build_calendar_layout(
    popup_rect: Rect,
    font_size: f32,
    year: i32,
    month: u32,
    year_list_open: bool,
    year_list_page_start: i32,
    min_year: i32,
    max_year: i32,
) -> CalendarLayout {
    const COLS: u32 = 7;
    const ROWS: u32 = 6;
    let row_h = font_size * 1.6 + 12.0;
    let cell_h = font_size * 1.8 + 10.0;
    let nav_w = row_h;

    let month_row_y = popup_rect.y;
    let month_prev_rect = Rect::new(popup_rect.x, month_row_y, nav_w, row_h);
    let month_next_rect = Rect::new(popup_rect.x + popup_rect.w - nav_w, month_row_y, nav_w, row_h);
    let month_label_rect = Rect::new(popup_rect.x + nav_w, month_row_y, popup_rect.w - nav_w * 2.0, row_h);

    let year_row_y = popup_rect.y + row_h;
    let year_next_rect = Rect::new(popup_rect.x + popup_rect.w - nav_w, year_row_y, nav_w, row_h);
    let year_prev_rect = Rect::new(year_next_rect.x - nav_w, year_row_y, nav_w, row_h);
    let year_dropdown_rect = Rect::new(popup_rect.x, year_row_y, popup_rect.w - nav_w * 2.0, row_h);

    let weekday_row = Rect::new(popup_rect.x, popup_rect.y + row_h * 2.0, popup_rect.w, cell_h);
    let grid_rect = Rect::new(popup_rect.x, weekday_row.y + cell_h, popup_rect.w, cell_h * ROWS as f32);

    let cell_w = popup_rect.w / COLS as f32;
    let first_weekday = weekday(year, month, 1);
    let ndays = days_in_month(year, month);
    let mut day_cells = Vec::with_capacity((ROWS * COLS) as usize);
    for i in 0..(ROWS * COLS) {
        let row = i / COLS;
        let col = i % COLS;
        let rect = Rect::new(grid_rect.x + col as f32 * cell_w, grid_rect.y + row as f32 * cell_h, cell_w, cell_h);
        let day = if i >= first_weekday && (i - first_weekday) < ndays { Some(i - first_weekday + 1) } else { None };
        day_cells.push((rect, day));
    }

    let year_list = if year_list_open {
        let page_header_h = cell_h;
        let last_page_start = (max_year - YEAR_LIST_PAGE_SIZE + 1).max(min_year);
        let page_start = year_list_page_start.clamp(min_year, last_page_start);
        let prev_page_rect = Rect::new(grid_rect.x, grid_rect.y, nav_w, page_header_h);
        let next_page_rect = Rect::new(grid_rect.x + grid_rect.w - nav_w, grid_rect.y, nav_w, page_header_h);
        let range_label_rect = Rect::new(prev_page_rect.x + nav_w, grid_rect.y, grid_rect.w - nav_w * 2.0, page_header_h);
        let ycell_w = grid_rect.w / YEAR_LIST_COLS as f32;
        let ycell_h = (grid_rect.h - page_header_h) / YEAR_LIST_ROWS as f32;
        let mut year_cells = Vec::with_capacity(YEAR_LIST_PAGE_SIZE as usize);
        for i in 0..YEAR_LIST_PAGE_SIZE as u32 {
            let row = i / YEAR_LIST_COLS;
            let col = i % YEAR_LIST_COLS;
            let y = page_start + i as i32;
            if y > max_year {
                break;
            }
            let rect = Rect::new(grid_rect.x + col as f32 * ycell_w, grid_rect.y + page_header_h + row as f32 * ycell_h, ycell_w, ycell_h);
            year_cells.push((rect, y));
        }
        Some(YearListLayout { prev_page_rect, next_page_rect, range_label_rect, year_cells })
    } else {
        None
    };

    let ftr_h = footer_h(font_size);
    let footer_y = popup_rect.y + popup_rect.h - ftr_h;
    let cancel_rect = Rect::new(popup_rect.x, footer_y, popup_rect.w / 2.0, ftr_h);
    let confirm_rect = Rect::new(popup_rect.x + popup_rect.w / 2.0, footer_y, popup_rect.w / 2.0, ftr_h);

    CalendarLayout {
        popup_rect,
        month_prev_rect,
        month_next_rect,
        month_label_rect,
        year_dropdown_rect,
        year_prev_rect,
        year_next_rect,
        weekday_row,
        day_cells,
        year_list,
        cancel_rect,
        confirm_rect,
    }
}

/// Lay out a full calendar popup (month stepper, year stepper/dropdown, day
/// grid, Cancel/Confirm footer), anchored below/above `box_rect` and kept
/// fully on-screen relative to `viewport` (see `place_popup`).
#[allow(clippy::too_many_arguments)]
pub fn layout_calendar(
    box_rect: Rect,
    viewport: Size,
    font_size: f32,
    year: i32,
    month: u32,
    year_list_open: bool,
    year_list_page_start: i32,
    min_year: i32,
    max_year: i32,
) -> CalendarLayout {
    let popup_rect = place_popup(box_rect, calendar_popup_height(font_size), viewport);
    build_calendar_layout(popup_rect, font_size, year, month, year_list_open, year_list_page_start, min_year, max_year)
}

/// Fixed popup width for a clock face — a circular dial doesn't sensibly
/// stretch to an arbitrary text-input box width, so (like the calendar,
/// which does stretch to `box_rect.w`) this picks a sensible constant,
/// widened only if the box itself is already wider.
pub const CLOCK_POPUP_W: f32 = 280.0;

pub struct ClockLayout {
    pub popup_rect: Rect,
    pub hour_segment_rect: Rect,
    pub minute_segment_rect: Rect,
    /// `Some` only with the `with-seconds` style flag.
    pub second_segment_rect: Option<Rect>,
    /// The whole square area the dial face occupies — any click/drag start
    /// inside here (not just within `dial_radius`) counts as hitting the
    /// dial, matching the real widget's forgiving hit area.
    pub dial_area: Rect,
    pub dial_center: Point,
    pub dial_radius: f32,
    pub ampm_rect: Rect,
    pub cancel_rect: Rect,
    pub confirm_rect: Rect,
}

fn clock_popup_height(font_size: f32, popup_w: f32) -> f32 {
    let header_h = font_size * 2.2 + 16.0;
    let dial_pad = 16.0;
    let dial_size = (popup_w - dial_pad * 2.0).min(220.0).max(120.0);
    let ampm_h = font_size * 1.6 + 12.0;
    header_h + dial_size + dial_pad * 2.0 + ampm_h + footer_h(font_size)
}

fn build_clock_layout(popup_rect: Rect, font_size: f32, with_seconds: bool) -> ClockLayout {
    let header_h = font_size * 2.2 + 16.0;
    let dial_pad = 16.0;
    let dial_size = (popup_rect.w - dial_pad * 2.0).min(220.0).max(120.0);
    let ampm_h = font_size * 1.6 + 12.0;

    let seg_w = if with_seconds { popup_rect.w / 3.0 } else { popup_rect.w / 2.0 };
    let hour_segment_rect = Rect::new(popup_rect.x, popup_rect.y, seg_w, header_h);
    let minute_segment_rect = Rect::new(popup_rect.x + seg_w, popup_rect.y, seg_w, header_h);
    let second_segment_rect = if with_seconds { Some(Rect::new(popup_rect.x + seg_w * 2.0, popup_rect.y, seg_w, header_h)) } else { None };

    let dial_top = popup_rect.y + header_h + dial_pad;
    let dial_area = Rect::new(popup_rect.x + (popup_rect.w - dial_size) / 2.0, dial_top, dial_size, dial_size);
    let dial_center = Point::new(dial_area.x + dial_size / 2.0, dial_area.y + dial_size / 2.0);
    let dial_radius = (dial_size / 2.0 - 24.0).max(50.0);

    let ampm_y = dial_top + dial_size + dial_pad;
    let ampm_rect = Rect::new(popup_rect.x + popup_rect.w / 2.0 - 40.0, ampm_y, 80.0, ampm_h);

    let ftr_h = footer_h(font_size);
    let footer_y = popup_rect.y + popup_rect.h - ftr_h;
    let cancel_rect = Rect::new(popup_rect.x, footer_y, popup_rect.w / 2.0, ftr_h);
    let confirm_rect = Rect::new(popup_rect.x + popup_rect.w / 2.0, footer_y, popup_rect.w / 2.0, ftr_h);

    ClockLayout { popup_rect, hour_segment_rect, minute_segment_rect, second_segment_rect, dial_area, dial_center, dial_radius, ampm_rect, cancel_rect, confirm_rect }
}

/// Lay out a full clock popup (hour/minute/[second] mode segments, the
/// draggable dial, an AM/PM toggle, Cancel/Confirm footer), anchored
/// below/above `box_rect` and kept fully on-screen relative to `viewport`.
pub fn layout_clock(box_rect: Rect, viewport: Size, font_size: f32, with_seconds: bool) -> ClockLayout {
    let popup_w = CLOCK_POPUP_W.max(box_rect.w);
    let popup_h = clock_popup_height(font_size, popup_w);
    let popup_rect = place_popup(Rect::new(box_rect.x, box_rect.y, popup_w, box_rect.h), popup_h, viewport);
    build_clock_layout(popup_rect, font_size, with_seconds)
}

pub struct DateTimeLayout {
    pub popup_rect: Rect,
    pub tab_calendar_rect: Rect,
    pub tab_clock_rect: Rect,
    /// `Some` when `active_tab == Calendar`, `None` otherwise — only the
    /// active tab's body is ever laid out/painted/hit-tested.
    pub calendar: Option<CalendarLayout>,
    pub clock: Option<ClockLayout>,
}

/// Lay out a combined `DateTime` popup: a two-button Calendar/Clock tab row,
/// then whichever one sub-view is active (never both at once — see
/// `NodeKind::DateTime`'s doc comment) below it.
#[allow(clippy::too_many_arguments)]
pub fn layout_datetime(
    box_rect: Rect,
    viewport: Size,
    font_size: f32,
    with_seconds: bool,
    active_tab: DateTimeTab,
    year: i32,
    month: u32,
    year_list_open: bool,
    year_list_page_start: i32,
    min_year: i32,
    max_year: i32,
) -> DateTimeLayout {
    let tab_row_h = font_size * 1.6 + 16.0;
    let popup_w = CLOCK_POPUP_W.max(box_rect.w);
    let body_h = match active_tab {
        DateTimeTab::Calendar => calendar_popup_height(font_size),
        DateTimeTab::Clock => clock_popup_height(font_size, popup_w),
    };
    let popup_rect = place_popup(Rect::new(box_rect.x, box_rect.y, popup_w, box_rect.h), tab_row_h + body_h, viewport);

    let tab_calendar_rect = Rect::new(popup_rect.x, popup_rect.y, popup_rect.w / 2.0, tab_row_h);
    let tab_clock_rect = Rect::new(popup_rect.x + popup_rect.w / 2.0, popup_rect.y, popup_rect.w / 2.0, tab_row_h);
    let body_rect = Rect::new(popup_rect.x, popup_rect.y + tab_row_h, popup_rect.w, body_h);

    let (calendar, clock) = match active_tab {
        DateTimeTab::Calendar => {
            (Some(build_calendar_layout(body_rect, font_size, year, month, year_list_open, year_list_page_start, min_year, max_year)), None)
        }
        DateTimeTab::Clock => (None, Some(build_clock_layout(body_rect, font_size, with_seconds))),
    };

    DateTimeLayout { popup_rect, tab_calendar_rect, tab_clock_rect, calendar, clock }
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
    fn twelve_hour_conversion_round_trips() {
        assert_eq!(to_12_hour(0), (12, false));
        assert_eq!(to_12_hour(12), (12, true));
        assert_eq!(to_12_hour(13), (1, true));
        assert_eq!(to_12_hour(23), (11, true));
        for h24 in 0..24 {
            let (h12, pm) = to_12_hour(h24);
            assert_eq!(from_12_hour(h12, pm), h24);
        }
    }

    #[test]
    fn step_month_wraps_the_year() {
        assert_eq!(step_month(2024, 12, 1), (2025, 1));
        assert_eq!(step_month(2024, 1, -1), (2023, 12));
        assert_eq!(step_month(2024, 6, 1), (2024, 7));
    }

    #[test]
    fn calendar_layout_places_day_one_after_its_leading_blanks() {
        // March 2024 starts on a Friday (weekday 5), so the first 5 cells
        // (Sun..Thu) are blank and day 1 lands in the 6th cell.
        let layout = layout_calendar(Rect::new(0.0, 0.0, 280.0, 40.0), Size::new(800.0, 2000.0), 16.0, 2024, 3, false, 2024, 1900, 2100);
        assert_eq!(layout.day_cells[4].1, None);
        assert_eq!(layout.day_cells[5].1, Some(1));
        assert_eq!(layout.day_cells[6].1, Some(2));
        assert_eq!(layout.day_cells[35].1, Some(31));
        assert_eq!(layout.day_cells[36].1, None);
        assert!(layout.year_list.is_none());
    }

    #[test]
    fn year_list_page_is_clamped_within_bounds_and_never_exceeds_max_year() {
        let layout = layout_calendar(Rect::new(0.0, 0.0, 280.0, 40.0), Size::new(800.0, 2000.0), 16.0, 2024, 3, true, 2099, 1900, 2100);
        let list = layout.year_list.unwrap();
        // Page start clamps so the 12-year page still ends at/under max_year.
        assert_eq!(list.year_cells.first().map(|(_, y)| *y), Some(2089));
        assert_eq!(list.year_cells.last().map(|(_, y)| *y), Some(2100));
    }

    #[test]
    fn place_popup_flips_above_when_it_genuinely_does_not_fit_below() {
        let box_rect = Rect::new(0.0, 600.0, 200.0, 40.0);
        // Below would be 640..940, past an 800-tall viewport; above (600 - 300 = 300) fits.
        let flipped = place_popup(box_rect, 300.0, Size::new(800.0, 800.0));
        assert_eq!(flipped.y, 300.0);
    }

    #[test]
    fn place_popup_prefers_below_when_both_fit() {
        let box_rect = Rect::new(0.0, 100.0, 200.0, 40.0);
        let placed = place_popup(box_rect, 300.0, Size::new(800.0, 800.0));
        assert_eq!(placed.y, 140.0, "below fits comfortably, so it's preferred");
    }

    #[test]
    fn place_popup_clamps_horizontally_so_it_never_runs_off_the_right_edge() {
        // A box near the right edge of a narrow viewport: the popup (same
        // width as the box) must still fit fully inside, sliding left.
        let box_rect = Rect::new(750.0, 100.0, 200.0, 40.0);
        let placed = place_popup(box_rect, 300.0, Size::new(800.0, 800.0));
        assert_eq!(placed.x, 600.0, "800 viewport width - 200 popup width");
        assert!(placed.x + placed.w <= 800.0);
    }

    #[test]
    fn place_popup_clamps_horizontally_so_it_never_runs_off_the_left_edge() {
        let box_rect = Rect::new(-50.0, 100.0, 200.0, 40.0);
        let placed = place_popup(box_rect, 300.0, Size::new(800.0, 800.0));
        assert_eq!(placed.x, 0.0);
    }

    #[test]
    fn reveal_scroll_delta_is_zero_when_the_popup_already_fits() {
        let popup = Rect::new(10.0, 10.0, 200.0, 200.0);
        assert_eq!(reveal_scroll_delta(popup, Size::new(800.0, 800.0), 16.0), Point::new(0.0, 0.0));
    }

    #[test]
    fn reveal_scroll_delta_scrolls_down_and_right_to_reveal_bottom_right_overflow() {
        // Popup's bottom-right corner sticks out 20px past an 800x800 viewport.
        let popup = Rect::new(700.0, 700.0, 120.0, 120.0);
        let delta = reveal_scroll_delta(popup, Size::new(800.0, 800.0), 16.0);
        assert_eq!(delta, Point::new(20.0 + 16.0, 20.0 + 16.0));
    }

    #[test]
    fn reveal_scroll_delta_scrolls_up_and_left_to_reveal_top_left_overflow() {
        let popup = Rect::new(-30.0, -10.0, 120.0, 120.0);
        let delta = reveal_scroll_delta(popup, Size::new(800.0, 800.0), 16.0);
        assert_eq!(delta, Point::new(-30.0 - 16.0, -10.0 - 16.0));
    }

    #[test]
    fn angle_from_center_matches_clock_positions() {
        let center = Point::new(100.0, 100.0);
        assert!((angle_from_center(center, Point::new(100.0, 50.0)) - 0.0).abs() < 0.01, "straight up is 0 degrees");
        assert!((angle_from_center(center, Point::new(150.0, 100.0)) - 90.0).abs() < 0.01, "right is 90 degrees");
        assert!((angle_from_center(center, Point::new(100.0, 150.0)) - 180.0).abs() < 0.01, "down is 180 degrees");
        assert!((angle_from_center(center, Point::new(50.0, 100.0)) - 270.0).abs() < 0.01, "left is 270 degrees");
    }

    #[test]
    fn angle_to_hour_12_snaps_to_the_twelve_ticks() {
        assert_eq!(angle_to_hour_12(0.0), 12);
        assert_eq!(angle_to_hour_12(29.0), 1);
        assert_eq!(angle_to_hour_12(31.0), 1);
        assert_eq!(angle_to_hour_12(90.0), 3);
        assert_eq!(angle_to_hour_12(359.0), 12);
    }

    #[test]
    fn angle_to_60_is_continuous_not_snapped_to_ticks() {
        assert_eq!(angle_to_60(0.0), 0);
        assert_eq!(angle_to_60(6.0), 1);
        assert_eq!(angle_to_60(90.0), 15);
        assert_eq!(angle_to_60(354.0), 59);
    }

    #[test]
    fn date_picker_state_seeds_from_value_or_falls_back_to_now() {
        let s = DatePickerState::from_value_or_now("05/03/2024");
        assert_eq!((s.staged_year, s.staged_month, s.staged_day), (2024, 3, 5));
        let (now_y, ..) = now();
        let empty = DatePickerState::from_value_or_now("");
        assert_eq!(empty.staged_year, now_y);
    }
}
