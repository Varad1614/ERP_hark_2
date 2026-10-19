//! **5A** — Financial aid: approved Domain C scholarship decision (`AidDecisionSource`) → allocation → disbursement / charge adjustment, renewal, revocation → recovery charge, programme reconciliation (L3-F-062…066).
//!
//! Owned by slice 5A (`docs/batch5-contracts.md` §1/§2). Tag `financial-aid`; every operation id
//! starts with `finance_aid_` (enforced by `apps/api/tests/architecture.rs`).

pub mod domain;
pub mod http;
pub mod repo;

use crate::shared::FinanceState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: FinanceState) -> OpenApiRouter {
    http::router(state)
}
