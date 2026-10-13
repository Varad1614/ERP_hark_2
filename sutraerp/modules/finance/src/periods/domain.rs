//! Pure rules of financial years and periods (L2-F09): legal transitions and the monthly period
//! calendar of a year.

use chrono::{Datelike, Months, NaiveDate};
use sutra_contracts::finance::status::{FinancialPeriodStatus as P, FinancialYearStatus as Y};

/// Legal financial period transitions (`409` otherwise).
pub fn can_transition_period(from: P, to: P) -> bool {
    matches!(
        (from, to),
        (P::Planned, P::Open)
            | (P::Open, P::Locked)
            | (P::Open, P::Closed)
            | (P::Locked, P::Open)
            | (P::Locked, P::Closed)
            | (P::Closed, P::Reopened)
            | (P::Closed, P::Archived)
            | (P::Reopened, P::Locked)
            | (P::Reopened, P::Closed)
    )
}

/// Legal financial year transitions.
pub fn can_transition_year(from: Y, to: Y) -> bool {
    matches!(
        (from, to),
        (Y::Draft, Y::Open)
            | (Y::Open, Y::Closing)
            | (Y::Open, Y::Closed)
            | (Y::Closing, Y::Open)
            | (Y::Closing, Y::Closed)
            | (Y::Closed, Y::Archived)
    )
}

/// One period of the calendar: `(period_no, name, start, end)`.
pub type PeriodSpan = (i32, String, NaiveDate, NaiveDate);

/// Monthly periods covering `[start, end]` (the first and last may be partial months). `None`
/// when the year would need more than 13 periods or the bounds are inverted.
pub fn monthly_periods(start: NaiveDate, end: NaiveDate) -> Option<Vec<PeriodSpan>> {
    if end < start {
        return None;
    }
    let mut out = Vec::new();
    let mut cur = start;
    let mut n = 1;
    while cur <= end {
        if n > 13 {
            return None;
        }
        let first_of_month = NaiveDate::from_ymd_opt(cur.year(), cur.month(), 1)?;
        let month_end = first_of_month
            .checked_add_months(Months::new(1))?
            .pred_opt()?;
        let stop = month_end.min(end);
        out.push((n, cur.format("%b %Y").to_string(), cur, stop));
        cur = stop.succ_opt()?;
        n += 1;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn indian_financial_year_has_twelve_monthly_periods() {
        let p = monthly_periods(d(2026, 4, 1), d(2027, 3, 31)).unwrap();
        assert_eq!(p.len(), 12);
        assert_eq!(p[0], (1, "Apr 2026".into(), d(2026, 4, 1), d(2026, 4, 30)));
        assert_eq!(p[10].3, d(2027, 2, 28));
        assert_eq!(
            p[11],
            (12, "Mar 2027".into(), d(2027, 3, 1), d(2027, 3, 31))
        );
        // partial first / last months
        let q = monthly_periods(d(2026, 7, 15), d(2026, 9, 10)).unwrap();
        assert_eq!(q.len(), 3);
        assert_eq!((q[0].2, q[0].3), (d(2026, 7, 15), d(2026, 7, 31)));
        assert_eq!((q[2].2, q[2].3), (d(2026, 9, 1), d(2026, 9, 10)));
        assert!(monthly_periods(d(2026, 1, 1), d(2027, 6, 30)).is_none());
        assert!(monthly_periods(d(2026, 5, 1), d(2026, 4, 1)).is_none());
    }

    #[test]
    fn period_machine() {
        assert!(can_transition_period(P::Planned, P::Open));
        assert!(can_transition_period(P::Open, P::Closed));
        assert!(can_transition_period(P::Closed, P::Reopened));
        assert!(can_transition_period(P::Reopened, P::Closed));
        assert!(!can_transition_period(P::Planned, P::Closed));
        assert!(!can_transition_period(P::Archived, P::Reopened));
        assert!(!can_transition_period(P::Closed, P::Open));
        for s in P::ALL {
            assert!(!can_transition_period(P::Archived, *s));
        }
        assert!(can_transition_year(Y::Draft, Y::Open));
        assert!(can_transition_year(Y::Open, Y::Closed));
        assert!(!can_transition_year(Y::Closed, Y::Open));
        assert!(!can_transition_year(Y::Draft, Y::Closed));
    }
}
