//! **6C** — Result computation engine (final marks → graded lines → SGPA / CGPA through `shared::grading`; withheld conditions and their resolution, recompute before approval, correction, moderation / grace, malpractice consequence, duplicates — L3-I-083, 085…090, 093, 095, 097…100), verification and approval, publication (publish, withhold / release, delay / resolution, cancellation, post-publication change, republication, withdrawal / restoration, revaluation / malpractice outcomes, absence consequence, failure recovery, discrepancy correction — L3-I-101…112, 114, 116, 117) and grade cards — all through `shared::results`.
//!
//! Tag `exam-results`; operation id prefixes `exam_result_` (incl. `exam_result_self_`), `exam_publication_`, `exam_grade_card_` (incl. `exam_grade_card_self_`) (contract §5.2). The router mount point is
//! already registered in `crate::router_with_state`; 6C fills in this `router` (and its own
//! files under this folder) and never edits `lib.rs`.

pub mod common;
pub mod domain;
pub mod http;
pub mod repo;
pub mod views;

use crate::shared::ExamsState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: ExamsState) -> OpenApiRouter {
    http::router(state)
}
