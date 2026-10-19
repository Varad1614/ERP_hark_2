//! Pure 7B business rules (no I/O): slice-owned lifecycle transitions, the working-day
//! calendar of a leave policy, day counting with half days split into calendar-month segments
//! (one consumption ledger entry per segment, contract §7), accrual amounts, year-end carry
//! forward / expiry split, encashment eligibility and faculty over-allocation.

use crate::shared::decimal::round2;
use chrono::{Datelike, NaiveDate};
use rust_decimal::Decimal;
use sutra_contracts::people::status::{
    EncashmentStatus, HrCaseStatus, LeaveAdjustmentStatus, LeaveExceptionStatus, LeavePolicyStatus,
    LeaveRequestStatus, WorkloadPlanStatus,
};

/// How a leave type earns its entitlement.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AccrualFrequency {
    /// `annual_entitlement / 12` credited by each monthly accrual run (kind `accrual`).
    Monthly,
    /// The full annual entitlement credited once per leave year (kind `opening`).
    Yearly,
    /// Never credited (e.g. loss of pay).
    None,
}

impl AccrualFrequency {
    pub const ALL: &'static [AccrualFrequency] = &[Self::Monthly, Self::Yearly, Self::None];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Monthly => "monthly",
            Self::Yearly => "yearly",
            Self::None => "none",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|v| v.as_str() == s)
    }
}

// ------------------------------------------------------------------ transitions (409 otherwise)

/// Leave policy versions (L2-E05 §6.3).
pub fn policy_can_transition(from: LeavePolicyStatus, to: LeavePolicyStatus) -> bool {
    use LeavePolicyStatus::*;
    matches!(
        (from, to),
        (Draft, UnderReview)
            | (UnderReview, Draft)
            | (UnderReview, Approved)
            | (Approved, Scheduled)
            | (Approved, Active)
            | (Scheduled, Active)
            | (Active, Superseded)
            | (Draft, Archived)
            | (Superseded, Archived)
    )
}

/// Leave requests (L2-E05 §6.4).
pub fn request_can_transition(from: LeaveRequestStatus, to: LeaveRequestStatus) -> bool {
    use LeaveRequestStatus::*;
    matches!(
        (from, to),
        (Draft, Submitted)
            | (Submitted, Validated)
            | (Validated, PendingApproval)
            | (PendingApproval, Approved)
            | (PendingApproval, Rejected)
            | (Approved, Reserved)
            | (Reserved, Consumed)
            | (Consumed, Closed)
            | (Draft, Cancelled)
            | (PendingApproval, Cancelled)
            | (Approved, Cancelled)
            | (Reserved, Cancelled)
            | (Consumed, Cancelled)
    )
}

/// Governed balance adjustments (L3-E-049).
pub fn adjustment_can_transition(from: LeaveAdjustmentStatus, to: LeaveAdjustmentStatus) -> bool {
    use LeaveAdjustmentStatus::*;
    matches!(
        (from, to),
        (Requested, PendingApproval)
            | (PendingApproval, Approved)
            | (PendingApproval, Rejected)
            | (Approved, Applied)
    )
}

/// Encashment eligibility decisions (L3-E-051; `paid` is set by payroll).
pub fn encashment_can_transition(from: EncashmentStatus, to: EncashmentStatus) -> bool {
    use EncashmentStatus::*;
    matches!(
        (from, to),
        (Requested, Eligible)
            | (Requested, NotEligible)
            | (Eligible, Approved)
            | (Eligible, Rejected)
            | (Approved, Paid)
    )
}

/// Leave exceptions (L3-E-052).
pub fn exception_can_transition(from: LeaveExceptionStatus, to: LeaveExceptionStatus) -> bool {
    use LeaveExceptionStatus::*;
    matches!(
        (from, to),
        (Raised, UnderReview) | (UnderReview, Resolved) | (Resolved, Closed)
    )
}

/// Leave case coordination (L3-E-034, generic HR case machine).
pub fn case_can_transition(from: HrCaseStatus, to: HrCaseStatus) -> bool {
    use HrCaseStatus::*;
    matches!(
        (from, to),
        (Initiated, PendingApproval)
            | (PendingApproval, Processing)
            | (PendingApproval, Rejected)
            | (Processing, Completed)
            | (Completed, Closed)
            | (Rejected, Closed)
    )
}

/// Faculty workload plans (L3-E-030).
pub fn workload_can_transition(from: WorkloadPlanStatus, to: WorkloadPlanStatus) -> bool {
    use WorkloadPlanStatus::*;
    matches!(
        (from, to),
        (Draft, Submitted)
            | (Submitted, Draft)
            | (Submitted, Approved)
            | (Approved, Active)
            | (Active, Superseded)
    )
}

