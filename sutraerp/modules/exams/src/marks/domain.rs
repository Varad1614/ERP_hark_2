//! Pure 6B rules: the mark sheet (per-paper evaluation) machine, the moderation machine, the
//! evaluation-round resolution (panel / second / adjudicating evaluation, L3-I-073, 080…082) and
//! the moderation arithmetic (grace / scaling, L3-I-075). No I/O; unit-tested.

use crate::shared::grading::round2;
use rust_decimal::Decimal;
use sutra_contracts::exams::status::{EvaluationRound, EvaluationStatus, ModerationStatus};

/// Legal moves of a mark sheet (L2-I08 §20 as used by 6B).
pub fn sheet_can_transition(from: EvaluationStatus, to: EvaluationStatus) -> bool {
    use EvaluationStatus::*;
    matches!(
        (from, to),
        (Pending, Planned)
            | (Planned, Assigned)
            | (Planned, InEvaluation)
            | (Assigned, InEvaluation)
            | (InEvaluation, Evaluated)
            | (Evaluated, InEvaluation)
            | (Evaluated, Validated)
            | (Validated, Moderation)
            | (Moderation, Validated)
            | (Validated, InEvaluation)
            | (Validated, Approved)
            | (Approved, Finalised)
            | (Finalised, InEvaluation)
            | (Finalised, Closed)
            | (Closed, Archived)
    )
}

/// Marks may be entered / imported while the sheet is in one of these states.
pub fn sheet_accepts_entry(s: EvaluationStatus) -> bool {
    matches!(
        s,
        EvaluationStatus::Planned | EvaluationStatus::Assigned | EvaluationStatus::InEvaluation
    )
}

/// Legal moves of a moderation (L3-I-075).
pub fn moderation_can_transition(from: ModerationStatus, to: ModerationStatus) -> bool {
    use ModerationStatus::*;
    matches!(
        (from, to),
        (Requested, Validated)
            | (Requested, Withdrawn)
            | (Requested, Rejected)
            | (Validated, Approved)
            | (Validated, Rejected)
            | (Validated, Withdrawn)
            | (Approved, Applied)
            | (Approved, Withdrawn)
    )
}

/// One recorded evaluation of a script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvaluationMark {
    pub round: EvaluationRound,
    pub evaluator: String,
    pub marks: Decimal,
}

/// Where a script stands after its evaluations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// No evaluation recorded.
    None,
    /// The authoritative evaluated marks.
    Resolved(Decimal),
    /// Policy requires a second evaluation that is not recorded yet.
    AwaitingSecond { first: Decimal },
    /// First / second difference above the threshold: an adjudicating evaluation is required.
    AwaitingAdjudication { first: Decimal, second: Decimal },
}

/// Mean of the first-round evaluations (a panel records one per member, L3-I-073).
pub fn first_round(evals: &[EvaluationMark]) -> Option<Decimal> {
    let firsts: Vec<Decimal> = evals
        .iter()
        .filter(|e| e.round == EvaluationRound::First)
        .map(|e| e.marks)
        .collect();
    if firsts.is_empty() {
        return None;
    }
    let sum: Decimal = firsts.iter().copied().sum();
    Some(round2(sum / Decimal::from(firsts.len() as i64)))
}

/// Resolves a script's evaluations (L3-I-080…082): an adjudicating evaluation is final; a second
/// evaluation within `threshold` of the first gives their mean, beyond it adjudication is required;
/// without a second evaluation the first stands unless policy requires one.
pub fn resolve(evals: &[EvaluationMark], second_required: bool, threshold: Decimal) -> Resolution {
    if let Some(adj) = evals
        .iter()
        .find(|e| e.round == EvaluationRound::Adjudication)
    {
        return Resolution::Resolved(adj.marks);
    }
    let Some(first) = first_round(evals) else {
        return Resolution::None;
    };
    match evals.iter().find(|e| e.round == EvaluationRound::Second) {
        Some(second) => {
            if (first - second.marks).abs() > threshold {
                Resolution::AwaitingAdjudication {
                    first,
                    second: second.marks,
                }
            } else {
                Resolution::Resolved(round2((first + second.marks) / Decimal::TWO))
            }
        }
        None if second_required => Resolution::AwaitingSecond { first },
        None => Resolution::Resolved(first),
    }
}

/// Kind of a moderation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModerationKind {
    /// `value` marks added to the grace marks of every verified script.
    Grace,
    /// External marks × `value` % (capped at the maximum).
    Scaling,
}

impl ModerationKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "grace" => Some(Self::Grace),
            "scaling" => Some(Self::Scaling),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Grace => "grace",
            Self::Scaling => "scaling",
        }
    }
}

/// Policy bounds of a moderation (`None` = valid): grace at most 10 % of the paper's external
/// maximum; scaling between 50 % and 120 %.
pub fn moderation_out_of_bounds(
    kind: ModerationKind,
    value: Decimal,
    external_max: Decimal,
) -> Option<&'static str> {
    if value <= Decimal::ZERO || value.normalize().scale() > 2 {
        return Some("must be positive, with at most two decimals");
    }
    match kind {
        ModerationKind::Grace if value > external_max / Decimal::TEN => {
            Some("grace may not exceed 10 % of the paper's maximum marks")
        }
        ModerationKind::Scaling if value < Decimal::new(50, 0) || value > Decimal::new(120, 0) => {
            Some("scaling must be between 50 and 120 percent")
        }
        _ => None,
    }
}

