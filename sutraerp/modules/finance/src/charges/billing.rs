//! **ERP_EXTENSIONS 1E** — periodic billing runs, pure rules.
//!
//! The billing period of a day, and which students a run bills for that period with which plan
//! lines, are ported from Everything's `childcare/src/core/billing.js` (`periodFor`,
//! `planBillingRun`, `planTotals`; MIT, HeyPuter/everything @ 06802a1) and replayed against the
//! original by `tests/golden_billing_run.rs`. Everything bills weekly or monthly plans, once per
//! child per period, from the child's start date. SutraERP adds quarterly, half-yearly and annual
//! plans aligned to the institution's financial year (April by default), a start and end date and
//! a hold on each enrolment, daily proration of a part period, and the schedule rule that decides
//! when an automatic run is due.

use chrono::{Datelike, Duration, Months, NaiveDate};
use rust_decimal::{Decimal, RoundingStrategy};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// How often a plan bills.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Frequency {
    Weekly,
    Monthly,
    Quarterly,
    HalfYearly,
    Annual,
}

impl Frequency {
    pub const ALL: [Frequency; 5] = [
        Frequency::Weekly,
        Frequency::Monthly,
        Frequency::Quarterly,
        Frequency::HalfYearly,
        Frequency::Annual,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Frequency::Weekly => "weekly",
            Frequency::Monthly => "monthly",
            Frequency::Quarterly => "quarterly",
            Frequency::HalfYearly => "half_yearly",
            Frequency::Annual => "annual",
        }
    }

    pub fn parse(s: &str) -> Option<Frequency> {
        Frequency::ALL.into_iter().find(|f| f.as_str() == s)
    }

    /// Months in one period (weekly: none).
    fn months(self) -> u32 {
        match self {
            Frequency::Weekly => 0,
            Frequency::Monthly => 1,
            Frequency::Quarterly => 3,
            Frequency::HalfYearly => 6,
            Frequency::Annual => 12,
        }
    }
}

/// How a plan bills a period the enrolment covers only in part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Proration {
    /// The full amount (Everything's behaviour).
    None,
    /// The amount times the days covered over the days in the period, to the paisa.
    Daily,
}

impl Proration {
    pub fn as_str(self) -> &'static str {
        match self {
            Proration::None => "none",
            Proration::Daily => "daily",
        }
    }

    pub fn parse(s: &str) -> Option<Proration> {
        match s {
            "none" => Some(Proration::None),
            "daily" => Some(Proration::Daily),
            _ => None,
        }
    }
}

/// One billing period: `key` identifies it (one charge per student per key), `start..=end`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Period {
    pub frequency: Frequency,
    pub key: String,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub label: String,
}

impl Period {
    pub fn days(&self) -> i64 {
        (self.end - self.start).num_days() + 1
    }
}

fn first_of_month(d: NaiveDate) -> NaiveDate {
    d.with_day(1).unwrap_or(d)
}

fn add_months(d: NaiveDate, n: u32) -> NaiveDate {
    d.checked_add_months(Months::new(n))
        .unwrap_or(NaiveDate::MAX)
}

/// `weekday` 0 = Sunday … 6 = Saturday, as in Everything's `dates.js`.
fn weekday0(d: NaiveDate) -> i64 {
    i64::from(d.weekday().num_days_from_sunday())
}

fn fy_label(start_year: i32) -> String {
    format!("{}-{:02}", start_year, (start_year + 1).rem_euclid(100))
}