/// Requests whose dates block an overlapping new request.
pub const BLOCKING_REQUEST_STATUSES: &[&str] = &[
    "submitted",
    "validated",
    "pending_approval",
    "approved",
    "reserved",
    "consumed",
];

// ------------------------------------------------------------------ calendar + day counting

/// The working-day calendar of a leave policy: weekly offs (ISO weekday numbers, 1 = Monday)
/// and holidays.
#[derive(Clone, Debug)]
pub struct WorkCalendar<'a> {
    pub weekly_off_days: &'a [i16],
    pub holidays: &'a [NaiveDate],
}

impl WorkCalendar<'_> {
    pub fn is_working(&self, d: NaiveDate) -> bool {
        let wd = i16::try_from(d.weekday().number_from_monday()).unwrap_or(0);
        !self.weekly_off_days.contains(&wd) && !self.holidays.contains(&d)
    }
}

/// The days of one calendar month a leave touches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonthSegment {
    /// First day of the month (`YYYY-MM-01`).
    pub month: NaiveDate,
    /// First / last leave (working) day inside the month.
    pub first_day: NaiveDate,
    pub last_day: NaiveDate,
    pub days: Decimal,
}

impl MonthSegment {
    /// `consume:<YYYY-MM>` posting key of this segment's consumption entry.
    pub fn posting_key(&self) -> String {
        format!("consume:{}", self.month.format("%Y-%m"))
    }
}

/// Leave days of a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaySplit {
    pub days: Decimal,
    pub working_days: Vec<NaiveDate>,
    pub segments: Vec<MonthSegment>,
}

/// Why a period cannot be requested (422 `code`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitError {
    EndBeforeStart,
    TooLong,
    SpansLeaveYears,
    HalfDaysSameDay,
    HalfDayOnNonWorkingDay,
    NoWorkingDays,
}

impl SplitError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::EndBeforeStart => "end_before_start",
            Self::TooLong => "too_long",
            Self::SpansLeaveYears => "spans_leave_years",
            Self::HalfDaysSameDay => "half_days_same_day",
            Self::HalfDayOnNonWorkingDay => "half_day_on_non_working_day",
            Self::NoWorkingDays => "no_working_days",
        }
    }

    pub fn message(&self) -> &'static str {
        match self {
            Self::EndBeforeStart => "the end date is before the start date",
            Self::TooLong => "a single request covers at most 366 days",
            Self::SpansLeaveYears => {
                "a request stays inside one leave year (calendar year); split it in two"
            }
            Self::HalfDaysSameDay => "a one-day request is either a half day or a full day",
            Self::HalfDayOnNonWorkingDay => "a half day must fall on a working day",
            Self::NoWorkingDays => "the period contains no working day",
        }
    }
}

fn month_start(d: NaiveDate) -> NaiveDate {
    d.with_day(1).unwrap_or(d)
}

/// Counts the leave days of `[start, end]` excluding weekly offs and holidays, half a day off the
/// first / last day when requested, and splits them per calendar month.
pub fn split_days(
    cal: &WorkCalendar<'_>,
    start: NaiveDate,
    end: NaiveDate,
    half_day_start: bool,
    half_day_end: bool,
) -> Result<DaySplit, SplitError> {
    if end < start {
        return Err(SplitError::EndBeforeStart);
    }
    if (end - start).num_days() > 365 {
        return Err(SplitError::TooLong);
    }
    if start.year() != end.year() {
        return Err(SplitError::SpansLeaveYears);
    }
    if start == end && half_day_start && half_day_end {
        return Err(SplitError::HalfDaysSameDay);
    }
    if (half_day_start && !cal.is_working(start)) || (half_day_end && !cal.is_working(end)) {
        return Err(SplitError::HalfDayOnNonWorkingDay);
    }
    let half = Decimal::new(5, 1);
    let mut working_days = Vec::new();
    let mut segments: Vec<MonthSegment> = Vec::new();
    let mut total = Decimal::ZERO;
    let mut d = start;
    loop {
        if cal.is_working(d) {
            let mut w = Decimal::ONE;
            if (half_day_start && d == start) || (half_day_end && d == end) {
                w = half;
            }
            total += w;
            working_days.push(d);
            let m = month_start(d);
            match segments.last_mut() {
                Some(s) if s.month == m => {
                    s.days += w;
                    s.last_day = d;
                }
                _ => segments.push(MonthSegment {
                    month: m,
                    first_day: d,
                    last_day: d,
                    days: w,
                }),
            }
        }
        if d >= end {
            break;
        }
        match d.succ_opt() {
            Some(n) => d = n,
            None => break,
        }
    }
    if total.is_zero() {
        return Err(SplitError::NoWorkingDays);
    }
    Ok(DaySplit {
        days: total,
        working_days,
        segments,
    })
}

// ------------------------------------------------------------------ entitlement rules

