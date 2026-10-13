//! **5A** — Fee heads, fee structures, charge generation → validation → publication, revision, cancel / suspend / resume, disputes, instalments and due schedules, settlement closure and archival (L3-F-001…006).
//!
//! Owned by slice 5A (`docs/batch5-contracts.md` §1/§2). Tag `finance-charges`; every operation id
//! starts with the prefixes of §5.2 (enforced by `apps/api/tests/architecture.rs`).
//!
//! * [`kit`] — request parsing, decisions, status events (shared by the 5A areas).
//! * [`domain`] — pure transition / schedule rules.
//! * [`fees`] — fee heads and versioned fee structures + charge generation.
//! * [`repo`] — charges: create, validate, publish, revise, lifecycle, instalments, disputes.
//! * [`http`] — the routes.

pub mod domain;
pub mod fees;
pub mod http;
pub mod kit;
pub mod repo;

use crate::shared::FinanceState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: FinanceState) -> OpenApiRouter {
    http::router(state)
}
