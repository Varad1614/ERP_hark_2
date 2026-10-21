//! **ERP_EXTENSIONS 5C** — staff time tracking maths, ported from HeyPuter/everything
//! `src/time-tracking/src/lib/{time,util,csv,importer,store}.js` (MIT, pinned in
//! `tools/golden/pin.json`). Golden vectors: `tests/golden/time_tracking.json`.
//!
//! Durations travel as whole seconds and dates as `YYYY-MM-DD` strings, exactly as in the
//! original. Clock times are wall-clock milliseconds on the entry's own (institution-local) day:
//! the original reads them in the browser's zone, the port treats them as naive local times
//! (`NaiveDateTime` ↔ "UTC" milliseconds), which is what the golden run pins (`TZ=UTC`).
//!
//! The store's billing cascade is generic here: [`resolve_rate`] takes the four levels the
//! original walks (entry override → project → client → default). Time tracking maps them to
//! *entry override → the staff member's own effective-dated rate → the payroll-derived hourly
//! rate → the institution default* (see the contract doc `docs/extensions-5c-time-tracking.md`).
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime};
use regex::Regex;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::sync::LazyLock;
use sutra_kernel::money::js_round_dp;
use unicode_normalization::UnicodeNormalization;

fn re(p: &str) -> Regex {
    #[allow(clippy::expect_used)]
    Regex::new(p).expect("static regex")
}

/// `Math.round`: halves go up (towards +∞), unlike `f64::round`.
pub fn js_round(x: f64) -> f64 {
    let f = x.floor();
    if x - f >= 0.5 {
        f + 1.0
    } else {
        f
    }
}

fn pad2(n: i64) -> String {
    format!("{n:02}")
}

// ------------------------------------------------------------------ calendar (time.js)

/// `dateOf`: an ISO date to a `NaiveDate` (lenient like `new Date(y, m - 1, d)`: day 0 or 32
/// roll over into the neighbouring month).
pub fn date_of(iso: &str) -> Option<NaiveDate> {
    let mut it = iso.split('-');
    let y: i32 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    let months = i64::from(y) * 12 + (m - 1);
    let first = NaiveDate::from_ymd_opt(
        months.div_euclid(12) as i32,
        months.rem_euclid(12) as u32 + 1,
        1,
    )?;
    first.checked_add_signed(Duration::days(d - 1))
}

/// `isoOf`.
pub fn iso_of(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

/// `addDays`.
pub fn add_days(iso: &str, days: i64) -> Option<String> {
    date_of(iso).map(|d| iso_of(d + Duration::days(days)))
}

/// `daysApart`: whole days from one ISO date to another (negative when `to` is earlier).
pub fn days_apart(from: &str, to: &str) -> Option<i64> {
    Some((date_of(to)? - date_of(from)?).num_days())
}

/// `weekStartOf`: first day of the week containing `iso`; `week_start` 1 = Monday, 0 = Sunday.
pub fn week_start_of(iso: &str, week_start: u32) -> Option<String> {
    let d = date_of(iso)?;
    let dow = d.weekday().num_days_from_sunday();
    let diff = (dow + 7 - week_start % 7) % 7;
    Some(iso_of(d - Duration::days(i64::from(diff))))
}

/// `weekDays`: the seven ISO dates from `start`.
pub fn week_days(start: &str) -> Vec<String> {
    (0..7).filter_map(|i| add_days(start, i)).collect()
}

/// `monthKeyOf`.
pub fn month_key_of(iso: &str) -> String {
    iso.chars().take(7).collect()
}

fn ym(month_key: &str) -> Option<(i64, i64)> {
    let mut it = month_key.split('-');
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

/// `monthDays`: every ISO date of a `YYYY-MM` month.
pub fn month_days(month_key: &str) -> Vec<String> {
    let Some((y, m)) = ym(month_key) else {
        return vec![];
    };
    let Some(first) = date_of(&format!("{y:04}-{m:02}-01")) else {
        return vec![];
    };
    let count = (1..=31)
        .take_while(|d| NaiveDate::from_ymd_opt(first.year(), first.month(), *d).is_some())
        .count();
    (1..=count).map(|d| format!("{month_key}-{d:02}")).collect()
}

/// `shiftMonth`.
pub fn shift_month(month_key: &str, delta: i64) -> Option<String> {
    let (y, m) = ym(month_key)?;
    let t = y * 12 + (m - 1) + delta;
    Some(format!(
        "{:04}-{:02}",
        t.div_euclid(12),
        t.rem_euclid(12) + 1
    ))
}

/// `monthKeysBetween`: month keys touched by an inclusive date range.
pub fn month_keys_between(from: &str, to: &str) -> Vec<String> {
    let (Some((mut y, mut m)), Some((ey, em))) = (ym(from), ym(to)) else {
        return vec![];
    };
    let mut keys = Vec::new();
    while y < ey || (y == ey && m <= em) {
        keys.push(format!("{y}-{}", pad2(m)));
        m += 1;
        if m > 12 {
            m = 1;
            y += 1;
        }
    }
    keys
}

// ------------------------------------------------------------------ durations (time.js)

static COLON: LazyLock<Regex> =
    LazyLock::new(|| re(r"^([0-9]+):([0-5]?[0-9])(?::([0-5]?[0-9]))?$"));
static BARE_MIN: LazyLock<Regex> = LazyLock::new(|| re(r"^:([0-5]?[0-9])$"));
static BARE_NUM: LazyLock<Regex> = LazyLock::new(|| re(r"^[0-9]+(?:\.[0-9]+)?$"));
static HOURS: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"^([0-9]+(?:\.[0-9]+)?)\s*h(?:ours?|rs?)?(?:\s*([0-9]+(?:\.[0-9]+)?)\s*m(?:in(?:ute)?s?)?|\s+([0-9]+(?:\.[0-9]+)?)|([0-9]+(?:\.[0-9]+)?))?$",
    )
});
static MINS: LazyLock<Regex> =
    LazyLock::new(|| re(r"^([0-9]+(?:\.[0-9]+)?)\s*m(?:in(?:ute)?s?)?$"));
