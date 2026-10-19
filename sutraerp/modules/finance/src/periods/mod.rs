//! **5C** — Financial years and periods: establish, open, lock, close, reopen, archive (L3-F-051…055); fills `shared::period::require_open`.
//!
//! Owned by slice 5C (`docs/batch5-contracts.md` §1/§2). Tag `finance-periods`; every operation id
//! starts with the prefixes of §5.2 (enforced by `apps/api/tests/architecture.rs`).

pub mod domain;
pub mod http;

use crate::shared::FinanceState;
use utoipa_axum::{router::OpenApiRouter, routes};

/// Routes of this area.
pub fn router(state: FinanceState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(http::year_list, http::year_create))
        .routes(routes!(http::year_get))
        .routes(routes!(http::year_open))
        .routes(routes!(http::year_close))
        .routes(routes!(http::year_archive))
        .routes(routes!(http::period_list))
        .routes(routes!(http::period_get))
        .routes(routes!(http::period_open))
        .routes(routes!(http::period_lock))
        .routes(routes!(http::period_close))
        .routes(routes!(http::period_reopen))
        .routes(routes!(http::period_archive))
        .with_state(state)
}
