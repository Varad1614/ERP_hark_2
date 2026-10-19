//! Progression aggregates (L2-C04): policy, decision (+ conditions, probation, detention,
//! revisions), credit ledger, credit transfer, programme migration, academic break,
//! qualification eligibility and ABC sync. DTOs, stored rows and the pure rules.

use chrono::{DateTime, NaiveDate, Utc};
use sutra_contracts::student::status::{
    AbcSyncStatus, AcademicBreakStatus, AcademicStanding, CaseStatus, CreditEntryKind,
    PolicyVersionStatus, ProbationStatus, ProgressionConditionStatus, ProgressionDecisionStatus,
    ProgressionOutcome, QualificationOutcome,
};
use uuid::Uuid;

// ================================================================== pure rules

/// Progression policy transitions (L3-C-075): draft → in_review → effective → superseded |
/// retired; a rejected review returns to draft; a draft may be discarded (retired).
pub fn policy_can_transition(from: PolicyVersionStatus, to: PolicyVersionStatus) -> bool {
    use PolicyVersionStatus::*;
    matches!(
        (from, to),
        (Draft, InReview)
            | (Draft, Retired)
            | (InReview, Effective)
            | (InReview, Draft)
            | (Effective, Superseded)
            | (Effective, Retired)
    )
}

/// Progression decision transitions: draft → pending_approval → effective | rejected;
/// effective → superseded (by a revision, L3-C-038).
pub fn decision_can_transition(
    from: ProgressionDecisionStatus,
    to: ProgressionDecisionStatus,
) -> bool {
    use ProgressionDecisionStatus::*;
    matches!(
        (from, to),
        (Draft, PendingApproval)
            | (Draft, Rejected)
            | (PendingApproval, Effective)
            | (PendingApproval, Rejected)
            | (Effective, Superseded)
    )
}

/// Governed case transitions (credit transfer, programme migration): requested → under_review
/// → approved → completed; under_review → rejected | requested (revision required);
/// requested → withdrawn.
pub fn case_can_transition(from: CaseStatus, to: CaseStatus) -> bool {
    use CaseStatus::*;
    matches!(
        (from, to),
        (Requested, UnderReview)
            | (Requested, Withdrawn)
            | (UnderReview, Approved)
            | (UnderReview, Rejected)
            | (UnderReview, Requested)
            | (UnderReview, Withdrawn)
            | (Approved, Completed)
    )
}

/// Academic break transitions (L3-C-034/035).
pub fn break_can_transition(from: AcademicBreakStatus, to: AcademicBreakStatus) -> bool {
    use AcademicBreakStatus::*;
    matches!(
        (from, to),
        (Requested, Approved)
            | (Requested, Rejected)
            | (Requested, Withdrawn)
            | (Approved, InEffect)
            | (Approved, Withdrawn)
            | (InEffect, Rejoined)
            | (InEffect, Lapsed)
    )
}

/// Condition transitions: pending → fulfilled | failed | waived.
pub fn condition_can_transition(
    from: ProgressionConditionStatus,
    to: ProgressionConditionStatus,
) -> bool {
    from == ProgressionConditionStatus::Pending && to != ProgressionConditionStatus::Pending
}

/// Probation transitions: active → cleared | escalated.
pub fn probation_can_transition(from: ProbationStatus, to: ProbationStatus) -> bool {
    from == ProbationStatus::Active && to != ProbationStatus::Active
}

/// Thresholds of a progression policy (stored as JSON in `progression_policy.rules`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicyRules {
    /// Earned / attempted credit ratio at or above which the student progresses (0…1).
    pub progress_min_ratio: f64,
    /// Ratio at or above which the student progresses with conditions.
    pub conditional_min_ratio: f64,
    /// Ratio at or above which the student is placed on probation; below → detained.
    pub probation_min_ratio: f64,
    /// Failed components tolerated for a conditional promotion (more → probation).
    #[serde(default = "default_max_failed")]
    pub max_failed_for_conditional: i32,
    /// A second consecutive probation escalates to detention.
    #[serde(default = "yes")]
    pub escalate_repeat_probation: bool,
    /// Repeat consequence of a detention (`term` or `year`).
    #[serde(default = "default_repeat")]
    pub detention_repeat_unit: String,
    /// Whether fresh evaluation after re-evaluation / supplementary / appeal is permitted.
    #[serde(default = "yes")]
    pub allow_revision: bool,
}

