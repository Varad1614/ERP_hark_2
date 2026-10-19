//! **7A** — staff records, engagements, positions, reporting lines, faculty, recruitment / onboarding, probation, separation, certificates, profile verification (L2-E01…E04, E07; L3-E-001…004, 007…009, 011…013, 018, 026, 029, 031, 033, 039, 041, 043, 058…060, 064…066, 068).
//!
//! Owned by slice 7A: add events here (constant + `EVENT_TYPES` entry + payload) and list
//! them in `docs/batch7-contracts.md` §4.3 in the PR. Payload: [`super::StatusChangedV1`] unless
//! noted.

use super::event_type;

event_type!(
    /// Staff record created (ModifiedV1 with every field; L3-E-058).
    STAFF_CREATED, STAFF_CREATED_SCHEMA, "staff_member.created");
event_type!(
    /// Profile identity verified (L3-E-058).
    STAFF_VERIFIED, STAFF_VERIFIED_SCHEMA, "staff_member.verified");
event_type!(
    /// Profile active (L3-E-058).
    STAFF_ACTIVATED, STAFF_ACTIVATED_SCHEMA, "staff_member.activated");
event_type!(
    /// Governed profile correction applied (ModifiedV1; L3-E-065).
    STAFF_CORRECTED, STAFF_CORRECTED_SCHEMA, "staff_member.corrected");
event_type!(
    /// Duplicate workforce record merged onto its canonical record (L3-E-009).
    STAFF_MERGED, STAFF_MERGED_SCHEMA, "staff_member.merged");
event_type!(
    /// Profile archived after separation (L3-E-068).
    STAFF_ARCHIVED, STAFF_ARCHIVED_SCHEMA, "staff_member.archived");
event_type!(
    /// Engagement submitted for activation (L3-E-001).
    ENGAGEMENT_SUBMITTED, ENGAGEMENT_SUBMITTED_SCHEMA, "staff_engagement.submitted");
event_type!(
    /// Engagement active (L3-E-001, rejoining L3-E-008).
    ENGAGEMENT_ACTIVATED, ENGAGEMENT_ACTIVATED_SCHEMA, "staff_engagement.activated");
event_type!(
    /// Active engagement modified (ModifiedV1; L3-E-002).
    ENGAGEMENT_MODIFIED, ENGAGEMENT_MODIFIED_SCHEMA, "staff_engagement.modified");
event_type!(
    /// Engagement transferred to another org unit / campus (ModifiedV1; L3-E-003).
    ENGAGEMENT_TRANSFERRED, ENGAGEMENT_TRANSFERRED_SCHEMA, "staff_engagement.transferred");
event_type!(
    /// Engagement suspended (L3-E-004).
    ENGAGEMENT_SUSPENDED, ENGAGEMENT_SUSPENDED_SCHEMA, "staff_engagement.suspended");
event_type!(
    /// Suspended engagement restored (L3-E-004).
    ENGAGEMENT_RESTORED, ENGAGEMENT_RESTORED_SCHEMA, "staff_engagement.restored");
event_type!(
    /// Engagement closed — institutional exit (L3-E-007, L3-E-041).
    ENGAGEMENT_CLOSED, ENGAGEMENT_CLOSED_SCHEMA, "staff_engagement.closed");
event_type!(
    /// Reporting manager established / changed (ModifiedV1; L3-E-018).
    REPORTING_LINE_SET, REPORTING_LINE_SET_SCHEMA, "staff_engagement.reporting_set");
event_type!(
    /// Position assignment active (L3-E-011/012).
    POSITION_ASSIGNED, POSITION_ASSIGNED_SCHEMA, "position_assignment.activated");
event_type!(
    /// Position assignment completed / terminated (L3-E-013).
    POSITION_ASSIGNMENT_ENDED, POSITION_ASSIGNMENT_ENDED_SCHEMA, "position_assignment.ended");
event_type!(
    /// Staff member established as active faculty (L3-E-026).
    FACULTY_ESTABLISHED, FACULTY_ESTABLISHED_SCHEMA, "faculty_profile.established");
