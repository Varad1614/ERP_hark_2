//! Pure rules of financial aid (L2-F11, L3-F-062…066).

use rust_decimal::Decimal;
use sutra_contracts::finance::status::AidAllocationStatus;

/// Legal allocation transitions.
pub fn can_transition(from: AidAllocationStatus, to: AidAllocationStatus) -> bool {
    use AidAllocationStatus::*;
    matches!(
        (from, to),
        (Proposed, Approved)
            | (Proposed, Closed)
            | (Approved, Allocated)
            | (Allocated, Disbursed)
            | (Allocated, Suspended)
            | (Disbursed, Suspended)
            | (Suspended, Allocated)
            | (Proposed, Revoked)
            | (Approved, Revoked)
            | (Allocated, Revoked)
            | (Disbursed, Revoked)
            | (Suspended, Revoked)
            | (Revoked, Recovered)
            | (Allocated, Closed)
            | (Disbursed, Closed)
            | (Revoked, Closed)
            | (Recovered, Closed)
    )
}

/// The benefit computed for a charge: fixed amounts are bounded by what remains of the
/// allocation, percentages are rounded once to paise, a waiver covers the outstanding amount;
/// every result is capped by the charge's outstanding amount. Stipends are paid in cash, never
/// applied to a charge (`None`).
pub fn benefit_for_charge(
    benefit_type: &str,
    benefit_value: Option<Decimal>,
    remaining: Option<Decimal>,
    charge_total: Decimal,
    outstanding: Decimal,
) -> Option<Decimal> {
    let raw = match benefit_type {
        "fixed_amount" => remaining?,
        "fee_percentage" => {
            crate::shared::money::round_paise(charge_total * benefit_value? / Decimal::ONE_HUNDRED)
        }
        "fee_waiver" => outstanding,
        _ => return None,
    };
    let capped = raw.min(outstanding);
    (capped > Decimal::ZERO).then_some(capped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    use AidAllocationStatus::*;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap_or_default()
    }

    #[test]
    fn allocation_transitions() {
        assert!(can_transition(Proposed, Approved));
        assert!(can_transition(Approved, Allocated));
        assert!(can_transition(Disbursed, Revoked));
        assert!(can_transition(Revoked, Recovered));
        assert!(!can_transition(Revoked, Allocated), "revocation is final");
        assert!(!can_transition(Proposed, Disbursed), "approve first");
        for s in AidAllocationStatus::ALL {
            assert!(!can_transition(Closed, *s));
        }
    }

    #[test]
    fn benefits() {
        let total = d("125000");
        assert_eq!(
            benefit_for_charge(
                "fixed_amount",
                Some(d("30000")),
                Some(d("30000")),
                total,
                total
            ),
            Some(d("30000"))
        );
        assert_eq!(
            benefit_for_charge(
                "fixed_amount",
                Some(d("30000")),
                Some(d("5000")),
                total,
                d("2000")
            ),
            Some(d("2000")),
            "capped by the outstanding amount"
        );
        assert_eq!(
            benefit_for_charge(
                "fee_percentage",
                Some(d("33.33")),
                None,
                d("1000"),
                d("1000")
            ),
            Some(d("333.30"))
        );
        assert_eq!(
            benefit_for_charge("fee_waiver", None, None, total, d("25000")),
            Some(d("25000"))
        );
        assert_eq!(
            benefit_for_charge("stipend", Some(d("1")), None, total, total),
            None
        );
        assert_eq!(
            benefit_for_charge("fixed_amount", Some(d("1")), Some(d("0")), total, total),
            None
        );
    }
}
