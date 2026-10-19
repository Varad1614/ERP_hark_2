//! **6B** — marks collection, evaluation, gradebook, moderation, malpractice basics (L2-I08, I14; L3-I-071…082, 162…171 in scope).
//!
//! Owned by slice 6B: add events here (constant + `EVENT_TYPES` entry + payload) and list
//! them in `docs/batch6-contracts.md` §4.3 in the PR. Payload: [`super::StatusChangedV1`] unless
//! noted.

use super::event_type;

event_type!(
    /// Evaluator submitted a paper's marks (L3-I-071).
    MARKS_SUBMITTED, MARKS_SUBMITTED_SCHEMA, "mark_sheet.submitted");
event_type!(
    /// Exam cell verified a paper's marks (L3-I-072).
    MARKS_VERIFIED, MARKS_VERIFIED_SCHEMA, "mark_sheet.verified");
event_type!(
    /// Approved moderation applied to a paper (L3-I-075).
    MARKS_MODERATED, MARKS_MODERATED_SCHEMA, "moderation.applied");
event_type!(
    /// Final approved marks established for a paper (L3-I-072).
    MARKS_APPROVED, MARKS_APPROVED_SCHEMA, "mark_sheet.approved");
event_type!(
    /// Final approved marks of a paper's mark sheet finalised (6B addition, L3-I-072).
    MARKS_FINALISED, MARKS_FINALISED_SCHEMA, "mark_sheet.finalised");
event_type!(
    /// A candidate's marks withheld (L3-I-076).
    MARKS_WITHHELD, MARKS_WITHHELD_SCHEMA, "paper_mark.withheld");
event_type!(
    /// Policy requires a second / adjudicating evaluation (L3-I-080…082).
    SECOND_EVALUATION_REQUIRED, SECOND_EVALUATION_REQUIRED_SCHEMA, "evaluation.second_required");
event_type!(
    /// Suspected malpractice reported (L3-I-170).
    MALPRACTICE_REPORTED, MALPRACTICE_REPORTED_SCHEMA, "malpractice_case.reported");
event_type!(
    /// Committee decided a malpractice case (penalty determination, L3-I-170).
    MALPRACTICE_DECIDED, MALPRACTICE_DECIDED_SCHEMA, "malpractice_case.decided");
event_type!(
    /// Malpractice decision upheld / modified / overturned on review (L3-I-163…167).
    MALPRACTICE_REVIEWED, MALPRACTICE_REVIEWED_SCHEMA, "malpractice_case.reviewed");
event_type!(
    /// Malpractice case closed (L3-I-171).
    MALPRACTICE_CLOSED, MALPRACTICE_CLOSED_SCHEMA, "malpractice_case.closed");

/// Event types of this slice.
pub const EVENT_TYPES: &[&str] = &[
    MARKS_SUBMITTED,
    MARKS_VERIFIED,
    MARKS_MODERATED,
    MARKS_APPROVED,
    MARKS_FINALISED,
    MARKS_WITHHELD,
    SECOND_EVALUATION_REQUIRED,
    MALPRACTICE_REPORTED,
    MALPRACTICE_DECIDED,
    MALPRACTICE_REVIEWED,
    MALPRACTICE_CLOSED,
];
