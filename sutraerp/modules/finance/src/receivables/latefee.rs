//! **ERP_EXTENSIONS 1D** — late fees and fee reminders, pure rules.
//!
//! The instalment fill order, the "what is actually late" base, the due date a charge is measured
//! late against, the flat / percentage fee and the reminder stage are ported from Everything's
//! `invoicing/src/lib/{money,doc,aging,latefee,reminders}.js` (MIT, HeyPuter/everything @ 06802a1)
//! and replayed against the originals by `tests/golden_latefee.rs`. Everything applies a fee by
//! hand, once; SutraERP makes the policy configurable (flat or percentage, once or per month, per
//! day, day slabs, grace days, caps, rounding to the rupee) and accrues it idempotently: a run
//! charges only the difference between the fee due today and the fees already raised.

use chrono::NaiveDate;
use rust_decimal::{Decimal, RoundingStrategy};
use serde_json::Value;
use std::str::FromStr;

fn round2(d: Decimal) -> Decimal {
    d.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero)
}

/// Half a paisa: Everything's "nothing left" threshold.
fn epsilon() -> Decimal {
    Decimal::new(5, 3)
}

// ----------------------------------------------------------------------------- schedule

/// How a schedule row asks for its part of the total.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowAmount {
    Fixed(Decimal),
    Percent(Decimal),
}

/// One row of a due schedule as defined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleRow {
    pub due: Option<NaiveDate>,
    pub amount: RowAmount,
}

/// One instalment with what it has taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instalment {
    pub due: Option<NaiveDate>,
    pub amount: Decimal,
    pub paid: Decimal,
    pub open: Decimal,
}

/// Splits `total` over the rows in date order (undated rows last, keeping their order), the last
/// row taking the remainder, then fills them with `paid` earliest first: the first instalment is
/// settled before the second takes anything. Port of `money.js` `scheduleRows`.
pub fn schedule_rows(rows: &[ScheduleRow], total: Decimal, paid: Decimal) -> Vec<Instalment> {
    if rows.is_empty() {
        return Vec::new();
    }
    let mut sorted: Vec<&ScheduleRow> = rows.iter().collect();
    // Stable: equal dates keep their order, undated rows fall in behind.
    sorted.sort_by_key(|r| r.due.map_or((1, NaiveDate::MAX), |d| (0, d)));
    let mut used = Decimal::ZERO;
    let last = sorted.len() - 1;
    let amounts: Vec<Decimal> = sorted
        .iter()
        .enumerate()
        .map(|(i, r)| {
            if i == last {
                return round2(total - used);
            }
            let raw = match r.amount {
                RowAmount::Fixed(v) => round2(v),
                RowAmount::Percent(p) => round2(total * p / Decimal::ONE_HUNDRED),
            };
            let amount = raw
                .max(Decimal::ZERO)
                .min(round2(total - used).max(Decimal::ZERO));
            used = round2(used + amount);
            amount
        })
        .collect();
    let mut left = paid.max(Decimal::ZERO);
    sorted
        .iter()
        .zip(amounts)
        .map(|(r, amount)| {
            let covered = round2(amount.max(Decimal::ZERO).min(left));
            left = round2(left - covered);
            Instalment {
                due: r.due,
                amount,
                paid: covered,
                open: round2(amount - covered),
            }
        })
        .collect()
}

/// The date a charge is measured late against: the earliest instalment still open, else the last
/// instalment, else the charge's own due date. Port of `doc.js` `dueDateFor`.
pub fn due_date_for(instalments: &[Instalment], due: Option<NaiveDate>) -> Option<NaiveDate> {
    match instalments.last() {
        None => due,
        Some(last) => instalments
            .iter()
            .find(|r| r.open > epsilon())
            .and_then(|r| r.due)
            .or(last.due)
            .or(due),
    }
}