static SECS: LazyLock<Regex> = LazyLock::new(|| re(r"^([0-9]+)\s*s(?:ec(?:ond)?s?)?$"));
static COMBO: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:([0-9]+(?:\.[0-9]+)?)\s*h)?\s*(?:([0-9]+(?:\.[0-9]+)?)\s*m)?\s*(?:([0-9]+)\s*s)?$")
});

/// `parseFloat` of a capture that is known to be a plain decimal.
fn num(s: &str) -> f64 {
    s.parse().unwrap_or(f64::NAN)
}

fn finite_secs(x: f64) -> Option<i64> {
    (x.is_finite() && x.abs() < 9.0e15).then_some(x as i64)
}

/// `parseDuration`: a typed duration in whole seconds — `1:30`, `1:30:00`, `90m`, `1.5h`,
/// `1h 30m`, `:45`; bare numbers are hours (`1.5`, `8`) except integers above 24, which are
/// minutes (`45`, `90`). `None` where the original returns `NaN`.
pub fn parse_duration(input: &str) -> Option<i64> {
    let mut s = input.trim().to_lowercase().replacen(',', ".", 1);
    if s.is_empty() {
        return None;
    }
    if s.starts_with('.') {
        s = format!("0{s}");
    }
    if let Some(c) = COLON.captures(&s) {
        let h = num(&c[1]);
        let m = num(&c[2]);
        let sec = c.get(3).map_or(0.0, |x| num(x.as_str()));
        return finite_secs(h * 3600.0 + m * 60.0 + sec);
    }
    if let Some(c) = BARE_MIN.captures(&s) {
        return finite_secs(num(&c[1]) * 60.0);
    }
    if BARE_NUM.is_match(&s) {
        let n = num(&s);
        if n.fract() == 0.0 && n > 24.0 {
            return finite_secs(n * 60.0);
        }
        return finite_secs(js_round(n * 3600.0));
    }
    if let Some(c) = HOURS.captures(&s) {
        let m = c
            .get(2)
            .or_else(|| c.get(3))
            .or_else(|| c.get(4))
            .map_or(0.0, |x| num(x.as_str()));
        return finite_secs(js_round(num(&c[1]) * 3600.0 + m * 60.0));
    }
    if let Some(c) = MINS.captures(&s) {
        return finite_secs(js_round(num(&c[1]) * 60.0));
    }
    if let Some(c) = SECS.captures(&s) {
        return finite_secs(num(&c[1]));
    }
    if let Some(c) = COMBO.captures(&s) {
        if c.get(1).is_some() || c.get(2).is_some() || c.get(3).is_some() {
            let g = |i: usize| c.get(i).map_or(0.0, |x| num(x.as_str()));
            return finite_secs(js_round(g(1) * 3600.0 + g(2) * 60.0 + g(3)));
        }
    }
    None
}

/// `fmtClock`: `h:mm`.
pub fn fmt_clock(seconds: f64) -> String {
    let total = js_round(seconds).max(0.0) as i64;
    format!("{}:{}", total / 3600, pad2((total % 3600) / 60))
}

/// `fmtTimer`: `h:mm:ss`.
pub fn fmt_timer(seconds: f64) -> String {
    let total = seconds.floor().max(0.0) as i64;
    format!(
        "{}:{}:{}",
        total / 3600,
        pad2((total % 3600) / 60),
        pad2(total % 60)
    )
}

/// `fmtDuration`: `1h 30m`, `45m`, `0m` (59.9 minutes carry into the hour).
pub fn fmt_duration(seconds: f64) -> String {
    let total_min = js_round(seconds.max(0.0) / 60.0) as i64;
    let (h, m) = (total_min / 60, total_min % 60);
    match (h, m) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

/// `toHours`: seconds as decimal hours rounded to `decimals` places.
pub fn to_hours(seconds: f64, decimals: i32) -> f64 {
    let f = 10f64.powi(decimals);
    js_round((seconds / 3600.0) * f) / f
}

/// `roundSeconds`: round a duration **up** to the next `minutes` increment (0 = exact).
pub fn round_seconds(seconds: i64, minutes: i64) -> i64 {
    if minutes == 0 || seconds <= 0 {
        return seconds.max(0);
    }
    let step = minutes * 60;
    (seconds + step - 1).div_euclid(step) * step
}

// ------------------------------------------------------------------ clock times (time.js)

static HM: LazyLock<Regex> = LazyLock::new(|| re(r"^([0-9]{1,2}):([0-5][0-9])(?::([0-5][0-9]))?$"));
static TP12: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)([0-9]{1,2}):([0-5][0-9])(?::([0-5][0-9]))?\s*([ap])\.?\s*m\.?"));
static TP24: LazyLock<Regex> = LazyLock::new(|| re(r"([0-9]{1,2}):([0-5][0-9])(?::([0-5][0-9]))?"));

/// Naive local milliseconds of a wall-clock moment (the golden run's `TZ=UTC` epoch).
pub fn ms_of(dt: NaiveDateTime) -> i64 {
    dt.and_utc().timestamp_millis()
}

