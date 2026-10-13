//! **4C** — flexible and external learning (open electives, MOOCs), providers, certificates, credit mapping and recognition, course resources (L2-D06, L3-D-034…050).
//!
//! Owned by slice 4C: add events here (constant + `EVENT_TYPES` entry + payload) and list
//! them in `docs/batch4-contracts.md` §4.3 in the PR.

use super::event_type;
use uuid::Uuid;

event_type!(
    /// (StatusChangedV1, L3-D-044).
    LEARNING_PROVIDER_ACTIVATED, LEARNING_PROVIDER_ACTIVATED_SCHEMA, "learning_provider.activated");
event_type!(
    /// (StatusChangedV1).
    LEARNING_PROVIDER_SUSPENDED, LEARNING_PROVIDER_SUSPENDED_SCHEMA, "learning_provider.suspended");
event_type!(
    /// (StatusChangedV1, L3-D-040).
    LEARNING_OPPORTUNITY_ACTIVATED, LEARNING_OPPORTUNITY_ACTIVATED_SCHEMA, "learning_opportunity.activated");
event_type!(
    /// (StatusChangedV1, L3-D-041).
    LEARNING_OPPORTUNITY_SUSPENDED, LEARNING_OPPORTUNITY_SUSPENDED_SCHEMA, "learning_opportunity.suspended");
event_type!(
    /// (StatusChangedV1, L3-D-042).
    LEARNING_OPPORTUNITY_ARCHIVED, LEARNING_OPPORTUNITY_ARCHIVED_SCHEMA, "learning_opportunity.archived");
event_type!(
    /// (StatusChangedV1, L3-D-034).
    FLEXIBLE_ENROLMENT_REQUESTED, FLEXIBLE_ENROLMENT_REQUESTED_SCHEMA, "flexible_enrolment.requested");
event_type!(
    /// Seat allocated (StatusChangedV1, L3-D-035/048).
    FLEXIBLE_ENROLMENT_ALLOCATED, FLEXIBLE_ENROLMENT_ALLOCATED_SCHEMA, "flexible_enrolment.allocated");
event_type!(
    /// (StatusChangedV1, L3-D-036).
    FLEXIBLE_ENROLMENT_WAITLISTED, FLEXIBLE_ENROLMENT_WAITLISTED_SCHEMA, "flexible_enrolment.waitlisted");
event_type!(
    /// External registration recorded (StatusChangedV1, L3-D-037).
    FLEXIBLE_ENROLMENT_REGISTERED, FLEXIBLE_ENROLMENT_REGISTERED_SCHEMA, "flexible_enrolment.registered");
event_type!(
    /// Learning completion recorded (StatusChangedV1, L3-D-038).
    FLEXIBLE_ENROLMENT_COMPLETED, FLEXIBLE_ENROLMENT_COMPLETED_SCHEMA, "flexible_enrolment.completed");
event_type!(
    /// Accepted / rejected / flagged (StatusChangedV1, L3-D-043).
    CERTIFICATE_VERIFICATION_DECIDED, CERTIFICATE_VERIFICATION_DECIDED_SCHEMA, "certificate_verification.decided");
event_type!(
    /// (StatusChangedV1, L3-D-039).
    CREDIT_RECOGNITION_APPROVED, CREDIT_RECOGNITION_APPROVED_SCHEMA, "credit_recognition.approved");
event_type!(
    /// Credits posted to the Domain C ledger ([`CreditRecognitionPostedV1`]).
    CREDIT_RECOGNITION_POSTED, CREDIT_RECOGNITION_POSTED_SCHEMA, "credit_recognition.posted");
event_type!(
    /// (StatusChangedV1, L3-D-046).
    CREDIT_MAPPING_RULE_EFFECTIVE, CREDIT_MAPPING_RULE_EFFECTIVE_SCHEMA, "credit_mapping_rule.effective");
event_type!(
    /// (StatusChangedV1, L3-D-045).
    FLEXIBLE_LEARNING_POLICY_EFFECTIVE, FLEXIBLE_LEARNING_POLICY_EFFECTIVE_SCHEMA, "flexible_learning_policy.effective");
event_type!(
    /// Resource / library link visible to the section's students (StatusChangedV1).
    COURSE_RESOURCE_PUBLISHED, COURSE_RESOURCE_PUBLISHED_SCHEMA, "course_resource.published");

pub const EVENT_TYPES: &[&str] = &[
    LEARNING_PROVIDER_ACTIVATED,
    LEARNING_PROVIDER_SUSPENDED,
    LEARNING_OPPORTUNITY_ACTIVATED,
    LEARNING_OPPORTUNITY_SUSPENDED,
    LEARNING_OPPORTUNITY_ARCHIVED,
    FLEXIBLE_ENROLMENT_REQUESTED,
    FLEXIBLE_ENROLMENT_ALLOCATED,
    FLEXIBLE_ENROLMENT_WAITLISTED,
    FLEXIBLE_ENROLMENT_REGISTERED,
    FLEXIBLE_ENROLMENT_COMPLETED,
    CERTIFICATE_VERIFICATION_DECIDED,
    CREDIT_RECOGNITION_APPROVED,
    CREDIT_RECOGNITION_POSTED,
    CREDIT_MAPPING_RULE_EFFECTIVE,
    FLEXIBLE_LEARNING_POLICY_EFFECTIVE,
    COURSE_RESOURCE_PUBLISHED,
];

/// Payload of [`CREDIT_RECOGNITION_POSTED`].
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreditRecognitionPostedV1 {
    pub recognition_id: Uuid,
    pub institution_id: Uuid,
    pub student_id: Uuid,
    pub credits: f64,
    pub curriculum_component_id: Option<Uuid>,
    /// `student.credit_ledger_entry` id returned by `CreditPostingPort`.
    pub ledger_entry_id: Uuid,
    pub student_subject_ref: Option<String>,
}
