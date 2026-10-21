//! **ERP_EXTENSIONS 5B** — timesheets, labour cost and the payroll export, ported from
//! HeyPuter/everything `src/shifts/src/core/timesheet.js` and `payroll.js` (MIT, see
//! THIRD_PARTY_NOTICES.md), plus SutraERP's reconciliation of shift punches with the 7B daily
//! attendance and the Indian working-time rules that feed 7C payroll.
//!
//! Everything part (golden-tested): one row per assigned shift with scheduled and worked hours
//! net of the unpaid break, lateness across midnight, pay that follows the best information
//! (absence pays nothing, complete punches pay worked time, anything else the schedule), the
//! per-person summary, the formula-safe CSV, and weekly-threshold overtime cost.
//!
//! SutraERP part: [`reconcile`] adds early leave, missed punches, the day's attendance (leave,
//! absence, holiday), night shifts (Factories Act 1948 s.57: a shift past midnight), holiday
//! shifts, overtime (s.51/s.54: over 9 hours a day or 48 a week by default) and loss-of-pay days
//! that daily attendance does not already carry.

use super::clock::{early_leave_minutes, late_minutes, worked_minutes, Record, ABSENT};
use super::rules::{duration_minutes, is_overnight, num, start_of_week, Shift};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize, Serializer};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use sutra_kernel::money::js_round_dp;

// Hours, rates, pay and cost are `Decimal` (audit M2): arithmetic is exact and rounding is
// Everything's `Math.round` applied to the exact value. They still go on the wire as JSON numbers
// (`rust_decimal::serde::float`), as before.

const SIXTY: Decimal = Decimal::from_parts(60, 0, 0, false, 0);

/// `round2`: `Math.round(n * 100) / 100`, exact.
pub fn round2(n: Decimal) -> Decimal {
    js_round_dp(n, 2)
}

/// Minutes as hours to two places (`round2(minutes / 60)`).
pub fn hours(minutes: Decimal) -> Decimal {
    round2(minutes / SIXTY)
}

/// `minutes / 60 * rate`, multiplied first so a whole-cent result stays exact.
pub fn minutes_cost(minutes: Decimal, rate: Decimal) -> Decimal {
    minutes * rate / SIXTY
}

/// `shiftBreakMinutes` as an exact decimal: a missing, non-finite or non-positive break is no
/// break (Everything's rule, not a conversion default).
pub fn break_dec(s: &Shift) -> Decimal {
    s.break_minutes
        .filter(|b| b.is_finite() && *b > 0.0)
        .and_then(Decimal::from_f64)
        .unwrap_or(Decimal::ZERO)
}

/// `shiftNetMinutes` as an exact decimal. A finite break too large for `Decimal` is longer than
/// any shift, so nothing is paid (what the float version computed too).
pub fn net_dec(s: &Shift) -> Decimal {
    let dur = Decimal::from(duration_minutes(&s.start, &s.end).unwrap_or(0));
    match s.break_minutes.filter(|b| b.is_finite() && *b > 0.0) {
        None => dur,
        Some(b) => Decimal::from_f64(b).map_or(Decimal::ZERO, |b| (dur - b).max(Decimal::ZERO)),
    }
}

/// Serialises `None` as `""` — Everything's blank cell — and a value as a JSON number.
pub fn blank<S: Serializer>(v: &Option<Decimal>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(x) => rust_decimal::serde::float::serialize(x, s),
        None => s.serialize_str(""),
    }
}

/// `blank` for whole-number cells (minutes late).
pub fn blank_f64<S: Serializer>(v: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(x) => s.serialize_f64(*x),
        None => s.serialize_str(""),
    }
}

/// A staff member as timesheets and cost see them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PayEmployee {
    pub id: String,
    pub name: String,
    pub active: bool,
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub hourly_rate: Option<Decimal>,
}

/// One payroll-ready row (`timesheetRows`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub date: String,
    pub employee_id: String,
    pub employee: String,
    pub role: String,
    pub start: String,
    pub end: String,
    #[serde(with = "rust_decimal::serde::float")]
    pub break_minutes: Decimal,
    #[serde(with = "rust_decimal::serde::float")]
    pub scheduled_hours: Decimal,
    pub status: String,
    pub clock_in: String,
    pub clock_out: String,
    #[serde(serialize_with = "blank")]
    pub worked_hours: Option<Decimal>,
    #[serde(serialize_with = "blank_f64")]
    pub minutes_late: Option<f64>,
    #[serde(serialize_with = "blank")]
    pub hourly_rate: Option<Decimal>,
    #[serde(serialize_with = "blank")]
    pub pay: Option<Decimal>,
}

fn assigned(s: &Shift) -> bool {
    s.assignee_id.as_deref().is_some_and(|a| !a.is_empty())
}

