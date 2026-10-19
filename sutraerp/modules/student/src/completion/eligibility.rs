//! **3C** qualification-eligibility read interface (contract doc §7 "3B → 3C").
//!
//! 3C consumes 3B's `QualificationEligibilityV1`-shaped facts (`studentId, eligible, outcomes[],
//! totalCredits, evaluatedAt`) read-only, for the completion's student **context**:
//!
//! 1. 3B's `student.qualification_eligibility` — the newest determination (`evaluated_at DESC`)
//!    of the context is the current one (`eligible`, `outcomes`, `total_credits`);
//! 2. else 3B's `student.credit_ledger_entry` of the context — Σ credits (`revoked` / `expired`
//!    entries subtract, `adjusted` is signed);
//! 3. else the evaluator's manual evidence (`creditsEarned`), since Exams results are not
//!    published yet (`ResultsPort` stub).
//!
//! The requirement is the curriculum's `min_total_credits` (programme completion) or the
//! policy-defined credit requirement supplied by the evaluator (MEME exit, amendment §2: no
//! fixed threshold is invented).

use super::domain::{CompletionPathway, EligibilityEvaluation, EligibilityResult};
use crate::shared::StudentState;
use chrono::Utc;
use sutra_contracts::student::status::QualificationOutcome;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Facts from 3B's progression area, when present.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProgressionFacts {
    pub eligibility_id: Option<Uuid>,
    pub eligible: Option<bool>,
    pub outcomes: Vec<QualificationOutcome>,
    pub total_credits: Option<f64>,
    pub source: &'static str,
}

/// Reads 3B's facts for the student (never fails the command: an unreadable 3B table falls back
/// to manual evidence and is logged).
pub async fn progression_facts(
    state: &StudentState,
    tenant: TenantId,
    student_context_id: Uuid,
) -> ProgressionFacts {
    match read_progression(state, tenant, student_context_id).await {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(error = %e, "3B progression facts unreadable; using manual evidence");
            ProgressionFacts {
                source: "manual",
                ..Default::default()
            }
        }
    }
}

