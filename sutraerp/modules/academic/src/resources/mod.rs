//! **4C** — Course resources and library links: faculty Resources (own sections via `academic.teach`) and the student Library links view.
//!
//! Owned by slice 4C (`docs/batch4-contracts.md` §1/§2). Tag `resources`. Library catalogue
//! integration (Resources J, Batch 8) is out of scope: links only.

pub mod http;
pub mod repo;

use crate::shared::AcademicState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: AcademicState) -> OpenApiRouter {
    http::router(state)
}