/// Days credited by one monthly accrual run (`annual / 12`, rounded half away from zero).
pub fn monthly_accrual(annual: Decimal) -> Decimal {
    round2(annual / Decimal::from(12))
}

/// Year-end split of an unused balance: `(carry_forward, expiry)` — carry forward up to the cap,
/// expire the rest (L3-E-050). Nothing moves for a zero / negative balance.
pub fn year_end_split(available: Decimal, cap: Decimal) -> (Decimal, Decimal) {
    if available <= Decimal::ZERO {
        return (Decimal::ZERO, Decimal::ZERO);
    }
    let carry = available.min(cap.max(Decimal::ZERO));
    (carry, available - carry)
}

/// Encashment eligibility (L3-E-051): `Err(code)` when not eligible.
pub fn encashment_eligibility(
    is_paid: bool,
    cap: Decimal,
    available: Decimal,
    requested: Decimal,
) -> Result<(), &'static str> {
    if !is_paid || cap <= Decimal::ZERO {
        return Err("not_encashable");
    }
    if requested > cap {
        return Err("over_encashment_cap");
    }
    if requested > available {
        return Err("insufficient_balance");
    }
    Ok(())
}

/// `true` when the planned or the actually assigned weekly teaching hours exceed the maximum.
pub fn over_allocated(
    teaching_hours: Decimal,
    planned_hours: Decimal,
    max_hours: Option<Decimal>,
) -> bool {
    max_hours.is_some_and(|m| teaching_hours.max(planned_hours) > m)
}

