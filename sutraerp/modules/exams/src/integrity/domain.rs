//! Pure malpractice rules (L2-I14; L3-I-162…168, 170, 171): the case machine and which
//! penalties debar the candidate's registered papers. Unit-tested.

use sutra_contracts::exams::status::{MalpracticeCaseStatus, MalpracticePenalty};

/// Legal moves of a malpractice case.
pub fn case_can_transition(from: MalpracticeCaseStatus, to: MalpracticeCaseStatus) -> bool {
    use MalpracticeCaseStatus::*;
    matches!(
        (from, to),
        (Reported, UnderInvestigation)
            | (Reported, Closed)
            | (UnderInvestigation, Hearing)
            | (UnderInvestigation, Decided)
            | (UnderInvestigation, Closed)
            | (Hearing, Decided)
            | (Hearing, Closed)
            | (Decided, Appealed)
            | (Decided, Closed)
            | (Decided, UnderInvestigation)
            | (Appealed, UnderReview)
            | (UnderReview, Upheld)
            | (UnderReview, Modified)
            | (UnderReview, Overturned)
            | (Upheld, Closed)
            | (Upheld, UnderInvestigation)
            | (Modified, Closed)
            | (Modified, UnderInvestigation)
            | (Overturned, Closed)
            | (Overturned, UnderInvestigation)
            | (Closed, Archived)
            | (Closed, UnderInvestigation)
    )
}

/// The case carries an effective decision (6C reads these read-only).
pub fn is_decided(s: MalpracticeCaseStatus) -> bool {
    use MalpracticeCaseStatus::*;
    matches!(s, Decided | Upheld | Modified | Overturned)
}

/// A case that may still be merged into another one (no decision yet).
pub fn is_undecided_open(s: MalpracticeCaseStatus) -> bool {
    use MalpracticeCaseStatus::*;
    matches!(s, Reported | UnderInvestigation | Hearing)
}

/// Penalties that set the registered paper(s) `debarred`.
pub fn debars(p: MalpracticePenalty) -> bool {
    matches!(
        p,
        MalpracticePenalty::PaperCancelled
            | MalpracticePenalty::SessionCancelled
            | MalpracticePenalty::Debarred
    )
}

/// Penalties that affect every paper of the registration (else only the case's paper; a case
/// without a paper concerns the whole registration).
pub fn whole_registration(p: MalpracticePenalty, has_paper: bool) -> bool {
    p == MalpracticePenalty::SessionCancelled || !has_paper
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_machine() {
        use MalpracticeCaseStatus::*;
        assert!(case_can_transition(Reported, UnderInvestigation));
        assert!(case_can_transition(Hearing, Decided));
        assert!(case_can_transition(Decided, Appealed));
        assert!(case_can_transition(UnderReview, Overturned));
        assert!(case_can_transition(Closed, UnderInvestigation));
        assert!(case_can_transition(Closed, Archived));
        assert!(!case_can_transition(Reported, Decided));
        assert!(!case_can_transition(Archived, Reported));
        assert!(!case_can_transition(Appealed, Closed));
        assert!(is_decided(Modified) && !is_decided(Hearing));
        assert!(is_undecided_open(Hearing) && !is_undecided_open(Decided));
    }

    #[test]
    fn debarring_penalties() {
        assert!(debars(MalpracticePenalty::Debarred));
        assert!(debars(MalpracticePenalty::PaperCancelled));
        assert!(!debars(MalpracticePenalty::Warning));
        assert!(!debars(MalpracticePenalty::MarksDeduction));
        assert!(whole_registration(
            MalpracticePenalty::SessionCancelled,
            true
        ));
        assert!(!whole_registration(MalpracticePenalty::Debarred, true));
        assert!(whole_registration(MalpracticePenalty::Debarred, false));
    }
}
