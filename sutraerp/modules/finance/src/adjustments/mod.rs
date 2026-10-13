//! **5A** — Credit notes, debit notes, write-off adjustments (approval through the WorkflowPort), posted adjustment reversal, corrected successor records (L3-F-020…024).
//!
//! Owned by slice 5A (`docs/batch5-contracts.md` §1/§2). Tag `finance-adjustments`; every
//! operation id starts with `finance_adjustment_` (enforced by `apps/api/tests/architecture.rs`).

pub mod domain;
pub mod http;
pub mod repo;

use crate::shared::FinanceState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: FinanceState) -> OpenApiRouter {
    http::router(state)
}
