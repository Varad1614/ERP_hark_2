//! **3C** completion & exit domain: DTOs, commands and the slice-owned state machines
//! (L2-C05 §9.4 + MEME amendment §4; L3-C-039 … C-050).

use chrono::{DateTime, NaiveDate, Utc};
use sutra_contracts::student::ids::{
    CompletionDeferralId, ExitClearanceId, QualificationCompletionId, QualificationCorrectionId,
    StudentContextId, StudentExitId, StudentId,
};
use sutra_contracts::student::status::{
    CaseStatus, ConferredQualificationStatus, ExitClearanceStatus, QualificationCompletionStatus,
    QualificationOutcome, StudentContextStatus, StudentExitKind, StudentExitStatus,
};
use uuid::Uuid;

/// Completion pathway: full programme completion or NEP multiple-entry/exit intermediate exit.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CompletionPathway {
    Completion,
    MemeExit,
}

impl CompletionPathway {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completion => "completion",
            Self::MemeExit => "meme_exit",
        }
    }
    /// The exit kind recorded when this pathway completes.
    pub fn exit_kind(self) -> StudentExitKind {
        match self {
            Self::Completion => StudentExitKind::Completion,
            Self::MemeExit => StudentExitKind::MemeExit,
        }
    }
}

/// Clearance item of an exit or a qualification completion (L3-C-046).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ClearanceItem {
    /// Read from Finance (`FinanceClearancePort::student_dues`); never cleared manually.
    Finance,
    Library,
    Hostel,
    Department,
    Laboratory,
    Sports,
    Administration,
    Examination,
}

impl ClearanceItem {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Finance => "finance",
            Self::Library => "library",
            Self::Hostel => "hostel",
            Self::Department => "department",
            Self::Laboratory => "laboratory",
            Self::Sports => "sports",
            Self::Administration => "administration",
            Self::Examination => "examination",
        }
    }
}

/// Default clearance set when a command does not name one.
pub const DEFAULT_CLEARANCES: &[ClearanceItem] = &[
    ClearanceItem::Finance,
    ClearanceItem::Library,
    ClearanceItem::Department,
];

/// Evidence of an eligibility evaluation (L4-C-039-BH-001).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EligibilityResult {
    pub eligible: bool,
    /// `progression` (3B qualification eligibility), `credit_ledger` (3B ledger) or `manual`
    /// (evidence entered by the evaluator, Exams results not published yet).
    pub source: String,
    pub credits_earned: Option<f64>,
    pub required_credits: Option<f64>,
    /// Outcomes the progression evaluation allows (empty when unknown).
    #[serde(default)]
    pub outcomes: Vec<QualificationOutcome>,
    /// Unresolved requirements (empty when eligible).
    #[serde(default)]
    pub unmet: Vec<String>,
    pub evaluated_at: DateTime<Utc>,
}

/// Degree / qualification audit basis (L4-C-039-BH-002).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuditResult {
    pub passed: bool,
    #[serde(default)]
    pub deficiencies: Vec<String>,
    pub note: Option<String>,
    pub credits_earned: Option<f64>,
    pub required_credits: Option<f64>,
    pub audited_at: DateTime<Utc>,
}

/// One clearance item.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExitClearance {
    pub id: ExitClearanceId,
    pub institution_id: Uuid,
    pub student_id: StudentId,
    pub exit_id: Option<StudentExitId>,
    pub completion_id: Option<QualificationCompletionId>,
    pub item: ClearanceItem,
    pub status: ExitClearanceStatus,
    pub note: Option<String>,
    pub resolved_by: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub version: i64,
}

#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
pub struct ExitClearanceList {
    pub items: Vec<ExitClearance>,
}

/// Qualification Completion aggregate (§5.3 minimum: id, studentId, outcome, status, conferredOn, version).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QualificationCompletion {
    pub id: QualificationCompletionId,
    pub institution_id: Uuid,
    pub student_id: StudentId,
    pub student_context_id: StudentContextId,
    pub programme_id: Option<Uuid>,
    pub curriculum_version_id: Option<Uuid>,
    pub pathway: CompletionPathway,
    pub outcome: QualificationOutcome,
    pub status: QualificationCompletionStatus,
    /// Clearance items opened when institutional clearance starts.
    #[serde(default)]
    pub requested_clearances: Vec<ClearanceItem>,
    /// 3B qualification eligibility the evaluation used, when available.
    pub eligibility_id: Option<Uuid>,
    pub eligibility_result: Option<EligibilityResult>,
    pub audit_result: Option<AuditResult>,
    pub process_instance_id: Option<String>,
    pub decision_reason: Option<String>,
    pub conferred_qualification_id: Option<Uuid>,
    pub conferred_on: Option<NaiveDate>,
    pub finalized_on: Option<NaiveDate>,
    pub exit_id: Option<StudentExitId>,
    pub status_reason: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Institutional clearance items (filled on reads).
    #[serde(default)]
    pub clearances: Vec<ExitClearance>,
    /// Deferrals of this completion, newest first (filled on reads).
    #[serde(default)]
    pub deferrals: Vec<CompletionDeferral>,
}

