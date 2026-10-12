//! **2C** — academic calendar (L2-B05; L3/L4-B-027 … B-033): calendars and their versions with
//! terms and key dates, academic years (the open academic context), scope assignments,
//! successor revisions, temporary exceptions, year closure into the next-cycle foundation,
//! side-effect-free change simulation and retirement with historical preservation.
//!
//! Paths: `/api/v1/institution/institutions/{id}/calendars|academic-years|calendar-assignments|calendar-resolution`,
//! `/api/v1/institution/calendars/...`, `/api/v1/institution/calendar-versions/...`,
//! `/api/v1/institution/academic-years/...`, `/api/v1/institution/calendar-assignments/...`,
//! `/api/v1/institution/calendar-exceptions/...`, `/api/v1/institution/calendar-simulations/...`.
//! operation_id prefixes: `calendar_`, `academic_year_`, `calendar_assignment_`,
//! `calendar_exception_`, `calendar_simulation_`; tag `calendar`.
//!
//! Business dates are interpreted in the institution business timezone; a published version
//! snapshots it (`timezone`).

pub mod domain;
pub mod http;
pub mod readiness;
pub mod repo;

use crate::shared::InstitutionState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area. Mounted by `crate::router` (already registered — do not edit lib.rs).
pub fn router(state: InstitutionState) -> OpenApiRouter {
    http::router(state)
}
