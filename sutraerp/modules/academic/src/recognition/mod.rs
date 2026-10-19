//! **4C** — Certificates, credit mapping and recognition (L3-D-039, 043, 046, 049): certificate verification, credit mapping rules, credit recognition → posting into the Domain C ledger through `AcademicState::credits` (`CreditPostingPort`), reconciliation.
//!
//! Owned by slice 4C (`docs/batch4-contracts.md` §1/§2). Tag `credit-recognition`. The
//! Domain C ledger is written only through `CreditPostingPort`, never by SQL.

pub mod certificate;
pub mod credit;
pub mod http;
pub mod mapping;
pub mod reconciliation;

use crate::shared::AcademicState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: AcademicState) -> OpenApiRouter {
    http::router(state)
}