fn default_max_failed() -> i32 {
    2
}
fn yes() -> bool {
    true
}
fn default_repeat() -> String {
    "term".into()
}

impl PolicyRules {
    /// Validation messages (empty = valid).
    pub fn problems(&self) -> Vec<(&'static str, &'static str)> {
        let mut v = vec![];
        let ok = |r: f64| (0.0..=1.0).contains(&r);
        if !ok(self.progress_min_ratio) {
            v.push(("rules.progressMinRatio", "must be between 0 and 1"));
        }
        if !ok(self.conditional_min_ratio) {
            v.push(("rules.conditionalMinRatio", "must be between 0 and 1"));
        }
        if !ok(self.probation_min_ratio) {
            v.push(("rules.probationMinRatio", "must be between 0 and 1"));
        }
        if !(self.probation_min_ratio <= self.conditional_min_ratio
            && self.conditional_min_ratio <= self.progress_min_ratio)
        {
            v.push((
                "rules",
                "ratios must satisfy probation ≤ conditional ≤ progress",
            ));
        }
        if self.max_failed_for_conditional < 0 {
            v.push(("rules.maxFailedForConditional", "must not be negative"));
        }
        if !matches!(self.detention_repeat_unit.as_str(), "term" | "year") {
            v.push(("rules.detentionRepeatUnit", "must be `term` or `year`"));
        }
        v
    }
}

/// Outcome the policy recommends for the evidence (L3-C-027 / C-031). `on_probation` = the
/// student's standing is already probation.
pub fn recommend(
    rules: &PolicyRules,
    attempted: Option<f64>,
    earned: f64,
    failed: usize,
    on_probation: bool,
    final_term: bool,
) -> ProgressionOutcome {
    let ratio = match attempted {
        Some(a) if a > 0.0 => earned / a,
        _ if failed == 0 => 1.0,
        _ => 0.0,
    };
    let outcome = if ratio + 1e-9 >= rules.progress_min_ratio && failed == 0 {
        ProgressionOutcome::Progressed
    } else if ratio + 1e-9 >= rules.conditional_min_ratio
        && failed as i64 <= i64::from(rules.max_failed_for_conditional)
    {
        ProgressionOutcome::ConditionallyProgressed
    } else if ratio + 1e-9 >= rules.probation_min_ratio {
        ProgressionOutcome::Probation
    } else {
        ProgressionOutcome::Detained
    };
    match outcome {
        ProgressionOutcome::Probation if on_probation && rules.escalate_repeat_probation => {
            ProgressionOutcome::Detained
        }
        ProgressionOutcome::Progressed if final_term => ProgressionOutcome::QualificationEligible,
        o => o,
    }
}

/// Programme term after an effective decision for `term_no`.
pub fn next_term_no(
    term_no: i32,
    outcome: ProgressionOutcome,
    repeat_unit: Option<&str>,
    term_count: i32,
) -> i32 {
    match outcome {
        ProgressionOutcome::Progressed
        | ProgressionOutcome::ConditionallyProgressed
        | ProgressionOutcome::Probation => (term_no + 1).min(term_count.max(1)),
        ProgressionOutcome::QualificationEligible => term_no,
        ProgressionOutcome::Detained | ProgressionOutcome::Repeat => match repeat_unit {
            // Repeat the academic year: back to its first (odd) term.
            Some("year") if term_no % 2 == 0 => term_no - 1,
            _ => term_no,
        },
    }
}

/// Standing an effective outcome establishes.
pub fn standing_for(outcome: ProgressionOutcome) -> AcademicStanding {
    match outcome {
        ProgressionOutcome::Progressed | ProgressionOutcome::QualificationEligible => {
            AcademicStanding::GoodStanding
        }
        ProgressionOutcome::ConditionallyProgressed => AcademicStanding::Conditional,
        ProgressionOutcome::Probation => AcademicStanding::Probation,
        ProgressionOutcome::Detained | ProgressionOutcome::Repeat => AcademicStanding::Detained,
    }
}