/// The naive local moment of [`ms_of`] milliseconds.
pub fn dt_of(ms: i64) -> Option<NaiveDateTime> {
    chrono::DateTime::from_timestamp_millis(ms).map(|d| d.naive_utc())
}

/// `msAt`: milliseconds of `HH:MM[:SS]` on an ISO date (`plus_days` later), or `None`.
pub fn ms_at(date_iso: &str, time_value: Option<&str>, plus_days: i64) -> Option<i64> {
    let c = HM.captures(time_value?)?;
    if date_iso.is_empty() {
        return None;
    }
    let mut it = date_iso.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let mo: i64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    let d: i64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    if y == 0 || mo == 0 || d == 0 {
        return None;
    }
    let day = date_of(&format!("{y:04}-{mo:02}-{d:02}"))? + Duration::days(plus_days);
    let secs = num(&c[1]) as i64 * 3600
        + num(&c[2]) as i64 * 60
        + c.get(3).map_or(0, |x| num(x.as_str()) as i64);
    let dt = day.and_time(NaiveTime::MIN) + Duration::seconds(secs);
    Some(ms_of(dt))
}

/// `timeValueOf`: `HH:MM` of naive local milliseconds.
pub fn time_value_of(ms: i64) -> String {
    dt_of(ms).map_or_else(String::new, |d| d.format("%H:%M").to_string())
}

/// `timePartOf`: the 24-hour `HH:MM` inside an imported cell (`09:30`, `2026-08-03 09:30:00`,
/// `1:30 PM`); an am/pm suffix is honoured unless it contradicts the hour.
pub fn time_part_of(value: &str) -> Option<String> {
    let c = TP12.captures(value).or_else(|| TP24.captures(value))?;
    let mut hour: i64 = num(&c[1]) as i64;
    if let Some(half) = c.get(4).map(|x| x.as_str().to_lowercase()) {
        if (1..=12).contains(&hour) {
            if half == "p" {
                hour = if hour == 12 { 12 } else { hour + 12 };
            } else {
                hour = if hour == 12 { 0 } else { hour };
            }
        }
    }
    if hour > 23 {
        return None;
    }
    Some(format!("{}:{}", pad2(hour), &c[2]))
}

// ------------------------------------------------------------------ money & text (util.js)

/// `round2` on an exact amount: `Math.round(n * 100) / 100` (audit M2). The original's
/// `+ Number.EPSILON` nudge only papered over binary drift, which `Decimal` does not have.
pub fn round2(n: Decimal) -> Decimal {
    js_round_dp(n, 2)
}

/// `round2` for ratios and percentages, which stay `f64` (not money).
pub fn round2_f64(n: f64) -> f64 {
    js_round((n + f64::EPSILON) * 100.0) / 100.0
}

/// `toHours` exactly: whole seconds as decimal hours, `Math.round`ed to `decimals` places.
pub fn hours_of(seconds: i64, decimals: u32) -> Decimal {
    js_round_dp(Decimal::from(seconds) / Decimal::from(3600), decimals)
}

static GROUPED: LazyLock<Regex> = LazyLock::new(|| re(r"^-?[0-9]{1,3}(,[0-9]{3})+$"));
static NUMERIC: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)^[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:e[+-]?[0-9]+)?$"));

fn js_number(s: &str) -> f64 {
    s.parse::<f64>().unwrap_or(f64::NAN)
}

/// `parseAmount`: `1,234.56` and `1.234,56` → 1234.56, `12,50` → 12.5; `None` for junk or a
/// partial number.
pub fn parse_amount(input: &str) -> Option<f64> {
    let s: String = input
        .chars()
        .filter(|c| !(c.is_whitespace() || *c == '\u{202f}' || *c == '\''))
        .collect();
    if s.is_empty() {
        return None;
    }
    let last_comma = s.rfind(',');
    let last_dot = s.rfind('.');
    let normalized = match (last_comma, last_dot) {
        (Some(c), Some(d)) => {
            if c > d {
                s.replace('.', "").replacen(',', ".", 1)
            } else {
                s.replace(',', "")
            }
        }
        (Some(_), None) => {
            if GROUPED.is_match(&s) {
                s.replace(',', "")
            } else {
                s.replacen(',', ".", 1)
            }
        }
        _ => s,
    };
    if !NUMERIC.is_match(&normalized) {
        return None;
    }
    let v = js_number(&normalized);
    v.is_finite().then_some(v)
}

fn fold(s: &str) -> String {
    s.to_lowercase()
        .nfd()
        .filter(|c| !('\u{0300}'..='\u{036f}').contains(c))
        .collect()
}

/// `textMatches`: case- and accent-insensitive "haystack contains needle".
pub fn text_matches(haystack: &str, needle: &str) -> bool {
    needle.is_empty() || fold(haystack).contains(&fold(needle))
}

// ------------------------------------------------------------------ CSV (csv.js)

/// `parseCSV`: RFC 4180 rows (quoted commas, quotes, newlines; CRLF; leading BOM); rows whose
/// cells are all blank are dropped.
pub fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let s = text.strip_prefix('\u{feff}').unwrap_or(text);
    let chars: Vec<char> = s.chars().collect();
    let mut rows = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if in_quotes {
            if ch == '"' {
                if chars.get(i + 1) == Some(&'"') {
                    field.push('"');
                    i += 1;
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(ch);
            }
        } else if ch == '"' {
            in_quotes = true;
        } else if ch == ',' {
            row.push(std::mem::take(&mut field));
        } else if ch == '\n' || ch == '\r' {
            if ch == '\r' && chars.get(i + 1) == Some(&'\n') {
                i += 1;
            }
            row.push(std::mem::take(&mut field));
            rows.push(std::mem::take(&mut row));
        } else {
            field.push(ch);
        }
        i += 1;
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows.into_iter()
        .filter(|r| r.iter().any(|c| !c.trim().is_empty()))
        .collect()
}

