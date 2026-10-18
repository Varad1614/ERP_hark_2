//! **ERP_EXTENSIONS 3A** — duty roster rules, ported from HeyPuter/everything
//! `src/shifts/src/core/scheduling.js` (validation, conflicts, availability, weekly hours,
//! move checks, ranked suggestions, coverage gaps), `rules.js` (minimum rest, daily hours,
//! consecutive days), `fill.js` (bulk staffing of open shifts), `shift-time.js` and
//! `lib/time.js` (MIT, see THIRD_PARTY_NOTICES.md).
//!
//! Dates are `YYYY-MM-DD`, times `HH:MM` in the institution's local time; a shift is always
//! shorter than 24 hours and an end before its start runs past midnight. Weeks start on Monday.
//! Pure: the HTTP layer loads shifts, staff, availability and approved leave and asks these.

use chrono::{Datelike, Duration, NaiveDate};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

// ------------------------------------------------------------------ time

fn parse(iso: &str) -> Option<NaiveDate> {
    let b = iso.as_bytes();
    let shape = b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
    if !shape {
        return None;
    }
    NaiveDate::parse_from_str(iso, "%Y-%m-%d").ok()
}

/// `isValidDate`: `YYYY-MM-DD` naming a real day.
pub fn is_valid_date(iso: &str) -> bool {
    parse(iso).is_some()
}

/// `isValidTime`: `HH:MM`, 00:00–23:59.
pub fn is_valid_time(t: &str) -> bool {
    to_minutes(t).is_some()
}

/// `toMinutes`.
pub fn to_minutes(t: &str) -> Option<i64> {
    let b = t.as_bytes();
    if b.len() != 5
        || b[2] != b':'
        || !b
            .iter()
            .enumerate()
            .all(|(i, c)| i == 2 || c.is_ascii_digit())
    {
        return None;
    }
    let h = i64::from(b[0] - b'0') * 10 + i64::from(b[1] - b'0');
    let m = i64::from(b[3] - b'0') * 10 + i64::from(b[4] - b'0');
    (h < 24 && m < 60).then_some(h * 60 + m)
}

/// `durationMinutes`: an end before the start runs past midnight; equal times are invalid.
pub fn duration_minutes(start: &str, end: &str) -> Option<i64> {
    let (s, e) = (to_minutes(start)?, to_minutes(end)?);
    if e == s {
        None
    } else if e > s {
        Some(e - s)
    } else {
        Some(e + 1440 - s)
    }
}

/// `isOvernight`.
pub fn is_overnight(start: &str, end: &str) -> bool {
    matches!((to_minutes(start), to_minutes(end)), (Some(s), Some(e)) if e < s)
}

/// `addDays`.
pub fn add_days(iso: &str, n: i64) -> String {
    parse(iso).map_or_else(
        || iso.to_owned(),
        |d| (d + Duration::days(n)).format("%Y-%m-%d").to_string(),
    )
}

/// `dayOfWeek`: 0 = Sunday.
pub fn day_of_week(iso: &str) -> u32 {
    parse(iso).map_or(0, |d| d.weekday().num_days_from_sunday())
}

/// `startOfWeek`: the Monday of the week.
pub fn start_of_week(iso: &str) -> String {
    let dow = i64::from(day_of_week(iso));
    add_days(iso, if dow == 0 { -6 } else { 1 - dow })
}

/// `weekDates`.
pub fn week_dates(start: &str) -> Vec<String> {
    (0..7).map(|i| add_days(start, i)).collect()
}

/// `dayNumber`: days since 1970-01-01.
pub fn day_number(iso: &str) -> i64 {
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap_or_default();
    parse(iso).map_or(0, |d| (d - epoch).num_days())
}

const DAY_SHORT: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTH_SHORT: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `formatDate`: "Mon, Oct 12".
pub fn format_date(iso: &str) -> String {
    parse(iso).map_or_else(
        || iso.to_owned(),
        |d| {
            format!(
                "{}, {} {}",
                DAY_SHORT[d.weekday().num_days_from_sunday() as usize],
                MONTH_SHORT[d.month0() as usize],
                d.day()
            )
        },
    )
}

