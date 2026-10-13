//! **6B** — Mark sheets per paper (evaluation lifecycle), internal assessment import from Domain D (`AcademicExamFacts::internal_results`), marks entry / submission by faculty on sections they teach (`OfferingDirectory::teaches`) and administrators, verification and approval of final marks (the reference approval of Batch 6), panel / second / adjudicating evaluation, marks submission exceptions, withheld marks, supplementary marks, moderation under policy, gradebook preview (`shared::grading`) — L3-I-071…077, 080…082 — all through `shared::marks`.
//!
//! Tag `exam-marks`; operation id prefixes `exam_mark_sheet_`, `exam_marks_` (incl. `exam_marks_self_`), `exam_evaluation_`, `exam_moderation_`, `exam_gradebook_` (contract §5.2). The router mount point is
//! already registered in `crate::router_with_state`; 6B fills in this `router` (and its own
//! files under this folder) and never edits `lib.rs`.

pub mod domain;
pub mod http;
pub mod repo;

use crate::shared::ExamsState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area (contract §5.2, tag `exam-marks`).
pub fn router(state: ExamsState) -> OpenApiRouter {
    http::router(state)
}