/// `timesheetRows`: assigned shifts in `[from, to]`, by date, start, then name.
pub fn timesheet_rows(
    employees: &[PayEmployee],
    shifts: &[Shift],
    attendance: &BTreeMap<String, Record>,
    from: &str,
    to: &str,
) -> Vec<Row> {
    let by_id: HashMap<&str, &PayEmployee> = employees.iter().map(|e| (e.id.as_str(), e)).collect();
    let mut rows: Vec<Row> = shifts
        .iter()
        .filter(|s| assigned(s) && s.date.as_str() >= from && s.date.as_str() <= to)
        .map(|shift| {
            let who = shift.assignee_id.clone().unwrap_or_default();
            let employee = by_id.get(who.as_str());
            let record = attendance.get(&shift.id);
            let brk = break_dec(shift);
            let absent = record.is_some_and(Record::is_absent);
            let gross = match record {
                Some(r) if !absent => worked_minutes(
                    r.clock_in.as_deref().unwrap_or(""),
                    r.clock_out.as_deref().unwrap_or(""),
                ),
                _ => None,
            };
            let worked = gross.map(|g| (Decimal::from(g) - brk).max(Decimal::ZERO));
            let rate = employee.and_then(|e| e.hourly_rate);
            let paid = if absent {
                Decimal::ZERO
            } else {
                worked.unwrap_or_else(|| net_dec(shift))
            };
            let text = |v: Option<&String>| {
                if absent {
                    String::new()
                } else {
                    v.cloned().unwrap_or_default()
                }
            };
            Row {
                date: shift.date.clone(),
                employee_id: who.clone(),
                employee: employee.map_or_else(|| "Former employee".to_owned(), |e| e.name.clone()),
                role: shift.role.clone(),
                start: shift.start.clone(),
                end: shift.end.clone(),
                break_minutes: brk,
                scheduled_hours: hours(net_dec(shift)),
                status: record
                    .and_then(|r| r.status.clone())
                    .unwrap_or_else(|| "not recorded".to_owned()),
                clock_in: text(record.and_then(|r| r.clock_in.as_ref())),
                clock_out: text(record.and_then(|r| r.clock_out.as_ref())),
                worked_hours: worked.map(hours),
                minutes_late: match record {
                    Some(r) if !absent => Some(late_minutes(
                        &shift.start,
                        &shift.end,
                        r.clock_in.as_deref().unwrap_or(""),
                    ) as f64),
                    _ => None,
                },
                hourly_rate: rate,
                pay: rate.map(|r| round2(minutes_cost(paid, r))),
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        a.date
            .cmp(&b.date)
            .then_with(|| a.start.cmp(&b.start))
            .then_with(|| a.employee.cmp(&b.employee))
    });
    rows
}

/// Per-person totals (`employeeSummaries`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub employee_id: String,
    pub employee: String,
    pub shifts: i64,
    #[serde(with = "rust_decimal::serde::float")]
    pub scheduled_hours: Decimal,
    #[serde(with = "rust_decimal::serde::float")]
    pub worked_hours: Decimal,
    pub present: i64,
    pub late: i64,
    pub absent: i64,
    pub not_recorded: i64,
    #[serde(serialize_with = "blank")]
    pub pay: Option<Decimal>,
}

/// `employeeSummaries`: keyed by id (two people sharing a name stay apart), sorted by name.
pub fn employee_summaries(rows: &[Row]) -> Vec<Summary> {
    let mut order: Vec<Summary> = Vec::new();
    for row in rows {
        let i = match order.iter().position(|s| s.employee_id == row.employee_id) {
            Some(i) => i,
            None => {
                order.push(Summary {
                    employee_id: row.employee_id.clone(),
                    employee: row.employee.clone(),
                    shifts: 0,
                    scheduled_hours: Decimal::ZERO,
                    worked_hours: Decimal::ZERO,
                    present: 0,
                    late: 0,
                    absent: 0,
                    not_recorded: 0,
                    pay: None,
                });
                order.len() - 1
            }
        };
        let e = &mut order[i];
        e.shifts += 1;
        e.scheduled_hours = round2(e.scheduled_hours + row.scheduled_hours);
        e.worked_hours = round2(e.worked_hours + row.worked_hours.unwrap_or(Decimal::ZERO));
        if let Some(p) = row.pay {
            e.pay = Some(round2(e.pay.unwrap_or(Decimal::ZERO) + p));
        }
        match row.status.as_str() {
            "present" => e.present += 1,
            "late" => e.late += 1,
            "absent" => e.absent += 1,
            _ => e.not_recorded += 1,
        }
    }
    order.sort_by(|a, b| a.employee.cmp(&b.employee));
    order
}

/// A CSV cell: text written by people is formula-guarded; numbers pass through.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Text(String),
    Num(f64),
    /// An exact amount or hours, written as JavaScript's `String(n)` would.
    Dec(Decimal),
}

