//! Pure receivable rules (L2-F07; unit-tested): status derivation from the charge, ageing
//! buckets, dunning escalation and the manual transitions.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use sutra_contracts::finance::status::{ChargeStatus, DunningStatus, ReceivableStatus};

/// Facts of the charge behind a receivable at evaluation time.
#[derive(Clone, Copy, Debug)]
pub struct ChargeFacts {
    pub status: ChargeStatus,
    /// total + adjustments
    pub owed: Decimal,
    pub outstanding: Decimal,
    pub due: Option<NaiveDate>,
}

/// The receivable status the charge facts imply (L3-F-039/040/041/043).
pub fn derive(current: ReceivableStatus, c: ChargeFacts, today: NaiveDate) -> ReceivableStatus {
    use ReceivableStatus as R;
    if current == R::WrittenOff || c.status == ChargeStatus::WrittenOff {
        return R::WrittenOff;
    }
    let finished = matches!(
        c.status,
        ChargeStatus::Cancelled
            | ChargeStatus::Waived
            | ChargeStatus::Closed
            | ChargeStatus::Archived
    );
    if c.outstanding <= Decimal::ZERO || finished {
        return R::Settled;
    }
    if current == R::Settled {
        return R::Reopened; // new evidence (e.g. a reversed payment) re-opened it
    }
    let overdue = c.status.accepts_settlement() && c.due.is_some_and(|d| d < today);
    if overdue {
        return R::Overdue;
    }
    if current == R::Reopened {
        return R::Reopened;
    }
    if c.outstanding < c.owed {
        R::PartiallySettled
    } else {
        R::Open
    }
}

/// Manual transitions (write-off decision, reopening).
pub fn can_reopen(from: ReceivableStatus) -> bool {
    matches!(
        from,
        ReceivableStatus::WrittenOff | ReceivableStatus::Settled
    )
}

/// A write-off needs something outstanding on a live receivable.
pub fn can_write_off(from: ReceivableStatus) -> bool {
    matches!(
        from,
        ReceivableStatus::Open
            | ReceivableStatus::PartiallySettled
            | ReceivableStatus::Overdue
            | ReceivableStatus::Reopened
    )
}

/// Days past due (0 when not yet due / no due date).
pub fn days_overdue(due: Option<NaiveDate>, today: NaiveDate) -> i64 {
    due.map(|d| (today - d).num_days().max(0)).unwrap_or(0)
}

/// Ageing bucket of a receivable.
pub fn ageing_bucket(days: i64) -> &'static str {
    match days {
        d if d <= 0 => "current",
        1..=30 => "1-30",
        31..=60 => "31-60",
        61..=90 => "61-90",
        _ => "90+",
    }
}

/// Maximum dunning level; the third notice escalates the case.
pub const MAX_DUNNING_LEVEL: i32 = 3;

/// Case status after sending notice `level`.
pub fn dunning_status_after(level: i32) -> DunningStatus {
    if level >= MAX_DUNNING_LEVEL {
        DunningStatus::Escalated
    } else {
        DunningStatus::NoticeSent
    }
}

/// Live dunning case states.
pub fn dunning_live(s: DunningStatus) -> bool {
    matches!(
        s,
        DunningStatus::Open | DunningStatus::NoticeSent | DunningStatus::Escalated
    )
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
    fn facts(status: ChargeStatus, out: &str, due: Option<NaiveDate>) -> ChargeFacts {
        ChargeFacts {
            status,
            owed: d("1000"),
            outstanding: d(out),
            due,
        }
    }

    #[test]
    fn derivation_follows_the_charge() {
        use ReceivableStatus as R;
        let t = day(8, 1);
        assert_eq!(
            derive(
                R::Open,
                facts(ChargeStatus::Published, "1000", Some(day(9, 1))),
                t
            ),
            R::Open
        );
        assert_eq!(
            derive(R::Open, facts(ChargeStatus::Active, "400", None), t),
            R::PartiallySettled
        );
        assert_eq!(
            derive(
                R::Open,
                facts(ChargeStatus::Active, "400", Some(day(7, 1))),
                t
            ),
            R::Overdue
        );
        assert_eq!(
            derive(
                R::Open,
                facts(ChargeStatus::Disputed, "400", Some(day(7, 1))),
                t
            ),
            R::PartiallySettled,
            "disputed dues are not dunned"
        );
        assert_eq!(
            derive(
                R::Overdue,
                facts(ChargeStatus::Active, "0", Some(day(7, 1))),
                t
            ),
            R::Settled
        );
        assert_eq!(
            derive(R::Settled, facts(ChargeStatus::Active, "100", None), t),
            R::Reopened
        );
        assert_eq!(
            derive(R::Reopened, facts(ChargeStatus::Active, "100", None), t),
            R::Reopened
        );
        assert_eq!(
            derive(R::WrittenOff, facts(ChargeStatus::Active, "0", None), t),
            R::WrittenOff
        );
        assert_eq!(
            derive(R::Open, facts(ChargeStatus::Cancelled, "1000", None), t),
            R::Settled
        );
    }

    #[test]
    fn ageing_and_dunning() {
        assert_eq!(days_overdue(Some(day(7, 1)), day(8, 1)), 31);
        assert_eq!(days_overdue(None, day(8, 1)), 0);
        assert_eq!(ageing_bucket(0), "current");
        assert_eq!(ageing_bucket(31), "31-60");
        assert_eq!(ageing_bucket(120), "90+");
        assert_eq!(dunning_status_after(1), DunningStatus::NoticeSent);
        assert_eq!(dunning_status_after(3), DunningStatus::Escalated);
        assert!(can_reopen(ReceivableStatus::WrittenOff));
        assert!(!can_reopen(ReceivableStatus::Open));
        assert!(can_write_off(ReceivableStatus::Overdue));
        assert!(!can_write_off(ReceivableStatus::Settled));
        assert!(dunning_live(DunningStatus::Escalated));
        assert!(!dunning_live(DunningStatus::Resolved));
    }
}