/// Signed ledger contribution of an entry.
pub fn signed(kind: CreditEntryKind, credits: f64) -> f64 {
    match kind {
        CreditEntryKind::Revoked | CreditEntryKind::Expired => -credits,
        _ => credits,
    }
}

/// Qualification outcomes reachable with `credits` of `required` (L3-C-036, NEP MEME aware).
/// Without MEME the only outcome is the full award (`degree`). With MEME the intermediate exits
/// follow the NEP 2020 credit milestones as fractions of the programme minimum: ¼ certificate,
/// ½ diploma, ¾ advanced diploma (UG: 40/80/120 of 160), full → degree.
pub fn reachable_outcomes(
    credits: f64,
    required: f64,
    meme: bool,
    mandatory_met: bool,
) -> Vec<QualificationOutcome> {
    let mut v = vec![];
    if required <= 0.0 {
        return v;
    }
    if meme {
        if credits + 1e-9 >= required * 0.25 {
            v.push(QualificationOutcome::Certificate);
        }
        if credits + 1e-9 >= required * 0.5 {
            v.push(QualificationOutcome::Diploma);
        }
        if credits + 1e-9 >= required * 0.75 {
            v.push(QualificationOutcome::AdvancedDiploma);
        }
    }
    if credits + 1e-9 >= required && mandatory_met {
        v.push(QualificationOutcome::Degree);
    }
    v
}

/// Mandatory curriculum categories for eligibility (core components).
pub fn is_mandatory_category(category: &str) -> bool {
    category == "core" || category.ends_with("_core")
}

// ================================================================== policies

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProgressionPolicy {
    pub id: Uuid,
    pub institution_id: Uuid,
    /// `null` = institution-wide.
    pub programme_id: Option<Uuid>,
    pub code: String,
    pub version_no: i32,
    pub name: String,
    pub effective_from: NaiveDate,
    pub rules: PolicyRules,
    pub status: PolicyVersionStatus,
    pub status_reason: Option<String>,
    pub supersedes_policy_id: Option<Uuid>,
    pub process_instance_id: Option<String>,
    pub effective_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateProgressionPolicy {
    pub code: String,
    pub name: String,
    pub programme_id: Option<Uuid>,
    pub effective_from: NaiveDate,
    pub rules: PolicyRules,
}

#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProgressionPolicy {
    pub name: Option<String>,
    pub effective_from: Option<NaiveDate>,
    pub rules: Option<PolicyRules>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProgressionPolicyList {
    pub items: Vec<ProgressionPolicy>,
}

// ================================================================== decisions

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProgressionCondition {
    pub id: Uuid,
    pub decision_id: Uuid,
    pub student_id: Uuid,
    pub description: String,
    pub curriculum_component_id: Option<Uuid>,
    pub due_on: Option<NaiveDate>,
    pub status: ProgressionConditionStatus,
    pub resolution_note: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub version: i64,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConditionInput {
    pub description: String,
    pub curriculum_component_id: Option<Uuid>,
    pub due_on: Option<NaiveDate>,
}

/// A progression decision of a student for one term (L3-C-027).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProgressionDecision {
    pub id: Uuid,
    pub student_id: Uuid,
    pub student_context_id: Uuid,
    pub institution_id: Uuid,
    pub academic_term_id: Uuid,
    pub term_no: i32,
    pub policy_id: Option<Uuid>,
    pub outcome: ProgressionOutcome,
    /// What the effective policy recommends for the evidence.
    pub recommended_outcome: Option<ProgressionOutcome>,
    /// `results` (Exams ResultsPort) or `manual`.
    pub evidence_source: String,
    pub credits_attempted: Option<f64>,
    pub credits_earned: f64,
    pub failed_component_ids: Vec<Uuid>,
    pub sgpa: Option<f64>,
    pub cgpa: Option<f64>,
    pub evidence_note: Option<String>,
    pub repeat_unit: Option<String>,
    pub probation_review_due_on: Option<NaiveDate>,
    pub status: ProgressionDecisionStatus,
    pub status_reason: Option<String>,
    pub supersedes_decision_id: Option<Uuid>,
    pub resulting_term_no: Option<i32>,
    pub process_instance_id: Option<String>,
    pub decided_by: Option<String>,
    pub effective_at: Option<DateTime<Utc>>,
    pub conditions: Vec<ProgressionCondition>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Draft a progression decision. Evidence comes from the Exams `ResultsPort` when a result is
/// published; until Exams exists it is entered here (`creditsEarned` required).
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateDecision {
    pub academic_term_id: Uuid,
    /// Defaults to the policy recommendation.
    pub outcome: Option<ProgressionOutcome>,
    pub credits_attempted: Option<f64>,
    pub credits_earned: Option<f64>,
    #[serde(default)]
    pub failed_component_ids: Vec<Uuid>,
    pub sgpa: Option<f64>,
    pub cgpa: Option<f64>,
    pub evidence_note: Option<String>,
    /// `term` or `year` (detention / repeat); `null` leaves the repeat undetermined.
    pub repeat_unit: Option<String>,
    pub probation_review_due_on: Option<NaiveDate>,
    /// Conditions of a conditional promotion (required for `conditionally_progressed`).
    #[serde(default)]
    pub conditions: Vec<ConditionInput>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProgressionDecisionList {
    pub items: Vec<ProgressionDecision>,
}

/// Trigger of a fresh evaluation (L3-C-038).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RevisionTrigger {
    Revaluation,
    Supplementary,
    Appeal,
    Correction,
}

impl RevisionTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Revaluation => "revaluation",
            Self::Supplementary => "supplementary",
            Self::Appeal => "appeal",
            Self::Correction => "correction",
        }
    }
}

