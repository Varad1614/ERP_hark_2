//! **4A** — course offerings, sections, enrolment governance, waitlist, faculty academic assignment (L2-D01/D02, L3-D-001…019).
//!
//! Owned by slice 4A: add events here (constant + `EVENT_TYPES` entry + payload) and list
//! them in `docs/batch4-contracts.md` §4.3 in the PR.

use super::event_type;
use uuid::Uuid;

event_type!(
    /// Draft → pending_approval through the WorkflowPort (StatusChangedV1, L3-D-001).
    COURSE_OFFERING_SUBMITTED, COURSE_OFFERING_SUBMITTED_SCHEMA, "course_offering.submitted");
event_type!(
    /// (StatusChangedV1).
    COURSE_OFFERING_APPROVED, COURSE_OFFERING_APPROVED_SCHEMA, "course_offering.approved");
event_type!(
    /// Visible for enrolment planning ([`CourseOfferingV1`]).
    COURSE_OFFERING_PUBLISHED, COURSE_OFFERING_PUBLISHED_SCHEMA, "course_offering.published");
event_type!(
    /// (StatusChangedV1, L3-D-014).
    COURSE_OFFERING_ENROLLMENT_OPENED, COURSE_OFFERING_ENROLLMENT_OPENED_SCHEMA, "course_offering.enrollment_opened");
event_type!(
    /// (StatusChangedV1, L3-D-014).
    COURSE_OFFERING_ENROLLMENT_CLOSED, COURSE_OFFERING_ENROLLMENT_CLOSED_SCHEMA, "course_offering.enrollment_closed");
event_type!(
    /// Teaching starts ([`CourseOfferingV1`]); 4B/4C and Exams key on it.
    COURSE_OFFERING_ACTIVATED, COURSE_OFFERING_ACTIVATED_SCHEMA, "course_offering.activated");
event_type!(
    /// (StatusChangedV1, L3-D-002).
    COURSE_OFFERING_SUSPENDED, COURSE_OFFERING_SUSPENDED_SCHEMA, "course_offering.suspended");
event_type!(
    /// (StatusChangedV1, L3-D-003).
    COURSE_OFFERING_REACTIVATED, COURSE_OFFERING_REACTIVATED_SCHEMA, "course_offering.reactivated");
event_type!(
    /// Administrative cancellation (StatusChangedV1, L3-D-004).
    COURSE_OFFERING_CANCELLED, COURSE_OFFERING_CANCELLED_SCHEMA, "course_offering.cancelled");
event_type!(
    /// (StatusChangedV1, L3-D-013).
    COURSE_OFFERING_FROZEN, COURSE_OFFERING_FROZEN_SCHEMA, "course_offering.frozen");
event_type!(
    /// Governed unfreeze (StatusChangedV1, L3-D-013).
    COURSE_OFFERING_UNFROZEN, COURSE_OFFERING_UNFROZEN_SCHEMA, "course_offering.unfrozen");
event_type!(
    /// (StatusChangedV1).
    COURSE_OFFERING_COMPLETED, COURSE_OFFERING_COMPLETED_SCHEMA, "course_offering.completed");
event_type!(
    /// (StatusChangedV1, L3-D-010).
    COURSE_OFFERING_ARCHIVED, COURSE_OFFERING_ARCHIVED_SCHEMA, "course_offering.archived");
event_type!(
    /// Revision / replacement / rollback applied (ModifiedV1, L3-D-009).
    COURSE_OFFERING_REVISED, COURSE_OFFERING_REVISED_SCHEMA, "course_offering.revised");
event_type!(
    /// Capacity / quota governance (ModifiedV1, L3-D-005/015).
    COURSE_OFFERING_CAPACITY_CHANGED, COURSE_OFFERING_CAPACITY_CHANGED_SCHEMA, "course_offering.capacity_changed");
event_type!(
    /// Merge / split applied (ModifiedV1, L3-D-011).
    COURSE_OFFERING_REORGANISED, COURSE_OFFERING_REORGANISED_SCHEMA, "course_offering.reorganised");
event_type!(
    /// Student seated in a section ([`SectionEnrolmentV1`], L3-D-006/007).
    SECTION_ENROLMENT_ENROLLED, SECTION_ENROLMENT_ENROLLED_SCHEMA, "section_enrolment.enrolled");
event_type!(
    /// ([`SectionEnrolmentV1`]).
    SECTION_ENROLMENT_DROPPED, SECTION_ENROLMENT_DROPPED_SCHEMA, "section_enrolment.dropped");
