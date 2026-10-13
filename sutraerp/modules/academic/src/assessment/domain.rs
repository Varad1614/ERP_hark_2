//! Pure internal-assessment rules (L2-D07 §5, L3-D-051…057). No I/O; unit-tested here.

use sutra_contracts::academic::status::{
    AssessmentBlueprintStatus, AssessmentWindowStatus, InternalAssessmentStatus,
};

/// One weighted blueprint component.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlueprintComponent {
    /// Stable key windows refer to (e.g. `assign1`).
    pub key: String,
    pub title: String,
    pub kind: sutra_contracts::academic::status::AssessmentKind,
    /// Weight in percent of the internal total (all components add up to 100).
    pub weight: f64,
}

/// Blueprint policy switches.
#[derive(
    Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "camelCase")]
pub struct BlueprintPolicy {
    /// Every marked submission needs an approved moderation before finalisation.
    #[serde(default)]
    pub moderation_required: bool,
    /// Submissions after the original due date are accepted (as `late`) while the window is extended.
    #[serde(default = "yes")]
    pub allow_late_submission: bool,
}

fn yes() -> bool {
    true
}

/// Validation errors of a blueprint before publication (L4-D-051-BH-004).
pub fn validate_blueprint(components: &[BlueprintComponent]) -> Vec<String> {
    let mut out = Vec::new();
    if components.is_empty() {
        out.push("the blueprint has no components".to_owned());
    }
    let mut keys: Vec<&str> = Vec::new();
    for c in components {
        if c.key.trim().is_empty()
            || !c
                .key
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        {
            out.push(format!("component key `{}` is invalid", c.key));
        }
        if keys.contains(&c.key.as_str()) {
            out.push(format!("component key `{}` is repeated", c.key));
        }
        keys.push(&c.key);
        if c.title.trim().is_empty() {
            out.push(format!("component `{}` needs a title", c.key));
        }
        if !(c.weight > 0.0 && c.weight <= 100.0) {
            out.push(format!(
                "component `{}` weight must be above 0 and at most 100",
                c.key
            ));
        }
    }
    let total: f64 = components.iter().map(|c| c.weight).sum();
    if !components.is_empty() && (total - 100.0).abs() > 0.001 {
        out.push(format!("component weights add up to {total}, not 100"));
    }
    out
}

/// Legal blueprint transitions.
pub fn blueprint_can_transition(
    from: AssessmentBlueprintStatus,
    to: AssessmentBlueprintStatus,
) -> bool {
    use AssessmentBlueprintStatus::*;
    matches!(
        (from, to),
        (Draft, Published)
            | (Published, Superseded)
            | (Published, Archived)
            | (Superseded, Archived)
            | (Draft, Archived)
    )
}

/// Legal assessment-window transitions.
pub fn window_can_transition(from: AssessmentWindowStatus, to: AssessmentWindowStatus) -> bool {
    use AssessmentWindowStatus::*;
    matches!(
        (from, to),
        (Scheduled, Open)
            | (Scheduled, Cancelled)
            | (Open, Extended)
            | (Extended, Extended)
            | (Open, Closed)
            | (Extended, Closed)
            | (Open, Cancelled)
    )
}

/// Students may submit while the window is open or extended.
pub fn accepts_submissions(s: AssessmentWindowStatus) -> bool {
    matches!(
        s,
        AssessmentWindowStatus::Open | AssessmentWindowStatus::Extended
    )
}

/// Legal internal-assessment result transitions (a correction marks the original `corrected` and
/// inserts the next version as `finalized`).
pub fn result_can_transition(from: InternalAssessmentStatus, to: InternalAssessmentStatus) -> bool {
    use InternalAssessmentStatus::*;
    matches!(
        (from, to),
        (InProgress, Finalized)
            | (Moderated, Finalized)
            | (InProgress, Moderated)
            | (Finalized, Corrected)
            | (Finalized, Archived)
    )
}

/// One scored window of a component: (obtained, max); `None` obtained = absent/unmarked → 0.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Score {
    pub obtained: f64,
    pub max: f64,
}