/// The part of the balance that is past due on `as_of`: on a schedule, the open instalments whose
/// date has passed (never the part not due until later), plus any balance beyond the schedule
/// once the charge itself is late. Port of `aging.js` `overdueAmount`.
pub fn overdue_amount(
    balance: Decimal,
    instalments: &[Instalment],
    due: Option<NaiveDate>,
    as_of: NaiveDate,
) -> Decimal {
    if balance <= epsilon() {
        return Decimal::ZERO;
    }
    let late = |d: Option<NaiveDate>| d.is_some_and(|d| d < as_of);
    let charge_late = late(due_date_for(instalments, due));
    let open: Vec<&Instalment> = instalments.iter().filter(|r| r.open > epsilon()).collect();
    let late_open: Decimal = open.iter().filter(|r| late(r.due)).map(|r| r.open).sum();
    if open.is_empty() || !open.iter().any(|r| late(r.due)) {
        return if charge_late {
            balance
        } else {
            Decimal::ZERO
        };
    }
    let rest = round2(balance - open.iter().map(|r| r.open).sum::<Decimal>());
    round2(late_open + if rest > epsilon() && charge_late { rest } else { Decimal::ZERO })
}

/// Whole days late (negative before the due date, 0 without one). Port of `reminders.js` `daysLate`.
pub fn days_late(due: Option<NaiveDate>, as_of: NaiveDate) -> i64 {
    due.map_or(0, |d| (as_of - d).num_days())
}

/// Everything's one-off fee on the late part: a flat amount or a percentage of it.
/// Port of `latefee.js` `lateFeeAmount` (with a date).
pub fn everything_fee(percent: bool, value: Decimal, overdue: Decimal) -> Decimal {
    if overdue <= epsilon() {
        return Decimal::ZERO;
    }
    if percent {
        round2(overdue * value / Decimal::ONE_HUNDRED)
    } else {
        round2(value)
    }
}

// ------------------------------------------------------------------------------ policy

/// How the fee grows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// A flat amount (once, or per month or part of a month late).
    Flat,
    /// A percentage of the late part (once, or per month or part of a month late).
    Percent,
    /// A flat amount per day late after the grace days.
    PerDay,
    /// The amount of the highest slab reached (`fromDay` = days late).
    Slabs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Frequency {
    Once,
    Monthly,
}

/// A slab: from `from_day` days late, the fee is `amount`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slab {
    pub from_day: i64,
    pub amount: Decimal,
}

/// An institution's late-fee policy (the effective `late_fee` rule version).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LateFeePolicy {
    pub mode: Mode,
    pub amount: Decimal,
    pub percent: Decimal,
    pub frequency: Frequency,
    /// No fee until the charge is more than this many days late.
    pub grace_days: i64,
    /// Never more than this in total per charge.
    pub cap: Option<Decimal>,
    /// Never more than this percentage of the late part.
    pub cap_percent: Option<Decimal>,
    pub slabs: Vec<Slab>,
    pub round_to_rupee: bool,
    /// No fee while the late part is at most this.
    pub min_overdue: Decimal,
    /// Fee head the fee is charged under (default: the institution's `LATE-FEE` head).
    pub fee_head_code: Option<String>,
}

/// Rule parameter keys of `late_fee`, for the configuration validator.
pub const LATE_FEE_KEYS: &[&str] = &[
    "mode",
    "amount",
    "percent",
    "frequency",
    "graceDays",
    "cap",
    "capPercent",
    "slabs",
    "roundToRupee",
    "minOverdue",
    "feeHeadCode",
];

fn dec(v: &Value, key: &str) -> Result<Option<Decimal>, String> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Decimal::from_str(s.trim())
            .ok()
            .filter(|d| *d >= Decimal::ZERO && d.scale() <= 4)
            .map(Some)
            .ok_or_else(|| format!("{key} must be a non-negative decimal string")),
        Some(_) => Err(format!("{key} must be a decimal string such as \"50.00\"")),
    }
}

fn int(v: &Value, key: &str, max: i64) -> Result<Option<i64>, String> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(x) => x
            .as_i64()
            .filter(|n| (0..=max).contains(n))
            .map(Some)
            .ok_or_else(|| format!("{key} must be a whole number from 0 to {max}")),
    }
}