event_type!(
    /// ([`SectionEnrolmentV1`]).
    SECTION_ENROLMENT_WITHDRAWN, SECTION_ENROLMENT_WITHDRAWN_SCHEMA, "section_enrolment.withdrawn");
event_type!(
    /// A waitlisted student was offered a seat (StatusChangedV1, L3-D-007).
    OFFERING_WAITLIST_SEAT_OFFERED, OFFERING_WAITLIST_SEAT_OFFERED_SCHEMA, "offering_waitlist.seat_offered");
event_type!(
    /// (StatusChangedV1, L3-D-017).
    FACULTY_ASSIGNMENT_SUBMITTED, FACULTY_ASSIGNMENT_SUBMITTED_SCHEMA, "faculty_assignment.submitted");
event_type!(
    /// (StatusChangedV1).
    FACULTY_ASSIGNMENT_APPROVED, FACULTY_ASSIGNMENT_APPROVED_SCHEMA, "faculty_assignment.approved");
event_type!(
    /// The faculty member now teaches the section ([`FacultyAssignmentV1`]).
    FACULTY_ASSIGNMENT_ACTIVATED, FACULTY_ASSIGNMENT_ACTIVATED_SCHEMA, "faculty_assignment.activated");
event_type!(
    /// Operational change applied (ModifiedV1, L3-D-018).
    FACULTY_ASSIGNMENT_CHANGED, FACULTY_ASSIGNMENT_CHANGED_SCHEMA, "faculty_assignment.changed");
event_type!(
    /// Completed or revoked ([`FacultyAssignmentV1`]).
    FACULTY_ASSIGNMENT_ENDED, FACULTY_ASSIGNMENT_ENDED_SCHEMA, "faculty_assignment.ended");
event_type!(
    /// Governance configuration version effective (StatusChangedV1, L3-D-019).
    FACULTY_ASSIGNMENT_POLICY_EFFECTIVE, FACULTY_ASSIGNMENT_POLICY_EFFECTIVE_SCHEMA, "faculty_assignment_policy.effective");

pub const EVENT_TYPES: &[&str] = &[
    COURSE_OFFERING_SUBMITTED,
    COURSE_OFFERING_APPROVED,
    COURSE_OFFERING_PUBLISHED,
    COURSE_OFFERING_ENROLLMENT_OPENED,
    COURSE_OFFERING_ENROLLMENT_CLOSED,
    COURSE_OFFERING_ACTIVATED,
    COURSE_OFFERING_SUSPENDED,
    COURSE_OFFERING_REACTIVATED,
    COURSE_OFFERING_CANCELLED,
    COURSE_OFFERING_FROZEN,
    COURSE_OFFERING_UNFROZEN,
    COURSE_OFFERING_COMPLETED,
    COURSE_OFFERING_ARCHIVED,
    COURSE_OFFERING_REVISED,
    COURSE_OFFERING_CAPACITY_CHANGED,
    COURSE_OFFERING_REORGANISED,
    SECTION_ENROLMENT_ENROLLED,
    SECTION_ENROLMENT_DROPPED,
    SECTION_ENROLMENT_WITHDRAWN,
    OFFERING_WAITLIST_SEAT_OFFERED,
    FACULTY_ASSIGNMENT_SUBMITTED,
    FACULTY_ASSIGNMENT_APPROVED,
    FACULTY_ASSIGNMENT_ACTIVATED,
    FACULTY_ASSIGNMENT_CHANGED,
    FACULTY_ASSIGNMENT_ENDED,
    FACULTY_ASSIGNMENT_POLICY_EFFECTIVE,
];

/// Payload of offering publication / activation.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CourseOfferingV1 {
    pub offering_id: Uuid,
    pub institution_id: Uuid,
    pub campus_id: Option<Uuid>,
    pub academic_term_id: Uuid,
    pub curriculum_component_id: Uuid,
    pub offering_code: String,
    pub title: String,
    pub credits: f64,
    pub status: String,
    pub version: i64,
}

/// Payload of section-enrolment events.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SectionEnrolmentV1 {
    pub enrolment_id: Uuid,
    pub section_id: Uuid,
    pub offering_id: Uuid,
    pub institution_id: Uuid,
    pub student_id: Uuid,
    pub student_context_id: Uuid,
    pub status: String,
    pub reason: Option<String>,
    pub student_subject_ref: Option<String>,
}

/// Payload of faculty-assignment activation / end.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FacultyAssignmentV1 {
    pub assignment_id: Uuid,
    pub section_id: Uuid,
    pub offering_id: Uuid,
    pub institution_id: Uuid,
    pub faculty_subject_ref: String,
    pub role: String,
    pub status: String,
    pub version: i64,
}
