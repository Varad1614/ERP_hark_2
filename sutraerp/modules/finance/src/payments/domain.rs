//! Pure rules of slice 5B payments (unit-tested): the payment / unidentified-receipt / bulk
//! collection state machines and the allocation planner (oldest due first, surplus kept as an
//! unapplied receipt — L2-F02 §allocation, L3-F-008).

use rust_decimal::Decimal;
use sutra_contracts::finance::ids::ChargeId;
use sutra_contracts::finance::status::{
    PaymentBatchStatus, PaymentStatus, UnidentifiedReceiptStatus,
};

/// Legal payment transitions. `failed | expired | cancelled → completed` is the safe-recovery
/// path for a capture the gateway confirms late (L3-F-010: money received is a fact).
pub fn can_transition(from: PaymentStatus, to: PaymentStatus) -> bool {
    use PaymentStatus as P;
    matches!(
        (from, to),
        (
            P::Initiated,
            P::Pending | P::Completed | P::Failed | P::Cancelled | P::Expired
        ) | (
            P::Pending,
            P::Completed | P::Failed | P::Cancelled | P::Expired
        ) | (P::Failed | P::Expired | P::Cancelled, P::Completed)
            | (P::Completed, P::Reversed | P::ChargedBack)
    )
}

/// Legal unidentified receipt transitions (L3-F-009).
pub fn can_transition_unidentified(
    from: UnidentifiedReceiptStatus,
    to: UnidentifiedReceiptStatus,
) -> bool {
    use UnidentifiedReceiptStatus as U;
    matches!(
        (from, to),
        (
            U::Unidentified,
            U::UnderInvestigation | U::Identified | U::Refunded
        ) | (U::UnderInvestigation, U::Identified | U::Refunded)
            | (U::Identified, U::Allocated)
    )
}

/// Legal bulk collection transitions (L3-F-013).
pub fn can_transition_batch(from: PaymentBatchStatus, to: PaymentBatchStatus) -> bool {
    use PaymentBatchStatus as B;
    matches!(
        (from, to),
        (B::Draft, B::Validated | B::Failed)
            | (B::Validated, B::Executing)
            | (
                B::Executing,
                B::Completed | B::PartiallyCompleted | B::Failed
            )
    )
}

/// Outcome of a finished bulk collection from its item counts.
pub fn batch_outcome(completed: usize, failed: usize) -> PaymentBatchStatus {
    match (completed, failed) {
        (0, _) => PaymentBatchStatus::Failed,
        (_, 0) => PaymentBatchStatus::Completed,
        _ => PaymentBatchStatus::PartiallyCompleted,
    }
}

/// An open charge as the planner sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenCharge {
    pub id: ChargeId,
    pub outstanding: Decimal,
}

/// Allocation plan: amounts per charge plus the unapplied surplus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllocationPlan {
    pub lines: Vec<(ChargeId, Decimal)>,
    pub unapplied: Decimal,
}

/// Distributes `amount` over `open` in the given order (callers pass due date / number order),
/// never more than a charge's outstanding; the rest stays unapplied.
pub fn plan_allocation(open: &[OpenCharge], amount: Decimal) -> AllocationPlan {
    let mut left = amount;
    let mut lines = Vec::new();
    for c in open {
        if left <= Decimal::ZERO {
            break;
        }
        if c.outstanding <= Decimal::ZERO {
            continue;
        }
        let take = left.min(c.outstanding);
        lines.push((c.id, take));
        left -= take;
    }
    AllocationPlan {
        lines,
        unapplied: left.max(Decimal::ZERO),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    use uuid::Uuid;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap_or_default()
    }

    fn charge(n: u128, out: &str) -> OpenCharge {
        OpenCharge {
            id: ChargeId(Uuid::from_u128(n)),
            outstanding: d(out),
        }
    }

    #[test]
    fn allocation_fills_oldest_first_and_keeps_the_surplus() {
        let open = [charge(1, "1000.00"), charge(2, "0"), charge(3, "500.50")];
        let p = plan_allocation(&open, d("1200.00"));
        assert_eq!(
            p.lines,
            vec![(open[0].id, d("1000.00")), (open[2].id, d("200.00"))]
        );
        assert_eq!(p.unapplied, d("0"));
        let p = plan_allocation(&open, d("2000.00"));
        assert_eq!(p.unapplied, d("499.50"));
        assert_eq!(p.lines.len(), 2);
        let p = plan_allocation(&[], d("10.00"));
        assert!(p.lines.is_empty());
        assert_eq!(p.unapplied, d("10.00"));
    }

    #[test]
    fn payment_machine() {
        use PaymentStatus as P;
        assert!(can_transition(P::Pending, P::Completed));
        assert!(can_transition(P::Failed, P::Completed), "late capture");
        assert!(can_transition(P::Completed, P::Reversed));
        assert!(can_transition(P::Completed, P::ChargedBack));
        assert!(!can_transition(P::Completed, P::Cancelled));
        assert!(!can_transition(P::Reversed, P::Completed));
        assert!(!can_transition(P::ChargedBack, P::Reversed));
        for s in P::ALL {
            assert!(!can_transition(*s, *s), "{s} → {s}");
        }
    }

    #[test]
    fn unidentified_and_batch_machines() {
        use PaymentBatchStatus as B;
        use UnidentifiedReceiptStatus as U;
        assert!(can_transition_unidentified(U::Unidentified, U::Identified));
        assert!(can_transition_unidentified(U::Identified, U::Allocated));
        assert!(!can_transition_unidentified(U::Allocated, U::Refunded));
        assert!(can_transition_batch(B::Draft, B::Validated));
        assert!(!can_transition_batch(B::Draft, B::Executing));
        assert_eq!(batch_outcome(2, 0), B::Completed);
        assert_eq!(batch_outcome(1, 1), B::PartiallyCompleted);
        assert_eq!(batch_outcome(0, 3), B::Failed);
    }
}
