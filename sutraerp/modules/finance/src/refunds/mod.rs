//! **5B** — Refund request → approval → completion, cancellation, failure / retry, reversal / recovery, closure (L3-F-014…019).
//!
//! Owned by slice 5B (`docs/batch5-contracts.md` §1/§2). Tag `finance-refunds`; every operation id
//! starts with the prefixes of §5.2 (enforced by `apps/api/tests/architecture.rs`).
//! Refund approval is **the reference approval of Batch 5** (§9): see [`http`].

pub mod domain;
pub mod http;

use crate::shared::FinanceState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: FinanceState) -> OpenApiRouter {
    http::router(state)
}