impl LateFeePolicy {
    /// Reads and validates rule parameters. The 5A shape `{percent, graceDays, cap}` stays valid
    /// (a one-off percentage). `Ok(None)` = the rule charges nothing.
    pub fn from_values(v: &Value) -> Result<Option<Self>, String> {
        let obj = v.as_object().ok_or("parameters must be a JSON object")?;
        if let Some(k) = obj.keys().find(|k| !LATE_FEE_KEYS.contains(&k.as_str())) {
            return Err(format!("{k} is not a late-fee setting"));
        }
        let amount = dec(v, "amount")?;
        let percent = dec(v, "percent")?;
        if percent.is_some_and(|p| p > Decimal::ONE_HUNDRED) {
            return Err("percent must be at most 100".into());
        }
        let mode = match v.get("mode").and_then(Value::as_str) {
            Some("flat") => Mode::Flat,
            Some("percent") => Mode::Percent,
            Some("per_day") => Mode::PerDay,
            Some("slabs") => Mode::Slabs,
            Some("none") => return Ok(None),
            Some(_) => return Err("mode must be flat, percent, per_day, slabs or none".into()),
            None if percent.is_some() => Mode::Percent,
            None if v.get("slabs").is_some() => Mode::Slabs,
            None => Mode::Flat,
        };
        let frequency = match v.get("frequency").and_then(Value::as_str) {
            None | Some("once") => Frequency::Once,
            Some("monthly") => Frequency::Monthly,
            Some(_) => return Err("frequency must be once or monthly".into()),
        };
        let mut slabs = Vec::new();
        if let Some(list) = v.get("slabs") {
            let arr = list.as_array().ok_or("slabs must be a list")?;
            if arr.len() > 20 {
                return Err("at most 20 slabs".into());
            }
            for s in arr {
                let from_day = int(s, "fromDay", 3650)?.ok_or("each slab needs fromDay")?;
                let amount = dec(s, "amount")?.ok_or("each slab needs amount")?;
                slabs.push(Slab { from_day, amount });
            }
            slabs.sort_by_key(|s| s.from_day);
        }
        let need = |x: Option<Decimal>, what: &str| x.ok_or(format!("mode {what} needs {}", if what == "percent" { "percent" } else { "amount" }));
        let (amount, percent) = match mode {
            Mode::Flat => (need(amount, "flat")?, Decimal::ZERO),
            Mode::PerDay => (need(amount, "per_day")?, Decimal::ZERO),
            Mode::Percent => (Decimal::ZERO, need(percent, "percent")?),
            Mode::Slabs => {
                if slabs.is_empty() {
                    return Err("mode slabs needs at least one slab".into());
                }
                (Decimal::ZERO, Decimal::ZERO)
            }
        };
        let fee_head_code = match v.get("feeHeadCode") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if valid_code(s) => Some(s.clone()),
            Some(_) => return Err("feeHeadCode must match ^[A-Z0-9][A-Z0-9_-]{1,31}$".into()),
        };
        let round_to_rupee = match v.get("roundToRupee") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err("roundToRupee must be true or false".into()),
        };
        let cap_percent = dec(v, "capPercent")?;
        if cap_percent.is_some_and(|p| p > Decimal::ONE_HUNDRED) {
            return Err("capPercent must be at most 100".into());
        }
        Ok(Some(LateFeePolicy {
            mode,
            amount,
            percent,
            frequency,
            grace_days: int(v, "graceDays", 365)?.unwrap_or(0),
            cap: dec(v, "cap")?,
            cap_percent,
            slabs,
            round_to_rupee,
            min_overdue: dec(v, "minOverdue")?.unwrap_or(Decimal::ZERO),
            fee_head_code,
        }))
    }

    /// The total fee due on a charge whose late part is `overdue`, `days_late` days late.
    pub fn fee_due(&self, overdue: Decimal, days_late: i64) -> Decimal {
        if overdue <= epsilon() || overdue <= self.min_overdue || days_late <= self.grace_days {
            return Decimal::ZERO;
        }
        let late = days_late - self.grace_days;
        // "Per month or part of a month" late, after the grace days.
        let months = Decimal::from((late + 29) / 30);
        let times = |d: Decimal| match self.frequency {
            Frequency::Once => d,
            Frequency::Monthly => d * months,
        };
        let raw = match self.mode {
            Mode::Flat => times(self.amount),
            Mode::Percent => times(overdue * self.percent / Decimal::ONE_HUNDRED),
            Mode::PerDay => self.amount * Decimal::from(late),
            Mode::Slabs => self
                .slabs
                .iter()
                .rev()
                .find(|s| s.from_day <= days_late)
                .map_or(Decimal::ZERO, |s| s.amount),
        };
        let mut fee = raw;
        if let Some(c) = self.cap {
            fee = fee.min(c);
        }
        if let Some(p) = self.cap_percent {
            fee = fee.min(overdue * p / Decimal::ONE_HUNDRED);
        }
        if self.round_to_rupee {
            fee.round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero)
                .round_dp(2)
        } else {
            round2(fee)
        }
    }

    /// Fee head code the fee is charged under.
    pub fn fee_head(&self) -> &str {
        self.fee_head_code.as_deref().unwrap_or("LATE-FEE")
    }
}