#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
pub struct QualificationCompletionList {
    pub items: Vec<QualificationCompletion>,
}

/// Initiate qualification completion evaluation (L3-C-039 / C-041).
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewQualificationCompletion {
    /// Target outcome (MEME: certificate / diploma / advanced diploma).
    pub outcome: QualificationOutcome,
    #[serde(default)]
    pub pathway: Option<CompletionPathway>,
    /// Clearance items required before approval (default finance, library, department).
    #[serde(default)]
    pub clearances: Option<Vec<ClearanceItem>>,
}

/// Evaluate eligibility. Evidence is optional: the 3B progression eligibility / credit ledger is
/// read when present; otherwise the evaluator's manual evidence applies.
#[derive(Clone, Debug, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EligibilityEvaluation {
    pub credits_earned: Option<f64>,
    /// Policy-defined requirement (MEME exits: mandatory; completion default: curriculum minimum).
    pub required_credits: Option<f64>,
}

/// Record the degree / qualification audit.
#[derive(Clone, Debug, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DegreeAudit {
    #[serde(default)]
    pub deficiencies: Vec<String>,
    pub note: Option<String>,
    pub credits_earned: Option<f64>,
}

/// Resolve a clearance item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClearanceResolution {
    Cleared,
    Waived,
    Blocked,
}

#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResolveClearance {
    pub status: ClearanceResolution,
    pub note: Option<String>,
}

/// Add a clearance item to an open exit.
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewExitClearance {
    pub item: ClearanceItem,
    pub note: Option<String>,
}

/// Confer the approved qualification (step-up).
#[derive(Clone, Debug, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AwardQualification {
    /// Default: the programme name + outcome.
    pub title: Option<String>,
    /// Default: today in the institution's business timezone.
    pub conferred_on: Option<NaiveDate>,
}

/// Defer completion pending remediation (L3-C-049).
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeferCompletion {
    pub reason: String,
    pub remediation: Option<String>,
    pub review_on: Option<NaiveDate>,
}

/// Resume a deferred completion once remediation evidence exists.
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResumeCompletion {
    pub resolution_note: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompletionDeferral {
    pub id: CompletionDeferralId,
    pub completion_id: QualificationCompletionId,
    pub student_id: StudentId,
    pub deferred_from: QualificationCompletionStatus,
    pub reason: String,
    pub remediation: Option<String>,
    pub review_on: Option<NaiveDate>,
    pub status: CaseStatus,
    pub resolution_note: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

/// A conferred qualification (corrections append a new row; revocation keeps the row).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConferredQualification {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub student_id: StudentId,
    pub student_context_id: StudentContextId,
    pub completion_id: QualificationCompletionId,
    pub programme_id: Option<Uuid>,
    pub outcome: QualificationOutcome,
    pub title: String,
    pub conferred_on: NaiveDate,
    pub status: ConferredQualificationStatus,
    pub supersedes_id: Option<Uuid>,
    pub status_reason: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    /// Correction / revocation cases (filled on reads).
    #[serde(default)]
    pub corrections: Vec<QualificationCorrection>,
}

#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
pub struct ConferredQualificationList {
    pub items: Vec<ConferredQualification>,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionKind {
    Correction,
    Revocation,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionResolution {
    Corrected,
    Revoked,
    NoChange,
}

/// Values a correction proposes (at least one).
#[derive(
    Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "camelCase")]
pub struct ProposedQualification {
    pub outcome: Option<QualificationOutcome>,
    pub title: Option<String>,
    pub conferred_on: Option<NaiveDate>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QualificationCorrection {
    pub id: QualificationCorrectionId,
    pub conferred_qualification_id: Uuid,
    pub student_id: StudentId,
    pub kind: CorrectionKind,
    pub status: CaseStatus,
    #[serde(default)]
    pub proposed: ProposedQualification,
    pub reason: String,
    pub evidence_ref: Option<String>,
    pub resolution: Option<CorrectionResolution>,
    pub decision_reason: Option<String>,
    pub result_qualification_id: Option<Uuid>,
    pub process_instance_id: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

/// Request a correction of a conferred qualification (step-up).
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewQualificationCorrection {
    pub proposed: ProposedQualification,
    pub reason: String,
    pub evidence_ref: Option<String>,
}

/// Request revocation of a conferred qualification (step-up, `qualification.revoke`).
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RevokeQualification {
    pub reason: String,
    pub evidence_ref: Option<String>,
}

/// Decision on a pending correction / revocation case.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionDecisionValue {
    /// Apply the correction / revocation.
    Apply,
    /// Evidence remains valid: the conferred qualification stays unchanged (L4-C-050).
    NoChange,
    Reject,
}

#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CorrectionDecision {
    pub decision: CorrectionDecisionValue,
    pub reason: Option<String>,
}

/// Student exit case (§5.3 minimum: id, studentId, kind, status, effectiveOn, version).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StudentExit {
    pub id: StudentExitId,
    pub institution_id: Uuid,
    pub student_id: StudentId,
    pub student_context_id: StudentContextId,
    pub kind: StudentExitKind,
    pub status: StudentExitStatus,
    pub reason: String,
    pub requested_effective_on: Option<NaiveDate>,
    pub effective_on: Option<NaiveDate>,
    pub destination_institution: Option<String>,
    pub evidence_ref: Option<String>,
    pub completion_id: Option<QualificationCompletionId>,
    pub process_instance_id: Option<String>,
    pub decision_reason: Option<String>,
    pub status_reason: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub clearances: Vec<ExitClearance>,
}