/// `csvField`: RFC 4180 escaping with the spreadsheet formula guard (`=`, `+`, `-`, `@`, tab,
/// CR get a leading apostrophe).
pub fn csv_field(value: &str) -> String {
    let mut s = value.to_owned();
    if s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        s = format!("'{s}");
    }
    if s.contains(['"', ',', '\r', '\n']) {
        return format!("\"{}\"", s.replace('"', "\"\""));
    }
    s
}

/// `toCSVLine`.
pub fn to_csv_line<S: AsRef<str>>(fields: &[S]) -> String {
    fields
        .iter()
        .map(|f| csv_field(f.as_ref()))
        .collect::<Vec<_>>()
        .join(",")
}

// ------------------------------------------------------------------ import (importer.js)

/// One field an import can fill and the headers that auto-match it.
pub struct ImportTarget {
    pub key: &'static str,
    pub label: &'static str,
    pub required: bool,
    pub candidates: &'static [&'static str],
}

/// `IMPORT_TARGETS` (Toggl, Clockify and Harvest detailed exports).
pub const IMPORT_TARGETS: [ImportTarget; 9] = [
    ImportTarget {
        key: "date",
        label: "Date",
        required: true,
        candidates: &["start date", "date"],
    },
    ImportTarget {
        key: "duration",
        label: "Duration",
        required: true,
        candidates: &[
            "duration",
            "duration (h)",
            "duration (decimal)",
            "hours",
            "time (h)",
        ],
    },
    ImportTarget {
        key: "project",
        label: "Project",
        required: true,
        candidates: &["project", "project name"],
    },
    ImportTarget {
        key: "client",
        label: "Client",
        required: false,
        candidates: &["client", "client name"],
    },
    ImportTarget {
        key: "task",
        label: "Task",
        required: false,
        candidates: &["description", "task", "title"],
    },
    ImportTarget {
        key: "notes",
        label: "Notes",
        required: false,
        candidates: &["notes", "note"],
    },
    ImportTarget {
        key: "billable",
        label: "Billable",
        required: false,
        candidates: &["billable", "billable?"],
    },
    ImportTarget {
        key: "start",
        label: "Start time",
        required: false,
        candidates: &["start time", "start", "from"],
    },
    ImportTarget {
        key: "end",
        label: "End time",
        required: false,
        candidates: &["end time", "end", "to"],
    },
];

/// Column index per import field (`-1` = absent).
pub type Mapping = BTreeMap<String, i64>;

/// `detectMapping`: guesses columns from the header row and names the tracker.
pub fn detect_mapping<S: AsRef<str>>(headers: &[S]) -> (String, Mapping) {
    let lower: Vec<String> = headers
        .iter()
        .map(|h| h.as_ref().trim().to_lowercase())
        .collect();
    let idx = |c: &str| lower.iter().position(|h| h == c);
    let mut mapping = Mapping::new();
    for t in &IMPORT_TARGETS {
        let found = t.candidates.iter().find_map(|c| idx(c));
        mapping.insert(t.key.to_owned(), found.map_or(-1, |i| i as i64));
    }
    let has = |c: &str| idx(c).is_some();
    let source = if has("duration (h)") || has("duration (decimal)") {
        "Clockify"
    } else if has("start date") && has("duration") {
        "Toggl"
    } else if has("hours") && has("date") {
        "Harvest"
    } else {
        "CSV"
    };
    (source.to_owned(), mapping)
}

const MONTH_LENGTHS: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

fn leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn iso_if_real(year: i64, month: i64, day: i64) -> Option<String> {
    if !(1..=12).contains(&month) || day < 1 {
        return None;
    }
    let longest = if month == 2 && leap(year) {
        29
    } else {
        MONTH_LENGTHS[(month - 1) as usize]
    };
    if day > longest {
        return None;
    }
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

static D_ISO: LazyLock<Regex> = LazyLock::new(|| re(r"^([0-9]{4})-([0-9]{2})-([0-9]{2})"));
static D_SLASH: LazyLock<Regex> =
    LazyLock::new(|| re(r"^([0-9]{1,2})([/.])([0-9]{1,2})([/.])([0-9]{4})"));

/// `parseImportDate`: ISO, slash (month-first) or dotted (day-first) dates; an impossible
/// value flips day and month; a day that does not exist is `None`.
pub fn parse_import_date(value: &str) -> Option<String> {
    let s = value.trim();
    if let Some(m) = D_ISO.captures(s) {
        return iso_if_real(num(&m[1]) as i64, num(&m[2]) as i64, num(&m[3]) as i64);
    }
    let m = D_SLASH.captures(s)?;
    if m[2] != m[4] {
        return None;
    }
    let dotted = &m[2] == ".";
    let (a, b) = (num(&m[1]) as i64, num(&m[3]) as i64);
    let (mut month, mut day) = if dotted { (b, a) } else { (a, b) };
    if month > 12 && day <= 12 {
        std::mem::swap(&mut month, &mut day);
    }
    iso_if_real(num(&m[5]) as i64, month, day)
}

/// `parseImportDuration`: clock durations, decimal hours (`7.5`, `7,5`, even above 24) or the
/// manual parser's unit forms; `None` when unreadable or not positive.
pub fn parse_import_duration(value: &str) -> Option<i64> {
    let s = value.trim();
    if s.is_empty() {
        return None;
    }
    let numeric = s.replacen(',', ".", 1);
    let seconds = if NUMERIC.is_match(&numeric) {
        let x = js_round(js_number(&numeric) * 3600.0);
        if !x.is_finite() {
            return None;
        }
        x
    } else {
        parse_duration(s)? as f64
    };
    (seconds.is_finite() && seconds > 0.0).then_some(seconds as i64)
}

/// One importable row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportItem {
    pub date: String,
    pub seconds: i64,
    pub project: String,
    pub client: String,
    pub task: String,
    pub notes: String,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub billable: bool,
}