/// Fresh evaluation of an effective decision with changed authoritative evidence.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateRevision {
    pub trigger: RevisionTrigger,
    pub reason: String,
    /// Reference of the changed evidence (re-evaluation result, appeal order, …).
    pub evidence_reference: Option<String>,
    pub outcome: Option<ProgressionOutcome>,
    pub credits_attempted: Option<f64>,
    pub credits_earned: Option<f64>,
    pub failed_component_ids: Option<Vec<Uuid>>,
    pub evidence_note: Option<String>,
    pub repeat_unit: Option<String>,
    #[serde(default)]
    pub conditions: Vec<ConditionInput>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProgressionRevision {
    pub id: Uuid,
    pub original_decision_id: Uuid,
    pub revised_decision_id: Option<Uuid>,
    pub trigger: String,
    pub same_outcome: bool,
    pub evaluated_outcome: ProgressionOutcome,
    pub reason: String,
    pub evidence_reference: Option<String>,
    pub actor: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RevisionResult {
    pub revision: ProgressionRevision,
    /// The decision now authoritative (the original when the outcome did not change).
    pub decision: ProgressionDecision,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProgressionRevisionList {
    pub items: Vec<ProgressionRevision>,
}

// ================================================================== cases

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProbationCase {
    pub id: Uuid,
    pub student_id: Uuid,
    pub decision_id: Uuid,
    pub review_due_on: Option<NaiveDate>,
    pub status: ProbationStatus,
    pub resolution_note: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DetentionCase {
    pub id: Uuid,
    pub student_id: Uuid,
    pub decision_id: Uuid,
    /// `term` / `year`; `null` while the repeat consequence is undetermined.
    pub repeat_unit: Option<String>,
    pub repeat_term_no: Option<i32>,
    pub determined_at: Option<DateTime<Utc>>,
    pub released_at: Option<DateTime<Utc>>,
    pub release_note: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DetermineRepeat {
    /// `term` or `year`.
    pub repeat_unit: String,
}

// ================================================================== ledger

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreditLedgerEntry {
    pub id: Uuid,
    pub kind: CreditEntryKind,
    /// Absolute amount; `revoked`/`expired` subtract, `adjusted` is signed.
    pub credits: f64,
    pub curriculum_component_id: Option<Uuid>,
    pub academic_term_id: Option<Uuid>,
    pub source_type: String,
    pub source_id: Option<Uuid>,
    pub reverses_entry_id: Option<Uuid>,
    pub institution_approved: bool,
    pub note: Option<String>,
    pub recorded_by: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreditLedger {
    pub student_id: Uuid,
    pub student_context_id: Option<Uuid>,
    pub total_credits: f64,
    pub entries: Vec<CreditLedgerEntry>,
}

/// Append a manual ledger entry (L3-C-028). The ledger is append-only: corrections are
/// `adjusted` or `revoked` (reversing an entry) entries.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AppendLedgerEntry {
    pub kind: CreditEntryKind,
    pub credits: f64,
    pub curriculum_component_id: Option<Uuid>,
    pub academic_term_id: Option<Uuid>,
    pub reverses_entry_id: Option<Uuid>,
    /// Defaults to `true`; provisional credits are excluded from ABC synchronisation.
    pub institution_approved: Option<bool>,
    pub note: String,
}

// ================================================================== mobility

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreditTransferItem {
    pub title: String,
    pub credits: f64,
    /// Equivalent component of the student's curriculum, when mapped.
    pub curriculum_component_id: Option<Uuid>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreditTransfer {
    pub id: Uuid,
    pub student_id: Uuid,
    pub institution_id: Uuid,
    /// `transferred_in` or `recognised`.
    pub kind: String,
    pub source_institution: String,
    pub source_programme: Option<String>,
    pub external_reference: Option<String>,
    pub evidence_note: Option<String>,
    pub items: Vec<CreditTransferItem>,
    pub credits_requested: f64,
    pub credits_recognised: Option<f64>,
    pub status: CaseStatus,
    pub status_reason: Option<String>,
    pub revision_requested: bool,
    pub process_instance_id: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateCreditTransfer {
    /// `transferred_in` (default) or `recognised`.
    pub kind: Option<String>,
    pub source_institution: String,
    pub source_programme: Option<String>,
    pub external_reference: Option<String>,
    pub evidence_note: Option<String>,
    pub items: Vec<CreditTransferItem>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCreditTransfer {
    pub source_programme: Option<String>,
    pub external_reference: Option<String>,
    pub evidence_note: Option<String>,
    pub items: Option<Vec<CreditTransferItem>>,
}

/// Academic recognition decision (L4-C-032-BH-003).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecognitionDecision {
    Approve,
    Reject,
    /// Revision required: back to `requested` for more evidence.
    Revise,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RecognitionDecisionBody {
    pub decision: RecognitionDecision,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreditTransferList {
    pub items: Vec<CreditTransfer>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreditMappingItem {
    pub from_component_id: Option<Uuid>,
    pub to_component_id: Option<Uuid>,
    pub credits: f64,
    pub note: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProgrammeMigration {
    pub id: Uuid,
    pub student_id: Uuid,
    pub institution_id: Uuid,
    pub from_programme_id: Uuid,
    pub from_curriculum_version_id: Option<Uuid>,
    pub from_term_no: Option<i32>,
    pub to_programme_id: Uuid,
    pub to_curriculum_version_id: Uuid,
    pub to_term_no: i32,
    pub credit_mapping: Vec<CreditMappingItem>,
    pub reason: String,
    pub effective_on: Option<NaiveDate>,
    pub status: CaseStatus,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateMigration {
    pub to_programme_id: Uuid,
    /// Defaults to the destination programme's current published curriculum version.
    pub to_curriculum_version_id: Option<Uuid>,
    pub to_term_no: i32,
    #[serde(default)]
    pub credit_mapping: Vec<CreditMappingItem>,
    pub reason: String,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProgrammeMigrationList {
    pub items: Vec<ProgrammeMigration>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AcademicBreak {
    pub id: Uuid,
    pub student_id: Uuid,
    pub institution_id: Uuid,
    pub reason: String,
    pub starts_on: NaiveDate,
    pub expected_return_on: NaiveDate,
    pub term_no_at_start: Option<i32>,
    pub status: AcademicBreakStatus,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub rejoin_term_no: Option<i32>,
    pub rejoin_curriculum_version_id: Option<Uuid>,
    pub rejoin_mapping_note: Option<String>,
    pub rejoined_on: Option<NaiveDate>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateBreak {
    pub reason: String,
    pub starts_on: NaiveDate,
    pub expected_return_on: NaiveDate,
}

/// Rejoining placement (L3-C-035).
#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RejoinBody {
    /// Programme term to rejoin in (defaults to the term at the start of the break).
    pub term_no: Option<i32>,
    /// Curriculum version after curriculum changes during the break (same programme, published).
    pub curriculum_version_id: Option<Uuid>,
    /// How preserved credits map to the current curriculum.
    pub mapping_note: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AcademicBreakList {
    pub items: Vec<AcademicBreak>,
}

// ================================================================== eligibility + ABC

/// Current qualification eligibility determination (L3-C-036). 3C reads it.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QualificationEligibility {
    pub id: Uuid,
    pub student_id: Uuid,
    pub student_context_id: Uuid,
    pub institution_id: Uuid,
    pub programme_id: Option<Uuid>,
    pub curriculum_version_id: Option<Uuid>,
    pub eligible: bool,
    pub outcomes: Vec<QualificationOutcome>,
    pub total_credits: f64,
    pub required_credits: Option<f64>,
    /// Outstanding conditions while not eligible ("Qualification Pending").
    pub outstanding: Vec<String>,
    /// `ledger` or `manual`.
    pub basis: String,
    pub note: Option<String>,
    pub evaluated_by: Option<String>,
    pub evaluated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EvaluateEligibility {
    /// Manual determination when the curriculum defines no minimum credits.
    pub manual_eligible: Option<bool>,
    pub manual_outcomes: Option<Vec<QualificationOutcome>>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QualificationEligibilityList {
    pub items: Vec<QualificationEligibility>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AbcSync {
    pub id: Uuid,
    pub student_id: Uuid,
    pub institution_id: Uuid,
    pub entry_ids: Vec<Uuid>,
    pub total_credits: f64,
    pub status: AbcSyncStatus,
    /// `not_configured` when no ABC adapter is configured (Batch 3), else the registry's reason.
    pub failure_reason: Option<String>,
    pub external_reference: Option<String>,
    pub attempted_at: DateTime<Utc>,
    pub version: i64,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AbcSyncList {
    pub items: Vec<AbcSync>,
}

/// Progression overview of a student (Student 360 progression tab).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProgressionSummary {
    pub student_id: Uuid,
    pub student_context_id: Option<Uuid>,
    pub programme_id: Option<Uuid>,
    pub curriculum_version_id: Option<Uuid>,
    pub current_term_no: Option<i32>,
    pub standing: AcademicStanding,
    pub total_credits: f64,
    pub effective_policy: Option<ProgressionPolicy>,
    pub probation_cases: Vec<ProbationCase>,
    pub detention_cases: Vec<DetentionCase>,
    pub pending_conditions: Vec<ProgressionCondition>,
    pub eligibility: Option<QualificationEligibility>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> PolicyRules {
        PolicyRules {
            progress_min_ratio: 1.0,
            conditional_min_ratio: 0.75,
            probation_min_ratio: 0.5,
            max_failed_for_conditional: 1,
            escalate_repeat_probation: true,
            detention_repeat_unit: "term".into(),
            allow_revision: true,
        }
    }

    #[test]
    fn transitions() {
        use PolicyVersionStatus as P;
        assert!(policy_can_transition(P::Draft, P::InReview));
        assert!(policy_can_transition(P::InReview, P::Effective));
        assert!(policy_can_transition(P::Effective, P::Superseded));
        assert!(!policy_can_transition(P::Retired, P::Effective));
        assert!(!policy_can_transition(P::Draft, P::Effective));
        use ProgressionDecisionStatus as D;
        assert!(decision_can_transition(D::Draft, D::PendingApproval));
        assert!(decision_can_transition(D::PendingApproval, D::Effective));
        assert!(decision_can_transition(D::Effective, D::Superseded));
        assert!(!decision_can_transition(D::Superseded, D::Effective));
        assert!(!decision_can_transition(D::Rejected, D::Effective));
        use CaseStatus as C;
        assert!(case_can_transition(C::Requested, C::UnderReview));
        assert!(case_can_transition(C::UnderReview, C::Requested));
        assert!(case_can_transition(C::Approved, C::Completed));
        assert!(!case_can_transition(C::Rejected, C::Approved));
        use AcademicBreakStatus as B;
        assert!(break_can_transition(B::Approved, B::InEffect));
        assert!(break_can_transition(B::InEffect, B::Rejoined));
        assert!(!break_can_transition(B::Requested, B::Rejoined));
        assert!(condition_can_transition(
            ProgressionConditionStatus::Pending,
            ProgressionConditionStatus::Fulfilled
        ));
        assert!(!condition_can_transition(
            ProgressionConditionStatus::Failed,
            ProgressionConditionStatus::Fulfilled
        ));
        assert!(probation_can_transition(
            ProbationStatus::Active,
            ProbationStatus::Cleared
        ));
        assert!(!probation_can_transition(
            ProbationStatus::Cleared,
            ProbationStatus::Escalated
        ));
    }

    #[test]
    fn recommendation_follows_the_thresholds() {
        let r = rules();
        use ProgressionOutcome as O;
        assert_eq!(
            recommend(&r, Some(20.0), 20.0, 0, false, false),
            O::Progressed
        );
        assert_eq!(
            recommend(&r, Some(20.0), 20.0, 0, false, true),
            O::QualificationEligible
        );
        assert_eq!(
            recommend(&r, Some(20.0), 16.0, 1, false, false),
            O::ConditionallyProgressed
        );
        assert_eq!(
            recommend(&r, Some(20.0), 16.0, 2, false, false),
            O::Probation
        );
        assert_eq!(
            recommend(&r, Some(20.0), 12.0, 1, false, false),
            O::Probation
        );
        assert_eq!(recommend(&r, Some(20.0), 12.0, 1, true, false), O::Detained);
        assert_eq!(recommend(&r, Some(20.0), 8.0, 2, false, false), O::Detained);
        assert_eq!(recommend(&r, None, 0.0, 1, false, false), O::Detained);
        assert!(r.problems().is_empty());
        let mut bad = r.clone();
        bad.probation_min_ratio = 0.9;
        bad.detention_repeat_unit = "decade".into();
        assert_eq!(bad.problems().len(), 2);
    }

    #[test]
    fn terms_standings_and_outcomes() {
        use ProgressionOutcome as O;
        assert_eq!(next_term_no(1, O::Progressed, None, 4), 2);
        assert_eq!(next_term_no(4, O::Progressed, None, 4), 4);
        assert_eq!(next_term_no(2, O::Detained, Some("year"), 4), 1);
        assert_eq!(next_term_no(2, O::Detained, Some("term"), 4), 2);
        assert_eq!(next_term_no(3, O::Repeat, Some("year"), 4), 3);
        assert_eq!(standing_for(O::Probation), AcademicStanding::Probation);
        assert_eq!(standing_for(O::Repeat), AcademicStanding::Detained);
        assert_eq!(signed(CreditEntryKind::Revoked, 4.0), -4.0);
        assert_eq!(signed(CreditEntryKind::Adjusted, -2.0), -2.0);
        use QualificationOutcome as Q;
        assert_eq!(reachable_outcomes(80.0, 80.0, false, true), vec![Q::Degree]);
        assert!(reachable_outcomes(80.0, 80.0, false, false).is_empty());
        assert_eq!(
            reachable_outcomes(40.0, 160.0, true, false),
            vec![Q::Certificate]
        );
        assert_eq!(
            reachable_outcomes(160.0, 160.0, true, true),
            vec![Q::Certificate, Q::Diploma, Q::AdvancedDiploma, Q::Degree]
        );
        assert!(is_mandatory_category("major_core") && !is_mandatory_category("multidisciplinary"));
    }
}
