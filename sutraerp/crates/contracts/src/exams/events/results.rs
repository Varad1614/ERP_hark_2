//! **6C** — result computation, verification, publication, grade cards, revaluation, supplementary, completion, session closure (L2-I09…I13, I16).
//!
//! Owned by slice 6C: add events here (constant + `EVENT_TYPES` entry + payload) and list
//! them in `docs/batch6-contracts.md` §4.3 in the PR. Payload: [`super::StatusChangedV1`] unless
//! noted.

use super::event_type;

event_type!(
    /// Result computation run completed for a session (L3-I-083).
    RESULT_COMPUTED, RESULT_COMPUTED_SCHEMA, "result_batch.computed");
event_type!(
    /// A candidate's result withheld (L3-I-085/097/102).
    RESULT_WITHHELD, RESULT_WITHHELD_SCHEMA, "term_result.withheld");
event_type!(
    /// Results institutionally approved (L3-I-083).
    RESULT_APPROVED, RESULT_APPROVED_SCHEMA, "result_batch.approved");
event_type!(
    /// Result published to the candidate ([`super::ResultPublishedV1`], L3-I-101).
    RESULT_PUBLISHED, RESULT_PUBLISHED_SCHEMA, "term_result.published");
event_type!(
    /// Published result withdrawn (L3-I-109).
    RESULT_WITHDRAWN, RESULT_WITHDRAWN_SCHEMA, "term_result.withdrawn");
event_type!(
    /// Published result replaced by a revision ([`super::ResultPublishedV1`], L3-I-107/108/111).
    RESULT_REVISED, RESULT_REVISED_SCHEMA, "term_result.revised");
event_type!(
    /// Publication delayed for a scope (L3-I-104).
    PUBLICATION_DELAYED, PUBLICATION_DELAYED_SCHEMA, "publication.delayed");
event_type!(
    /// Grade card issued / reissued (L3-I-101).
    GRADE_CARD_ISSUED, GRADE_CARD_ISSUED_SCHEMA, "grade_card.issued");
event_type!(
    /// Revaluation / rechecking requested (L3-I-118/119).
    REVALUATION_REQUESTED, REVALUATION_REQUESTED_SCHEMA, "revaluation_request.submitted");
event_type!(
    /// Revaluation request approved / rejected (L3-I-118).
    REVALUATION_DECIDED, REVALUATION_DECIDED_SCHEMA, "revaluation_request.decided");
event_type!(
    /// Review outcome verified and revised marks approved (L3-I-118/119).
    REVALUATION_COMPLETED, REVALUATION_COMPLETED_SCHEMA, "revaluation_request.completed");
event_type!(
    /// Subsequent-attempt eligibility determined (L3-I-133/142).
    SUPPLEMENTARY_ELIGIBILITY_DETERMINED, SUPPLEMENTARY_ELIGIBILITY_DETERMINED_SCHEMA, "supplementary_eligibility.determined");
event_type!(
    /// Academic completion evaluated (CGPA / credits, L3-I-145).
    COMPLETION_EVALUATED, COMPLETION_EVALUATED_SCHEMA, "completion_evaluation.evaluated");
event_type!(
    /// Session closed and finalised (L3-I-187).
    EXAM_SESSION_CLOSED, EXAM_SESSION_CLOSED_SCHEMA, "exam_session.closed");

/// Event types of this slice.
pub const EVENT_TYPES: &[&str] = &[
    RESULT_COMPUTED,
    RESULT_WITHHELD,
    RESULT_APPROVED,
    RESULT_PUBLISHED,
    RESULT_WITHDRAWN,
    RESULT_REVISED,
    PUBLICATION_DELAYED,
    GRADE_CARD_ISSUED,
    REVALUATION_REQUESTED,
    REVALUATION_DECIDED,
    REVALUATION_COMPLETED,
    SUPPLEMENTARY_ELIGIBILITY_DETERMINED,
    COMPLETION_EVALUATED,
    EXAM_SESSION_CLOSED,
];