/// `formatHours`: "7.5h", "8h".
pub fn format_hours(minutes: f64) -> String {
    let rounded = js_round(minutes / 60.0 * 10.0) / 10.0;
    format!("{}h", num(rounded))
}

/// `Math.round`.
pub fn js_round(x: f64) -> f64 {
    let f = x.floor();
    if x - f >= 0.5 {
        f + 1.0
    } else {
        f
    }
}

/// A number as JavaScript's `String(n)` writes it (integers without a fraction).
pub fn num(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

// ------------------------------------------------------------------ records

/// A duty shift.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Shift {
    pub id: String,
    pub date: String,
    pub start: String,
    pub end: String,
    /// Unpaid break.
    pub break_minutes: Option<f64>,
    /// The duty or role ("Warden", "Security", "Lab assistant").
    pub role: String,
    /// The staff member holding it; `None` = an open shift.
    pub assignee_id: Option<String>,
}

/// One weekday's available window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = RosterDayWindow)]
pub struct DayWindow {
    #[serde(default)]
    pub off: bool,
    #[serde(default)]
    pub start: String,
    #[serde(default)]
    pub end: String,
}

/// Time off (inclusive dates).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = RosterTimeOff)]
pub struct TimeOff {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub note: Option<String>,
}

/// Weekly availability (index 0 = Sunday) and time off.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Availability {
    pub days: Option<Vec<Option<DayWindow>>>,
    pub exceptions: Vec<TimeOff>,
}

/// A staff member as the roster sees them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Employee {
    pub id: String,
    pub name: String,
    pub role: String,
    pub active: bool,
    pub max_hours: Option<f64>,
    pub target_hours: Option<f64>,
}

/// Optional working-time limits (all off by default; warnings, never blockers).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", default)]
#[schema(as = RosterRules)]
pub struct Rules {
    pub min_rest_hours: Option<f64>,
    pub max_daily_hours: Option<f64>,
    pub max_consecutive_days: Option<f64>,
}

/// A coverage requirement: `count` people in `role` on `days` (0 = Sunday), optionally within
/// a window.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Requirement {
    pub id: String,
    pub role: String,
    pub days: Vec<u32>,
    pub count: i64,
    pub start: Option<String>,
    pub end: Option<String>,
}

// ------------------------------------------------------------------ shift time

/// `absoluteRange`: `[start, end)` minutes on an absolute timeline.
pub fn absolute_range(s: &Shift) -> Option<(i64, i64)> {
    let start = to_minutes(&s.start)?;
    let d = duration_minutes(&s.start, &s.end)?;
    if s.date.is_empty() {
        return None;
    }
    let base = day_number(&s.date) * 1440 + start;
    Some((base, base + d))
}

/// `shiftBreakMinutes`.
pub fn break_minutes(s: &Shift) -> f64 {
    s.break_minutes
        .filter(|b| b.is_finite() && *b > 0.0)
        .unwrap_or(0.0)
}

/// `shiftNetMinutes`: paid minutes.
pub fn net_minutes(s: &Shift) -> f64 {
    (duration_minutes(&s.start, &s.end).unwrap_or(0) as f64 - break_minutes(s)).max(0.0)
}

// ------------------------------------------------------------------ scheduling

/// `validateShift`: problems in words; empty = valid.
pub fn validate_shift(s: &Shift) -> Vec<String> {
    let mut errors = Vec::new();
    if !is_valid_date(&s.date) {
        errors.push("A valid date is required.".to_owned());
    }
    if !is_valid_time(&s.start) {
        errors.push("A valid start time is required.".to_owned());
    }
    if !is_valid_time(&s.end) {
        errors.push("A valid end time is required.".to_owned());
    }
    if errors.is_empty() && duration_minutes(&s.start, &s.end).is_none() {
        errors.push("End time must differ from start time.".to_owned());
    }
    if s.role.trim().is_empty() {
        errors.push("A role or label is required.".to_owned());
    }
    if let Some(b) = s.break_minutes {
        if !b.is_finite() || b < 0.0 {
            errors.push("The unpaid break must be zero or more minutes.".to_owned());
        } else if errors.is_empty() && b >= duration_minutes(&s.start, &s.end).unwrap_or(0) as f64 {
            errors.push("The unpaid break must be shorter than the shift itself.".to_owned());
        }
    }
    errors
}

