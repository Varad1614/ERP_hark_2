//! **3C** scholarship domain: DTOs, the eligibility evaluator and the slice-owned machines
//! (L2-C07 §12, L3-C-062 … C-071).

use chrono::{DateTime, NaiveDate, Utc};
use sutra_contracts::student::ids::{
    ScholarshipDefinitionId, ScholarshipEligibilityId, ScholarshipPolicyId,
    ScholarshipRecalculationId, StudentContextId, StudentId,
};
use sutra_contracts::student::status::{
    PolicyVersionStatus, ScholarshipDefinitionStatus, ScholarshipEligibilityStatus,
};
use uuid::Uuid;

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SponsorType {
    Institutional,
    Government,
    Private,
    External,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum BenefitType {
    FixedAmount,
    FeePercentage,
    FeeWaiver,
    Stipend,
}

/// Scholarship definition (L3-C-062).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Scholarship {
    pub id: ScholarshipDefinitionId,
    pub institution_id: Uuid,
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub sponsor_type: SponsorType,
    pub external_scheme: Option<String>,
    pub benefit_type: BenefitType,
    pub benefit_value: Option<f64>,
    pub currency: String,
    pub valid_from: NaiveDate,
    pub valid_until: Option<NaiveDate>,
    pub renewable: bool,
    pub status: ScholarshipDefinitionStatus,
    pub status_reason: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    /// The effective eligibility policy (filled on reads).
    #[serde(default)]
    pub effective_policy: Option<ScholarshipPolicy>,
}

#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
pub struct ScholarshipList {
    pub items: Vec<Scholarship>,
}

#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewScholarship {
    /// `^[A-Z0-9][A-Z0-9_-]{1,31}$`, unique per institution.
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub sponsor_type: SponsorType,
    pub external_scheme: Option<String>,
    pub benefit_type: BenefitType,
    pub benefit_value: Option<f64>,
    pub valid_from: NaiveDate,
    pub valid_until: Option<NaiveDate>,
    #[serde(default)]
    pub renewable: bool,
}

/// Draft update (absent members are kept).
#[derive(Clone, Debug, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScholarshipUpdate {
    pub name: Option<String>,
    pub description: Option<String>,
    pub external_scheme: Option<String>,
    pub benefit_value: Option<f64>,
    pub valid_from: Option<NaiveDate>,
    pub valid_until: Option<NaiveDate>,
    pub renewable: Option<bool>,
}

/// Eligibility criteria of a policy version. Missing evidence is not-eligible unless
/// `conditionalEvidence` makes it a condition (policy-defined treatment, L4-C-064).
#[derive(
    Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "camelCase")]
pub struct PolicyCriteria {
    pub min_academic_score: Option<f64>,
    pub max_family_income: Option<f64>,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub programme_ids: Vec<Uuid>,
    pub min_term_no: Option<i32>,
    #[serde(default)]
    pub required_documents: Vec<String>,
    /// Missing evidence / documents yield a conditional eligibility instead of a rejection.
    #[serde(default)]
    pub conditional_evidence: bool,
    /// Days a condition may stay open (default 30).
    pub condition_days: Option<i32>,
    /// Validity of an awarded eligibility in days (default: until the scholarship ends, else 365).
    pub validity_days: Option<i32>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScholarshipPolicy {
    pub id: ScholarshipPolicyId,
    pub scholarship_id: ScholarshipDefinitionId,
    pub version_no: i32,
    pub status: PolicyVersionStatus,
    #[serde(default)]
    pub criteria: PolicyCriteria,
    pub effective_from: NaiveDate,
    pub process_instance_id: Option<String>,
    pub decision_reason: Option<String>,
    pub status_reason: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
pub struct ScholarshipPolicyList {
    pub items: Vec<ScholarshipPolicy>,
}

#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewScholarshipPolicy {
    pub criteria: PolicyCriteria,
    pub effective_from: Option<NaiveDate>,
}

#[derive(Clone, Debug, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScholarshipPolicyUpdate {
    pub criteria: Option<PolicyCriteria>,
    pub effective_from: Option<NaiveDate>,
}

/// Applicant facts supplied with a determination (Domain C holds no income / marks data).
#[derive(
    Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "camelCase")]
pub struct EligibilityFacts {
    pub academic_score: Option<f64>,
    pub family_income: Option<f64>,
    pub category: Option<String>,
    #[serde(default)]
    pub documents: Vec<String>,
}

