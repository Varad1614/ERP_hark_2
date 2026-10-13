//! **5B** — Payments → receipt + accounting, partial / multi allocation, unidentified receipts, failure / timeout recovery, reversal / chargeback, cancellation, bulk collection (L3-F-007…013); the payment gateway adapter (`gateway.rs`).
//!
//! Owned by slice 5B (`docs/batch5-contracts.md` §1/§2). Tag `finance-payments`; every operation id
//! starts with the prefixes of §5.2 (enforced by `apps/api/tests/architecture.rs`).
//!
//! * [`domain`] — pure state machines + the allocation planner (unit-tested).
//! * [`service`] — the transactional core (completion, allocation, failure, reversal).
//! * [`http`] — payments, receipts, self-service checkout, the gateway webhook.
//! * [`unidentified`] — unidentified / unallocated receipts (F-009).
//! * [`collections`] — bulk collection (F-013).
//! * [`common`] — helpers shared with `refunds` and `receivables`.

pub mod collections;
pub mod common;
pub mod domain;
pub mod gateway;
pub mod http;
pub mod service;
pub mod unidentified;
pub mod views;

use crate::shared::FinanceState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: FinanceState) -> OpenApiRouter {
    OpenApiRouter::new()
        .merge(http::router(state.clone()))
        .merge(unidentified::router(state.clone()))
        .merge(collections::router(state))
}