/// The billing period containing `day`. Weekly periods start on `week_start` (0 = Sunday,
/// 1 = Monday); quarters, halves and years are counted from the financial year's first month
/// (`fy_start_month`, 1–12). Port of `periodFor`, extended past weekly / monthly.
pub fn period_for(
    frequency: Frequency,
    day: NaiveDate,
    week_start: u32,
    fy_start_month: u32,
) -> Period {
    match frequency {
        Frequency::Weekly => {
            let back = (weekday0(day) - i64::from(week_start) + 7).rem_euclid(7);
            let start = day - Duration::days(back);
            Period {
                frequency,
                key: format!("weekly:{start}"),
                start,
                end: start + Duration::days(6),
                label: format!("Week of {}", start.format("%-d %b %Y")),
            }
        }
        Frequency::Monthly => {
            let start = first_of_month(day);
            Period {
                frequency,
                key: format!("monthly:{}", start.format("%Y-%m")),
                start,
                end: add_months(start, 1) - Duration::days(1),
                label: start.format("%B %Y").to_string(),
            }
        }
        _ => {
            let span = frequency.months();
            let fy_month = fy_start_month.clamp(1, 12);
            // Months since the financial year began (0..12).
            let since = (day.month() + 12 - fy_month) % 12;
            let fy_year = if day.month() >= fy_month {
                day.year()
            } else {
                day.year() - 1
            };
            let index = since / span;
            let fy_start = NaiveDate::from_ymd_opt(fy_year, fy_month, 1).unwrap_or(day);
            let start = add_months(fy_start, index * span);
            let end = add_months(start, span) - Duration::days(1);
            let fy = fy_label(fy_year);
            let range = format!("{}–{}", start.format("%b"), end.format("%b"));
            let label = match frequency {
                Frequency::Quarterly => format!("Q{} {fy} ({range})", index + 1),
                Frequency::HalfYearly => format!("H{} {fy} ({range})", index + 1),
                _ => format!("FY {fy}"),
            };
            Period {
                frequency,
                key: format!("{}:{}", frequency.as_str(), start.format("%Y-%m")),
                start,
                end,
                label,
            }
        }
    }
}

/// A plan as the run sees it.
#[derive(Clone, Debug)]
pub struct Plan {
    pub id: Uuid,
    pub name: String,
    pub amount: Decimal,
    pub frequency: Frequency,
    pub proration: Proration,
}

/// A student's place on one plan.
#[derive(Clone, Debug)]
pub struct Enrolled {
    pub plan_id: Uuid,
    /// Overrides the plan's amount for this student.
    pub amount: Option<Decimal>,
    pub start: Option<NaiveDate>,
    pub end: Option<NaiveDate>,
    pub on_hold: bool,
}

/// A student on the roster.
#[derive(Clone, Debug)]
pub struct Student {
    pub id: Uuid,
    /// Active in the institution (Everything: `status === 'active'`).
    pub active: bool,
    /// Everything's student-level start date (SutraERP uses the enrolment's).
    pub start: Option<NaiveDate>,
    pub plans: Vec<Enrolled>,
}

/// One line a run would bill.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftLine {
    pub plan_id: Uuid,
    pub description: String,
    pub amount: Decimal,
    /// `(days covered, days in the period)` when the line was prorated.
    pub prorated: Option<(i64, i64)>,
}

/// One charge a run would raise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub student_id: Uuid,
    pub issue_date: NaiveDate,
    pub due_date: NaiveDate,
    pub period_key: String,
    pub lines: Vec<DraftLine>,
}

impl Draft {
    pub fn total(&self) -> Decimal {
        self.lines.iter().map(|l| l.amount).sum()
    }
}

/// Why a student with plans of the run's frequency is not billed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    Inactive,
    AlreadyBilled,
    NotStarted,
    Ended,
    OnHold,
    Excluded,
}

impl SkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            SkipReason::Inactive => "inactive",
            SkipReason::AlreadyBilled => "already_billed",
            SkipReason::NotStarted => "not_started",
            SkipReason::Ended => "ended",
            SkipReason::OnHold => "on_hold",
            SkipReason::Excluded => "excluded",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skip {
    pub student_id: Uuid,
    pub reason: SkipReason,
}

/// What a run for a period would do.
#[derive(Clone, Debug)]
pub struct RunPlan {
    pub period: Period,
    pub drafts: Vec<Draft>,
    pub skipped: Vec<Skip>,
}

/// Run options.
#[derive(Clone, Debug)]
pub struct RunOptions {
    pub frequency: Frequency,
    pub day: NaiveDate,
    pub due_days: i64,
    pub week_start: u32,
    pub fy_start_month: u32,
}

fn round2(d: Decimal) -> Decimal {
    d.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero)
}