/// `findConflicts`: ids of the assignee's other shifts that overlap (open shifts never conflict).
pub fn find_conflicts<'a>(
    shift: &Shift,
    all: &'a [Shift],
    assignee: Option<&str>,
) -> Vec<&'a Shift> {
    let Some(who) = assignee.filter(|a| !a.is_empty()) else {
        return Vec::new();
    };
    let Some(range) = absolute_range(shift) else {
        return Vec::new();
    };
    let before = add_days(&shift.date, -1);
    let after = add_days(&shift.date, 1);
    all.iter()
        .filter(|s| {
            if s.id == shift.id || s.assignee_id.as_deref() != Some(who) {
                return false;
            }
            if s.date != shift.date && s.date != before && s.date != after {
                return false;
            }
            absolute_range(s).is_some_and(|o| o.0 < range.1 && range.0 < o.1)
        })
        .collect()
}

/// `timeOffOn`.
pub fn time_off_on<'a>(a: Option<&'a Availability>, iso: &str) -> Option<&'a TimeOff> {
    a?.exceptions
        .iter()
        .find(|x| x.from.as_str() <= iso && iso <= x.to.as_str())
}

#[derive(PartialEq)]
enum Cover {
    Full,
    None,
    Partial,
}

fn segment_coverage(day: Option<&DayWindow>, s: i64, e: i64) -> Cover {
    if e <= s {
        return Cover::Full;
    }
    let Some(day) = day else { return Cover::Full };
    if day.off {
        return Cover::None;
    }
    let (Some(ws), Some(mut we)) = (to_minutes(&day.start), to_minutes(&day.end)) else {
        return Cover::Full;
    };
    if we == 1439 {
        we = 1440;
    }
    if s >= ws && e <= we {
        return Cover::Full;
    }
    if !(s < we && ws < e) {
        return Cover::None;
    }
    Cover::Partial
}

/// `availabilityStatus`: `available | partial | unavailable`.
pub fn availability_status(shift: &Shift, a: Option<&Availability>) -> &'static str {
    let Some(av) = a else { return "available" };
    if av.days.is_none() && av.exceptions.is_empty() {
        return "available";
    }
    let dow = day_of_week(&shift.date);
    let (Some(s), Some(e)) = (to_minutes(&shift.start), to_minutes(&shift.end)) else {
        return "available";
    };
    let segments: Vec<(String, u32, i64, i64)> = if e > s {
        vec![(shift.date.clone(), dow, s, e)]
    } else {
        vec![
            (shift.date.clone(), dow, s, 1440),
            (add_days(&shift.date, 1), (dow + 1) % 7, 0, e),
        ]
    };
    let results: Vec<Cover> = segments
        .into_iter()
        .filter(|(_, _, a, b)| b > a)
        .map(|(date, day, a, b)| {
            if time_off_on(Some(av), &date).is_some() {
                Cover::None
            } else {
                let w = av
                    .days
                    .as_ref()
                    .and_then(|d| d.get(day as usize))
                    .and_then(Option::as_ref);
                segment_coverage(w, a, b)
            }
        })
        .collect();
    if results.iter().all(|r| *r == Cover::Full) {
        "available"
    } else if results.iter().all(|r| *r == Cover::None) {
        "unavailable"
    } else {
        "partial"
    }
}

/// `weeklyMinutes`: paid minutes of an employee in the 7 days from `week_start`.
pub fn weekly_minutes(shifts: &[Shift], employee: &str, week_start: &str) -> f64 {
    let days: HashSet<String> = week_dates(week_start).into_iter().collect();
    shifts
        .iter()
        .filter(|s| s.assignee_id.as_deref() == Some(employee) && days.contains(&s.date))
        .map(net_minutes)
        .sum()
}

