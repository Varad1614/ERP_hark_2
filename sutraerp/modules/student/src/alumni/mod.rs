//! **3C** — student → alumni transition, correction and archival (L2-C08 + boundary
//! clarification 2026-08-26, L3-C-072…074).
//!
//! Paths: `/api/v1/student/students/{id}/alumni-transitions`, `/api/v1/student/alumni-transitions/{id}[/…]`.
//! operation_id prefix: `alumni_transition_`. Tag: `alumni`.
//!
//! `created → pending_verification → verified → pending_approval → approved → transitioning →
//! activated → completed → archived` (L2-C08 §12.4; `rejected` from the approval). Eligibility:
//! a terminal StudentContext that is `qualification_completed` with a current (not revoked)
//! conferred qualification, or `withdrawn` / `transferred` with a completed withdrawal /
//! transfer exit ("eligible exit"). Completion archives the persona when no live context
//! remains (contract §4.2). The H06 Alumni Profile is downstream (`alumni_transition.completed`).

pub mod domain;
pub mod http;
pub mod repo;

use crate::shared::StudentState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area. Mounted by `crate::router` (already registered — do not edit lib.rs).
pub fn router(state: StudentState) -> OpenApiRouter {
    http::router(state)
}
