//! Pure rules of financial adjustments (L2-F04, L3-F-020…024).

use rust_decimal::Decimal;
use sutra_contracts::finance::status::{AdjustmentKind, AdjustmentStatus};

/// Legal adjustment transitions.
pub fn can_transition(from: AdjustmentStatus, to: AdjustmentStatus) -> bool {
    use AdjustmentStatus::*;
    matches!(
        (from, to),
        (Draft, PendingApproval)
            | (Draft, Superseded)
            | (PendingApproval, Approved)
            | (PendingApproval, Rejected)
            | (PendingApproval, Posted)
            | (Approved, Posted)
            | (Posted, Reversed)
            | (Posted, Superseded)
            | (Rejected, Superseded)
    )
}

/// Signed effect on the charge's `adjustment_amount`: a debit note increases the liability,
/// every other kind reduces it.
pub fn delta(kind: AdjustmentKind, amount: Decimal) -> Decimal {
    if kind == AdjustmentKind::DebitNote {
        amount
    } else {
        -amount
    }
}

/// Document number kind (contract §3.4): `CN-` credit notes and concessions, `DN-` debit notes,
/// `ADJ-` write-offs and waivers.
pub fn number_kind(kind: AdjustmentKind) -> (&'static str, &'static str) {
    use crate::shared::numbering::kind;
    match kind {
        AdjustmentKind::CreditNote | AdjustmentKind::Concession => kind::CREDIT_NOTE,
        AdjustmentKind::DebitNote => kind::DEBIT_NOTE,
        AdjustmentKind::WriteOff | AdjustmentKind::Waiver => kind::ADJUSTMENT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use AdjustmentStatus::*;

    #[test]
    fn adjustment_transitions() {
        assert!(can_transition(Draft, PendingApproval));
        assert!(can_transition(PendingApproval, Posted));
        assert!(can_transition(Posted, Reversed));
        assert!(can_transition(Posted, Superseded));
        assert!(!can_transition(Reversed, Posted), "a reversal is final");
        assert!(!can_transition(Draft, Posted), "approval first");
        assert!(!can_transition(Rejected, Posted));
        for s in AdjustmentStatus::ALL {
            assert!(!can_transition(Superseded, *s));
        }
    }

    #[test]
    fn deltas_and_numbers() {
        let ten = Decimal::TEN;
        assert_eq!(delta(AdjustmentKind::DebitNote, ten), ten);
        assert_eq!(delta(AdjustmentKind::WriteOff, ten), -ten);
        assert_eq!(delta(AdjustmentKind::CreditNote, ten), -ten);
        assert_eq!(number_kind(AdjustmentKind::Concession).1, "CN");
        assert_eq!(number_kind(AdjustmentKind::DebitNote).1, "DN");
        assert_eq!(number_kind(AdjustmentKind::Waiver).1, "ADJ");
    }
}
