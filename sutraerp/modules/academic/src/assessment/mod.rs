//! **4B** — Internal assessment and coursework (L2-D07, L3-D-051…057): blueprint → assessment window / assignment → student submission → moderation → finalised internal assessment (`internal_assessment.finalized`, consumed by Exams) → archive; governed correction.
//!
//! Owned by slice 4B (`docs/batch4-contracts.md` §1/§2). Tag `assessment`. Coursework is an
//! assessment window of kind `assignment`; students act through `/me/assignments` (resolved from
//! the session, never a client student id).

pub mod domain;
pub mod http;
pub mod repo;

use crate::shared::AcademicState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: AcademicState) -> OpenApiRouter {
    http::router(state)
}