impl From<&str> for Cell {
    fn from(s: &str) -> Self {
        Cell::Text(s.to_owned())
    }
}
impl From<String> for Cell {
    fn from(s: String) -> Self {
        Cell::Text(s)
    }
}
impl From<f64> for Cell {
    fn from(x: f64) -> Self {
        Cell::Num(x)
    }
}
impl From<i64> for Cell {
    fn from(x: i64) -> Self {
        Cell::Num(x as f64)
    }
}
impl From<Decimal> for Cell {
    fn from(x: Decimal) -> Self {
        Cell::Dec(x)
    }
}
impl From<Option<Decimal>> for Cell {
    fn from(x: Option<Decimal>) -> Self {
        x.map_or(Cell::Text(String::new()), Cell::Dec)
    }
}
impl From<Option<f64>> for Cell {
    fn from(x: Option<f64>) -> Self {
        x.map_or(Cell::Text(String::new()), Cell::Num)
    }
}

/// `csvField`: a leading `= + - @ TAB CR` in text gets an apostrophe (spreadsheet formula
/// injection); quotes when the cell holds `" , CR LF`.
pub fn csv_field(c: &Cell) -> String {
    let mut s = match c {
        Cell::Text(t) => t.clone(),
        Cell::Num(n) => num(*n),
        // JavaScript prints `-0` as `0`.
        Cell::Dec(d) if d.is_zero() => "0".to_owned(),
        Cell::Dec(d) => d.normalize().to_string(),
    };
    if matches!(c, Cell::Text(_)) && s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        s = format!("'{s}");
    }
    if s.contains(['"', ',', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

/// `toCSV`: header row plus rows, CRLF line endings.
pub fn to_csv(headers: &[&str], rows: &[Vec<Cell>]) -> String {
    let mut lines = vec![headers
        .iter()
        .map(|h| csv_field(&Cell::from(*h)))
        .collect::<Vec<_>>()
        .join(",")];
    for r in rows {
        lines.push(r.iter().map(csv_field).collect::<Vec<_>>().join(","));
    }
    lines.join("\r\n")
}

const ROW_HEADERS: [&str; 14] = [
    "Date",
    "Employee",
    "Role",
    "Scheduled start",
    "Scheduled end",
    "Break minutes",
    "Scheduled hours",
    "Status",
    "Clock in",
    "Clock out",
    "Worked hours",
    "Minutes late",
    "Hourly rate",
    "Pay",
];

const SUMMARY_HEADERS: [&str; 9] = [
    "Employee",
    "Shifts",
    "Scheduled hours",
    "Worked hours",
    "Present",
    "Late",
    "Absent",
    "Not recorded",
    "Pay",
];

/// `timesheetCSV`: detail table, blank line, per-person summary.
pub fn timesheet_csv(
    employees: &[PayEmployee],
    shifts: &[Shift],
    attendance: &BTreeMap<String, Record>,
    from: &str,
    to: &str,
) -> String {
    let rows = timesheet_rows(employees, shifts, attendance, from, to);
    let detail: Vec<Vec<Cell>> = rows
        .iter()
        .map(|r| {
            vec![
                r.date.clone().into(),
                r.employee.clone().into(),
                r.role.clone().into(),
                r.start.clone().into(),
                r.end.clone().into(),
                r.break_minutes.into(),
                r.scheduled_hours.into(),
                r.status.clone().into(),
                r.clock_in.clone().into(),
                r.clock_out.clone().into(),
                r.worked_hours.into(),
                r.minutes_late.into(),
                r.hourly_rate.into(),
                r.pay.into(),
            ]
        })
        .collect();
    let summary: Vec<Vec<Cell>> = employee_summaries(&rows)
        .iter()
        .map(|s| {
            vec![
                s.employee.clone().into(),
                s.shifts.into(),
                s.scheduled_hours.into(),
                s.worked_hours.into(),
                s.present.into(),
                s.late.into(),
                s.absent.into(),
                s.not_recorded.into(),
                s.pay.into(),
            ]
        })
        .collect();
    format!(
        "{}\r\n\r\n{}\r\n",
        to_csv(&ROW_HEADERS, &detail),
        to_csv(&SUMMARY_HEADERS, &summary)
    )
}

// ------------------------------------------------------------------ labour cost (payroll.js)

/// One breakdown entry.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Bucket {
    pub key: String,
    #[serde(with = "rust_decimal::serde::float")]
    pub minutes: Decimal,
    #[serde(with = "rust_decimal::serde::float")]
    pub cost: Decimal,
}

/// `laborCost`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaborCost {
    #[serde(with = "rust_decimal::serde::float")]
    pub cost: Decimal,
    #[serde(with = "rust_decimal::serde::float")]
    pub base_cost: Decimal,
    #[serde(with = "rust_decimal::serde::float")]
    pub overtime_cost: Decimal,
    #[serde(with = "rust_decimal::serde::float")]
    pub minutes: Decimal,
    #[serde(with = "rust_decimal::serde::float")]
    pub overtime_minutes: Decimal,
    pub rated_count: usize,
    pub employee_count: usize,
    pub by_day: Vec<Bucket>,
    pub by_role: Vec<Bucket>,
    pub by_employee: Vec<Bucket>,
}

