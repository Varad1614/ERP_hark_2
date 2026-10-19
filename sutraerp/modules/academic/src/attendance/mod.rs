//! **4B** — Teaching sessions, attendance and syllabus coverage (L2-D04, L3-D-023…026): session from a timetable slot → outcome; attendance capture → finalisation; governed correction; attendance policy + threshold evaluation (`attendance.threshold_breached`); syllabus coverage → published record.
//!
//! Owned by slice 4B (`docs/batch4-contracts.md` §1/§2). Tag `attendance`. Rosters are the
//! seated roster at capture time (`AcademicState::offerings`); faculty act only on sections they
//! actively teach (`shared::faculty::require_teaching`).

pub mod domain;
pub mod http;
pub mod repo;
pub mod syllabus;

use crate::shared::AcademicState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: AcademicState) -> OpenApiRouter {
    http::router(state)
}