/// A row that could not be imported (1-based file line, after the header).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportInvalid {
    pub line: i64,
    pub reason: String,
}

fn truthy(x: Option<i64>) -> bool {
    x.is_some_and(|v| v != 0)
}

/// `rowsToItems`: data rows to import items through a mapping; rows without a billable column
/// import as billable; clock times decorate the duration and never fail a row.
pub fn rows_to_items<S: AsRef<str>>(
    rows: &[Vec<S>],
    mapping: &Mapping,
) -> (Vec<ImportItem>, Vec<ImportInvalid>) {
    let mut items = Vec::new();
    let mut invalid = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let cell = |key: &str| -> String {
            let ix = mapping.get(key).copied().unwrap_or(-1);
            if ix < 0 {
                return String::new();
            }
            row.get(ix as usize)
                .map_or_else(String::new, |c| c.as_ref().trim().to_owned())
        };
        let date = parse_import_date(&cell("date"));
        let seconds = parse_import_duration(&cell("duration"));
        let project = cell("project");
        let (Some(date), Some(seconds), false) = (date.clone(), seconds, project.is_empty()) else {
            let reason = if date.is_none() {
                "unreadable date"
            } else if seconds.is_none() {
                "unreadable duration"
            } else {
                "missing project"
            };
            invalid.push(ImportInvalid {
                line: i as i64 + 2,
                reason: reason.into(),
            });
            continue;
        };
        let start_t = time_part_of(&cell("start"));
        let end_t = time_part_of(&cell("end"));
        let started = ms_at(&date, start_t.as_deref(), 0);
        let mut ended = ms_at(&date, end_t.as_deref(), 0);
        if truthy(started) && truthy(ended) && ended <= started {
            ended = ms_at(&date, end_t.as_deref(), 1);
        }
        let both = truthy(started) && truthy(ended);
        let billable = if mapping.get("billable").copied().unwrap_or(-1) >= 0 {
            matches!(
                cell("billable").to_lowercase().as_str(),
                "yes" | "true" | "1" | "billable"
            )
        } else {
            true
        };
        items.push(ImportItem {
            date,
            seconds,
            project,
            client: cell("client"),
            task: cell("task"),
            notes: cell("notes"),
            started_at: if both { started } else { None },
            ended_at: if both { ended } else { None },
            billable,
        });
    }
    (items, invalid)
}

/// The duplicate signature `importEntries` skips on (same day, project, task, duration, notes).
pub fn import_signature(
    date: &str,
    project: &str,
    task: &str,
    seconds: i64,
    notes: &str,
) -> String {
    format!(
        "{date}|{project}|{}|{seconds}|{}",
        task.trim().to_lowercase(),
        notes.trim().to_lowercase()
    )
}

// ------------------------------------------------------------------ rates (store.js)

/// One effective-dated period of a rate (`from` null = since the beginning; `rate` null = no
/// rate of this level's own from here on).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = PeopleTimeRatePeriod)]
pub struct RatePeriod {
    pub from: Option<String>,
    pub rate: Option<f64>,
}

/// A rate: a plain number ("always") or a list of effective-dated periods.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(untagged)]
#[schema(as = PeopleTimeRateValue)]
pub enum RateValue {
    Plain(f64),
    Periods(Vec<RatePeriod>),
}

fn from_key(p: &RatePeriod) -> &str {
    p.from.as_deref().unwrap_or("")
}

/// `rateAt`: the rate a value yields on `date`, or `None`.
pub fn rate_at(value: Option<&RateValue>, date: &str) -> Option<f64> {
    match value? {
        RateValue::Plain(n) => Some(*n),
        RateValue::Periods(list) => {
            let mut best: Option<&RatePeriod> = None;
            for p in list {
                if p.from.as_deref().is_some_and(|f| !f.is_empty() && date < f) {
                    continue;
                }
                if best.is_none_or(|b| from_key(p) >= from_key(b)) {
                    best = Some(p);
                }
            }
            best.and_then(|b| b.rate)
        }
    }
}

/// `withRateFrom`: merge one rate edit into an effective-dated value. `from` `None` edits the
/// period in effect `today`; a date starts (or updates) the period starting then; single
/// "always" periods collapse back to a plain number.
pub fn with_rate_from(
    current: Option<&RateValue>,
    rate: Option<f64>,
    from: Option<&str>,
    today: &str,
) -> Option<RateValue> {
    if rate.is_none() && from.is_none() {
        return None;
    }
    let mut list: Vec<RatePeriod> = match current {
        None => vec![],
        Some(RateValue::Plain(n)) => vec![RatePeriod {
            from: None,
            rate: Some(*n),
        }],
        Some(RateValue::Periods(l)) => l.clone(),
    };
    match from {
        None => {
            if list.is_empty() {
                return rate.map(RateValue::Plain);
            }
            let mut idx: Option<usize> = None;
            for (i, p) in list.iter().enumerate() {
                if p.from
                    .as_deref()
                    .is_some_and(|f| !f.is_empty() && today < f)
                {
                    continue;
                }
                if idx.is_none_or(|j| from_key(p) >= from_key(&list[j])) {
                    idx = Some(i);
                }
            }
            list[idx.unwrap_or(0)].rate = rate;
        }
        Some(f) => {
            if let Some(p) = list.iter_mut().find(|p| p.from.as_deref() == Some(f)) {
                p.rate = rate;
            } else {
                list.push(RatePeriod {
                    from: Some(f.to_owned()),
                    rate,
                });
            }
        }
    }
    list.sort_by(|a, b| from_key(a).cmp(from_key(b)));
    if list.iter().all(|p| p.rate.is_none()) {
        return None;
    }
    if list.len() == 1 && list[0].from.is_none() {
        return list[0].rate.map(RateValue::Plain);
    }
    Some(RateValue::Periods(list))
}