/// `hoursStatus`: `over | under | ok`.
pub fn hours_status(minutes: f64, e: &Employee) -> &'static str {
    if e.max_hours.is_some_and(|m| minutes > m * 60.0) {
        "over"
    } else if e.target_hours.is_some_and(|t| minutes < t * 60.0) {
        "under"
    } else {
        "ok"
    }
}

// ------------------------------------------------------------------ rules

/// A broken working-time limit.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
#[schema(as = RosterRuleWarning)]
pub struct Violation {
    /// `rest | daily | consecutive`.
    pub rule: String,
    pub short: String,
    pub message: String,
}

fn positive_or_null(v: Option<f64>, max: f64) -> Option<f64> {
    v.filter(|n| n.is_finite() && *n > 0.0 && *n <= max)
}

/// `normalizeRules`.
pub fn normalize_rules(r: &Rules) -> Rules {
    Rules {
        min_rest_hours: positive_or_null(r.min_rest_hours, 24.0),
        max_daily_hours: positive_or_null(r.max_daily_hours, 24.0),
        max_consecutive_days: positive_or_null(r.max_consecutive_days, 60.0),
    }
}

/// `hasRules`.
pub fn has_rules(r: &Rules) -> bool {
    let n = normalize_rules(r);
    n.min_rest_hours.is_some() || n.max_daily_hours.is_some() || n.max_consecutive_days.is_some()
}

/// `ruleViolations`: which limits putting `shift` on `employee` would break.
pub fn rule_violations(
    shift: &Shift,
    employee: &str,
    shifts: &[Shift],
    rules: &Rules,
    name: &str,
) -> Vec<Violation> {
    let r = normalize_rules(rules);
    if employee.is_empty() || !has_rules(&r) {
        return Vec::new();
    }
    let Some(range) = absolute_range(shift) else {
        return Vec::new();
    };
    let others: Vec<&Shift> = shifts
        .iter()
        .filter(|s| s.assignee_id.as_deref() == Some(employee) && s.id != shift.id)
        .collect();
    let mut out = Vec::new();
    if let Some(min_rest) = r.min_rest_hours {
        let earliest = add_days(&shift.date, -2);
        let latest = add_days(&shift.date, 2);
        let mut worst: Option<(i64, &Shift)> = None;
        for o in &others {
            if o.date < earliest || o.date > latest {
                continue;
            }
            let Some(or) = absolute_range(o) else {
                continue;
            };
            if or.0 < range.1 && range.0 < or.1 {
                continue;
            }
            let gap = if or.0 >= range.1 {
                or.0 - range.1
            } else {
                range.0 - or.1
            };
            if (gap as f64) < min_rest * 60.0 && worst.is_none_or(|(g, _)| gap < g) {
                worst = Some((gap, o));
            }
        }
        if let Some((gap, o)) = worst {
            let message = if gap == 0 {
                format!(
                    "{name} would work this back to back with their {} shift, with {}h rest set.",
                    format_date(&o.date),
                    num(min_rest)
                )
            } else {
                format!(
                    "{name} would get {} rest around their {} shift, under the {}h minimum.",
                    format_hours(gap as f64),
                    format_date(&o.date),
                    num(min_rest)
                )
            };
            out.push(Violation {
                rule: "rest".into(),
                short: "Short rest".into(),
                message,
            });
        }
    }
    if let Some(max_daily) = r.max_daily_hours {
        let total: f64 = others
            .iter()
            .filter(|s| s.date == shift.date)
            .map(|s| net_minutes(s))
            .sum::<f64>()
            + net_minutes(shift);
        if total > max_daily * 60.0 {
            out.push(Violation {
                rule: "daily".into(),
                short: "Long day".into(),
                message: format!(
                    "{name} would work {} on {}, over the {}h daily limit.",
                    format_hours(total),
                    format_date(&shift.date),
                    num(max_daily)
                ),
            });
        }
    }
    if let Some(max_run) = r.max_consecutive_days {
        let mut worked: HashSet<&str> = others.iter().map(|s| s.date.as_str()).collect();
        worked.insert(shift.date.as_str());
        let mut run = 1;
        let mut day = add_days(&shift.date, -1);
        let mut i = 0;
        while worked.contains(day.as_str()) && i < 400 {
            run += 1;
            day = add_days(&day, -1);
            i += 1;
        }
        let mut day = add_days(&shift.date, 1);
        let mut i = 0;
        while worked.contains(day.as_str()) && i < 400 {
            run += 1;
            day = add_days(&day, 1);
            i += 1;
        }
        if f64::from(run) > max_run {
            out.push(Violation {
                rule: "consecutive".into(),
                short: format!("{run} days in a row"),
                message: format!(
                    "{name} would be working {run} days in a row, over the {}-day limit.",
                    num(max_run)
                ),
            });
        }
    }
    out
}