/// A computed component line of a result.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ComponentMark {
    pub key: String,
    pub title: String,
    pub weight: f64,
    /// Raw marks obtained over all windows of the component.
    pub obtained: f64,
    pub max: f64,
    /// Weighted contribution to the internal total.
    pub contribution: f64,
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Weighted internal total: Σ (obtained / max × weight) × total_marks / 100.
pub fn weighted_total(
    components: &[BlueprintComponent],
    scores: &dyn Fn(&str) -> Vec<Score>,
    total_marks: f64,
) -> (Vec<ComponentMark>, f64) {
    let mut lines = Vec::new();
    let mut sum = 0.0;
    for c in components {
        let s = scores(&c.key);
        let obtained: f64 = s.iter().map(|x| x.obtained).sum();
        let max: f64 = s.iter().map(|x| x.max).sum();
        let contribution = if max > 0.0 {
            obtained / max * c.weight * total_marks / 100.0
        } else {
            0.0
        };
        sum += contribution;
        lines.push(ComponentMark {
            key: c.key.clone(),
            title: c.title.clone(),
            weight: c.weight,
            obtained: round2(obtained),
            max: round2(max),
            contribution: round2(contribution),
        });
    }
    (lines, round2(sum))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutra_contracts::academic::status::AssessmentKind;

    fn comp(key: &str, w: f64) -> BlueprintComponent {
        BlueprintComponent {
            key: key.into(),
            title: key.to_uppercase(),
            kind: AssessmentKind::Assignment,
            weight: w,
        }
    }

    #[test]
    fn blueprint_validation() {
        assert!(validate_blueprint(&[comp("a1", 40.0), comp("t1", 60.0)]).is_empty());
        assert!(!validate_blueprint(&[]).is_empty());
        assert!(
            !validate_blueprint(&[comp("a1", 40.0), comp("a1", 60.0)]).is_empty(),
            "repeated key"
        );
        assert!(
            !validate_blueprint(&[comp("a1", 40.0), comp("t1", 50.0)]).is_empty(),
            "weights add to 90"
        );
        assert!(!validate_blueprint(&[comp("bad key", 100.0)]).is_empty());
    }

    #[test]
    fn machines() {
        use AssessmentBlueprintStatus as B;
        use AssessmentWindowStatus as W;
        use InternalAssessmentStatus as R;
        assert!(blueprint_can_transition(B::Draft, B::Published));
        assert!(!blueprint_can_transition(B::Published, B::Draft));
        assert!(window_can_transition(W::Scheduled, W::Open));
        assert!(window_can_transition(W::Open, W::Extended));
        assert!(
            !window_can_transition(W::Closed, W::Open),
            "no new submissions after closure"
        );
        assert!(accepts_submissions(W::Extended) && !accepts_submissions(W::Closed));
        assert!(result_can_transition(R::InProgress, R::Finalized));
        assert!(result_can_transition(R::Finalized, R::Corrected));
        assert!(
            !result_can_transition(R::Archived, R::Finalized),
            "archived is final"
        );
        assert!(!result_can_transition(R::Corrected, R::Finalized));
    }

    #[test]
    fn weighted_totals() {
        let comps = [comp("a1", 40.0), comp("t1", 60.0)];
        let scores = |k: &str| match k {
            "a1" => vec![
                Score {
                    obtained: 8.0,
                    max: 10.0,
                },
                Score {
                    obtained: 6.0,
                    max: 10.0,
                },
            ],
            _ => vec![Score {
                obtained: 45.0,
                max: 50.0,
            }],
        };
        let (lines, total) = weighted_total(&comps, &scores, 50.0);
        // a1: 14/20 × 40 × 0.5 = 14 ; t1: 45/50 × 60 × 0.5 = 27
        assert_eq!(lines[0].contribution, 14.0);
        assert_eq!(lines[1].contribution, 27.0);
        assert_eq!(total, 41.0);
        let (_, none) = weighted_total(&comps, &|_| vec![], 50.0);
        assert_eq!(none, 0.0);
    }
}