#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
pub struct StudentExitList {
    pub items: Vec<StudentExit>,
}

/// Initiate an exit (withdrawal, transfer-out, discontinuation, termination, administrative
/// closure, deceased record closure). Completion / MEME exits come from the completion flow.
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewStudentExit {
    pub kind: StudentExitKind,
    pub reason: String,
    pub requested_effective_on: Option<NaiveDate>,
    /// Transfer-out: the receiving (external) institution. Required for `transfer`.
    pub destination_institution: Option<String>,
    /// Deceased: reference of the verified notification. Required for `deceased`;
    /// termination: the disciplinary / regulatory decision reference (required).
    pub evidence_ref: Option<String>,
    #[serde(default)]
    pub clearances: Option<Vec<ClearanceItem>>,
}

/// Complete an approved exit.
#[derive(Clone, Debug, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompleteExit {
    /// Default: the requested effective date, else today (business timezone).
    pub effective_on: Option<NaiveDate>,
}

// ---- state machines (pure) -------------------------------------------------------------------

/// Qualification Completion transitions (L2-C05 §9.4 + deferral L3-C-049 + cancellation).
pub fn completion_can_transition(
    from: QualificationCompletionStatus,
    to: QualificationCompletionStatus,
) -> bool {
    use QualificationCompletionStatus::*;
    matches!(
        (from, to),
        (EligibilityEvaluation, DegreeAudit)
            | (DegreeAudit, InstitutionalClearance)
            | (InstitutionalClearance, ApprovalPending)
            | (ApprovalPending, Approved)
            | (ApprovalPending, DegreeAudit)
            | (Approved, Awarded)
            | (Awarded, RecordFinalized)
            | (RecordFinalized, ExitCompleted)
            | (
                EligibilityEvaluation | DegreeAudit | InstitutionalClearance,
                Deferred
            )
            | (Deferred, EligibilityEvaluation)
            | (
                EligibilityEvaluation | DegreeAudit | InstitutionalClearance | Deferred,
                Cancelled
            )
    )
}

/// Student exit transitions.
pub fn exit_can_transition(from: StudentExitStatus, to: StudentExitStatus) -> bool {
    use StudentExitStatus::*;
    matches!(
        (from, to),
        (Initiated, ClearancePending)
            | (ClearancePending, ApprovalPending)
            | (ApprovalPending, Approved)
            | (ApprovalPending, Rejected)
            | (Approved, Completed)
            | (
                Initiated | ClearancePending | ApprovalPending | Approved,
                Cancelled
            )
    )
}

/// Clearance item transitions (cleared / waived are final).
pub fn clearance_can_transition(from: ExitClearanceStatus, to: ExitClearanceStatus) -> bool {
    use ExitClearanceStatus::*;
    matches!(
        (from, to),
        (Pending, Cleared | Waived | Blocked) | (Blocked, Cleared | Waived | Pending)
    )
}

