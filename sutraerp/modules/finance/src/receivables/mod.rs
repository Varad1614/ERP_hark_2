//! **5B** — Accounts receivable recognition, dunning, settlement, write-off decision, reopening (L3-F-039…043).
//!
//! Owned by slice 5B (`docs/batch5-contracts.md` §1/§2). Tag `finance-receivables`; every operation id
//! starts with the prefixes of §5.2 (enforced by `apps/api/tests/architecture.rs`).

pub mod domain;
pub mod http;

use crate::shared::FinanceState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: FinanceState) -> OpenApiRouter {
    http::router(state)
}
