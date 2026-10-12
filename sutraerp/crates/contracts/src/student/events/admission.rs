//! **3A** — admissions: applications, Admission Record lifecycle, readmission (L2-C01, L3-C-001…007).
//!
//! Owned by slice 3A: add events here (constant + `EVENT_TYPES` entry + payload) and list
//! them in `docs/batch3-contracts.md` §4.3 in the PR.

use super::event_type;
use chrono::NaiveDate;
use uuid::Uuid;

event_type!(
    /// Draft → submitted (StatusChangedV1).
    ADMISSION_APPLICATION_SUBMITTED, ADMISSION_APPLICATION_SUBMITTED_SCHEMA, "admission_application.submitted");
event_type!(
    /// Application offered; an Admission Record was initiated (StatusChangedV1).
    ADMISSION_APPLICATION_OFFERED, ADMISSION_APPLICATION_OFFERED_SCHEMA, "admission_application.offered");
event_type!(
    /// (StatusChangedV1).
    ADMISSION_APPLICATION_REJECTED, ADMISSION_APPLICATION_REJECTED_SCHEMA, "admission_application.rejected");
event_type!(
    /// (StatusChangedV1).
    ADMISSION_APPLICATION_WITHDRAWN, ADMISSION_APPLICATION_WITHDRAWN_SCHEMA, "admission_application.withdrawn");
event_type!(
    /// AdmissionInitiated: Admission Record created with an offer ([`AdmissionInitiatedV1`]). Also readmission (L3-C-005).
    ADMISSION_INITIATED, ADMISSION_INITIATED_SCHEMA, "admission.initiated");
event_type!(
    /// Applicant accepted the offer (StatusChangedV1, status unchanged).
    ADMISSION_OFFER_ACCEPTED, ADMISSION_OFFER_ACCEPTED_SCHEMA, "admission.offer_accepted");
event_type!(
    /// DocumentsSubmitted (StatusChangedV1).
    ADMISSION_DOCUMENTS_SUBMITTED, ADMISSION_DOCUMENTS_SUBMITTED_SCHEMA, "admission.documents_submitted");
event_type!(
    /// DocumentsVerified → verification_completed (StatusChangedV1).
    ADMISSION_DOCUMENTS_VERIFIED, ADMISSION_DOCUMENTS_VERIFIED_SCHEMA, "admission.documents_verified");
event_type!(
    /// Approval step started through the WorkflowPort (StatusChangedV1, carries processInstanceId).
    ADMISSION_APPROVAL_REQUESTED, ADMISSION_APPROVAL_REQUESTED_SCHEMA, "admission.approval_requested");
event_type!(
    /// AdmissionApproved (StatusChangedV1).
    ADMISSION_APPROVED, ADMISSION_APPROVED_SCHEMA, "admission.approved");
event_type!(
    /// FinancialCleared: Admissions' own clearance decision from Finance evidence (amendment 2026-10-05; StatusChangedV1).
    ADMISSION_FINANCIALLY_CLEARED, ADMISSION_FINANCIALLY_CLEARED_SCHEMA, "admission.financially_cleared");
event_type!(
    /// AdmissionConfirmed (StatusChangedV1).
    ADMISSION_CONFIRMED, ADMISSION_CONFIRMED_SCHEMA, "admission.confirmed");
event_type!(
    /// AdmissionStudentCreated (StatusChangedV1; studentId set).
    ADMISSION_STUDENT_CREATED, ADMISSION_STUDENT_CREATED_SCHEMA, "admission.student_created");
event_type!(
    /// AdmissionCompleted (StatusChangedV1).
    ADMISSION_COMPLETED, ADMISSION_COMPLETED_SCHEMA, "admission.completed");
event_type!(
    /// AdmissionCancelled (StatusChangedV1).
    ADMISSION_CANCELLED, ADMISSION_CANCELLED_SCHEMA, "admission.cancelled");
event_type!(
    /// AdmissionWithdrawn (StatusChangedV1).
    ADMISSION_WITHDRAWN, ADMISSION_WITHDRAWN_SCHEMA, "admission.withdrawn");
event_type!(
    /// AdmissionRevoked (StatusChangedV1).
    ADMISSION_REVOKED, ADMISSION_REVOKED_SCHEMA, "admission.revoked");
event_type!(
    /// AdmissionExpired (StatusChangedV1). Publisher: Admissions only (ADR-STU-ADM-EXP-001).
    ADMISSION_EXPIRED, ADMISSION_EXPIRED_SCHEMA, "admission.expired");

pub const EVENT_TYPES: &[&str] = &[
    ADMISSION_APPLICATION_SUBMITTED,
    ADMISSION_APPLICATION_OFFERED,
    ADMISSION_APPLICATION_REJECTED,
    ADMISSION_APPLICATION_WITHDRAWN,
    ADMISSION_INITIATED,
    ADMISSION_OFFER_ACCEPTED,
    ADMISSION_DOCUMENTS_SUBMITTED,
    ADMISSION_DOCUMENTS_VERIFIED,
    ADMISSION_APPROVAL_REQUESTED,
    ADMISSION_APPROVED,
    ADMISSION_FINANCIALLY_CLEARED,
    ADMISSION_CONFIRMED,
    ADMISSION_STUDENT_CREATED,
    ADMISSION_COMPLETED,
    ADMISSION_CANCELLED,
    ADMISSION_WITHDRAWN,
    ADMISSION_REVOKED,
    ADMISSION_EXPIRED,
];

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdmissionInitiatedV1 {
    pub admission_id: Uuid,
    pub institution_id: Uuid,
    pub application_id: Option<Uuid>,
    pub programme_id: Uuid,
    pub curriculum_version_id: Option<Uuid>,
    pub admission_term_id: Option<Uuid>,
    /// Business-local date (institution timezone) after which the offer expires.
    pub offer_expires_on: Option<NaiveDate>,
    /// Set for readmission (L3-C-005): the existing persona and the closed predecessor record.
    pub student_id: Option<Uuid>,
    pub previous_admission_id: Option<Uuid>,
}