/// Leave type code rule (`^[A-Z0-9][A-Z0-9_-]{0,15}$`).
pub fn valid_code(code: &str) -> bool {
    let b = code.as_bytes();
    (1..=16).contains(&b.len())
        && (b[0].is_ascii_uppercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

/// One leave type as the policy validation sees it.
#[derive(Clone, Debug)]
pub struct TypeRule<'a> {
    pub code: &'a str,
    pub annual_entitlement: Decimal,
    pub accrual: AccrualFrequency,
    pub encashment_cap: Decimal,
    pub carry_forward_cap: Decimal,
    pub is_paid: bool,
    pub requires_balance: bool,
}

/// Rule validation of a policy version before review (L4-E-044-BH-002/003): returns the
/// findings (conflicts) — empty means valid.
pub fn validate_policy(weekly_off_days: &[i16], types: &[TypeRule<'_>]) -> Vec<String> {
    let mut f = Vec::new();
    if types.is_empty() {
        f.push("the policy defines no leave type".to_owned());
    }
    if weekly_off_days.iter().any(|d| !(1..=7).contains(d)) {
        f.push("weekly off days are ISO weekdays 1 (Monday) … 7 (Sunday)".to_owned());
    }
    let mut offs = weekly_off_days.to_vec();
    offs.sort_unstable();
    offs.dedup();
    if offs.len() >= 7 {
        f.push("every weekday cannot be a weekly off".to_owned());
    }
    for t in types {
        if t.requires_balance
            && (t.accrual == AccrualFrequency::None || t.annual_entitlement.is_zero())
        {
            f.push(format!(
                "{}: requires a balance but never accrues one (set an entitlement or requiresBalance = false)",
                t.code
            ));
        }
        if !t.is_paid && t.encashment_cap > Decimal::ZERO {
            f.push(format!("{}: unpaid leave cannot be encashed", t.code));
        }
        if !t.requires_balance && t.carry_forward_cap > Decimal::ZERO {
            f.push(format!(
                "{}: a type without a balance cannot carry days forward",
                t.code
            ));
        }
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap_or_default()
    }

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap_or_default()
    }

    #[test]
    fn days_skip_weekly_offs_and_holidays_and_split_per_month() {
        let holidays = [d("2026-07-31")];
        let cal = WorkCalendar {
            weekly_off_days: &[7],
            holidays: &holidays,
        };
        // Thu 30 Jul … Tue 4 Aug 2026: 31 Jul holiday, 2 Aug Sunday.
        let s =
            split_days(&cal, d("2026-07-30"), d("2026-08-04"), false, true).unwrap_or_else(|_| {
                DaySplit {
                    days: Decimal::ZERO,
                    working_days: vec![],
                    segments: vec![],
                }
            });
        assert_eq!(s.days, dec("3.5"));
        assert_eq!(s.working_days.len(), 4);
        assert_eq!(s.segments.len(), 2);
        assert_eq!(s.segments[0].days, dec("1"));
        assert_eq!(s.segments[0].posting_key(), "consume:2026-07");
        assert_eq!(s.segments[1].first_day, d("2026-08-01"));
        assert_eq!(s.segments[1].days, dec("2.5"));
        assert_eq!(
            split_days(&cal, d("2026-08-02"), d("2026-08-02"), false, false),
            Err(SplitError::NoWorkingDays)
        );
        assert_eq!(
            split_days(&cal, d("2026-08-03"), d("2026-08-01"), false, false),
            Err(SplitError::EndBeforeStart)
        );
        assert_eq!(
            split_days(&cal, d("2026-12-31"), d("2027-01-02"), false, false),
            Err(SplitError::SpansLeaveYears)
        );
        assert_eq!(
            split_days(&cal, d("2026-08-03"), d("2026-08-03"), true, true),
            Err(SplitError::HalfDaysSameDay)
        );
        assert_eq!(
            split_days(&cal, d("2026-08-02"), d("2026-08-04"), true, false),
            Err(SplitError::HalfDayOnNonWorkingDay)
        );
        let half = split_days(&cal, d("2026-08-03"), d("2026-08-03"), true, false);
        assert_eq!(half.map(|s| s.days), Ok(dec("0.5")));
    }

    #[test]
    fn transitions_follow_the_slice_machines() {
        use LeaveRequestStatus as R;
        assert!(request_can_transition(R::PendingApproval, R::Approved));
        assert!(request_can_transition(R::Reserved, R::Consumed));
        assert!(request_can_transition(R::Consumed, R::Cancelled));
        assert!(!request_can_transition(R::Rejected, R::Approved));
        assert!(!request_can_transition(R::Cancelled, R::Reserved));
        use LeavePolicyStatus as P;
        assert!(policy_can_transition(P::Approved, P::Active));
        assert!(!policy_can_transition(P::Draft, P::Active));
        assert!(adjustment_can_transition(
            LeaveAdjustmentStatus::Approved,
            LeaveAdjustmentStatus::Applied
        ));
        assert!(!encashment_can_transition(
            EncashmentStatus::NotEligible,
            EncashmentStatus::Approved
        ));
        assert!(exception_can_transition(
            LeaveExceptionStatus::UnderReview,
            LeaveExceptionStatus::Resolved
        ));
        assert!(!case_can_transition(
            HrCaseStatus::Initiated,
            HrCaseStatus::Completed
        ));
        assert!(workload_can_transition(
            WorkloadPlanStatus::Approved,
            WorkloadPlanStatus::Active
        ));
    }

    #[test]
    fn entitlement_rules() {
        assert_eq!(monthly_accrual(dec("15")), dec("1.25"));
        assert_eq!(monthly_accrual(dec("10")), dec("0.83"));
        assert_eq!(year_end_split(dec("9.5"), dec("5")), (dec("5"), dec("4.5")));
        assert_eq!(year_end_split(dec("3"), dec("5")), (dec("3"), dec("0")));
        assert_eq!(year_end_split(dec("-1"), dec("5")), (dec("0"), dec("0")));
        assert_eq!(
            encashment_eligibility(true, dec("10"), dec("4"), dec("3")),
            Ok(())
        );
        assert_eq!(
            encashment_eligibility(true, dec("2"), dec("4"), dec("3")),
            Err("over_encashment_cap")
        );
        assert_eq!(
            encashment_eligibility(false, dec("10"), dec("4"), dec("3")),
            Err("not_encashable")
        );
        assert_eq!(
            encashment_eligibility(true, dec("10"), dec("1"), dec("3")),
            Err("insufficient_balance")
        );
        assert!(over_allocated(dec("14"), dec("10"), Some(dec("12"))));
        assert!(!over_allocated(dec("4"), dec("10"), Some(dec("12"))));
        assert!(!over_allocated(dec("40"), dec("10"), None));
        assert!(valid_code("CL") && valid_code("LOP") && !valid_code("cl") && !valid_code(""));
        assert_eq!(
            AccrualFrequency::parse("monthly"),
            Some(AccrualFrequency::Monthly)
        );
    }

    #[test]
    fn policy_validation_finds_conflicts() {
        let ok = TypeRule {
            code: "CL",
            annual_entitlement: dec("12"),
            accrual: AccrualFrequency::Yearly,
            encashment_cap: dec("0"),
            carry_forward_cap: dec("5"),
            is_paid: true,
            requires_balance: true,
        };
        assert!(validate_policy(&[7], std::slice::from_ref(&ok)).is_empty());
        assert_eq!(validate_policy(&[7], &[]).len(), 1);
        let bad = TypeRule {
            code: "LOP",
            annual_entitlement: dec("0"),
            accrual: AccrualFrequency::None,
            encashment_cap: dec("5"),
            carry_forward_cap: dec("0"),
            is_paid: false,
            requires_balance: true,
        };
        assert_eq!(validate_policy(&[7], &[ok, bad]).len(), 2);
        assert!(!validate_policy(&[1, 2, 3, 4, 5, 6, 7], &[]).is_empty());
    }
}
