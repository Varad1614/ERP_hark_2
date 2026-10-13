//! **4B** — Academic monitoring (L2-D05 + purity amendment, L3-D-027…033): requirement → published outcome, risk assessment, governance exceptions, interventions from approved decisions, governance review, certification → closure, reopening as a new cycle.
//!
//! Owned by slice 4B (`docs/batch4-contracts.md` §1/§2). Tag `monitoring`. Evidence (finalised
//! attendance) is read, never re-owned; repeated authoritative facts (`sourceKey`) never create a
//! second outcome.

pub mod domain;
pub mod http;
pub mod repo;

use crate::shared::AcademicState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: AcademicState) -> OpenApiRouter {
    http::router(state)
}
