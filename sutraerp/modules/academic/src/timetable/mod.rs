//! **4B** — Timetable (L2-D03, L3-D-020…022): scheduling requirement → generated draft → validation → approval → publication → operational timetable; governed revision; retirement. Student and faculty timetable views.
//!
//! Owned by slice 4B (`docs/batch4-contracts.md` §1/§2). Tag `timetable`. Rooms through
//! `AcademicState::spaces` (stub: free-text room label). [`kit`] is the 4B-wide persistence and
//! HTTP kit the other 4B areas (`attendance`, `monitoring`, `assessment`) reuse.

pub mod domain;
pub mod http;
pub mod kit;
pub mod repo;

use crate::shared::AcademicState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: AcademicState) -> OpenApiRouter {
    http::router(state)
}
