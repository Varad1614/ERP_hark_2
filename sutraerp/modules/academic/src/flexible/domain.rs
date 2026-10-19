//! Pure rules of flexible and external learning (L2-D06): the slice-owned state machines and
//! the seat-allocation / conflict-resolution arithmetic. No I/O; unit-tested here.

use sutra_contracts::academic::status::{
    FlexibleEnrolmentStatus as E, LearningOpportunityStatus as O, LearningProviderStatus as P,
    PolicyVersionStatus as V,
};
use uuid::Uuid;

/// Learning provider onboarding (L3-D-044): proposed → under_review → active | proposed
/// (returned); active ⇄ suspended; anything but offboarded → offboarded.
pub fn provider_can_transition(from: P, to: P) -> bool {
    matches!(
        (from, to),
        (P::Proposed, P::UnderReview)
            | (P::UnderReview, P::Active)
            | (P::UnderReview, P::Proposed)
            | (P::Active, P::Suspended)
            | (P::Suspended, P::Active)
            | (P::Proposed | P::Active | P::Suspended, P::Offboarded)
    )
}

/// Learning opportunity (L3-D-040…042): draft → pending_approval → active | draft (returned);
/// active ⇄ suspended; draft / active / suspended → archived (archived is final).
pub fn opportunity_can_transition(from: O, to: O) -> bool {
    matches!(
        (from, to),
        (O::Draft, O::PendingApproval)
            | (O::PendingApproval, O::Active)
            | (O::PendingApproval, O::Draft)
            | (O::Active, O::Suspended)
            | (O::Suspended, O::Active)
            | (O::Draft | O::Active | O::Suspended, O::Archived)
    )
}

/// Flexible enrolment (L3-D-034…038, 048).
pub fn enrolment_can_transition(from: E, to: E) -> bool {
    matches!(
        (from, to),
        (E::Requested, E::Eligible | E::Rejected | E::Withdrawn)
            | (E::Eligible, E::Allocated | E::Waitlisted | E::Rejected | E::Withdrawn)
            | (E::Waitlisted, E::Allocated | E::Withdrawn | E::Rejected)
            // conflict resolution releases a seat back to the waitlist / unallocated outcome
            | (E::Allocated, E::Registered | E::Waitlisted | E::Rejected | E::Withdrawn)
            | (E::Registered, E::InProgress | E::Completed | E::Withdrawn)
            | (E::InProgress, E::Completed | E::Withdrawn)
    )
}

/// `true` while the enrolment is a live institutional obligation (blocks archival).
pub fn enrolment_is_open(s: E) -> bool {
    !matches!(s, E::Completed | E::Withdrawn | E::Rejected)
}

/// Versioned configuration (flexible-learning policy L3-D-045, credit mapping rule L3-D-046):
/// draft → in_review → effective | draft (rejected); effective → superseded | retired.
pub fn version_can_transition(from: V, to: V) -> bool {
    matches!(
        (from, to),
        (V::Draft, V::InReview)
            | (V::Draft, V::Retired)
            | (V::InReview, V::Effective)
            | (V::InReview, V::Draft)
            | (V::Effective, V::Superseded | V::Retired)
            | (V::Superseded, V::Retired)
    )
}

/// Exception case statuses (L3-D-050).
pub const EXCEPTION_STATUSES: &[&str] = &[
    "open",
    "plan_pending",
    "plan_approved",
    "resolved",
    "closed",
];

/// Exception case: open → plan_pending → plan_approved | open (rejected) → resolved → closed;
/// a failed correction re-opens the approved plan (resolved → plan_approved).
pub fn exception_can_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("open", "plan_pending")
            | ("plan_pending", "plan_approved")
            | ("plan_pending", "open")
            | ("plan_approved", "resolved")
            | ("resolved", "plan_approved")
            | ("resolved", "closed")
    )
}

/// Flexible-learning policy rules (stored as JSON on the policy version).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FlexiblePolicyRules {
    /// Live flexible enrolments a student may hold at once (≥ 1).
    pub max_concurrent_enrolments: i32,
    /// Flexible credits a student may hold per academic term (> 0).
    pub max_credits_per_term: f64,
    /// Every request needs a human academic decision (else validated requests are approved by
    /// the policy path without a human decision, L4-D-034-BH-004).
    #[serde(default)]
    pub require_human_approval: bool,
    /// Days a waitlist entry stays valid (≥ 1).
    pub waitlist_validity_days: i32,
    /// Withdrawal allowed up to and including this status (`allocated` | `registered` |
    /// `in_progress`).
    pub withdrawal_allowed_until: String,
}