/// The charges a billing run would raise for the period containing `opts.day`: one per active
/// student with plans of that frequency, started by the period's end, not billed for the period
/// yet (`billed`), and not `excluded`. Each enrolled plan is a line (its override or the plan's
/// amount); an enrolment on hold, not started by the period's end or ended before it starts adds
/// nothing, and a part period is prorated by day when the plan says so. Due `due_days` after the
/// later of the period's start and the run day. Port of `planBillingRun`.
pub fn plan_billing_run(
    students: &[Student],
    plans: &[Plan],
    billed: &HashSet<Uuid>,
    excluded: &HashSet<Uuid>,
    opts: &RunOptions,
) -> RunPlan {
    let period = period_for(
        opts.frequency,
        opts.day,
        opts.week_start,
        opts.fy_start_month,
    );
    let by_id: HashMap<Uuid, &Plan> = plans.iter().map(|p| (p.id, p)).collect();
    let mut drafts = Vec::new();
    let mut skipped = Vec::new();
    let has_plans = |s: &Student| {
        s.plans.iter().any(|e| {
            by_id
                .get(&e.plan_id)
                .is_some_and(|p| p.frequency == opts.frequency)
        })
    };
    for s in students {
        let skip = if !s.active {
            Some(SkipReason::Inactive)
        } else if billed.contains(&s.id) {
            Some(SkipReason::AlreadyBilled)
        } else if s.start.is_some_and(|d| d > period.end) {
            Some(SkipReason::NotStarted)
        } else if excluded.contains(&s.id) {
            Some(SkipReason::Excluded)
        } else {
            None
        };
        if let Some(reason) = skip {
            if has_plans(s) {
                skipped.push(Skip {
                    student_id: s.id,
                    reason,
                });
            }
            continue;
        }
        let mut lines = Vec::new();
        let mut why_none = None;
        for e in &s.plans {
            let Some(plan) = by_id.get(&e.plan_id) else {
                continue;
            };
            if plan.frequency != opts.frequency {
                continue;
            }
            if e.on_hold {
                why_none.get_or_insert(SkipReason::OnHold);
                continue;
            }
            if e.start.is_some_and(|d| d > period.end) {
                why_none.get_or_insert(SkipReason::NotStarted);
                continue;
            }
            if e.end.is_some_and(|d| d < period.start) {
                why_none.get_or_insert(SkipReason::Ended);
                continue;
            }
            let full = e.amount.unwrap_or(plan.amount);
            let from = e.start.map_or(period.start, |d| d.max(period.start));
            let to = e.end.map_or(period.end, |d| d.min(period.end));
            let covered = (to - from).num_days() + 1;
            let (amount, prorated) =
                if plan.proration == Proration::Daily && covered < period.days() {
                    (
                        round2(full * Decimal::from(covered) / Decimal::from(period.days())),
                        Some((covered, period.days())),
                    )
                } else {
                    (full, None)
                };
            lines.push(DraftLine {
                plan_id: plan.id,
                description: format!("{}, {}", plan.name, period.label),
                amount,
                prorated,
            });
        }
        if lines.is_empty() {
            if let Some(reason) = why_none {
                skipped.push(Skip {
                    student_id: s.id,
                    reason,
                });
            }
            continue;
        }
        let from = if period.start > opts.day {
            period.start
        } else {
            opts.day
        };
        drafts.push(Draft {
            student_id: s.id,
            issue_date: opts.day,
            due_date: from + Duration::days(opts.due_days),
            period_key: period.key.clone(),
            lines,
        });
    }
    RunPlan {
        period,
        drafts,
        skipped,
    }
}

/// What a student's plans add up to per frequency (Everything's `planTotals`).
pub fn plan_totals(student: &Student, plans: &[Plan]) -> HashMap<Frequency, Decimal> {
    let mut out = HashMap::new();
    for e in &student.plans {
        if let Some(p) = plans.iter().find(|p| p.id == e.plan_id) {
            *out.entry(p.frequency).or_insert(Decimal::ZERO) += e.amount.unwrap_or(p.amount);
        }
    }
    out
}