#[derive(Default)]
struct Buckets(Vec<Bucket>);

impl Buckets {
    fn add(&mut self, key: &str, net: Decimal, money: Decimal) {
        match self.0.iter_mut().find(|b| b.key == key) {
            Some(b) => {
                b.minutes += net;
                b.cost += money;
            }
            None => self.0.push(Bucket {
                key: key.to_owned(),
                minutes: net,
                cost: money,
            }),
        }
    }
}

fn desc(a: Decimal, b: Decimal) -> std::cmp::Ordering {
    b.cmp(&a)
}

/// `laborCost`: net hours × the holder's hourly rate over `dates`; with a weekly threshold and a
/// multiplier above 1, minutes past the threshold (in the order shifts are worked, each week on
/// its own, counted over the whole week) cost the multiplier.
pub fn labor_cost(
    shifts: &[Shift],
    employees: &[PayEmployee],
    dates: &[String],
    threshold_hours: Option<Decimal>,
    multiplier: Option<Decimal>,
) -> LaborCost {
    let days: HashSet<&str> = dates.iter().map(String::as_str).collect();
    let by_id: HashMap<&str, &PayEmployee> = employees.iter().map(|e| (e.id.as_str(), e)).collect();
    let threshold = threshold_hours.map(|h| h * SIXTY);
    let overtime_on = matches!((threshold, multiplier), (Some(_), Some(m)) if m > Decimal::ONE);
    let rate = multiplier.unwrap_or(Decimal::ONE);
    let weeks: HashSet<String> = days.iter().map(|d| start_of_week(d)).collect();
    let mut ordered: Vec<&Shift> = shifts
        .iter()
        .filter(|s| {
            assigned(s)
                && (days.contains(s.date.as_str())
                    || (overtime_on && weeks.contains(&start_of_week(&s.date))))
        })
        .collect();
    ordered.sort_by(|a, b| {
        a.date
            .cmp(&b.date)
            .then_with(|| a.start.cmp(&b.start))
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut involved: HashSet<&str> = HashSet::new();
    let mut rated: HashSet<&str> = HashSet::new();
    let (mut by_day, mut by_role, mut by_emp) =
        (Buckets::default(), Buckets::default(), Buckets::default());
    let mut week_minutes: HashMap<String, Decimal> = HashMap::new();
    let z = Decimal::ZERO;
    let (mut cost, mut base_cost, mut overtime_cost, mut minutes, mut overtime_minutes) =
        (z, z, z, z, z);
    for shift in ordered {
        let who = shift.assignee_id.as_deref().unwrap_or("");
        let net = net_dec(shift);
        let key = format!("{who}|{}", start_of_week(&shift.date));
        let before = week_minutes.get(&key).copied().unwrap_or(Decimal::ZERO);
        week_minutes.insert(key, before + net);
        if !days.contains(shift.date.as_str()) {
            continue;
        }
        involved.insert(who);
        minutes += net;
        let mut money = Decimal::ZERO;
        if let Some((e, hr)) = by_id.get(who).and_then(|e| e.hourly_rate.map(|r| (e, r))) {
            rated.insert(e.id.as_str());
            let mut overtime = Decimal::ZERO;
            if overtime_on {
                let t = threshold.unwrap_or(Decimal::ZERO);
                overtime = (before + net - t).max(z) - (before - t).max(z);
            }
            let base = net - overtime;
            let base_money = minutes_cost(base, hr);
            let overtime_money =
                minutes_cost(overtime, hr * if overtime_on { rate } else { Decimal::ONE });
            money = base_money + overtime_money;
            base_cost += base_money;
            overtime_cost += overtime_money;
            overtime_minutes += overtime;
            cost += money;
        }
        by_day.add(&shift.date, net, money);
        let role = shift.role.trim();
        by_role.add(if role.is_empty() { "No role" } else { role }, net, money);
        by_emp.add(who, net, money);
    }
    let mut by_day = by_day.0;
    by_day.sort_by(|a, b| a.key.cmp(&b.key));
    let mut by_role = by_role.0;
    by_role.sort_by(|a, b| {
        desc(a.cost, b.cost)
            .then_with(|| desc(a.minutes, b.minutes))
            .then_with(|| a.key.cmp(&b.key))
    });
    let mut by_employee = by_emp.0;
    by_employee.sort_by(|a, b| desc(a.cost, b.cost).then_with(|| desc(a.minutes, b.minutes)));
    LaborCost {
        cost,
        base_cost,
        overtime_cost,
        minutes,
        overtime_minutes,
        rated_count: rated.len(),
        employee_count: involved.len(),
        by_day,
        by_role,
        by_employee,
    }
}

/// `budgetStatus`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Budget {
    #[serde(with = "rust_decimal::serde::float_option")]
    pub budget: Option<Decimal>,
    #[serde(with = "rust_decimal::serde::float_option")]
    pub ratio: Option<Decimal>,
    pub over: bool,
    #[serde(with = "rust_decimal::serde::float")]
    pub difference: Decimal,
}

/// `budgetStatus`: a cost against an optional weekly budget (none set = nothing to show).
pub fn budget_status(cost: Decimal, budget: Option<Decimal>) -> Budget {
    match budget.filter(|b| *b > Decimal::ZERO) {
        None => Budget {
            budget: None,
            ratio: None,
            over: false,
            difference: Decimal::ZERO,
        },
        Some(t) => Budget {
            budget: Some(t),
            ratio: Some(cost / t),
            over: cost > t,
            difference: cost - t,
        },
    }
}

// ------------------------------------------------------------------ SutraERP reconciliation

/// Working-time settings of the reconciliation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    /// Minutes after the start that still count as on time.
    pub late_grace_minutes: i64,
    /// Minutes before the end that do not count as leaving early.
    pub early_leave_grace_minutes: i64,
    /// Paid hours a day before overtime (Factories Act s.54: 9); `None` = no daily limit.
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub daily_overtime_hours: Option<Decimal>,
    /// Paid hours a week before overtime (s.51: 48); `None` = no weekly limit.
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub weekly_overtime_hours: Option<Decimal>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            late_grace_minutes: 10,
            early_leave_grace_minutes: 10,
            daily_overtime_hours: Some(Decimal::from(9)),
            weekly_overtime_hours: Some(Decimal::from(48)),
        }
    }
}