impl FlexiblePolicyRules {
    /// Problems with the rules (empty = valid), as `(field, message)`.
    pub fn problems(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        if !(1..=20).contains(&self.max_concurrent_enrolments) {
            out.push((
                "rules.maxConcurrentEnrolments",
                "must be between 1 and 20".to_owned(),
            ));
        }
        if !(self.max_credits_per_term > 0.0 && self.max_credits_per_term <= 100.0) {
            out.push((
                "rules.maxCreditsPerTerm",
                "must be above 0 and at most 100".to_owned(),
            ));
        }
        if !(1..=365).contains(&self.waitlist_validity_days) {
            out.push((
                "rules.waitlistValidityDays",
                "must be between 1 and 365".to_owned(),
            ));
        }
        if !matches!(
            self.withdrawal_allowed_until.as_str(),
            "allocated" | "registered" | "in_progress"
        ) {
            out.push((
                "rules.withdrawalAllowedUntil",
                "must be allocated, registered or in_progress".to_owned(),
            ));
        }
        out
    }

    /// May a student withdraw an enrolment in `status` under this policy?
    pub fn allows_withdrawal(&self, status: E) -> bool {
        let rank = |s: E| match s {
            E::Requested | E::Eligible | E::Waitlisted => 0,
            E::Allocated => 1,
            E::Registered => 2,
            E::InProgress => 3,
            _ => 99,
        };
        let limit = match self.withdrawal_allowed_until.as_str() {
            "allocated" => 1,
            "registered" => 2,
            "in_progress" => 3,
            _ => 0,
        };
        rank(status) <= limit
    }
}

/// One demand considered by an allocation run (already eligible).
#[derive(Debug, Clone, PartialEq)]
pub struct Demand {
    pub enrolment_id: Uuid,
    /// Lower = higher priority.
    pub priority: i32,
    /// Tie-breaker: earlier request first (`created_at` / `waitlisted_at` as a sortable key).
    pub order_key: i64,
}

/// Outcome of one demand in an allocation run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocationOutcome {
    Allocate,
    Waitlist,
    /// No seat and no waitlist: governed unallocated / no-seat outcome.
    Unallocated,
}

/// Free seats under governed capacity (never negative).
pub fn free_seats(capacity: i32, reserved: i32, confirmed: i64) -> i64 {
    (i64::from(capacity) - i64::from(reserved) - confirmed).max(0)
}

/// Seat allocation (L4-D-035): highest priority first (then earliest), never above capacity;
/// the rest go to the waitlist when it is enabled, else get the no-seat outcome.
pub fn allocate(
    capacity: i32,
    reserved: i32,
    confirmed: i64,
    waitlist_enabled: bool,
    mut demand: Vec<Demand>,
) -> Vec<(Uuid, AllocationOutcome)> {
    demand.sort_by_key(|d| (d.priority, d.order_key));
    let mut free = free_seats(capacity, reserved, confirmed);
    demand
        .into_iter()
        .map(|d| {
            let o = if free > 0 {
                free -= 1;
                AllocationOutcome::Allocate
            } else if waitlist_enabled {
                AllocationOutcome::Waitlist
            } else {
                AllocationOutcome::Unallocated
            };
            (d.enrolment_id, o)
        })
        .collect()
}

/// Conflict resolution (L4-D-048): given the confirmed allocations, the ones that must be
/// released so the opportunity is back within governed capacity — the lowest-priority, latest
/// allocations first. Allocations of the same enrolment beyond the first are always released.
pub fn over_allocated(capacity: i32, reserved: i32, mut confirmed: Vec<Demand>) -> Vec<Uuid> {
    confirmed.sort_by_key(|d| (d.priority, d.order_key));
    let mut seen = std::collections::HashSet::new();
    let mut keep = Vec::new();
    let mut release = Vec::new();
    for d in confirmed {
        if seen.insert(d.enrolment_id) {
            keep.push(d.enrolment_id);
        } else {
            release.push(d.enrolment_id);
        }
    }
    let cap = free_seats(capacity, reserved, 0) as usize;
    if keep.len() > cap {
        release.extend(keep.split_off(cap));
    }
    release
}