/// When a schedule's automatic run is due: on or after day `run_day` (1 = the period's first
/// day) of the period containing `today`, once per period (`last_key` is the period it last ran
/// for). Returns the period to bill.
pub fn scheduled_period(
    frequency: Frequency,
    today: NaiveDate,
    run_day: u32,
    last_key: Option<&str>,
    week_start: u32,
    fy_start_month: u32,
) -> Option<Period> {
    let period = period_for(frequency, today, week_start, fy_start_month);
    let offset = i64::from(run_day.max(1)) - 1;
    let run_on = (period.start + Duration::days(offset)).min(period.end);
    (today >= run_on && last_key != Some(period.key.as_str())).then_some(period)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    fn plan(id: u128, freq: Frequency, amount: i64, proration: Proration) -> Plan {
        Plan {
            id: Uuid::from_u128(id),
            name: format!("P{id}"),
            amount: Decimal::new(amount, 2),
            frequency: freq,
            proration,
        }
    }

    fn enrolled(id: u128) -> Enrolled {
        Enrolled {
            plan_id: Uuid::from_u128(id),
            amount: None,
            start: None,
            end: None,
            on_hold: false,
        }
    }

    #[test]
    fn periods_follow_the_financial_year() {
        let q = period_for(Frequency::Quarterly, d("2027-02-10"), 1, 4);
        assert_eq!(
            (q.start, q.end, q.key.as_str()),
            (d("2027-01-01"), d("2027-03-31"), "quarterly:2027-01")
        );
        assert_eq!(q.label, "Q4 2026-27 (Jan–Mar)");
        let h = period_for(Frequency::HalfYearly, d("2026-10-17"), 1, 4);
        assert_eq!((h.start, h.end), (d("2026-10-01"), d("2027-03-31")));
        assert_eq!(h.label, "H2 2026-27 (Oct–Mar)");
        let y = period_for(Frequency::Annual, d("2026-03-31"), 1, 4);
        assert_eq!(
            (y.start, y.end, y.label.as_str()),
            (d("2025-04-01"), d("2026-03-31"), "FY 2025-26")
        );
        let w = period_for(Frequency::Weekly, d("2026-10-17"), 1, 4);
        assert_eq!((w.start, w.end), (d("2026-10-12"), d("2026-10-18")));
        let m = period_for(Frequency::Monthly, d("2028-02-17"), 1, 4);
        assert_eq!(
            (m.end, m.label.as_str()),
            (d("2028-02-29"), "February 2028")
        );
    }

    #[test]
    fn enrolment_dates_holds_and_daily_proration() {
        let plans = [
            plan(1, Frequency::Monthly, 300000, Proration::Daily),
            plan(2, Frequency::Monthly, 100000, Proration::None),
        ];
        let mut late = enrolled(1);
        late.start = Some(d("2026-11-21")); // 10 of 30 days
        let mut held = enrolled(2);
        held.on_hold = true;
        let mut gone = enrolled(1);
        gone.end = Some(d("2026-10-31"));
        let students = vec![
            Student {
                id: Uuid::from_u128(10),
                active: true,
                start: None,
                plans: vec![late, held.clone()],
            },
            Student {
                id: Uuid::from_u128(11),
                active: true,
                start: None,
                plans: vec![held],
            },
            Student {
                id: Uuid::from_u128(12),
                active: true,
                start: None,
                plans: vec![gone],
            },
            Student {
                id: Uuid::from_u128(13),
                active: true,
                start: None,
                plans: vec![enrolled(2)],
            },
        ];
        let opts = RunOptions {
            frequency: Frequency::Monthly,
            day: d("2026-11-05"),
            due_days: 10,
            week_start: 1,
            fy_start_month: 4,
        };
        let excluded = HashSet::from([Uuid::from_u128(13)]);
        let run = plan_billing_run(&students, &plans, &HashSet::new(), &excluded, &opts);
        assert_eq!(run.drafts.len(), 1);
        assert_eq!(run.drafts[0].lines[0].amount, Decimal::new(100000, 2));
        assert_eq!(run.drafts[0].lines[0].prorated, Some((10, 30)));
        assert_eq!(run.drafts[0].due_date, d("2026-11-15"));
        let reasons: Vec<_> = run.skipped.iter().map(|s| s.reason).collect();
        assert_eq!(
            reasons,
            vec![SkipReason::OnHold, SkipReason::Ended, SkipReason::Excluded]
        );
    }

    #[test]
    fn schedules_run_once_per_period_from_their_day() {
        let f = Frequency::Monthly;
        assert!(scheduled_period(f, d("2026-10-04"), 5, None, 1, 4).is_none());
        let p = scheduled_period(f, d("2026-10-05"), 5, Some("monthly:2026-09"), 1, 4).unwrap();
        assert_eq!(p.key, "monthly:2026-10");
        assert!(scheduled_period(f, d("2026-10-20"), 5, Some("monthly:2026-10"), 1, 4).is_none());
        // A run day past a short month's end runs on its last day.
        assert!(scheduled_period(f, d("2027-02-28"), 31, None, 1, 4).is_some());
    }
}
