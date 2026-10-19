//! **3C** — qualification completion, conferment, record finalisation, exits, clearance, deferral, correction/revocation (L2-C05 + amendment, L3-C-039…050).
//!
//! Owned by slice 3C: add events here (constant + `EVENT_TYPES` entry + payload) and list
//! them in `docs/batch3-contracts.md` §4.3 in the PR.

use super::event_type;
use chrono::NaiveDate;
use uuid::Uuid;

event_type!(
    /// QualificationEligibilityEvaluated (StatusChangedV1).
    QUALIFICATION_COMPLETION_ELIGIBILITY_EVALUATED, QUALIFICATION_COMPLETION_ELIGIBILITY_EVALUATED_SCHEMA, "qualification_completion.eligibility_evaluated");
event_type!(
    /// DegreeAuditCompleted (StatusChangedV1).
    QUALIFICATION_COMPLETION_DEGREE_AUDIT_COMPLETED, QUALIFICATION_COMPLETION_DEGREE_AUDIT_COMPLETED_SCHEMA, "qualification_completion.degree_audit_completed");
event_type!(
    /// InstitutionalClearanceCompleted (StatusChangedV1).
    QUALIFICATION_COMPLETION_INSTITUTIONAL_CLEARANCE_COMPLETED, QUALIFICATION_COMPLETION_INSTITUTIONAL_CLEARANCE_COMPLETED_SCHEMA, "qualification_completion.institutional_clearance_completed");
event_type!(
    /// QualificationCompletionApproved (StatusChangedV1).
    QUALIFICATION_COMPLETION_APPROVED, QUALIFICATION_COMPLETION_APPROVED_SCHEMA, "qualification_completion.approved");
event_type!(
    /// QualificationAwarded ([`QualificationAwardedV1`]).
    QUALIFICATION_COMPLETION_QUALIFICATION_AWARDED, QUALIFICATION_COMPLETION_QUALIFICATION_AWARDED_SCHEMA, "qualification_completion.qualification_awarded");
event_type!(
    /// AcademicRecordFinalized (StatusChangedV1).
    QUALIFICATION_COMPLETION_ACADEMIC_RECORD_FINALIZED, QUALIFICATION_COMPLETION_ACADEMIC_RECORD_FINALIZED_SCHEMA, "qualification_completion.academic_record_finalized");
event_type!(
    /// (StatusChangedV1, L3-C-049).
    QUALIFICATION_COMPLETION_DEFERRED, QUALIFICATION_COMPLETION_DEFERRED_SCHEMA, "qualification_completion.deferred");
event_type!(
    /// ([`QualificationAwardedV1`] of the corrected version, L3-C-050).
    QUALIFICATION_CORRECTED, QUALIFICATION_CORRECTED_SCHEMA, "qualification.corrected");
event_type!(
    /// (StatusChangedV1, L3-C-050).
    QUALIFICATION_REVOKED, QUALIFICATION_REVOKED_SCHEMA, "qualification.revoked");
event_type!(
    /// ([`StudentExitV1`]).
    STUDENT_EXIT_INITIATED, STUDENT_EXIT_INITIATED_SCHEMA, "student_exit.initiated");
event_type!(
    /// StudentExitCompleted ([`StudentExitV1`]); the context is terminal.
    STUDENT_EXIT_COMPLETED, STUDENT_EXIT_COMPLETED_SCHEMA, "student_exit.completed");
event_type!(
    /// ([`StudentExitV1`]).
    STUDENT_EXIT_CANCELLED, STUDENT_EXIT_CANCELLED_SCHEMA, "student_exit.cancelled");
event_type!(
    /// All clearance items cleared/waived (StatusChangedV1, L3-C-046).
    EXIT_CLEARANCE_COMPLETED, EXIT_CLEARANCE_COMPLETED_SCHEMA, "exit_clearance.completed");

pub const EVENT_TYPES: &[&str] = &[
    QUALIFICATION_COMPLETION_ELIGIBILITY_EVALUATED,
    QUALIFICATION_COMPLETION_DEGREE_AUDIT_COMPLETED,
    QUALIFICATION_COMPLETION_INSTITUTIONAL_CLEARANCE_COMPLETED,
    QUALIFICATION_COMPLETION_APPROVED,
    QUALIFICATION_COMPLETION_QUALIFICATION_AWARDED,
    QUALIFICATION_COMPLETION_ACADEMIC_RECORD_FINALIZED,
    QUALIFICATION_COMPLETION_DEFERRED,
    QUALIFICATION_CORRECTED,
    QUALIFICATION_REVOKED,
    STUDENT_EXIT_INITIATED,
    STUDENT_EXIT_COMPLETED,
    STUDENT_EXIT_CANCELLED,
    EXIT_CLEARANCE_COMPLETED,
];

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QualificationAwardedV1 {
    pub completion_id: Uuid,
    pub student_id: Uuid,
    pub student_context_id: Uuid,
    pub institution_id: Uuid,
    pub programme_id: Uuid,
    /// `QualificationOutcome` wire name.
    pub outcome: String,
    pub conferred_on: NaiveDate,
    pub supersedes_completion_id: Option<Uuid>,
    pub process_instance_id: Option<String>,
    pub student_subject_ref: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StudentExitV1 {
    pub exit_id: Uuid,
    pub student_id: Uuid,
    pub student_context_id: Uuid,
    pub institution_id: Uuid,
    /// `StudentExitKind` wire name.
    pub kind: String,
    /// `StudentExitStatus` wire name.
    pub status: String,
    pub effective_on: Option<NaiveDate>,
    pub reason: Option<String>,
    pub process_instance_id: Option<String>,
    /// 3C: login subject of the student (additive optional member) so inbox rules can notify.
    #[serde(default)]
    pub student_subject_ref: Option<String>,
}