impl EligibilityFacts {
    /// `other` overrides the members it sets; documents are unioned.
    pub fn merged(&self, other: Option<&EligibilityFacts>) -> EligibilityFacts {
        let Some(o) = other else { return self.clone() };
        let mut documents = self.documents.clone();
        for d in &o.documents {
            if !documents.contains(d) {
                documents.push(d.clone());
            }
        }
        EligibilityFacts {
            academic_score: o.academic_score.or(self.academic_score),
            family_income: o.family_income.or(self.family_income),
            category: o.category.clone().or_else(|| self.category.clone()),
            documents,
        }
    }
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationOutcome {
    Eligible,
    NotEligible,
    ConditionallyEligible,
}

impl EvaluationOutcome {
    pub fn status(self) -> ScholarshipEligibilityStatus {
        match self {
            Self::Eligible => ScholarshipEligibilityStatus::Eligible,
            Self::NotEligible => ScholarshipEligibilityStatus::NotEligible,
            Self::ConditionallyEligible => ScholarshipEligibilityStatus::ConditionallyEligible,
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CriterionResult {
    pub criterion: String,
    /// `None` = evidence missing.
    pub met: Option<bool>,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Evaluation {
    pub outcome: EvaluationOutcome,
    pub criteria: Vec<CriterionResult>,
    #[serde(default)]
    pub conditions: Vec<String>,
    pub policy_id: ScholarshipPolicyId,
    pub policy_version_no: i32,
    pub evaluated_at: DateTime<Utc>,
}

/// One appended assessment (history is never rewritten).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EligibilityAssessment {
    pub id: Uuid,
    pub kind: String,
    pub policy_id: Option<Uuid>,
    pub from_status: Option<ScholarshipEligibilityStatus>,
    pub to_status: ScholarshipEligibilityStatus,
    #[serde(default)]
    pub facts: EligibilityFacts,
    pub evaluation: Option<Evaluation>,
    pub reason: Option<String>,
    pub recalculation_id: Option<Uuid>,
    pub actor: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

/// Student scholarship eligibility (§5.3: id, scholarshipId, studentId, status, validUntil, version).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScholarshipEligibility {
    pub id: ScholarshipEligibilityId,
    pub institution_id: Uuid,
    pub scholarship_id: ScholarshipDefinitionId,
    pub policy_id: Option<ScholarshipPolicyId>,
    pub student_id: StudentId,
    pub student_context_id: StudentContextId,
    pub academic_year_id: Option<Uuid>,
    pub status: ScholarshipEligibilityStatus,
    #[serde(default)]
    pub facts: EligibilityFacts,
    pub evaluation: Option<Evaluation>,
    #[serde(default)]
    pub conditions: Vec<String>,
    pub condition_deadline: Option<NaiveDate>,
    pub recommendation_pending: bool,
    pub process_instance_id: Option<String>,
    pub valid_from: Option<NaiveDate>,
    pub valid_until: Option<NaiveDate>,
    pub renewal_count: i32,
    pub handed_off_at: Option<DateTime<Utc>>,
    pub status_reason: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Assessment history, oldest first (filled on single reads).
    #[serde(default)]
    pub assessments: Vec<EligibilityAssessment>,
}

#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
pub struct ScholarshipEligibilityList {
    pub items: Vec<ScholarshipEligibility>,
}

#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewScholarshipEligibility {
    pub scholarship_id: ScholarshipDefinitionId,
    pub academic_year_id: Option<Uuid>,
    #[serde(default)]
    pub facts: EligibilityFacts,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConditionOutcome {
    /// Evidence supplied: re-evaluate (may stay conditional).
    Fulfilled,
    Failed,
    /// The condition deadline passed without evidence.
    Expired,
}

#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResolveCondition {
    pub outcome: ConditionOutcome,
    pub facts: Option<EligibilityFacts>,
    pub note: Option<String>,
}

/// Reassessment (L3-C-066) / renewal (L3-C-067) evidence.
#[derive(Clone, Debug, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Reevaluation {
    pub facts: Option<EligibilityFacts>,
    pub reason: Option<String>,
    /// Renewal: the academic year renewed for.
    pub academic_year_id: Option<Uuid>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScholarshipExternalSync {
    pub id: Uuid,
    pub scholarship_id: ScholarshipDefinitionId,
    pub external_scheme: String,
    pub student_count: i32,
    /// `accepted` | `rejected` | `not_configured` (the external status stays externally owned).
    pub outcome: String,
    pub reference: Option<String>,
    pub detail: Option<String>,
    pub as_of: NaiveDate,
    pub actor: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
pub struct ScholarshipExternalSyncList {
    pub items: Vec<ScholarshipExternalSync>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RecalculationChange {
    pub eligibility_id: ScholarshipEligibilityId,
    pub student_id: StudentId,
    pub from_status: ScholarshipEligibilityStatus,
    pub to_status: ScholarshipEligibilityStatus,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScholarshipRecalculation {
    pub id: ScholarshipRecalculationId,
    pub institution_id: Uuid,
    pub scholarship_id: Option<ScholarshipDefinitionId>,
    pub reason: Option<String>,
    pub evaluated: i32,
    pub changed: i32,
    #[serde(default)]
    pub changes: Vec<RecalculationChange>,
    pub status: String,
    pub actor: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
pub struct ScholarshipRecalculationList {
    pub items: Vec<ScholarshipRecalculation>,
}

#[derive(Clone, Debug, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewScholarshipRecalculation {
    /// Limit to one scholarship (default: every scholarship of the institution).
    pub scholarship_id: Option<ScholarshipDefinitionId>,
    pub reason: Option<String>,
}

// ---- machines + evaluator (pure) ---------------------------------------------------------------

/// Scholarship definition transitions (L2-C07 §12).
pub fn definition_can_transition(
    from: ScholarshipDefinitionStatus,
    to: ScholarshipDefinitionStatus,
) -> bool {
    use ScholarshipDefinitionStatus::*;
    matches!(
        (from, to),
        (Draft, Published)
            | (Published, Active)
            | (Active, Suspended)
            | (Suspended, Active)
            | (Active, Expired)
            | (Published | Suspended | Expired, Archived)
    )
}

/// Policy version transitions (`active` of L2-C07 = `effective`).
pub fn policy_can_transition(from: PolicyVersionStatus, to: PolicyVersionStatus) -> bool {
    use PolicyVersionStatus::*;
    matches!(
        (from, to),
        (Draft, InReview)
            | (InReview, Effective)
            | (InReview, Draft)
            | (Effective, Superseded | Retired)
    )
}

/// Student eligibility transitions (L2-C07 §12 + expiry of conditional / pending outcomes).
pub fn eligibility_can_transition(
    from: ScholarshipEligibilityStatus,
    to: ScholarshipEligibilityStatus,
) -> bool {
    use ScholarshipEligibilityStatus::*;
    matches!(
        (from, to),
        (Created, AssessmentPending)
            | (AssessmentPending, UnderEvaluation)
            | (
                UnderEvaluation,
                Eligible | NotEligible | ConditionallyEligible
            )
            | (ConditionallyEligible, UnderEvaluation)
            | (Eligible, RecommendationGenerated)
            | (RecommendationGenerated, Eligible)
            | (RecommendationGenerated, Active)
            | (Active, Reassessment | Renewal)
            | (Reassessment, Active | NotEligible | ConditionallyEligible)
            | (Renewal, Active | Expired | ConditionallyEligible)
            | (
                Active | Eligible | ConditionallyEligible | RecommendationGenerated,
                Expired
            )
            | (
                Active | Eligible | ConditionallyEligible | RecommendationGenerated,
                NotEligible
            )
            | (Eligible | RecommendationGenerated, ConditionallyEligible)
            | (ConditionallyEligible, Eligible)
            | (Expired | NotEligible, Archived)
    )
}

/// Evaluates `facts` (+ the student's programme / term) against a policy's criteria.
pub fn evaluate(
    criteria: &PolicyCriteria,
    facts: &EligibilityFacts,
    programme_id: Option<Uuid>,
    term_no: Option<i32>,
) -> (EvaluationOutcome, Vec<CriterionResult>, Vec<String>) {
    let mut results = Vec::new();
    let mut conditions = Vec::new();
    let mut push = |criterion: &str, met: Option<bool>, detail: String| {
        results.push(CriterionResult {
            criterion: criterion.into(),
            met,
            detail,
        });
    };
    if let Some(min) = criteria.min_academic_score {
        match facts.academic_score {
            Some(s) => push(
                "minAcademicScore",
                Some(s >= min),
                format!("{s} (minimum {min})"),
            ),
            None => push(
                "minAcademicScore",
                None,
                "academic score not supplied".into(),
            ),
        }
    }
    if let Some(max) = criteria.max_family_income {
        match facts.family_income {
            Some(i) => push(
                "maxFamilyIncome",
                Some(i <= max),
                format!("{i} (maximum {max})"),
            ),
            None => push("maxFamilyIncome", None, "family income not supplied".into()),
        }
    }
    if !criteria.categories.is_empty() {
        match &facts.category {
            Some(c) => push(
                "categories",
                Some(
                    criteria
                        .categories
                        .iter()
                        .any(|k| k.eq_ignore_ascii_case(c)),
                ),
                format!("category {c}"),
            ),
            None => push("categories", None, "category not supplied".into()),
        }
    }
    if !criteria.programme_ids.is_empty() {
        let met = programme_id.is_some_and(|p| criteria.programme_ids.contains(&p));
        push(
            "programmeIds",
            Some(met),
            "programme of the student context".into(),
        );
    }
    if let Some(min) = criteria.min_term_no {
        let met = term_no.is_some_and(|t| t >= min);
        push(
            "minTermNo",
            Some(met),
            format!("term {} (minimum {min})", term_no.unwrap_or(0)),
        );
    }
    for doc in &criteria.required_documents {
        let has = facts.documents.iter().any(|d| d.eq_ignore_ascii_case(doc));
        push(
            "requiredDocuments",
            if has { Some(true) } else { None },
            format!("document {doc}"),
        );
    }
    let failed = results.iter().any(|r| r.met == Some(false));
    let missing: Vec<&CriterionResult> = results.iter().filter(|r| r.met.is_none()).collect();
    for m in &missing {
        conditions.push(format!("provide evidence: {}", m.detail));
    }
    let outcome = if failed || (!missing.is_empty() && !criteria.conditional_evidence) {
        EvaluationOutcome::NotEligible
    } else if !missing.is_empty() {
        EvaluationOutcome::ConditionallyEligible
    } else {
        EvaluationOutcome::Eligible
    };
    if outcome != EvaluationOutcome::ConditionallyEligible {
        conditions.clear();
    }
    (outcome, results, conditions)
}

/// Normalised scholarship code.
pub fn valid_code(code: &str) -> bool {
    let b = code.as_bytes();
    (2..=32).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn criteria() -> PolicyCriteria {
        PolicyCriteria {
            min_academic_score: Some(75.0),
            max_family_income: Some(500_000.0),
            required_documents: vec!["income_certificate".into()],
            ..Default::default()
        }
    }

    #[test]
    fn evaluator_outcomes() {
        let facts = EligibilityFacts {
            academic_score: Some(80.0),
            family_income: Some(300_000.0),
            category: None,
            documents: vec!["income_certificate".into()],
        };
        assert_eq!(
            evaluate(&criteria(), &facts, None, None).0,
            EvaluationOutcome::Eligible
        );
        let low = EligibilityFacts {
            academic_score: Some(60.0),
            ..facts.clone()
        };
        assert_eq!(
            evaluate(&criteria(), &low, None, None).0,
            EvaluationOutcome::NotEligible
        );
        let missing = EligibilityFacts {
            documents: vec![],
            ..facts.clone()
        };
        assert_eq!(
            evaluate(&criteria(), &missing, None, None).0,
            EvaluationOutcome::NotEligible
        );
        let conditional = PolicyCriteria {
            conditional_evidence: true,
            ..criteria()
        };
        let (o, _, c) = evaluate(&conditional, &missing, None, None);
        assert_eq!(o, EvaluationOutcome::ConditionallyEligible);
        assert_eq!(c.len(), 1);
        let merged = missing.merged(Some(&EligibilityFacts {
            documents: vec!["income_certificate".into()],
            ..Default::default()
        }));
        assert_eq!(
            evaluate(&conditional, &merged, None, None).0,
            EvaluationOutcome::Eligible
        );
    }

    #[test]
    fn machines() {
        use ScholarshipEligibilityStatus::*;
        assert!(eligibility_can_transition(
            Eligible,
            RecommendationGenerated
        ));
        assert!(!eligibility_can_transition(Eligible, Active));
        assert!(eligibility_can_transition(Active, Renewal));
        assert!(!eligibility_can_transition(Archived, Active));
        assert!(definition_can_transition(
            ScholarshipDefinitionStatus::Suspended,
            ScholarshipDefinitionStatus::Active
        ));
        assert!(!definition_can_transition(
            ScholarshipDefinitionStatus::Draft,
            ScholarshipDefinitionStatus::Active
        ));
        assert!(policy_can_transition(
            PolicyVersionStatus::Effective,
            PolicyVersionStatus::Superseded
        ));
        assert!(valid_code("MERIT-2026") && !valid_code("m") && !valid_code("bad code"));
    }
}