/// Certificate checks → outcome (L4-D-043): any fraud indicator → flagged; all checks
/// confirmed → accepted; otherwise rejected. `None` when a required check is still missing.
pub fn certificate_outcome(
    authenticity: Option<bool>,
    identity: Option<bool>,
    provider_accredited: Option<bool>,
    fraud_indicators: &[String],
) -> Option<&'static str> {
    if !fraud_indicators.is_empty() {
        return Some("flagged");
    }
    match (authenticity, identity, provider_accredited) {
        (Some(true), Some(true), Some(true)) => Some("accepted"),
        (Some(false), _, _) | (_, Some(false), _) | (_, _, Some(false)) => Some("rejected"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(n: u128, priority: i32, order_key: i64) -> Demand {
        Demand {
            enrolment_id: Uuid::from_u128(n),
            priority,
            order_key,
        }
    }

    #[test]
    fn provider_machine() {
        assert!(provider_can_transition(P::Proposed, P::UnderReview));
        assert!(provider_can_transition(P::UnderReview, P::Active));
        assert!(provider_can_transition(P::Active, P::Suspended));
        assert!(provider_can_transition(P::Suspended, P::Active));
        assert!(!provider_can_transition(P::Proposed, P::Active));
        assert!(!provider_can_transition(P::Offboarded, P::Active));
        for s in P::ALL {
            assert!(!provider_can_transition(P::Offboarded, *s));
        }
    }

    #[test]
    fn opportunity_machine() {
        assert!(opportunity_can_transition(O::Draft, O::PendingApproval));
        assert!(opportunity_can_transition(O::PendingApproval, O::Active));
        assert!(opportunity_can_transition(O::Active, O::Suspended));
        assert!(opportunity_can_transition(O::Suspended, O::Archived));
        assert!(!opportunity_can_transition(O::Draft, O::Active));
        assert!(!opportunity_can_transition(O::PendingApproval, O::Archived));
        for s in O::ALL {
            assert!(!opportunity_can_transition(O::Archived, *s), "{s}");
        }
    }

    #[test]
    fn enrolment_machine() {
        assert!(enrolment_can_transition(E::Requested, E::Eligible));
        assert!(enrolment_can_transition(E::Eligible, E::Waitlisted));
        assert!(enrolment_can_transition(E::Waitlisted, E::Allocated));
        assert!(enrolment_can_transition(E::Allocated, E::Registered));
        assert!(enrolment_can_transition(E::Registered, E::InProgress));
        assert!(enrolment_can_transition(E::InProgress, E::Completed));
        assert!(!enrolment_can_transition(E::Requested, E::Allocated));
        assert!(!enrolment_can_transition(E::Completed, E::Withdrawn));
        assert!(enrolment_is_open(E::Waitlisted));
        assert!(!enrolment_is_open(E::Completed));
    }

    #[test]
    fn version_and_exception_machines() {
        assert!(version_can_transition(V::Draft, V::InReview));
        assert!(version_can_transition(V::InReview, V::Effective));
        assert!(version_can_transition(V::Effective, V::Superseded));
        assert!(!version_can_transition(V::Draft, V::Effective));
        assert!(!version_can_transition(V::Retired, V::Effective));
        assert!(exception_can_transition("open", "plan_pending"));
        assert!(exception_can_transition("plan_approved", "resolved"));
        assert!(exception_can_transition("resolved", "closed"));
        assert!(!exception_can_transition("open", "resolved"));
        assert!(!exception_can_transition("open", "closed"));
        assert!(!exception_can_transition("closed", "open"));
        assert_eq!(EXCEPTION_STATUSES.len(), 5);
    }

    #[test]
    fn allocation_respects_capacity_and_priority() {
        let out = allocate(
            3,
            1,
            0,
            true,
            vec![d(1, 5, 1), d(2, 1, 2), d(3, 1, 1), d(4, 9, 0)],
        );
        assert_eq!(out[0], (Uuid::from_u128(3), AllocationOutcome::Allocate));
        assert_eq!(out[1], (Uuid::from_u128(2), AllocationOutcome::Allocate));
        assert_eq!(out[2], (Uuid::from_u128(1), AllocationOutcome::Waitlist));
        assert_eq!(out[3], (Uuid::from_u128(4), AllocationOutcome::Waitlist));
        let out = allocate(1, 0, 1, false, vec![d(1, 1, 1)]);
        assert_eq!(out[0].1, AllocationOutcome::Unallocated);
        assert_eq!(free_seats(2, 1, 5), 0);
    }

    #[test]
    fn conflict_resolution_releases_lowest_priority() {
        let rel = over_allocated(2, 0, vec![d(1, 1, 1), d(2, 5, 2), d(3, 1, 3), d(1, 1, 4)]);
        assert_eq!(rel, vec![Uuid::from_u128(1), Uuid::from_u128(2)]);
        assert!(over_allocated(5, 0, vec![d(1, 1, 1)]).is_empty());
    }

    #[test]
    fn policy_rules_validate_and_gate_withdrawal() {
        let r = FlexiblePolicyRules {
            max_concurrent_enrolments: 2,
            max_credits_per_term: 8.0,
            require_human_approval: false,
            waitlist_validity_days: 14,
            withdrawal_allowed_until: "registered".into(),
        };
        assert!(r.problems().is_empty());
        assert!(r.allows_withdrawal(E::Waitlisted));
        assert!(r.allows_withdrawal(E::Registered));
        assert!(!r.allows_withdrawal(E::InProgress));
        let bad = FlexiblePolicyRules {
            max_concurrent_enrolments: 0,
            withdrawal_allowed_until: "x".into(),
            ..r
        };
        assert_eq!(bad.problems().len(), 2);
    }

    #[test]
    fn certificate_outcomes() {
        assert_eq!(
            certificate_outcome(Some(true), Some(true), Some(true), &[]),
            Some("accepted")
        );
        assert_eq!(
            certificate_outcome(Some(true), Some(false), Some(true), &[]),
            Some("rejected")
        );
        assert_eq!(certificate_outcome(Some(true), None, Some(true), &[]), None);
        assert_eq!(
            certificate_outcome(
                Some(true),
                Some(true),
                Some(true),
                &["duplicate serial".into()]
            ),
            Some("flagged")
        );
    }
}
