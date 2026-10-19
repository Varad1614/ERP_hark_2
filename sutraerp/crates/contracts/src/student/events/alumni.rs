//! **3C** — student → alumni transition, correction, archival (L2-C08, L3-C-072…074).
//!
//! Owned by slice 3C: add events here (constant + `EVENT_TYPES` entry + payload) and list
//! them in `docs/batch3-contracts.md` §4.3 in the PR.

use super::event_type;

event_type!(
    /// AlumniTransitionInitiated (StatusChangedV1).
    ALUMNI_TRANSITION_INITIATED, ALUMNI_TRANSITION_INITIATED_SCHEMA, "alumni_transition.initiated");
event_type!(
    /// TransitionEligibilityValidated (StatusChangedV1).
    ALUMNI_TRANSITION_ELIGIBILITY_VALIDATED, ALUMNI_TRANSITION_ELIGIBILITY_VALIDATED_SCHEMA, "alumni_transition.eligibility_validated");
event_type!(
    /// TransitionPolicyValidated (StatusChangedV1).
    ALUMNI_TRANSITION_POLICY_VALIDATED, ALUMNI_TRANSITION_POLICY_VALIDATED_SCHEMA, "alumni_transition.policy_validated");
event_type!(
    /// AlumniTransitionApproved (StatusChangedV1).
    ALUMNI_TRANSITION_APPROVED, ALUMNI_TRANSITION_APPROVED_SCHEMA, "alumni_transition.approved");
event_type!(
    /// StudentRelationshipClosed (StatusChangedV1).
    ALUMNI_TRANSITION_STUDENT_RELATIONSHIP_CLOSED, ALUMNI_TRANSITION_STUDENT_RELATIONSHIP_CLOSED_SCHEMA, "alumni_transition.student_relationship_closed");
event_type!(
    /// AlumniRelationshipActivated (StatusChangedV1).
    ALUMNI_TRANSITION_ALUMNI_RELATIONSHIP_ACTIVATED, ALUMNI_TRANSITION_ALUMNI_RELATIONSHIP_ACTIVATED_SCHEMA, "alumni_transition.alumni_relationship_activated");
event_type!(
    /// AlumniTransitionCompleted (StatusChangedV1).
    ALUMNI_TRANSITION_COMPLETED, ALUMNI_TRANSITION_COMPLETED_SCHEMA, "alumni_transition.completed");
event_type!(
    /// (ModifiedV1, L3-C-073).
    ALUMNI_TRANSITION_CORRECTED, ALUMNI_TRANSITION_CORRECTED_SCHEMA, "alumni_transition.corrected");
event_type!(
    /// AlumniTransitionArchived (StatusChangedV1, L3-C-074).
    ALUMNI_TRANSITION_ARCHIVED, ALUMNI_TRANSITION_ARCHIVED_SCHEMA, "alumni_transition.archived");

pub const EVENT_TYPES: &[&str] = &[
    ALUMNI_TRANSITION_INITIATED,
    ALUMNI_TRANSITION_ELIGIBILITY_VALIDATED,
    ALUMNI_TRANSITION_POLICY_VALIDATED,
    ALUMNI_TRANSITION_APPROVED,
    ALUMNI_TRANSITION_STUDENT_RELATIONSHIP_CLOSED,
    ALUMNI_TRANSITION_ALUMNI_RELATIONSHIP_ACTIVATED,
    ALUMNI_TRANSITION_COMPLETED,
    ALUMNI_TRANSITION_CORRECTED,
    ALUMNI_TRANSITION_ARCHIVED,
];
