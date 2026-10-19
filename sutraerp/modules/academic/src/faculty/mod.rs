//! **4A** — Faculty academic assignment (L2-D02, L3-D-017…019): establishment → approval → activation, operational change, governance configuration; faculty "My classes" read model.
//!
//! Owned by slice 4A (`docs/batch4-contracts.md` §1/§2). Tag `faculty-assignments`. Writes
//! `faculty_assignment` (status only via `crate::shared::lifecycle`), `faculty_assignment_change`
//! and `faculty_assignment_policy`.

pub mod assignment;
pub mod my;
pub mod policy;

use crate::shared::AcademicState;
use utoipa_axum::{router::OpenApiRouter, routes};

/// Routes of this area.
pub fn router(state: AcademicState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(assignment::create, assignment::list))
        .routes(routes!(assignment::get, assignment::update))
        .routes(routes!(assignment::submit))
        .routes(routes!(assignment::decision))
        .routes(routes!(assignment::activate))
        .routes(routes!(assignment::revoke))
        .routes(routes!(assignment::complete))
        .routes(routes!(assignment::withdraw))
        .routes(routes!(assignment::archive))
        .routes(routes!(assignment::change_create, assignment::change_list))
        .routes(routes!(assignment::change_decision))
        .routes(routes!(policy::create, policy::list))
        .routes(routes!(policy::get, policy::update))
        .routes(routes!(policy::submit))
        .routes(routes!(policy::review))
        .routes(routes!(policy::decision))
        .routes(routes!(policy::retire))
        .routes(routes!(my::classes))
        .routes(routes!(my::students))
        .with_state(state)
}