/// New `(external, grace)` of one script after a moderation.
pub fn moderate(
    kind: ModerationKind,
    value: Decimal,
    external: Option<Decimal>,
    grace: Decimal,
    external_max: Decimal,
) -> (Option<Decimal>, Decimal) {
    match kind {
        ModerationKind::Grace => (external, grace + value),
        ModerationKind::Scaling => (
            external.map(|e| round2(e * value / Decimal::ONE_HUNDRED).min(external_max)),
            grace,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap_or_default()
    }

    fn ev(round: EvaluationRound, who: &str, m: &str) -> EvaluationMark {
        EvaluationMark {
            round,
            evaluator: who.into(),
            marks: d(m),
        }
    }

    #[test]
    fn sheet_machine_follows_the_evaluation_lifecycle() {
        use EvaluationStatus::*;
        assert!(sheet_can_transition(Planned, InEvaluation));
        assert!(sheet_can_transition(InEvaluation, Evaluated));
        assert!(sheet_can_transition(Evaluated, Validated));
        assert!(sheet_can_transition(Validated, Approved));
        assert!(sheet_can_transition(Approved, Finalised));
        assert!(sheet_can_transition(Finalised, InEvaluation));
        assert!(!sheet_can_transition(InEvaluation, Approved));
        assert!(!sheet_can_transition(Planned, Finalised));
        assert!(!sheet_can_transition(Archived, Planned));
        assert!(sheet_accepts_entry(InEvaluation) && !sheet_accepts_entry(Validated));
    }

    #[test]
    fn moderation_machine() {
        use ModerationStatus::*;
        assert!(moderation_can_transition(Requested, Validated));
        assert!(moderation_can_transition(Validated, Approved));
        assert!(moderation_can_transition(Approved, Applied));
        assert!(!moderation_can_transition(Requested, Applied));
        assert!(!moderation_can_transition(Applied, Withdrawn));
    }

    #[test]
    fn evaluations_resolve_by_policy() {
        use EvaluationRound::*;
        assert_eq!(resolve(&[], false, d("10")), Resolution::None);
        let one = [ev(First, "a", "60")];
        assert_eq!(resolve(&one, false, d("10")), Resolution::Resolved(d("60")));
        assert_eq!(
            resolve(&one, true, d("10")),
            Resolution::AwaitingSecond { first: d("60.00") }
        );
        // Panel: mean of the members' first-round marks.
        let panel = [
            ev(First, "a", "60"),
            ev(First, "b", "65"),
            ev(First, "c", "62"),
        ];
        assert_eq!(
            resolve(&panel, false, d("10")),
            Resolution::Resolved(d("62.33"))
        );
        // Second within the threshold → mean.
        let close = [ev(First, "a", "60"), ev(Second, "b", "67")];
        assert_eq!(
            resolve(&close, true, d("10")),
            Resolution::Resolved(d("63.50"))
        );
        // Beyond the threshold → adjudication, whose marks are final.
        let far = [ev(First, "a", "40"), ev(Second, "b", "62")];
        assert_eq!(
            resolve(&far, true, d("10")),
            Resolution::AwaitingAdjudication {
                first: d("40.00"),
                second: d("62")
            }
        );
        let adj = [
            ev(First, "a", "40"),
            ev(Second, "b", "62"),
            ev(Adjudication, "c", "55"),
        ];
        assert_eq!(resolve(&adj, true, d("10")), Resolution::Resolved(d("55")));
    }

    #[test]
    fn moderation_arithmetic_and_bounds() {
        let max = d("100");
        assert_eq!(
            moderate(ModerationKind::Grace, d("3"), Some(d("37")), d("0"), max),
            (Some(d("37")), d("3"))
        );
        assert_eq!(
            moderate(
                ModerationKind::Scaling,
                d("110"),
                Some(d("95")),
                d("0"),
                max
            ),
            (Some(d("100")), d("0"))
        );
        assert_eq!(
            moderate(
                ModerationKind::Scaling,
                d("105"),
                Some(d("41")),
                d("1"),
                max
            ),
            (Some(d("43.05")), d("1"))
        );
        assert!(moderation_out_of_bounds(ModerationKind::Grace, d("5"), max).is_none());
        assert!(moderation_out_of_bounds(ModerationKind::Grace, d("11"), max).is_some());
        assert!(moderation_out_of_bounds(ModerationKind::Scaling, d("130"), max).is_some());
        assert!(moderation_out_of_bounds(ModerationKind::Scaling, d("0"), max).is_some());
        assert_eq!(ModerationKind::parse("grace"), Some(ModerationKind::Grace));
        assert_eq!(ModerationKind::Scaling.as_str(), "scaling");
    }
}
