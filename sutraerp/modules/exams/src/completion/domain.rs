//! Pure rules of completion evaluation (L2-I13) and session closure (L2-I16).

use rust_decimal::Decimal;
use std::collections::HashSet;
use sutra_contracts::exams::status::CompletionEvaluationStatus;
use uuid::Uuid;

/// Inputs of one completion evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionInput {
    /// `false` when the student's context carries no curriculum version.
    pub has_curriculum: bool,
    pub credits_required: Decimal,
    pub components_required: Vec<Uuid>,
    pub passed_components: HashSet<Uuid>,
    pub credits_earned: Decimal,
    /// A current result is withheld / withdrawn (L3-I-145 blocking condition).
    pub blocked: bool,
}

/// Outcome of a completion evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionOutcome {
    pub status: CompletionEvaluationStatus,
    pub reason: Option<String>,
    pub missing_components: Vec<Uuid>,
    pub components_passed: usize,
}

/// Evaluates completion: every curriculum component passed **and** credits earned ≥ required.
pub fn evaluate(i: &CompletionInput) -> CompletionOutcome {
    let missing: Vec<Uuid> = i
        .components_required
        .iter()
        .filter(|c| !i.passed_components.contains(c))
        .copied()
        .collect();
    let passed = i.components_required.len() - missing.len();
    let (status, reason) = if !i.has_curriculum {
        (
            CompletionEvaluationStatus::Pending,
            Some("the student has no curriculum version".to_owned()),
        )
    } else if i.blocked {
        (
            CompletionEvaluationStatus::Blocked,
            Some("a current result is withheld or withdrawn".to_owned()),
        )
    } else if missing.is_empty() && i.credits_earned >= i.credits_required {
        (CompletionEvaluationStatus::Eligible, None)
    } else {
        (
            CompletionEvaluationStatus::NotEligible,
            Some(format!(
                "{} component(s) outstanding; {} of {} credits earned",
                missing.len(),
                i.credits_earned,
                i.credits_required
            )),
        )
    };
    CompletionOutcome {
        status,
        reason,
        missing_components: missing,
        components_passed: passed,
    }
}

/// What still stands in the way of closing a session (L3-I-187).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClosureChecks {
    /// Current results neither published nor withdrawn.
    pub unpublished_results: i64,
    /// Review requests still in progress.
    pub live_reviews: i64,
    /// Publications not yet completed.
    pub open_publications: i64,
}

impl ClosureChecks {
    pub fn ready(&self) -> bool {
        self.unpublished_results == 0 && self.live_reviews == 0 && self.open_publications == 0
    }

    pub fn blockers(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.unpublished_results > 0 {
            v.push(format!(
                "{} unpublished result(s)",
                self.unpublished_results
            ));
        }
        if self.live_reviews > 0 {
            v.push(format!(
                "{} review request(s) in progress",
                self.live_reviews
            ));
        }
        if self.open_publications > 0 {
            v.push(format!("{} open publication(s)", self.open_publications));
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> CompletionInput {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        CompletionInput {
            has_curriculum: true,
            credits_required: Decimal::new(8, 0),
            components_required: vec![a, b],
            passed_components: [a, b].into_iter().collect(),
            credits_earned: Decimal::new(8, 0),
            blocked: false,
        }
    }

    #[test]
    fn completion_needs_every_component_and_the_credits() {
        assert_eq!(
            evaluate(&input()).status,
            CompletionEvaluationStatus::Eligible
        );
        let mut short = input();
        short.credits_earned = Decimal::new(4, 0);
        assert_eq!(
            evaluate(&short).status,
            CompletionEvaluationStatus::NotEligible
        );
        let mut missing = input();
        let first = missing.components_required[0];
        missing.passed_components.remove(&first);
        let o = evaluate(&missing);
        assert_eq!(o.status, CompletionEvaluationStatus::NotEligible);
        assert_eq!(
            (o.missing_components, o.components_passed),
            (vec![first], 1)
        );
        let mut blocked = input();
        blocked.blocked = true;
        assert_eq!(
            evaluate(&blocked).status,
            CompletionEvaluationStatus::Blocked
        );
        let mut none = input();
        none.has_curriculum = false;
        assert_eq!(evaluate(&none).status, CompletionEvaluationStatus::Pending);
    }

    #[test]
    fn closure_checks() {
        assert!(ClosureChecks::default().ready());
        let c = ClosureChecks {
            unpublished_results: 1,
            live_reviews: 2,
            open_publications: 0,
        };
        assert!(!c.ready());
        assert_eq!(c.blockers().len(), 2);
    }
}
