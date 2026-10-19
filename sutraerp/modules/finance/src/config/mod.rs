//! **5A** — Governed finance configuration and financial rule versions (L3-F-084/085).
//!
//! Owned by slice 5A (`docs/batch5-contracts.md` §1/§2). Tag `finance-config`; every operation id
//! starts with the prefixes of §5.2 (enforced by `apps/api/tests/architecture.rs`).

pub mod http;
pub mod repo;

use crate::shared::FinanceState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: FinanceState) -> OpenApiRouter {
    http::router(state)
}