/// One shift of a staff member with what is known about it.
#[derive(Debug, Clone)]
pub struct Actual<'a> {
    pub shift: &'a Shift,
    pub punch: Option<&'a Record>,
    /// The 7B attendance day of the shift's date (`present | absent | half_day | on_leave |
    /// holiday | weekly_off`).
    pub day: Option<&'a str>,
    /// The date is a holiday of the active leave policy.
    pub holiday: bool,
}

/// A reconciled shift line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = RosterTimesheetLine)]
pub struct Line {
    pub shift_id: String,
    pub date: String,
    pub start: String,
    pub end: String,
    pub role: String,
    #[serde(with = "rust_decimal::serde::float")]
    #[schema(value_type = f64)]
    pub scheduled_minutes: Decimal,
    /// Paid minutes: punched time net of the break; the schedule when punches are incomplete;
    /// nothing for an absence or leave; half for a half day.
    #[serde(with = "rust_decimal::serde::float")]
    #[schema(value_type = f64)]
    pub worked_minutes: Decimal,
    pub clock_in: Option<String>,
    pub clock_out: Option<String>,
    /// `present | late | absent | half_day | on_leave | missed_punch | not_recorded | upcoming`.
    pub status: String,
    pub late_minutes: i64,
    pub early_leave_minutes: i64,
    pub night: bool,
    pub holiday: bool,
    /// `late | early_leave | missed_punch | not_recorded | absent | on_leave |
    /// day_absent_but_punched | holiday`.
    pub flags: Vec<String>,
}

/// Totals of a reconciled period.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", default)]
#[schema(as = RosterTimesheetTotals)]
pub struct Totals {
    pub shifts: i64,
    #[serde(with = "rust_decimal::serde::float")]
    #[schema(value_type = f64)]
    pub scheduled_minutes: Decimal,
    #[serde(with = "rust_decimal::serde::float")]
    #[schema(value_type = f64)]
    pub worked_minutes: Decimal,
    #[serde(with = "rust_decimal::serde::float")]
    #[schema(value_type = f64)]
    pub overtime_minutes: Decimal,
    pub late_count: i64,
    pub late_minutes: i64,
    pub early_leave_count: i64,
    pub early_leave_minutes: i64,
    pub missed_punches: i64,
    pub not_recorded: i64,
    pub absences: i64,
    /// Loss-of-pay days from shift absences that daily attendance does not already show as
    /// absent (those reach payroll through the leave reader; counting them here would charge
    /// them twice).
    #[serde(with = "rust_decimal::serde::float")]
    #[schema(value_type = f64)]
    pub lop_days: Decimal,
    pub night_shifts: i64,
    pub holiday_shifts: i64,
}

