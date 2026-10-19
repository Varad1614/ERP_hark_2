//! Pure rules of revaluation / rechecking (L2-I11 + amendment) and supplementary eligibility
//! (L2-I12).

use chrono::{Duration, NaiveDate};
use rust_decimal::Decimal;
use sutra_contracts::exams::status::{
    ExamSessionKind, RevaluationStatus, ReviewKind, SupplementaryEligibilityStatus,
};

/// Days after publication during which a candidate may request a review (default until 6A's
/// examination policy makes it configurable — contract change).
pub const REVIEW_WINDOW_DAYS: i64 = 15;

/// Attempts allowed per curriculum component (default until 6A's policy carries `maxAttempts`).
pub const DEFAULT_MAX_ATTEMPTS: i32 = 4;

/// Fee code + amount of a review kind (Finance fee head created on first use by `ExamFeePort`).
pub fn fee_of(kind: ReviewKind) -> (&'static str, Decimal) {
    match kind {
        ReviewKind::Revaluation => ("EXAM-REVAL", Decimal::new(50000, 2)),
        ReviewKind::Rechecking => ("EXAM-RECHECK", Decimal::new(20000, 2)),
    }
}

/// `true` while the review window of a result published on `published_on` is open on `today`.
pub fn within_window(published_on: NaiveDate, today: NaiveDate) -> bool {
    today >= published_on && today <= published_on + Duration::days(REVIEW_WINDOW_DAYS)
}

/// Legal moves of a review request.
pub fn can_transition(from: RevaluationStatus, to: RevaluationStatus) -> bool {
    use RevaluationStatus::*;
    matches!(
        (from, to),
        (Submitted, EligibilityVerified)
            | (Submitted | EligibilityVerified | FeeVerified, Rejected)
            | (EligibilityVerified, FeePending)
            | (FeePending, FeeVerified)
            | (FeeVerified, Approved)
            | (Approved, ReviewInProgress)
            | (ReviewInProgress, ReviewCompleted)
            | (ReviewCompleted, ReviewInProgress)
            | (ReviewCompleted, OutcomeVerified)
            | (OutcomeVerified, RevisedMarksApproved)
            | (OutcomeVerified, Closed)
            | (RevisedMarksApproved, ResultRevisionInitiated)
            | (RevisedMarksApproved, Closed)
            | (ResultRevisionInitiated, Closed)
            | (Closed, CorrectionReview)
            | (CorrectionReview, Closed)
            | (
                Submitted | EligibilityVerified | FeePending | FeeVerified | Approved,
                Withdrawn
            )
            | (Withdrawn | Rejected | Closed, Archived)
    )
}

/// States in which the candidate (or the office) may still withdraw (before review starts,
/// L3-I-120/121).
pub fn withdrawable(s: RevaluationStatus) -> bool {
    can_transition(s, RevaluationStatus::Withdrawn)
}

/// Legal moves of a supplementary eligibility.
pub fn eligibility_can_transition(
    from: SupplementaryEligibilityStatus,
    to: SupplementaryEligibilityStatus,
) -> bool {
    use SupplementaryEligibilityStatus::*;
    matches!(
        (from, to),
        (Eligible, Applied)
            | (Eligible | Applied, Authorised)
            | (Eligible | Applied | Authorised, Cancelled)
            | (Eligible, Exhausted)
    )
}

/// Status of a new eligibility from the attempts used (L3-I-142 exhausted attempts).
pub fn eligibility_status(attempts_used: i32, max_attempts: i32) -> SupplementaryEligibilityStatus {
    if attempts_used >= max_attempts {
        SupplementaryEligibilityStatus::Exhausted
    } else {
        SupplementaryEligibilityStatus::Eligible
    }
}

/// Session kinds that offer subsequent attempts (L3-I-134 opportunity hand-off).
pub fn offers_attempts(kind: ExamSessionKind) -> bool {
    matches!(
        kind,
        ExamSessionKind::Supplementary | ExamSessionKind::Improvement | ExamSessionKind::Makeup
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_machine_and_window() {
        use RevaluationStatus::*;
        assert!(can_transition(FeePending, FeeVerified));
        assert!(!can_transition(Submitted, Approved));
        assert!(withdrawable(Approved) && !withdrawable(ReviewInProgress));
        let d = NaiveDate::from_ymd_opt(2026, 12, 1).unwrap_or_default();
        assert!(within_window(d, d + Duration::days(15)));
        assert!(!within_window(d, d + Duration::days(16)));
        assert_eq!(fee_of(ReviewKind::Revaluation).1.to_string(), "500.00");
        assert_eq!(fee_of(ReviewKind::Rechecking).0, "EXAM-RECHECK");
    }

    #[test]
    fn eligibility_rules() {
        use SupplementaryEligibilityStatus::*;
        assert_eq!(eligibility_status(1, 4), Eligible);
        assert_eq!(eligibility_status(4, 4), Exhausted);
        assert!(eligibility_can_transition(Eligible, Authorised));
        assert!(!eligibility_can_transition(Exhausted, Authorised));
        assert!(offers_attempts(ExamSessionKind::Supplementary));
        assert!(!offers_attempts(ExamSessionKind::Regular));
    }
}