fn valid_code(s: &str) -> bool {
    let b = s.as_bytes();
    (2..=32).contains(&b.len())
        && (b[0].is_ascii_uppercase() || b[0].is_ascii_digit())
        && b
            .iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

// ---------------------------------------------------------------------------- reminders

/// The three escalation stages of a reminder. Port of `reminders.js` `reminderStage`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Upcoming,
    Due,
    Late,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Upcoming => "upcoming",
            Stage::Due => "due",
            Stage::Late => "late",
        }
    }
}

/// Before the due date `Upcoming`, then `Due`, and `Late` from `late_from` days (Everything: 30).
pub fn reminder_stage(days_late: i64, late_from: i64) -> Stage {
    if days_late <= 0 {
        Stage::Upcoming
    } else if days_late < late_from {
        Stage::Due
    } else {
        Stage::Late
    }
}

/// When reminders go out (the effective `reminder_schedule` rule).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReminderSchedule {
    /// Days before the due date on which a reminder goes out (e.g. `[7, 1]`).
    pub before_days: Vec<i64>,
    /// Days late on which dunning notice 1, 2, 3 go out (e.g. `[1, 15, 30]`).
    pub after_days: Vec<i64>,
    /// Days late from which the wording is "late" rather than "just overdue".
    pub late_from: i64,
}

pub const REMINDER_KEYS: &[&str] = &["beforeDays", "afterDays", "lateFromDays"];