/// The four levels of the original rate cascade.
#[derive(Debug, Clone, Default)]
pub struct RateLevels<'a> {
    pub entry: Option<f64>,
    pub project: Option<&'a RateValue>,
    pub client: Option<&'a RateValue>,
    pub default: Option<&'a RateValue>,
}

/// `resolveRate`: entry override, then each effective-dated level in effect on the entry's
/// date (an empty date means `today`), else 0.
pub fn resolve_rate(levels: &RateLevels<'_>, date: &str, today: &str) -> f64 {
    if let Some(r) = levels.entry {
        return r;
    }
    let d = if date.is_empty() { today } else { date };
    rate_at(levels.project, d)
        .or_else(|| rate_at(levels.client, d))
        .or_else(|| rate_at(levels.default, d))
        .unwrap_or(0.0)
}

/// `resolveRounding`: the narrower level's rounding (0 is a real answer), else the global one.
pub fn resolve_rounding(own: Option<f64>, global: Option<f64>) -> i64 {
    own.or(global)
        .map_or(0, |v| if v.is_finite() { v as i64 } else { 0 })
}

/// `billedSeconds`: billable seconds after rounding up and any minimum (the minimum is a floor
/// on the rounded time, never charged on top of it).
pub fn billed_seconds(billable: bool, seconds: i64, rounding: i64, minimum_minutes: i64) -> i64 {
    if !billable || seconds <= 0 {
        return 0;
    }
    round_seconds(seconds, rounding).max(minimum_minutes * 60)
}

/// `entryAmount`: rounded billable hours (4 places) times the rate, to the paisa — exact, so
/// `1.2547 h × 250 = 313.675` rounds to `313.68` (the original's floats gave `313.67`).
pub fn entry_amount(billable: bool, billed: i64, rate: Decimal) -> Decimal {
    if !billable {
        return Decimal::ZERO;
    }
    round2(hours_of(billed, 4) * rate)
}

// ------------------------------------------------------------------ entries (store.js)

/// The resolved facts of one entry the aggregations need.
#[derive(Debug, Clone, PartialEq)]
pub struct Tracked {
    pub id: String,
    pub seconds: i64,
    pub billable: bool,
    pub billed: i64,
    pub amount: Decimal,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
}

/// `totals`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    pub seconds: i64,
    pub billable_seconds: i64,
    pub tracked_billable_seconds: i64,
    #[serde(with = "rust_decimal::serde::float")]
    pub amount: Decimal,
}

/// `totals`: tracked seconds, billed seconds (rounding included), raw seconds on billable
/// entries, and the money they earn.
pub fn totals(entries: &[Tracked]) -> Totals {
    Totals {
        seconds: entries.iter().map(|e| e.seconds).sum(),
        billable_seconds: entries.iter().map(|e| e.billed).sum(),
        tracked_billable_seconds: entries
            .iter()
            .map(|e| if e.billable { e.seconds } else { 0 })
            .sum(),
        amount: round2(entries.iter().map(|e| e.amount).sum()),
    }
}

/// One report group (`groupEntries`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub key: String,
    pub label: String,
    pub sublabel: String,
    pub ids: Vec<String>,
    pub seconds: i64,
    pub billable_seconds: i64,
    #[serde(with = "rust_decimal::serde::float")]
    pub amount: Decimal,
}

/// `groupEntries`: groups in first-seen order, then sorted by tracked seconds, largest first
/// (stable). `key_of` gives (key, label, sublabel) per entry.
pub fn group_entries<F>(entries: &[Tracked], key_of: F) -> Vec<Group>
where
    F: Fn(&Tracked) -> (String, String, String),
{
    let mut groups: Vec<Group> = Vec::new();
    for e in entries {
        let (key, label, sublabel) = key_of(e);
        let i = if let Some(i) = groups.iter().position(|g| g.key == key) {
            i
        } else {
            groups.push(Group {
                key,
                label,
                sublabel,
                ids: vec![],
                seconds: 0,
                billable_seconds: 0,
                amount: Decimal::ZERO,
            });
            groups.len() - 1
        };
        let g = &mut groups[i];
        g.ids.push(e.id.clone());
        g.seconds += e.seconds;
        g.billable_seconds += e.billed;
        g.amount = round2(g.amount + e.amount);
    }
    groups.sort_by_key(|g| std::cmp::Reverse(g.seconds));
    groups
}

/// The task grouping key (`task:<lower>` so a task literally named "none" stays apart).
pub fn task_key(task: &str) -> (String, String) {
    if task.is_empty() {
        ("none".into(), "No task".into())
    } else {
        (format!("task:{}", task.to_lowercase()), task.to_owned())
    }
}

