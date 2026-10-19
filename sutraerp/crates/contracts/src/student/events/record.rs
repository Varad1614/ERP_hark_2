//! **3A** — student record: creation, activation, enrolment, assignment, suspension, transfer, profile, identity, duplicates, documents, archival (L2-C02, L3-C-001, C-008…C-019).
//!
//! Owned by slice 3A: add events here (constant + `EVENT_TYPES` entry + payload) and list
//! them in `docs/batch3-contracts.md` §4.3 in the PR.

use super::event_type;
use chrono::NaiveDate;
use uuid::Uuid;

event_type!(
    /// StudentCreated ([`StudentCreatedV1`]).
    STUDENT_CREATED, STUDENT_CREATED_SCHEMA, "student.created");
event_type!(
    /// StudentActivated: context pending_activation (StatusChangedV1).
    STUDENT_ACTIVATED, STUDENT_ACTIVATED_SCHEMA, "student.activated");
event_type!(
    /// Enrolment completed; context → active (StatusChangedV1).
    STUDENT_ENROLLED, STUDENT_ENROLLED_SCHEMA, "student.enrolled");
event_type!(
    /// Enrolment attributes changed (ModifiedV1, L3-C-008).
    STUDENT_ENROLMENT_UPDATED, STUDENT_ENROLMENT_UPDATED_SCHEMA, "student.enrolment_updated");
event_type!(
    /// Programme/department/batch/section changed (ModifiedV1, L3-C-009).
    STUDENT_ACADEMIC_ASSIGNMENT_CHANGED, STUDENT_ACADEMIC_ASSIGNMENT_CHANGED_SCHEMA, "student.academic_assignment_changed");
event_type!(
    /// Context active → suspended (StatusChangedV1, L3-C-010).
    STUDENT_SUSPENDED, STUDENT_SUSPENDED_SCHEMA, "student.suspended");
event_type!(
    /// Context suspended → active (StatusChangedV1, L3-C-011).
    STUDENT_RESTORED, STUDENT_RESTORED_SCHEMA, "student.restored");
event_type!(
    /// Intra- or inter-institute transfer completed ([`StudentTransferredV1`], L3-C-012/013).
    STUDENT_TRANSFERRED, STUDENT_TRANSFERRED_SCHEMA, "student.transferred");
event_type!(
    /// (ModifiedV1, L3-C-014).
    STUDENT_PROFILE_UPDATED, STUDENT_PROFILE_UPDATED_SCHEMA, "student.profile_updated");
event_type!(
    /// (ModifiedV1, L3-C-015).
    STUDENT_COMMUNICATION_PREFERENCES_UPDATED, STUDENT_COMMUNICATION_PREFERENCES_UPDATED_SCHEMA, "student.communication_preferences_updated");
event_type!(
    /// Governed identity correction applied (ModifiedV1, L3-C-016).
    STUDENT_IDENTITY_CORRECTED, STUDENT_IDENTITY_CORRECTED_SCHEMA, "student.identity_corrected");
event_type!(
    /// Duplicate resolved; the duplicate persona is archived and points at the survivor ([`StudentDuplicateResolvedV1`], L3-C-017).
    STUDENT_DUPLICATE_RESOLVED, STUDENT_DUPLICATE_RESOLVED_SCHEMA, "student.duplicate_resolved");
event_type!(
    /// Persona archived / permanent record closure (StatusChangedV1, L3-C-019).
    STUDENT_ARCHIVED, STUDENT_ARCHIVED_SCHEMA, "student.archived");
event_type!(
    /// Student document/certificate request raised (StatusChangedV1, L3-C-018).
    DOCUMENT_REQUEST_SUBMITTED, DOCUMENT_REQUEST_SUBMITTED_SCHEMA, "document_request.submitted");
event_type!(
    /// (StatusChangedV1).
    DOCUMENT_REQUEST_ISSUED, DOCUMENT_REQUEST_ISSUED_SCHEMA, "document_request.issued");
event_type!(
    /// (StatusChangedV1).
    DOCUMENT_REQUEST_REJECTED, DOCUMENT_REQUEST_REJECTED_SCHEMA, "document_request.rejected");

pub const EVENT_TYPES: &[&str] = &[
    STUDENT_CREATED,
    STUDENT_ACTIVATED,
    STUDENT_ENROLLED,
    STUDENT_ENROLMENT_UPDATED,
    STUDENT_ACADEMIC_ASSIGNMENT_CHANGED,
    STUDENT_SUSPENDED,
    STUDENT_RESTORED,
    STUDENT_TRANSFERRED,
    STUDENT_PROFILE_UPDATED,
    STUDENT_COMMUNICATION_PREFERENCES_UPDATED,
    STUDENT_IDENTITY_CORRECTED,
    STUDENT_DUPLICATE_RESOLVED,
    STUDENT_ARCHIVED,
    DOCUMENT_REQUEST_SUBMITTED,
    DOCUMENT_REQUEST_ISSUED,
    DOCUMENT_REQUEST_REJECTED,
];

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StudentCreatedV1 {
    pub student_id: Uuid,
    pub student_context_id: Uuid,
    pub institution_id: Uuid,
    pub admission_id: Option<Uuid>,
    pub student_number: String,
    pub programme_id: Option<Uuid>,
    pub student_subject_ref: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StudentTransferredV1 {
    pub transfer_id: Uuid,
    pub student_id: Uuid,
    /// `intra_institute` | `inter_institute`.
    pub kind: String,
    pub from_context_id: Uuid,
    /// Equal to `fromContextId` for intra-institute transfers.
    pub to_context_id: Uuid,
    pub from_institution_id: Uuid,
    pub to_institution_id: Uuid,
    pub effective_on: NaiveDate,
    pub process_instance_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StudentDuplicateResolvedV1 {
    pub case_id: Uuid,
    pub surviving_student_id: Uuid,
    pub archived_student_id: Uuid,
    pub institution_id: Uuid,
}