/// Reconciles one staff member's shifts with punches and daily attendance. Shifts after `today`
/// without punches are `upcoming` and count for nothing.
pub fn reconcile(actuals: &[Actual<'_>], limits: &Limits, today: &str) -> Vec<Line> {
    let mut out: Vec<Line> = actuals.iter().map(|a| line(a, limits, today)).collect();
    out.sort_by(|a, b| {
        a.date
            .cmp(&b.date)
            .then_with(|| a.start.cmp(&b.start))
            .then_with(|| a.shift_id.cmp(&b.shift_id))
    });
    out
}

fn line(a: &Actual<'_>, l: &Limits, today: &str) -> Line {
    let s = a.shift;
    let scheduled = net_dec(s);
    let brk = break_dec(s);
    let mut flags: Vec<String> = Vec::new();
    let clock_in = a
        .punch
        .and_then(|p| p.clock_in.clone())
        .filter(|x| !x.is_empty());
    let clock_out = a
        .punch
        .and_then(|p| p.clock_out.clone())
        .filter(|x| !x.is_empty());
    let punch_absent = a.punch.is_some_and(Record::is_absent);
    let day = a.day.unwrap_or("");
    let (mut late, mut early) = (0, 0);
    let mut late_check = |i: &str, flags: &mut Vec<String>| {
        let raw = late_minutes(&s.start, &s.end, i);
        if raw > l.late_grace_minutes {
            late = raw;
            flags.push("late".into());
        }
    };
    let (status, worked) = if s.date.as_str() > today && a.punch.is_none() {
        ("upcoming", Decimal::ZERO)
    } else if day == "on_leave" && clock_in.is_none() {
        flags.push("on_leave".into());
        ("on_leave", Decimal::ZERO)
    } else if punch_absent || (a.punch.is_none() && day == ABSENT) {
        flags.push("absent".into());
        (ABSENT, Decimal::ZERO)
    } else if let (Some(i), Some(o)) = (&clock_in, &clock_out) {
        let w = worked_minutes(i, o).map_or(Decimal::ZERO, |g| {
            (Decimal::from(g) - brk).max(Decimal::ZERO)
        });
        late_check(i, &mut flags);
        let raw_early = early_leave_minutes(&s.start, &s.end, o);
        if raw_early > l.early_leave_grace_minutes {
            early = raw_early;
            flags.push("early_leave".into());
        }
        (
            if flags.iter().any(|f| f == "late") {
                "late"
            } else {
                "present"
            },
            w,
        )
    } else if clock_in.is_some() || clock_out.is_some() {
        if let Some(i) = &clock_in {
            late_check(i, &mut flags);
        }
        flags.push("missed_punch".into());
        ("missed_punch", scheduled)
    } else if day == "half_day" {
        ("half_day", scheduled / Decimal::TWO)
    } else if matches!(day, "present" | "holiday" | "weekly_off") || a.punch.is_some() {
        // A day marked present (or a holiday duty) without both punches.
        flags.push("missed_punch".into());
        ("missed_punch", scheduled)
    } else {
        flags.push("not_recorded".into());
        ("not_recorded", scheduled)
    };
    if day == ABSENT && clock_in.is_some() {
        flags.push("day_absent_but_punched".into());
    }
    let worked_any = worked > Decimal::ZERO;
    let holiday = (a.holiday || day == "holiday") && worked_any;
    if holiday {
        flags.push("holiday".into());
    }
    Line {
        shift_id: s.id.clone(),
        date: s.date.clone(),
        start: s.start.clone(),
        end: s.end.clone(),
        role: s.role.clone(),
        scheduled_minutes: scheduled,
        worked_minutes: worked,
        clock_in,
        clock_out,
        status: status.to_owned(),
        late_minutes: late,
        early_leave_minutes: early,
        night: worked_any && is_overnight(&s.start, &s.end),
        holiday,
        flags,
    }
}

/// Totals of reconciled lines: overtime per week is the larger of the daily excess (over the
/// daily limit) and the weekly excess (over the weekly limit), each week counted on its own.
pub fn totals(lines: &[Line], limits: &Limits, days: &HashMap<String, String>) -> Totals {
    let mut t = Totals::default();
    let mut per_day: BTreeMap<String, Decimal> = BTreeMap::new();
    let mut absent_days: BTreeSet<&str> = BTreeSet::new();
    let mut worked_days: HashSet<&str> = HashSet::new();
    for l in lines {
        if l.status == "upcoming" {
            continue;
        }
        t.shifts += 1;
        t.scheduled_minutes += l.scheduled_minutes;
        t.worked_minutes += l.worked_minutes;
        *per_day.entry(l.date.clone()).or_default() += l.worked_minutes;
        if l.late_minutes > 0 {
            t.late_count += 1;
            t.late_minutes += l.late_minutes;
        }
        if l.early_leave_minutes > 0 {
            t.early_leave_count += 1;
            t.early_leave_minutes += l.early_leave_minutes;
        }
        match l.status.as_str() {
            "missed_punch" => t.missed_punches += 1,
            "not_recorded" => t.not_recorded += 1,
            ABSENT => {
                t.absences += 1;
                absent_days.insert(&l.date);
            }
            _ => {}
        }
        if l.worked_minutes > Decimal::ZERO {
            worked_days.insert(&l.date);
        }
        if l.night {
            t.night_shifts += 1;
        }
        if l.holiday {
            t.holiday_shifts += 1;
        }
    }
    // A day counts as lost pay when every shift that day was missed and daily attendance does
    // not already carry it (absent / half day / leave / holiday / weekly off).
    t.lop_days = absent_days
        .iter()
        .filter(|d| !worked_days.contains(**d))
        .filter(|d| {
            !matches!(
                days.get(**d).map(String::as_str),
                Some("absent" | "half_day" | "on_leave" | "holiday" | "weekly_off")
            )
        })
        .count()
        .into();
    let mut weeks: BTreeMap<String, (Decimal, Decimal)> = BTreeMap::new();
    for (d, m) in &per_day {
        let w = weeks.entry(start_of_week(d)).or_default();
        if let Some(h) = limits.daily_overtime_hours {
            w.0 += (*m - h * SIXTY).max(Decimal::ZERO);
        }
        w.1 += *m;
    }
    t.overtime_minutes = weeks
        .values()
        .map(|(daily, total)| {
            let weekly = limits
                .weekly_overtime_hours
                .map_or(Decimal::ZERO, |h| (*total - h * SIXTY).max(Decimal::ZERO));
            (*daily).max(weekly)
        })
        .sum();
    t
}

/// The quantities the payroll export hands 7C: `(code, quantity)` for the non-zero ones.
pub fn payroll_quantities(t: &Totals) -> Vec<(&'static str, Decimal)> {
    [
        ("overtime_hours", hours(t.overtime_minutes)),
        ("lop_days", t.lop_days),
        ("night_shifts", Decimal::from(t.night_shifts)),
        ("holiday_shifts", Decimal::from(t.holiday_shifts)),
    ]
    .into_iter()
    .filter(|(_, q)| *q > Decimal::ZERO)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(x: &str) -> Decimal {
        Decimal::from_str(x).unwrap()
    }

    fn sh(id: &str, date: &str, s: &str, e: &str, who: Option<&str>) -> Shift {
        Shift {
            id: id.into(),
            date: date.into(),
            start: s.into(),
            end: e.into(),
            role: "Floor".into(),
            assignee_id: who.map(str::to_owned),
            ..Shift::default()
        }
    }

    fn rec(status: &str, i: &str, o: &str) -> Record {
        Record {
            status: Some(status.into()),
            clock_in: Some(i.into()),
            clock_out: Some(o.into()),
            note: None,
        }
    }

    #[test]
    fn rows_summaries_and_csv() {
        let emps = vec![
            PayEmployee {
                id: "e1".into(),
                name: "Ava Chen".into(),
                active: true,
                hourly_rate: Some(d("20")),
            },
            PayEmployee {
                id: "e2".into(),
                name: "Liam \"Lee\" Torres, Jr.".into(),
                active: true,
                hourly_rate: None,
            },
        ];
        let shifts = vec![
            sh("s1", "2026-08-03", "09:00", "17:00", Some("e1")),
            sh("s2", "2026-08-03", "22:00", "06:00", Some("e2")),
            sh("s3", "2026-08-04", "09:00", "13:00", Some("e1")),
        ];
        let mut att = BTreeMap::new();
        att.insert("s1".to_owned(), rec("late", "09:20", "17:00"));
        att.insert("s3".to_owned(), rec("absent", "", ""));
        let rows = timesheet_rows(&emps, &shifts, &att, "2026-08-03", "2026-08-09");
        assert_eq!(rows[0].worked_hours, Some(d("7.67")));
        assert_eq!(rows[0].minutes_late, Some(20.0));
        assert_eq!(rows[2].pay, Some(Decimal::ZERO));
        let sums = employee_summaries(&rows);
        assert_eq!(sums[0].scheduled_hours, d("12"));
        let csv = timesheet_csv(&emps, &shifts, &att, "2026-08-03", "2026-08-09");
        assert!(csv.contains("\"Liam \"\"Lee\"\" Torres, Jr.\""));
        assert_eq!(csv_field(&Cell::from("=SUM(A1)")), "'=SUM(A1)");
        let cost = labor_cost(
            &shifts,
            &emps,
            &["2026-08-03".into(), "2026-08-04".into()],
            None,
            None,
        );
        assert_eq!(cost.cost, d("240"));
        assert!(budget_status(cost.cost, Some(d("100"))).over);
    }

    #[test]
    fn reconciliation_overtime_night_and_lop() {
        let a = sh("a", "2026-08-03", "08:00", "20:00", Some("e1"));
        let b = sh("b", "2026-08-04", "22:00", "06:00", Some("e1"));
        let c = sh("c", "2026-08-05", "09:00", "17:00", Some("e1"));
        let d = sh("d", "2026-08-06", "09:00", "17:00", Some("e1"));
        let pa = rec("present", "08:30", "20:00");
        let pb = rec("present", "22:00", "05:00");
        let pc = rec("absent", "", "");
        let pd = Record {
            status: Some("present".into()),
            clock_in: Some("09:00".into()),
            ..Record::default()
        };
        let act = |shift, punch, holiday| Actual {
            shift,
            punch: Some(punch),
            day: None,
            holiday,
        };
        let acts = vec![
            act(&a, &pa, false),
            act(&b, &pb, true),
            act(&c, &pc, false),
            act(&d, &pd, false),
        ];
        let l = Limits::default();
        let lines = reconcile(&acts, &l, "2026-08-09");
        assert_eq!(lines[0].late_minutes, 30);
        assert_eq!(lines[0].status, "late");
        assert_eq!(lines[1].early_leave_minutes, 60);
        assert!(lines[1].night && lines[1].holiday);
        assert_eq!(lines[3].status, "missed_punch");
        let t = totals(&lines, &l, &HashMap::new());
        // 11.5h on Monday: 2.5h over 9.
        assert_eq!(t.overtime_minutes, Decimal::from(150));
        assert_eq!(
            (t.night_shifts, t.holiday_shifts, t.lop_days),
            (1, 1, Decimal::ONE)
        );
        let mut days = HashMap::new();
        days.insert("2026-08-05".to_owned(), "absent".to_owned());
        assert_eq!(totals(&lines, &l, &days).lop_days, Decimal::ZERO);
        assert_eq!(
            payroll_quantities(&t)[0],
            ("overtime_hours", Decimal::new(25, 1))
        );
    }

    #[test]
    fn pay_is_exact_where_floats_drift() {
        // 7 minutes at 0.30/h: floats give 0.034999.. -> 0.03; exact is 0.035 -> 0.04.
        let r = d("0.30");
        assert_eq!(round2(minutes_cost(d("7"), r)), d("0.04"));
        // Summing ten tenths of an hour is exactly one hour.
        let total: Decimal = (0..10).map(|_| d("0.1")).sum();
        assert_eq!(total, Decimal::ONE);
        // 1.005 rounds half up, as Math.round on the exact value would.
        assert_eq!(round2(d("1.005")), d("1.01"));
        assert_eq!(hours(d("50")), d("0.83"));
        assert_eq!(hours(d("45")), d("0.75"));
        // 1350 min at 333.33/h = 7499.925 exactly -> 7499.93 (floats: 7499.92).
        assert_eq!(round2(minutes_cost(d("1350"), d("333.33"))), d("7499.93"));
    }

    #[test]
    fn overtime_cost_is_exact() {
        // 50h in one week at 123.45/h with a 40h threshold and 1.5x: 40h base + 10h overtime.
        let emps = vec![PayEmployee {
            id: "e1".into(),
            name: "A".into(),
            active: true,
            hourly_rate: Some(d("123.45")),
        }];
        let dates: Vec<String> = (3..8).map(|n| format!("2026-08-0{n}")).collect();
        let shifts: Vec<Shift> = dates
            .iter()
            .enumerate()
            .map(|(i, day)| sh(&format!("s{i}"), day, "08:00", "18:00", Some("e1")))
            .collect();
        let c = labor_cost(&shifts, &emps, &dates, Some(d("40")), Some(d("1.5")));
        assert_eq!(c.minutes, d("3000"));
        assert_eq!(c.overtime_minutes, d("600"));
        assert_eq!(c.base_cost, d("4938.00"));
        assert_eq!(c.overtime_cost, d("1851.75"));
        assert_eq!(c.cost, c.base_cost + c.overtime_cost);
        let by_emp: Decimal = c.by_employee.iter().map(|b| b.cost).sum();
        assert_eq!(by_emp, c.cost);
    }

    #[test]
    fn overtime_over_the_daily_limit_is_exact() {
        // 11.5 paid hours on one day with a 9h daily limit: 150 minutes = 2.5 h of overtime.
        let s = sh("s1", "2026-08-03", "08:00", "19:30", Some("e1"));
        let lines = reconcile(
            &[Actual {
                shift: &s,
                punch: None,
                day: Some("present"),
                holiday: false,
            }],
            &Limits::default(),
            "2026-08-31",
        );
        let t = totals(&lines, &Limits::default(), &HashMap::new());
        assert_eq!(t.overtime_minutes, d("150"));
        assert_eq!(hours(t.overtime_minutes), d("2.5"));
    }

    #[test]
    fn many_small_shifts_sum_exactly() {
        // 1000 shifts of 7 minutes at 10/h: exact 1166.67 after one rounding of the total.
        let total: Decimal = (0..1000).map(|_| minutes_cost(d("7"), d("10"))).sum();
        assert_eq!(round2(total), d("1166.67"));
        let per_row: Decimal = (0..1000)
            .map(|_| round2(minutes_cost(d("7"), d("10"))))
            .sum();
        assert_eq!(per_row, d("1170.00"));
        // Hours of a thousand 6-minute shifts: exactly 100.
        let h: Decimal = (0..1000).map(|_| d("6") / SIXTY).sum();
        assert_eq!(h, d("100"));
    }
}
