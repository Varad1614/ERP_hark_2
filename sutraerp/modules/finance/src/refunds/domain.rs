//! Pure refund rules (L2-F03; unit-tested): the refund state machine and how approved credit is
//! drawn from the party's unapplied receipts.

use rust_decimal::Decimal;
use sutra_contracts::finance::status::RefundStatus;
use uuid::Uuid;

/// Legal refund transitions (L3-F-014 … F-019).
pub fn can_transition(from: RefundStatus, to: RefundStatus) -> bool {
    use RefundStatus as R;
    matches!(
        (from, to),
        (R::Requested, R::UnderReview | R::Cancelled)
            | (R::UnderReview, R::Approved | R::Rejected | R::Cancelled)
            | (R::Approved, R::Processing | R::Cancelled)
            | (R::Processing, R::Completed | R::Failed)
            | (R::Failed, R::Processing | R::Completed | R::Cancelled)
            | (R::Completed, R::Reversed | R::Closed)
            | (R::Rejected | R::Cancelled | R::Reversed, R::Closed)
    )
}

/// States in which the refund holds (consumed) unapplied credit.
pub fn holds_credit(s: RefundStatus) -> bool {
    matches!(
        s,
        RefundStatus::Approved
            | RefundStatus::Processing
            | RefundStatus::Failed
            | RefundStatus::Completed
    )
}

/// Draws `amount` from `(payment, unapplied)` pots in order; `None` when they do not cover it.
pub fn draw_credit(pots: &[(Uuid, Decimal)], amount: Decimal) -> Option<Vec<(Uuid, Decimal)>> {
    let mut left = amount;
    let mut out = Vec::new();
    for (p, avail) in pots {
        if left <= Decimal::ZERO {
            break;
        }
        if *avail <= Decimal::ZERO {
            continue;
        }
        let take = left.min(*avail);
        out.push((*p, take));
        left -= take;
    }
    (left == Decimal::ZERO).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap_or_default()
    }

    #[test]
    fn refund_machine_follows_the_reference_approval() {
        use RefundStatus as R;
        assert!(can_transition(R::Requested, R::UnderReview));
        assert!(can_transition(R::UnderReview, R::Approved));
        assert!(can_transition(R::UnderReview, R::Rejected));
        assert!(
            !can_transition(R::Requested, R::Approved),
            "never skips review"
        );
        assert!(can_transition(R::Failed, R::Processing), "retry");
        assert!(can_transition(R::Completed, R::Reversed));
        assert!(!can_transition(R::Closed, R::Reversed));
        assert!(!can_transition(R::Rejected, R::Approved));
        for s in R::ALL {
            assert!(!can_transition(*s, *s));
        }
        assert!(holds_credit(R::Failed));
        assert!(!holds_credit(R::Reversed));
    }

    #[test]
    fn credit_is_drawn_in_order_and_must_cover_the_amount() {
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        assert_eq!(
            draw_credit(&[(a, d("100")), (b, d("50"))], d("120")),
            Some(vec![(a, d("100")), (b, d("20"))])
        );
        assert_eq!(draw_credit(&[(a, d("100"))], d("100.01")), None);
        assert_eq!(draw_credit(&[], d("1")), None);
    }
}