/// `scheduleRuleViolations`: limits already broken by what an employee holds on `dates`, one
/// per rule.
pub fn schedule_rule_violations(
    employee: &str,
    shifts: &[Shift],
    dates: &[String],
    rules: &Rules,
    name: &str,
) -> Vec<Violation> {
    if !has_rules(rules) {
        return Vec::new();
    }
    let days: HashSet<&str> = dates.iter().map(String::as_str).collect();
    let mine: Vec<Shift> = shifts
        .iter()
        .filter(|s| s.assignee_id.as_deref() == Some(employee))
        .cloned()
        .collect();
    let mut by: Vec<Violation> = Vec::new();
    for s in mine.iter().filter(|s| days.contains(s.date.as_str())) {
        for v in rule_violations(s, employee, &mine, rules, name) {
            if !by.iter().any(|x| x.rule == v.rule) {
                by.push(v);
            }
        }
    }
    by
}

// ------------------------------------------------------------------ moves and suggestions

/// The outcome of checking a move or copy.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
#[schema(as = RosterMoveCheck)]
pub struct MoveCheck {
    pub changed: bool,
    /// Why it cannot happen (an overlap, an archived person), or null.
    pub blocked: Option<String>,
    pub warnings: Vec<String>,
}

/// What a move check reads.
#[derive(Debug, Clone)]
pub struct Context<'a> {
    pub shifts: &'a [Shift],
    pub employees: &'a [Employee],
    pub availability: &'a BTreeMap<String, Availability>,
    /// Shifts with attendance already recorded.
    pub attendance: &'a HashSet<String>,
}

