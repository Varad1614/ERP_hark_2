//! **2A** — effective-dated position definitions (establish / activate / change / retire /
//! archive). A definition is never a person or an appointment (Section E owns assignments).
//!
//! Paths: `/api/v1/institution/institutions/{id}/positions`, `/api/v1/institution/positions/...`.
//! operation_id prefix: `position_`; tag `organisation`.

pub mod domain;
pub mod http;
pub mod repo;

use crate::shared::InstitutionState;
use utoipa_axum::{router::OpenApiRouter, routes};

/// Routes of this area. Mounted by `crate::router` (already registered — do not edit lib.rs).
pub fn router(state: InstitutionState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(http::establish, http::list))
        .routes(routes!(http::get, http::update_draft))
        .routes(routes!(http::list_revisions))
        .routes(routes!(http::activate))
        .routes(routes!(http::change))
        .routes(routes!(http::retire))
        .routes(routes!(http::archive))
        .with_state(state)
}