event_type!(
    /// Faculty teaching eligibility changed (L3-E-029/031).
    FACULTY_ELIGIBILITY_CHANGED, FACULTY_ELIGIBILITY_CHANGED_SCHEMA, "faculty_profile.eligibility_changed");
event_type!(
    /// Onboarding case submitted for acceptance (L3-E-033).
    ONBOARDING_SUBMITTED, ONBOARDING_SUBMITTED_SCHEMA, "onboarding_case.submitted");
event_type!(
    /// Onboarding accepted: staff record + engagement handed off (L3-E-033).
    ONBOARDING_COMPLETED, ONBOARDING_COMPLETED_SCHEMA, "onboarding_case.completed");
event_type!(
    /// Probation confirmed / extended / terminated (L3-E-043).
    PROBATION_DECIDED, PROBATION_DECIDED_SCHEMA, "probation_review.decided");
event_type!(
    /// Separation case submitted for approval (L3-E-041).
    SEPARATION_SUBMITTED, SEPARATION_SUBMITTED_SCHEMA, "separation_case.submitted");
event_type!(
    /// Separation completed: engagement closed, exit hand-offs (L3-E-041).
    SEPARATION_COMPLETED, SEPARATION_COMPLETED_SCHEMA, "separation_case.completed");
event_type!(
    /// Employee certificate issued (L3-E-039).
    CERTIFICATE_ISSUED, CERTIFICATE_ISSUED_SCHEMA, "certificate_request.issued");
event_type!(
    /// Qualification verified (L3-E-060).
    QUALIFICATION_VERIFIED, QUALIFICATION_VERIFIED_SCHEMA, "staff_qualification.verified");
event_type!(
    /// Profile document verified (L3-E-064).
    DOCUMENT_VERIFIED, DOCUMENT_VERIFIED_SCHEMA, "staff_document.verified");

event_type!(
    /// Core profile maintained (ModifiedV1; L3-E-059). 7A addition.
    STAFF_MODIFIED, STAFF_MODIFIED_SCHEMA, "staff_member.modified");
event_type!(
    /// Profile temporarily restricted (L2-E07). 7A addition.
    STAFF_SUSPENDED, STAFF_SUSPENDED_SCHEMA, "staff_member.suspended");
event_type!(
    /// Suspended / archived profile back in use (rejoining, L3-E-008). 7A addition.
    STAFF_RESTORED, STAFF_RESTORED_SCHEMA, "staff_member.restored");
event_type!(
    /// Workforce change synchronised into the profile (ModifiedV1; L3-E-066). 7A addition.
    STAFF_SYNCHRONISED, STAFF_SYNCHRONISED_SCHEMA, "staff_member.synchronised");
event_type!(
    /// Requisition approved (opened) or rejected (L3-E-033). 7A addition.
    REQUISITION_DECIDED, REQUISITION_DECIDED_SCHEMA, "requisition.decided");

/// Event types of this slice (inbox rules may claim only these).
pub const EVENT_TYPES: &[&str] = &[
    STAFF_CREATED,
    STAFF_VERIFIED,
    STAFF_ACTIVATED,
    STAFF_CORRECTED,
    STAFF_MERGED,
    STAFF_ARCHIVED,
    ENGAGEMENT_SUBMITTED,
    ENGAGEMENT_ACTIVATED,
    ENGAGEMENT_MODIFIED,
    ENGAGEMENT_TRANSFERRED,
    ENGAGEMENT_SUSPENDED,
    ENGAGEMENT_RESTORED,
    ENGAGEMENT_CLOSED,
    REPORTING_LINE_SET,
    POSITION_ASSIGNED,
    POSITION_ASSIGNMENT_ENDED,
    FACULTY_ESTABLISHED,
    FACULTY_ELIGIBILITY_CHANGED,
    ONBOARDING_SUBMITTED,
    ONBOARDING_COMPLETED,
    PROBATION_DECIDED,
    SEPARATION_SUBMITTED,
    SEPARATION_COMPLETED,
    CERTIFICATE_ISSUED,
    QUALIFICATION_VERIFIED,
    DOCUMENT_VERIFIED,
    STAFF_MODIFIED,
    STAFF_SUSPENDED,
    STAFF_RESTORED,
    STAFF_SYNCHRONISED,
    REQUISITION_DECIDED,
];
