//! **2A** — institution lifecycle (Draft → Configured → activation decision → Active →
//! Suspended/Reactivated → Archived), profile revision, business timezone, institutional policies.
//!
//! Paths: `/api/v1/institution/institutions/{id}/...` (sub-resources of the reference
//! aggregate, whose root CRUD stays in `crate::http`) and `/api/v1/institution/policies/...`.
//! operation_id prefixes: `institution_`, `institution_policy_`; tag `institution`.
//!
//! * [`domain`] — profile / timezone / decision rules (pure); the transition table is
//!   [`crate::domain::can_transition`].
//! * [`repo`] — lifecycle commands (activation through the WorkflowPort, readiness-gated submit).
//! * [`policy`] — versioned institutional policies.
//! * [`readiness`] — this slice's activation-readiness provider.
//! * `common` — helpers shared by the 2A areas (`org`, `position`).

pub(crate) mod common;
pub mod domain;
pub mod http;
pub mod policy;
pub mod policy_http;
pub mod readiness;
pub mod repo;

use crate::shared::InstitutionState;
use utoipa_axum::{router::OpenApiRouter, routes};

/// Routes of this area. Mounted by `crate::router` (already registered — do not edit lib.rs).
pub fn router(state: InstitutionState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(http::configure))
        .routes(routes!(http::submit))
        .routes(routes!(http::decide))
        .routes(routes!(http::suspend))
        .routes(routes!(http::reactivate))
        .routes(routes!(http::archive))
        .routes(routes!(http::revise_profile))
        .routes(routes!(http::list_profile_revisions))
        .routes(routes!(http::change_timezone))
        .routes(routes!(http::list_activation_requests))
        .routes(routes!(policy_http::create, policy_http::list))
        .routes(routes!(policy_http::get))
        .routes(routes!(
            policy_http::list_revisions,
            policy_http::draft_revision
        ))
        .routes(routes!(policy_http::publish))
        .with_state(state)
}
