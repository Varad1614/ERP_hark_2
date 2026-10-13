//! **Foundation** — grading schemes (grade bands → grade points) of an institution, shared by
//! 6A (session binding), 6B (gradebook preview) and 6C (result computation). Tag
//! `exam-grading`, operation ids `exam_grading_*`. Schemes are immutable once created (bands
//! never change; a new scheme is created and the old one retired), so results computed under a
//! scheme stay reproducible.

pub mod http;

use crate::shared::ExamsState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: ExamsState) -> OpenApiRouter {
    http::router(state)
}