/// `evaluateMove`: moving (or copying) a shift onto a day and/or a person. Overlap is the only
/// hard blocker; time off, availability, weekly maximum and recorded attendance are warnings.
pub fn evaluate_move(
    shift: &Shift,
    date: &str,
    assignee: Option<&str>,
    ctx: &Context<'_>,
    copy: bool,
) -> MoveCheck {
    let to = assignee.filter(|a| !a.is_empty());
    let from = shift.assignee_id.as_deref().filter(|a| !a.is_empty());
    let changed = copy || date != shift.date || to != from;
    let mut warnings = Vec::new();
    let target = Shift {
        date: date.to_owned(),
        assignee_id: to.map(str::to_owned),
        id: if copy {
            String::new()
        } else {
            shift.id.clone()
        },
        ..shift.clone()
    };
    let employee = to.and_then(|t| ctx.employees.iter().find(|e| e.id == t));
    if !is_valid_date(date) {
        return MoveCheck {
            changed,
            blocked: Some("That is not a valid date.".into()),
            warnings,
        };
    }
    if !changed {
        return MoveCheck {
            changed,
            blocked: None,
            warnings,
        };
    }
    if to.is_some() && employee.is_none() {
        return MoveCheck {
            changed,
            blocked: Some("That person is no longer on the team.".into()),
            warnings,
        };
    }
    if let Some(e) = employee {
        if !e.active {
            return MoveCheck {
                changed,
                blocked: Some(format!("{} is archived.", e.name)),
                warnings,
            };
        }
        if !find_conflicts(&target, ctx.shifts, to).is_empty() {
            return MoveCheck {
                changed,
                blocked: Some(format!("{} already has an overlapping shift then.", e.name)),
                warnings,
            };
        }
        let record = to.and_then(|t| ctx.availability.get(t));
        let off = time_off_on(record, date).or_else(|| {
            if is_overnight(&shift.start, &shift.end) {
                time_off_on(record, &add_days(date, 1))
            } else {
                None
            }
        });
        if let Some(t) = off {
            let note = t
                .note
                .as_deref()
                .filter(|n| !n.is_empty())
                .map(|n| format!(" ({n})"))
                .unwrap_or_default();
            warnings.push(format!("{} has time off then{note}.", e.name));
        } else {
            match availability_status(&target, record) {
                "unavailable" => {
                    warnings.push(format!("{} is marked unavailable at this time.", e.name))
                }
                "partial" => warnings.push(format!(
                    "This only partially fits {}\u{2019}s availability.",
                    e.name
                )),
                _ => {}
            }
        }
        if let Some(max) = e.max_hours {
            let others: Vec<Shift> = if copy {
                ctx.shifts.to_vec()
            } else {
                ctx.shifts
                    .iter()
                    .filter(|s| s.id != shift.id)
                    .cloned()
                    .collect()
            };
            let projected =
                weekly_minutes(&others, &e.id, &start_of_week(date)) + net_minutes(&target);
            if projected > max * 60.0 {
                warnings.push(format!(
                    "This puts {} at {} that week, over their {}h max.",
                    e.name,
                    format_hours(projected),
                    num(max)
                ));
            }
        }
    }
    if !copy && ctx.attendance.contains(&shift.id) {
        warnings.push(if to == from {
            "Attendance was already recorded for this shift.".to_owned()
        } else if let Some(e) = employee {
            format!(
                "The attendance already recorded for this shift becomes {}\u{2019}s.",
                e.name
            )
        } else {
            "Unassigning takes the attendance already recorded for this shift off the timesheet."
                .to_owned()
        });
    }
    MoveCheck {
        changed,
        blocked: None,
        warnings,
    }
}

/// A ranked candidate for a shift.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = RosterCandidate)]
pub struct Candidate {
    pub employee_id: String,
    pub name: String,
    /// `available | partial`.
    pub status: String,
    pub deficit_minutes: f64,
    pub role_match: bool,
    pub projected_minutes: f64,
    pub rule_warnings: Vec<Violation>,
    pub reasons: Vec<String>,
}