/// `overlappingEntries`: entries whose clock times overlap a span, ignoring `id`; entries
/// without times never clash and touching ends are not an overlap.
pub fn overlapping<'a>(
    entries: &'a [Tracked],
    started_at: Option<i64>,
    ended_at: Option<i64>,
    id: Option<&str>,
) -> Vec<&'a Tracked> {
    let (Some(s), Some(e)) = (started_at.filter(|x| *x != 0), ended_at.filter(|x| *x != 0)) else {
        return vec![];
    };
    entries
        .iter()
        .filter(|x| Some(x.id.as_str()) != id)
        .filter(|x| match (x.started_at, x.ended_at) {
            (Some(a), Some(b)) if a != 0 && b != 0 => a < e && s < b,
            _ => false,
        })
        .collect()
}

/// `elapsedSeconds`: whole seconds a timer has run.
pub fn elapsed_seconds(started_at_ms: i64, now_ms: i64) -> i64 {
    (now_ms - started_at_ms).div_euclid(1000).max(0)
}

/// `stopTimer`'s duration: sub-minute accidents still record a minute; `cap` trims a
/// forgotten timer (never inflating a short one).
pub fn stopped_seconds(elapsed: i64, cap: Option<i64>) -> i64 {
    cap.map_or(elapsed, |c| elapsed.min(c)).max(60)
}

/// `durationPatch`: a new duration; an entry with clock times keeps its start and moves its
/// end to where the duration lands. Returns (seconds, new end).
pub fn duration_patch(
    started_at: Option<i64>,
    ended_at: Option<i64>,
    seconds: f64,
) -> (i64, Option<i64>) {
    let next = js_round(seconds).max(0.0) as i64;
    match (started_at.filter(|x| *x != 0), ended_at.filter(|x| *x != 0)) {
        (Some(s), Some(_)) => (next, Some(s + next * 1000)),
        _ => (next, None),
    }
}

/// `filterEntries` billing filter (`all | billable | nonbillable | unbilled`; nothing is ever
/// invoiced here, so `unbilled` = billable).
pub fn billing_filter(billing: &str, billable: bool) -> bool {
    match billing {
        "billable" | "unbilled" => billable,
        "nonbillable" => !billable,
        "invoiced" => false,
        _ => true,
    }
}

// ------------------------------------------------------------------ budgets (store.js)

/// `budgetProgress` over spent figures already summed for the budget's period.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = PeopleTimeBudget)]
pub struct Budget {
    /// `hours` | `amount`.
    pub kind: String,
    /// `total` | `monthly`.
    pub period: String,
    /// Seconds (hours budget) or rupees (amount budget).
    #[serde(with = "rust_decimal::serde::float")]
    #[schema(value_type = f64)]
    pub spent: Decimal,
    #[serde(with = "rust_decimal::serde::float")]
    #[schema(value_type = f64)]
    pub value: Decimal,
    /// `spent / value` — a ratio, so `f64`.
    pub ratio: f64,
}

/// `budgetProgress`: `None` without a positive budget; hours budgets compare seconds.
pub fn budget_progress(kind: &str, period: &str, value: Decimal, spent: Decimal) -> Option<Budget> {
    if value <= Decimal::ZERO {
        return None;
    }
    let v = if kind == "amount" {
        value
    } else {
        value * Decimal::from(3600)
    };
    // `Decimal::to_f64` is total (every decimal has a nearest double); the ratio is display-only.
    let f = |d: Decimal| d.to_f64().unwrap_or(f64::NAN);
    Some(Budget {
        kind: kind.into(),
        period: period.into(),
        spent,
        value: v,
        ratio: f(spent) / f(v),
    })
}

/// The budget window of a period on `today`: the month for `monthly`, all time for `total`.
pub fn budget_window(period: &str, today: &str) -> (String, String) {
    if period == "monthly" {
        let key = month_key_of(today);
        let last = month_days(&key).pop().unwrap_or_else(|| today.to_owned());
        (format!("{key}-01"), last)
    } else {
        ("2000-01-01".into(), "9999-12-31".into())
    }
}

// ------------------------------------------------------------------ SutraERP additions

/// Effort of one staff member on one project in one month (hours, share of the month's
/// tracked time, person-months = share × 1 month — the DST / SERB / UGC utilisation basis).
#[derive(Debug, Clone, PartialEq)]
pub struct Effort {
    pub seconds: i64,
    pub total_seconds: i64,
    pub percent: f64,
    pub person_months: f64,
}

/// Share of a staff member's month spent on one project: `seconds / total_seconds`, as a
/// percentage to two places, and person-months to three.
pub fn effort(seconds: i64, total_seconds: i64) -> Effort {
    let share = if total_seconds > 0 {
        seconds as f64 / total_seconds as f64
    } else {
        0.0
    };
    Effort {
        seconds,
        total_seconds,
        percent: round2_f64(share * 100.0),
        person_months: js_round(share * 1000.0) / 1000.0,
    }
}

/// Hourly cost rate derived from a monthly salary: gross ÷ standard working hours a month, to the
/// paisa (exact; `None` without a positive salary and positive hours).
pub fn hourly_from_monthly(
    monthly_gross: Decimal,
    standard_monthly_hours: Decimal,
) -> Option<Decimal> {
    (standard_monthly_hours > Decimal::ZERO && monthly_gross > Decimal::ZERO)
        .then(|| round2(monthly_gross / standard_monthly_hours))
}

/// Timesheet states: `draft` (open) → `submitted` → `approved` (locked) | `rejected` (open).
pub fn timesheet_locked(status: &str) -> bool {
    matches!(status, "submitted" | "approved")
}

/// Allowed timesheet transitions.
pub fn timesheet_can(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("draft" | "rejected", "submitted") | ("submitted", "approved" | "rejected" | "draft")
    )
}