fn days_list(v: &Value, key: &str, max_len: usize) -> Result<Vec<i64>, String> {
    let Some(x) = v.get(key) else {
        return Ok(Vec::new());
    };
    let arr = x.as_array().ok_or(format!("{key} must be a list of days"))?;
    if arr.len() > max_len {
        return Err(format!("{key}: at most {max_len} entries"));
    }
    let mut out = arr
        .iter()
        .map(|d| d.as_i64().filter(|n| (0..=365).contains(n)))
        .collect::<Option<Vec<_>>>()
        .ok_or(format!("{key} must hold whole numbers from 0 to 365"))?;
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

impl ReminderSchedule {
    pub fn from_values(v: &Value) -> Result<Self, String> {
        let obj = v.as_object().ok_or("parameters must be a JSON object")?;
        if let Some(k) = obj.keys().find(|k| !REMINDER_KEYS.contains(&k.as_str())) {
            return Err(format!("{k} is not a reminder setting"));
        }
        let after = days_list(v, "afterDays", 3)?;
        if after.first() == Some(&0) {
            return Err("afterDays start at 1 (the first day late)".into());
        }
        Ok(ReminderSchedule {
            before_days: days_list(v, "beforeDays", 10)?,
            after_days: after,
            late_from: int(v, "lateFromDays", 365)?.unwrap_or(30),
        })
    }

    /// The reminder due today before the due date, as days-before, when the schedule has one.
    pub fn upcoming_today(&self, days_late: i64) -> Option<i64> {
        let before = -days_late;
        (before > 0 && self.before_days.contains(&before)).then_some(before)
    }

    /// Whether dunning notice `level_sent + 1` is due `days_late` days late.
    pub fn notice_due(&self, days_late: i64, level_sent: i32) -> bool {
        usize::try_from(level_sent)
            .ok()
            .and_then(|i| self.after_days.get(i))
            .is_some_and(|d| days_late >= *d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    #[test]
    fn legacy_rule_is_a_one_off_percentage() {
        let p = LateFeePolicy::from_values(&json!({"percent": "2.5", "graceDays": 7, "cap": "500"}))
            .unwrap()
            .unwrap();
        assert_eq!(p.mode, Mode::Percent);
        assert_eq!(p.fee_due(d("10000"), 7), Decimal::ZERO);
        assert_eq!(p.fee_due(d("10000"), 8), d("250.00"));
        assert_eq!(p.fee_due(d("100000"), 90), d("500"));
    }

    #[test]
    fn modes() {
        let per_day = LateFeePolicy::from_values(&json!({"mode": "per_day", "amount": "50", "graceDays": 5, "cap": "1000"})).unwrap().unwrap();
        assert_eq!(per_day.fee_due(d("5000"), 6), d("50.00"));
        assert_eq!(per_day.fee_due(d("5000"), 15), d("500.00"));
        assert_eq!(per_day.fee_due(d("5000"), 60), d("1000.00"));
        let monthly = LateFeePolicy::from_values(&json!({"mode": "percent", "percent": "1.5", "frequency": "monthly", "roundToRupee": true})).unwrap().unwrap();
        assert_eq!(monthly.fee_due(d("33333"), 1), d("500.00"));
        assert_eq!(monthly.fee_due(d("33333"), 31), d("1000.00"));
        let slabs = LateFeePolicy::from_values(&json!({"slabs": [{"fromDay": 31, "amount": "500"}, {"fromDay": 1, "amount": "100"}, {"fromDay": 16, "amount": "250"}]})).unwrap().unwrap();
        assert_eq!(slabs.mode, Mode::Slabs);
        assert_eq!([1, 15, 16, 45].map(|n| slabs.fee_due(d("100"), n)), [d("100"), d("100"), d("250"), d("500")]);
        let flat = LateFeePolicy::from_values(&json!({"amount": "200", "capPercent": "10", "minOverdue": "100"})).unwrap().unwrap();
        assert_eq!(flat.fee_due(d("100"), 3), Decimal::ZERO);
        assert_eq!(flat.fee_due(d("1000"), 3), d("100.00"));
        assert!(LateFeePolicy::from_values(&json!({"mode": "none"})).unwrap().is_none());
        for bad in [json!({"mode": "percent"}), json!({"percent": 2.5}), json!({"amount": "1", "x": 1}), json!({"mode": "slabs"}), json!({"amount": "1", "feeHeadCode": "late fee"})] {
            assert!(LateFeePolicy::from_values(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn reminder_schedule() {
        let s = ReminderSchedule::from_values(&json!({"beforeDays": [1, 7], "afterDays": [30, 1, 15]})).unwrap();
        assert_eq!(s.upcoming_today(-7), Some(7));
        assert_eq!(s.upcoming_today(-3), None);
        assert!(s.notice_due(1, 0) && !s.notice_due(14, 1) && s.notice_due(15, 1) && !s.notice_due(99, 3));
        assert!(ReminderSchedule::from_values(&json!({"afterDays": [0]})).is_err());
        assert_eq!(reminder_stage(0, 30), Stage::Upcoming);
        assert_eq!(reminder_stage(29, 30), Stage::Due);
    }
}
