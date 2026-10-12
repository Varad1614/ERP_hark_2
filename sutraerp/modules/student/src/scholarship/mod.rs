//! **3C** — scholarship definitions, eligibility policy lifecycle, determination/recommendation, conditional resolution/expiry, reassessment, renewal, expiry/archival, external ecosystem sync (ExternalScholarshipPort, no-op), bulk recalculation, hand-off to financial aid (event only) (L3-C-062…071).
//!
//! Paths: `/api/v1/student/institutions/{id}/scholarships`, `/api/v1/student/scholarships/{id}[/…]`, `/api/v1/student/scholarship-policies/{id}[/…]`, `/api/v1/student/students/{id}/scholarship-eligibilities`, `/api/v1/student/scholarship-eligibilities/{id}[/…]`, `/api/v1/student/institutions/{id}/scholarship-recalculations`.
//! operation_id prefixes: `scholarship_`, `scholarship_policy_`, `scholarship_eligibility_`, `scholarship_recalculation_`. Tag: `scholarships`.
//!
//! Owned by slice 3C (see `docs/batch3-contracts.md`). Add `domain.rs`, `repo.rs`, `http.rs`
//! (or finer submodules) inside this directory only. Handlers take
//! [`StudentState`](crate::shared::StudentState); every `#[utoipa::path]` sets an explicit
//! `operation_id` with this slice's prefix and `tag = "scholarships"`. The first path parameter after a
//! collection segment is always named `{id}` (contract doc §5.2).

pub mod domain;
pub mod http;
pub mod repo;

use crate::shared::StudentState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area. Mounted by `crate::router` (already registered — do not edit lib.rs).
pub fn router(state: StudentState) -> OpenApiRouter {
    http::router(state)
}