/// Validation problems of one entry (empty = fine).
pub fn entry_problems(
    seconds: i64,
    date: &str,
    project_from: Option<&str>,
    project_to: Option<&str>,
    started_at: Option<i64>,
    ended_at: Option<i64>,
) -> Vec<String> {
    let mut out = Vec::new();
    if seconds <= 0 {
        out.push("duration must be more than zero".to_owned());
    }
    if seconds > 24 * 3600 {
        out.push("an entry cannot exceed 24 hours".to_owned());
    }
    if project_from.is_some_and(|f| date < f) {
        out.push("date is before the project starts".to_owned());
    }
    if project_to.is_some_and(|t| date > t) {
        out.push("date is after the project ends".to_owned());
    }
    match (started_at, ended_at) {
        (Some(s), Some(e)) if e <= s => out.push("end must be after start".to_owned()),
        (Some(_), None) | (None, Some(_)) => {
            out.push("give both a start and an end, or neither".to_owned());
        }
        _ => {}
    }
    out
}

/// Distinct values in first-seen order.
pub fn distinct<I: IntoIterator<Item = String>>(it: I) -> Vec<String> {
    let mut seen = HashSet::new();
    it.into_iter().filter(|x| seen.insert(x.clone())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(x: &str) -> Decimal {
        Decimal::from_str(x).unwrap()
    }

    #[test]
    fn durations_follow_everything() {
        assert_eq!(parse_duration("1h30"), Some(5400));
        assert_eq!(parse_duration("1.5"), Some(5400));
        assert_eq!(parse_duration("45"), Some(2700));
        assert_eq!(parse_duration(":45"), Some(2700));
        assert_eq!(parse_duration("1:99"), None);
        assert_eq!(fmt_duration(3599.0), "1h");
        assert_eq!(round_seconds(61, 15), 900);
        assert_eq!(round_seconds(-5, 15), 0);
    }

    #[test]
    fn rates_cascade_and_stack() {
        let v = with_rate_from(
            Some(&RateValue::Plain(500.0)),
            Some(700.0),
            Some("2026-04-01"),
            "2026-01-01",
        );
        assert_eq!(rate_at(v.as_ref(), "2026-03-31"), Some(500.0));
        assert_eq!(rate_at(v.as_ref(), "2026-04-01"), Some(700.0));
        assert_eq!(with_rate_from(v.as_ref(), None, None, "2026-01-01"), None);
        let levels = RateLevels {
            default: Some(&RateValue::Plain(300.0)),
            ..RateLevels::default()
        };
        assert!((resolve_rate(&levels, "", "2026-01-01") - 300.0).abs() < 1e-9);
    }

    #[test]
    fn effort_and_cost() {
        let e = effort(36000, 144_000);
        assert!((e.percent - 25.0).abs() < 1e-9);
        assert!((e.person_months - 0.25).abs() < 1e-9);
        assert_eq!(hourly_from_monthly(d("88000"), d("176")), Some(d("500")));
        assert_eq!(hourly_from_monthly(d("50000"), d("176")), Some(d("284.09")));
        assert_eq!(hourly_from_monthly(d("50000"), Decimal::ZERO), None);
        assert_eq!(billed_seconds(true, 180, 10, 15), 900);
        assert_eq!(entry_amount(true, 5400, d("500")), d("750"));
        assert_eq!(entry_amount(false, 5400, d("500")), Decimal::ZERO);
        assert!(timesheet_locked("approved"));
        assert!(timesheet_can("rejected", "submitted"));
        assert!(!timesheet_can("approved", "draft"));
    }

    #[test]
    fn budgets_and_windows() {
        assert_eq!(
            budget_window("monthly", "2026-02-10"),
            ("2026-02-01".into(), "2026-02-28".into())
        );
        let b = budget_progress("hours", "total", d("10"), d("18000"));
        assert!(b.is_some_and(|b| (b.ratio - 0.5).abs() < 1e-9 && b.value == d("36000")));
        assert!(budget_progress("hours", "total", Decimal::ZERO, Decimal::ONE).is_none());
        let a = budget_progress("amount", "monthly", d("1000.10"), d("250.025"));
        assert!(a.is_some_and(|b| b.spent == d("250.025") && b.value == d("1000.10")));
    }

    #[test]
    fn money_is_exact_where_floats_drift() {
        // 4517 s = 1.2547 h; × 250 = 313.675 exactly -> 313.68 (floats: 313.67499.. -> 313.67).
        assert_eq!(hours_of(4517, 4), d("1.2547"));
        assert_eq!(entry_amount(true, 4517, d("250")), d("313.68"));
        // 7 minutes at 0.30/h: the original rounds hours to 4 places first, 0.1167 × 0.30 -> 0.04.
        assert_eq!(entry_amount(true, 420, d("0.30")), d("0.04"));
        // Ten entries of 0.1 h at 1/h total exactly 1.
        let e = |i: usize| Tracked {
            id: i.to_string(),
            seconds: 360,
            billable: true,
            billed: 360,
            amount: entry_amount(true, 360, Decimal::ONE),
            started_at: None,
            ended_at: None,
        };
        let ten: Vec<Tracked> = (0..10).map(e).collect();
        assert_eq!(totals(&ten).amount, Decimal::ONE);
        // A thousand small entries sum without drift.
        let many: Vec<Tracked> = (0..1000).map(e).collect();
        assert_eq!(totals(&many).amount, d("100"));
        let groups = group_entries(&many, |_| ("k".into(), "K".into(), String::new()));
        assert_eq!(groups[0].amount, d("100"));
        // Half a paisa rounds up, as Math.round on the exact value.
        assert_eq!(round2(d("1.005")), d("1.01"));
    }
}