/// `suggestAssignees`: hard blockers (inactive, time off, overlap, unavailable, past the weekly
/// max) excluded; the rest ranked clean-of-rules first, then fully available, then furthest
/// below target, then role match, then name.
pub fn suggest_assignees(
    shift: &Shift,
    employees: &[Employee],
    shifts: &[Shift],
    availability: &BTreeMap<String, Availability>,
    rules: Option<&Rules>,
    limit: usize,
) -> Vec<Candidate> {
    let week = start_of_week(&shift.date);
    let others: Vec<Shift> = shifts
        .iter()
        .filter(|s| s.id != shift.id)
        .cloned()
        .collect();
    let net = net_minutes(shift);
    let role = shift.role.trim().to_lowercase();
    let mut out = Vec::new();
    for e in employees {
        if !e.active || shift.assignee_id.as_deref() == Some(e.id.as_str()) {
            continue;
        }
        let record = availability.get(&e.id);
        if time_off_on(record, &shift.date).is_some() {
            continue;
        }
        if is_overnight(&shift.start, &shift.end)
            && time_off_on(record, &add_days(&shift.date, 1)).is_some()
        {
            continue;
        }
        let status = availability_status(shift, record);
        if status == "unavailable" {
            continue;
        }
        if !find_conflicts(shift, &others, Some(&e.id)).is_empty() {
            continue;
        }
        let current = weekly_minutes(&others, &e.id, &week);
        let projected = current + net;
        if e.max_hours.is_some_and(|m| projected > m * 60.0) {
            continue;
        }
        let deficit = e
            .target_hours
            .map_or(0.0, |t| (t * 60.0 - current).max(0.0));
        let role_match = !role.is_empty() && e.role.trim().to_lowercase() == role;
        let warnings = rules.map_or_else(Vec::new, |r| {
            rule_violations(shift, &e.id, &others, r, &e.name)
        });
        let mut reasons = vec![if status == "available" {
            "Available"
        } else {
            "Partially available"
        }
        .to_owned()];
        if deficit > 0.0 {
            reasons.push(format!("{} below target", format_hours(deficit)));
        }
        if role_match {
            reasons.push("Role matches".into());
        }
        reasons.extend(warnings.iter().map(|v| v.short.clone()));
        out.push(Candidate {
            employee_id: e.id.clone(),
            name: e.name.clone(),
            status: status.into(),
            deficit_minutes: deficit,
            role_match,
            projected_minutes: projected,
            rule_warnings: warnings,
            reasons,
        });
    }
    out.sort_by(|a, b| {
        (!a.rule_warnings.is_empty())
            .cmp(&!b.rule_warnings.is_empty())
            .then_with(|| (a.status != "available").cmp(&(b.status != "available")))
            .then_with(|| {
                b.deficit_minutes
                    .partial_cmp(&a.deficit_minutes)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| b.role_match.cmp(&a.role_match))
            .then_with(|| a.name.cmp(&b.name))
    });
    out.truncate(limit);
    out
}

/// A staffing gap.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = RosterCoverageGap)]
pub struct Gap {
    pub date: String,
    pub requirement_id: String,
    pub role: String,
    pub scheduled: i64,
    pub missing: i64,
}

/// `coverageGaps`: per date and requirement, how many shifts of its role overlap its window
/// (open shifts count; overnight spill from the day before counts).
pub fn coverage_gaps(shifts: &[Shift], reqs: &[Requirement], dates: &[String]) -> Vec<Gap> {
    let mut gaps = Vec::new();
    if reqs.is_empty() || dates.is_empty() {
        return gaps;
    }
    let norm = |s: &str| s.trim().to_lowercase();
    let prepared: Vec<(&Requirement, i64, i64, String)> = reqs
        .iter()
        .map(|r| {
            let mut ws = r
                .start
                .as_deref()
                .filter(|s| !s.is_empty())
                .map_or(0, |s| to_minutes(s).unwrap_or(0));
            let mut we = r
                .end
                .as_deref()
                .filter(|s| !s.is_empty())
                .map_or(1440, |s| to_minutes(s).unwrap_or(1440));
            if we == 1439 {
                we = 1440;
            }
            if we <= ws {
                ws = 0;
                we = 1440;
            }
            (r, ws, we, norm(&r.role))
        })
        .collect();
    let wanted: HashSet<&str> = prepared.iter().map(|p| p.3.as_str()).collect();
    let mut by_role: BTreeMap<String, Vec<(i64, i64)>> = BTreeMap::new();
    for s in shifts {
        let role = norm(&s.role);
        if !wanted.contains(role.as_str()) {
            continue;
        }
        if let Some(r) = absolute_range(s) {
            by_role.entry(role).or_default().push(r);
        }
    }
    for date in dates {
        let dow = day_of_week(date);
        let base = day_number(date) * 1440;
        for (req, ws, we, role) in &prepared {
            if !req.days.contains(&dow) {
                continue;
            }
            let (lo, hi) = (base + ws, base + we);
            let scheduled = by_role
                .get(role)
                .map_or(0, |v| v.iter().filter(|r| r.0 < hi && lo < r.1).count())
                as i64;
            if scheduled < req.count {
                gaps.push(Gap {
                    date: date.clone(),
                    requirement_id: req.id.clone(),
                    role: req.role.clone(),
                    scheduled,
                    missing: req.count - scheduled,
                });
            }
        }
    }
    gaps
}

// ------------------------------------------------------------------ fill

/// One row of a fill plan.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = RosterFillRow)]
pub struct FillRow {
    pub shift_id: String,
    pub candidates: Vec<Candidate>,
    /// The employee to assign, or null when nobody is a clean fit (or the row is skipped).
    pub proposal: Option<String>,
    pub skipped: bool,
    /// A pinned pick an earlier row made impossible.
    pub stale: bool,
    pub pinned: bool,
}

/// `planFill`: proposals for every open shift on `dates`, in the order they are worked; each
/// accepted proposal is written into a working copy before the next shift is ranked. `choices`
/// pins a shift to an employee, or to `None` to leave it open.
pub fn plan_fill(
    shifts: &[Shift],
    employees: &[Employee],
    availability: &BTreeMap<String, Availability>,
    rules: Option<&Rules>,
    dates: &[String],
    limit: usize,
    choices: &BTreeMap<String, Option<String>>,
) -> Vec<FillRow> {
    let days: HashSet<&str> = dates.iter().map(String::as_str).collect();
    let mut open: Vec<&Shift> = shifts
        .iter()
        .filter(|s| {
            s.assignee_id.as_deref().is_none_or(str::is_empty) && days.contains(s.date.as_str())
        })
        .collect();
    open.sort_by(|a, b| {
        a.date
            .cmp(&b.date)
            .then_with(|| {
                to_minutes(&a.start)
                    .unwrap_or(0)
                    .cmp(&to_minutes(&b.start).unwrap_or(0))
            })
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut working: Vec<Shift> = shifts.to_vec();
    let mut plan = Vec::new();
    for shift in open {
        let candidates = suggest_assignees(shift, employees, &working, availability, rules, limit);
        let pinned = choices.get(&shift.id);
        let (mut proposal, mut stale) = (None, false);
        match pinned {
            None => proposal = candidates.first().map(|c| c.employee_id.clone()),
            Some(Some(p)) => {
                proposal = candidates
                    .iter()
                    .find(|c| &c.employee_id == p)
                    .map(|c| c.employee_id.clone());
                stale = proposal.is_none();
            }
            Some(None) => {}
        }
        if let Some(p) = &proposal {
            for s in working.iter_mut().filter(|s| s.id == shift.id) {
                s.assignee_id = Some(p.clone());
            }
        }
        plan.push(FillRow {
            shift_id: shift.id.clone(),
            candidates,
            proposal,
            skipped: matches!(pinned, Some(None)),
            stale,
            pinned: pinned.is_some(),
        });
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(id: &str, date: &str, s: &str, e: &str, who: Option<&str>) -> Shift {
        Shift {
            id: id.into(),
            date: date.into(),
            start: s.into(),
            end: e.into(),
            role: "Warden".into(),
            assignee_id: who.map(str::to_owned),
            ..Shift::default()
        }
    }

    #[test]
    fn overnight_conflicts_rest_and_fill() {
        let night = sh("a", "2026-10-12", "22:00", "06:00", Some("e1"));
        let morning = sh("b", "2026-10-13", "05:00", "09:00", Some("e1"));
        let late = sh("c", "2026-10-13", "10:00", "14:00", None);
        let all = vec![night.clone(), morning.clone(), late.clone()];
        assert_eq!(find_conflicts(&morning, &all, Some("e1")).len(), 1);
        assert_eq!(format_date("2026-10-12"), "Mon, Oct 12");
        assert_eq!(format_hours(450.0), "7.5h");
        let rules = Rules {
            min_rest_hours: Some(11.0),
            ..Rules::default()
        };
        let v = rule_violations(&late, "e1", std::slice::from_ref(&night), &rules, "Asha");
        assert_eq!(
            v[0].message,
            "Asha would get 4h rest around their Mon, Oct 12 shift, under the 11h minimum."
        );
        let emps = vec![
            Employee {
                id: "e1".into(),
                name: "Asha".into(),
                active: true,
                ..Employee::default()
            },
            Employee {
                id: "e2".into(),
                name: "Bilal".into(),
                active: true,
                target_hours: Some(10.0),
                ..Employee::default()
            },
        ];
        let plan = plan_fill(
            &all,
            &emps,
            &BTreeMap::new(),
            Some(&rules),
            &["2026-10-13".into()],
            3,
            &BTreeMap::new(),
        );
        assert_eq!(plan[0].proposal.as_deref(), Some("e2"));
        assert_eq!(start_of_week("2026-10-18"), "2026-10-12");
    }
}
