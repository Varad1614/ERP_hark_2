//! Pure rules of the 5A charge area: fee structure and dispute transitions, instalment splits
//! and the derived instalment status (L3-F-001, F-004, F-005).

use chrono::{Months, NaiveDate};
use rust_decimal::Decimal;
use sutra_contracts::finance::status::{ChargeDisputeStatus, FeeStructureStatus, InstalmentStatus};

/// Legal fee structure transitions (draft → in_review → approved → published → superseded,
/// with return to draft on rejection and retirement of unpublished / superseded versions).
pub fn structure_can_transition(from: FeeStructureStatus, to: FeeStructureStatus) -> bool {
    use FeeStructureStatus::*;
    matches!(
        (from, to),
        (Draft, InReview)
            | (InReview, Approved)
            | (InReview, Draft)
            | (Approved, Published)
            | (Approved, Draft)
            | (Published, Superseded)
            | (Draft, Retired)
            | (Approved, Retired)
            | (Superseded, Retired)
            | (Published, Retired)
    )
}

/// Legal dispute transitions.
pub fn dispute_can_transition(from: ChargeDisputeStatus, to: ChargeDisputeStatus) -> bool {
    use ChargeDisputeStatus::*;
    matches!(
        (from, to),
        (Raised, UnderReview)
            | (Raised, Upheld)
            | (Raised, Rejected)
            | (Raised, Withdrawn)
            | (UnderReview, Upheld)
            | (UnderReview, Rejected)
            | (UnderReview, Withdrawn)
    )
}

/// Equal split of `total` into `count` instalments, each rounded once to paise (half away from
/// zero); the remainder goes to the last instalment (contract §3.1).
pub fn split_equal(total: Decimal, count: u32) -> Vec<Decimal> {
    if count == 0 {
        return Vec::new();
    }
    let each = crate::shared::money::round_paise(total / Decimal::from(count));
    let mut v = vec![each; count as usize];
    let allocated = each * Decimal::from(count - 1);
    if let Some(last) = v.last_mut() {
        *last = total - allocated;
    }
    v
}

/// `count` monthly due dates from `first` every `interval_months`.
pub fn monthly_dates(first: NaiveDate, count: u32, interval_months: u32) -> Vec<NaiveDate> {
    (0..count)
        .filter_map(|i| first.checked_add_months(Months::new(i * interval_months.max(1))))
        .collect()
}

/// Validates a schedule: 1…24 instalments, positive amounts summing to `net`, strictly
/// increasing due dates. Returns a human-readable reason on failure.
pub fn check_schedule(net: Decimal, items: &[(NaiveDate, Decimal)]) -> Result<(), String> {
    if items.is_empty() || items.len() > 24 {
        return Err("a schedule has between 1 and 24 instalments".into());
    }
    if items.iter().any(|(_, a)| *a <= Decimal::ZERO) {
        return Err("every instalment amount must be positive".into());
    }
    if items.windows(2).any(|w| w[1].0 <= w[0].0) {
        return Err("due dates must be strictly increasing".into());
    }
    let sum: Decimal = items.iter().map(|(_, a)| *a).sum();
    if sum != net {
        return Err(format!(
            "instalments sum to {} but the charge amount is {}",
            crate::shared::money::fmt(sum),
            crate::shared::money::fmt(net)
        ));
    }
    Ok(())
}

/// Status of each instalment derived from the charge's settled amount (5B allocates to the
/// charge, oldest due first) and the business date: paid while the cumulative amount is
/// covered, otherwise overdue / due / scheduled by due date; waived / cancelled stay.
pub fn derive_statuses(
    items: &[(NaiveDate, Decimal, InstalmentStatus)],
    settled: Decimal,
    today: NaiveDate,
) -> Vec<InstalmentStatus> {
    let mut cumulative = Decimal::ZERO;
    items
        .iter()
        .map(|(due, amount, stored)| {
            if matches!(
                stored,
                InstalmentStatus::Waived | InstalmentStatus::Cancelled
            ) {
                return *stored;
            }
            cumulative += *amount;
            if cumulative <= settled {
                InstalmentStatus::Paid
            } else if *due < today {
                InstalmentStatus::Overdue
            } else if *due == today {
                InstalmentStatus::Due
            } else {
                InstalmentStatus::Scheduled
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap_or_default()
    }
    fn day(m: u32, dd: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, dd).unwrap_or_default()
    }

    #[test]
    fn structure_and_dispute_transitions() {
        use FeeStructureStatus::*;
        assert!(structure_can_transition(Draft, InReview));
        assert!(structure_can_transition(Approved, Published));
        assert!(
            !structure_can_transition(Draft, Published),
            "approval first"
        );
        assert!(!structure_can_transition(Superseded, Published));
        use ChargeDisputeStatus as D;
        assert!(dispute_can_transition(D::Raised, D::Upheld));
        assert!(!dispute_can_transition(D::Rejected, D::Upheld));
        assert!(!dispute_can_transition(D::Withdrawn, D::Raised));
    }

    #[test]
    fn splits_keep_the_remainder_in_the_last_instalment() {
        assert_eq!(
            split_equal(d("100000"), 3),
            vec![d("33333.33"), d("33333.33"), d("33333.34")]
        );
        assert_eq!(split_equal(d("125000"), 2), vec![d("62500"), d("62500")]);
        assert_eq!(split_equal(d("0.05"), 2), vec![d("0.03"), d("0.02")]);
        assert!(split_equal(d("1"), 0).is_empty());
        assert_eq!(
            monthly_dates(day(1, 31), 3, 1),
            vec![day(1, 31), day(2, 28), day(3, 31)]
        );
    }

    #[test]
    fn schedules_validate() {
        let ok = [(day(7, 1), d("600")), (day(8, 1), d("400"))];
        assert!(check_schedule(d("1000"), &ok).is_ok());
        assert!(check_schedule(d("999"), &ok).is_err());
        let back = [(day(8, 1), d("600")), (day(7, 1), d("400"))];
        assert!(check_schedule(d("1000"), &back).is_err());
        assert!(check_schedule(d("0"), &[]).is_err());
    }

    #[test]
    fn instalment_status_follows_settlement() {
        use InstalmentStatus::*;
        let items = [
            (day(7, 1), d("500"), Scheduled),
            (day(8, 1), d("500"), Scheduled),
            (day(9, 1), d("500"), Scheduled),
        ];
        assert_eq!(
            derive_statuses(&items, d("500"), day(8, 1)),
            vec![Paid, Due, Scheduled]
        );
        assert_eq!(
            derive_statuses(&items, d("0"), day(8, 2)),
            vec![Overdue, Overdue, Scheduled]
        );
        assert_eq!(
            derive_statuses(&items, d("1500"), day(12, 1)),
            vec![Paid, Paid, Paid]
        );
    }
}
