//! **2A** — organisation units tree (register / activate / modify / restructure / suspend /
//! restore / decommission / archive), organisation type blueprints, governance configuration.
//!
//! Paths: `/api/v1/institution/institutions/{id}/org-units`, `/api/v1/institution/org-units/...`,
//! `/api/v1/institution/org-type-blueprints/...`, `/api/v1/institution/governance-configs/...`,
//! `/api/v1/institution/org-restructures/...`.
//! operation_id prefixes: `org_unit_`, `org_type_blueprint_`, `governance_config_`,
//! `org_restructure_`; tag `organisation`.
//!
//! * [`domain`] — pure rules (unit transitions, blueprint validation, cycle detection).
//! * [`blueprint`] — versioned type blueprints (publication through the WorkflowPort).
//! * [`unit`] — the unit tree, history, type migration.
//! * [`governance`] — effective-dated governance configuration per unit.
//! * [`restructure`] — approved move / merge transformations.

pub mod blueprint;
pub mod domain;
pub mod governance;
pub mod restructure;
pub mod unit;

use crate::shared::InstitutionState;
use utoipa_axum::{router::OpenApiRouter, routes};

/// Routes of this area. Mounted by `crate::router` (already registered — do not edit lib.rs).
pub fn router(state: InstitutionState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(blueprint::http_create, blueprint::http_list))
        .routes(routes!(blueprint::http_get, blueprint::http_update))
        .routes(routes!(blueprint::http_publish))
        .routes(routes!(blueprint::http_decide))
        .routes(routes!(unit::http_register, unit::http_list))
        .routes(routes!(unit::http_get, unit::http_modify))
        .routes(routes!(unit::http_activate))
        .routes(routes!(unit::http_suspend))
        .routes(routes!(unit::http_restore))
        .routes(routes!(unit::http_decommission))
        .routes(routes!(unit::http_archive))
        .routes(routes!(unit::http_migrate_type))
        .routes(routes!(unit::http_history))
        .routes(routes!(governance::http_create, governance::http_list))
        .routes(routes!(governance::http_effective))
        .routes(routes!(governance::http_get, governance::http_update))
        .routes(routes!(governance::http_activate))
        .routes(routes!(restructure::http_request, restructure::http_list))
        .routes(routes!(restructure::http_get))
        .routes(routes!(restructure::http_decide))
        .with_state(state)
}