/// Correction / revocation case transitions (CaseStatus subset).
pub fn correction_can_transition(from: CaseStatus, to: CaseStatus) -> bool {
    use CaseStatus::*;
    matches!(
        (from, to),
        (Requested, UnderReview) | (UnderReview, Completed | Rejected) | (Requested, Withdrawn)
    )
}

/// Terminal StudentContext state of an exit kind (contract §4.2).
pub fn terminal_context_status(kind: StudentExitKind) -> StudentContextStatus {
    match kind {
        StudentExitKind::Completion | StudentExitKind::MemeExit => {
            StudentContextStatus::QualificationCompleted
        }
        StudentExitKind::Withdrawal => StudentContextStatus::Withdrawn,
        StudentExitKind::Transfer => StudentContextStatus::Transferred,
        StudentExitKind::Discontinuation
        | StudentExitKind::AdministrativeClosure
        | StudentExitKind::Deceased => StudentContextStatus::Discontinued,
        StudentExitKind::Termination => StudentContextStatus::Terminated,
    }
}

/// Intermediate outcomes a MEME exit may award (amendment §2: the outcome set is L1's; the
/// two terminal ones are programme completion, not an intermediate exit).
pub fn meme_outcome_allowed(outcome: QualificationOutcome) -> bool {
    matches!(
        outcome,
        QualificationOutcome::Certificate
            | QualificationOutcome::Diploma
            | QualificationOutcome::AdvancedDiploma
    )
}

/// Human label of an outcome for the default conferment title.
pub fn outcome_label(outcome: QualificationOutcome) -> &'static str {
    match outcome {
        QualificationOutcome::Certificate => "Certificate",
        QualificationOutcome::Diploma => "Diploma",
        QualificationOutcome::AdvancedDiploma => "Advanced Diploma",
        QualificationOutcome::Degree => "Degree",
        QualificationOutcome::DegreeWithResearch => "Degree with Research",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_machine_follows_l2_c05() {
        use QualificationCompletionStatus::*;
        assert!(completion_can_transition(
            EligibilityEvaluation,
            DegreeAudit
        ));
        assert!(!completion_can_transition(
            EligibilityEvaluation,
            InstitutionalClearance
        ));
        assert!(!completion_can_transition(DegreeAudit, ApprovalPending));
        assert!(completion_can_transition(ApprovalPending, DegreeAudit));
        assert!(!completion_can_transition(ApprovalPending, Awarded));
        assert!(!completion_can_transition(Approved, Deferred));
        assert!(completion_can_transition(Deferred, EligibilityEvaluation));
        assert!(!completion_can_transition(Deferred, Approved));
        for to in QualificationCompletionStatus::ALL {
            assert!(!completion_can_transition(ExitCompleted, *to));
            assert!(!completion_can_transition(Cancelled, *to));
        }
    }

    #[test]
    fn exit_machine_and_terminal_states() {
        use StudentExitStatus::*;
        assert!(exit_can_transition(ClearancePending, ApprovalPending));
        assert!(!exit_can_transition(ClearancePending, Completed));
        assert!(!exit_can_transition(Completed, Cancelled));
        assert!(exit_can_transition(Approved, Cancelled));
        assert_eq!(
            terminal_context_status(StudentExitKind::Deceased),
            StudentContextStatus::Discontinued
        );
        assert_eq!(
            terminal_context_status(StudentExitKind::MemeExit),
            StudentContextStatus::QualificationCompleted
        );
        for k in StudentExitKind::ALL {
            assert!(terminal_context_status(*k).is_terminal());
        }
    }

    #[test]
    fn clearance_and_correction_machines() {
        use ExitClearanceStatus::*;
        assert!(clearance_can_transition(Pending, Blocked));
        assert!(clearance_can_transition(Blocked, Cleared));
        assert!(!clearance_can_transition(Cleared, Blocked));
        assert!(!clearance_can_transition(Waived, Pending));
        assert!(correction_can_transition(
            CaseStatus::UnderReview,
            CaseStatus::Completed
        ));
        assert!(!correction_can_transition(
            CaseStatus::Completed,
            CaseStatus::UnderReview
        ));
        assert!(meme_outcome_allowed(QualificationOutcome::Diploma));
        assert!(!meme_outcome_allowed(QualificationOutcome::Degree));
    }
}
