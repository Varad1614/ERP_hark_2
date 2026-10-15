//! **9E** — accreditation (Domain H09; L3-H-061…071; `docs/batch9-contracts.md` §12.2).
//!
//! Owned by slice 9E. Payload: [`super::StatusChangedV1`].

use super::event_type;

event_type!(
    /// An accreditation framework version became active (H-061).
    ACCREDITATION_FRAMEWORK_ACTIVATED, ACCREDITATION_FRAMEWORK_ACTIVATED_SCHEMA, "accreditation_framework.activated");
event_type!(
    /// The assessment plan of an accreditation cycle was approved (H-062).
    ACCREDITATION_PLAN_APPROVED, ACCREDITATION_PLAN_APPROVED_SCHEMA, "accreditation_cycle.plan_approved");
event_type!(
    /// An accreditation record (obligation, evidence, audit, assessment, finding, improvement,
    /// readiness, compliance review, self-study) was decided (H-063…071).
    ACCREDITATION_RECORD_DECIDED, ACCREDITATION_RECORD_DECIDED_SCHEMA, "accreditation_record.decided");
event_type!(
    /// An approved self-study report was handed off to the accrediting body (H-071).
    SELF_STUDY_HANDED_OFF, SELF_STUDY_HANDED_OFF_SCHEMA, "accreditation_record.handed_off");

/// Event types of this file (keep in sync; `all_event_types` collects them).
pub const EVENT_TYPES: &[&str] = &[
    ACCREDITATION_FRAMEWORK_ACTIVATED,
    ACCREDITATION_PLAN_APPROVED,
    ACCREDITATION_RECORD_DECIDED,
    SELF_STUDY_HANDED_OFF,
];
