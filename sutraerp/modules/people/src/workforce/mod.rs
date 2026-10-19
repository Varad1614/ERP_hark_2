//! **7A** — Staff records and engagements: creation, verification, activation (`ENGAGEMENT_ACTIVATION`), modification, transfer, suspension / restoration, closure, rejoining, duplicates → canonical (L3-E-001…004, 007…009, 058, 059, 065, 066, 068); faculty establishment and teaching eligibility (L3-E-026, 029, 031).
//!
//! Tag `people-workforce`; operation id prefixes `people_staff_`, `people_engagement_`,
//! `people_reporting_`, `people_faculty_` (contract §5.2).
//!
//! * [`common`] — 7A request bodies, validation, event / audit helpers shared by `positions`,
//!   `hr` and `profile`.
//! * [`staff`] — staff records (create, `PUT /staff/{id}`, verification, activation, suspension,
//!   archive, corrections, merge).
//! * [`engagement`] — engagements, governed changes, closure, rejoining, reporting manager.
//! * [`faculty`] — faculty profile and teaching eligibility.

pub mod common;
pub mod engagement;
pub mod faculty;
pub mod staff;

use crate::shared::PeopleState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .merge(staff::router(state.clone()))
        .merge(engagement::router(state.clone()))
        .merge(faculty::router(state))
}