async fn read_progression(
    state: &StudentState,
    tenant: TenantId,
    student_context_id: Uuid,
) -> Result<ProgressionFacts, KernelError> {
    let newest: Option<(Uuid, bool, Vec<String>, f64)> = sqlx::query_as(
        "SELECT id, eligible, outcomes, total_credits::float8 \
         FROM student.qualification_eligibility \
         WHERE tenant_id = $1 AND student_context_id = $2 \
         ORDER BY evaluated_at DESC, id DESC LIMIT 1",
    )
    .bind(tenant.as_uuid())
    .bind(student_context_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(map_db_error)?;
    if let Some((id, eligible, outcomes, total)) = newest {
        return Ok(ProgressionFacts {
            eligibility_id: Some(id),
            eligible: Some(eligible),
            outcomes: outcomes
                .iter()
                .filter_map(|o| QualificationOutcome::parse(o))
                .collect(),
            total_credits: Some(total),
            source: "progression",
        });
    }
    let ledger: (i64, Option<f64>) = sqlx::query_as(
        "SELECT count(*), sum(CASE WHEN kind IN ('revoked', 'expired') THEN -abs(credits) \
                                   ELSE credits END)::float8 \
         FROM student.credit_ledger_entry \
         WHERE tenant_id = $1 AND student_context_id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(student_context_id)
    .fetch_one(&state.pool)
    .await
    .map_err(map_db_error)?;
    if ledger.0 > 0 {
        return Ok(ProgressionFacts {
            total_credits: ledger.1,
            source: "credit_ledger",
            ..Default::default()
        });
    }
    Ok(ProgressionFacts {
        source: "manual",
        ..Default::default()
    })
}

/// Pure decision of the eligibility evaluation.
pub fn decide(
    pathway: CompletionPathway,
    outcome: QualificationOutcome,
    facts: &ProgressionFacts,
    evidence: &EligibilityEvaluation,
    curriculum_min_credits: Option<f64>,
    multiple_entry_exit: bool,
) -> EligibilityResult {
    let mut unmet = Vec::new();
    let credits_earned = facts.total_credits.or(evidence.credits_earned);
    let required_credits = match pathway {
        CompletionPathway::Completion => evidence.required_credits.or(curriculum_min_credits),
        CompletionPathway::MemeExit => evidence.required_credits,
    };
    if pathway == CompletionPathway::MemeExit {
        if !multiple_entry_exit {
            unmet.push("the curriculum does not permit multiple entry / exit".to_owned());
        }
        if required_credits.is_none() {
            unmet.push("the MEME policy credit requirement was not supplied".to_owned());
        }
    }
    if let Some(false) = facts.eligible {
        unmet.push("progression has not established qualification eligibility".to_owned());
    }
    if !facts.outcomes.is_empty() && !facts.outcomes.contains(&outcome) {
        unmet.push(format!(
            "progression eligibility does not allow the outcome {outcome}"
        ));
    }
    match (credits_earned, required_credits) {
        (None, _) if facts.eligible != Some(true) => {
            unmet.push("no credit evidence is available".to_owned());
        }
        (Some(e), Some(r)) if e < r => {
            unmet.push(format!("{e} credits earned; {r} required"));
        }
        _ => {}
    }
    EligibilityResult {
        eligible: unmet.is_empty(),
        source: if facts.source == "manual" && evidence.credits_earned.is_none() {
            "manual".into()
        } else {
            facts.source.into()
        },
        credits_earned,
        required_credits,
        outcomes: facts.outcomes.clone(),
        unmet,
        evaluated_at: Utc::now(),
    }
}

/// Curriculum facts from Domain B (`min_total_credits`, `multiple_entry_exit`).
pub async fn curriculum_facts(
    state: &StudentState,
    scope: &ExecutionScope,
    curriculum_version_id: Option<Uuid>,
) -> Result<(Option<f64>, bool), KernelError> {
    let Some(cv) = curriculum_version_id else {
        return Ok((None, false));
    };
    Ok(state
        .references
        .curriculum_version(scope, cv)
        .await?
        .map(|c| (c.min_total_credits, c.multiple_entry_exit))
        .unwrap_or((None, false)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manual() -> ProgressionFacts {
        ProgressionFacts {
            source: "manual",
            ..Default::default()
        }
    }

    #[test]
    fn completion_uses_curriculum_minimum() {
        let ev = EligibilityEvaluation {
            credits_earned: Some(150.0),
            required_credits: None,
        };
        let r = decide(
            CompletionPathway::Completion,
            QualificationOutcome::Degree,
            &manual(),
            &ev,
            Some(160.0),
            false,
        );
        assert!(!r.eligible);
        assert_eq!(r.required_credits, Some(160.0));
        let ev = EligibilityEvaluation {
            credits_earned: Some(160.0),
            ..ev
        };
        assert!(
            decide(
                CompletionPathway::Completion,
                QualificationOutcome::Degree,
                &manual(),
                &ev,
                Some(160.0),
                false
            )
            .eligible
        );
    }

    #[test]
    fn meme_needs_policy_requirement_and_mee_curriculum() {
        let ev = EligibilityEvaluation {
            credits_earned: Some(44.0),
            required_credits: None,
        };
        let r = decide(
            CompletionPathway::MemeExit,
            QualificationOutcome::Certificate,
            &manual(),
            &ev,
            Some(160.0),
            true,
        );
        assert!(!r.eligible && r.unmet.len() == 1);
        let ev = EligibilityEvaluation {
            required_credits: Some(40.0),
            ..ev
        };
        let r = decide(
            CompletionPathway::MemeExit,
            QualificationOutcome::Certificate,
            &manual(),
            &ev,
            Some(160.0),
            true,
        );
        assert!(r.eligible, "{:?}", r.unmet);
        let r = decide(
            CompletionPathway::MemeExit,
            QualificationOutcome::Certificate,
            &manual(),
            &ev,
            None,
            false,
        );
        assert!(!r.eligible);
    }

    #[test]
    fn progression_facts_take_precedence() {
        let facts = ProgressionFacts {
            eligibility_id: None,
            eligible: Some(true),
            outcomes: vec![QualificationOutcome::Diploma],
            total_credits: None,
            source: "progression",
        };
        let ev = EligibilityEvaluation::default();
        assert!(
            decide(
                CompletionPathway::Completion,
                QualificationOutcome::Diploma,
                &facts,
                &ev,
                None,
                false
            )
            .eligible
        );
        assert!(
            !decide(
                CompletionPathway::Completion,
                QualificationOutcome::Degree,
                &facts,
                &ev,
                None,
                false
            )
            .eligible
        );
    }
}
