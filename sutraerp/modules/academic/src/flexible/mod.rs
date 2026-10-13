//! **4C** — Flexible and external learning (L2-D06, L3-D-034…038, 040…042, 044, 045, 047, 048, 050): providers, opportunities, demand → seat allocation → waitlist, external registration, completion, policy configuration, session archival, allocation conflicts, exception cases.
//!
//! Owned by slice 4C (`docs/batch4-contracts.md` §1/§2). Tag `flexible-learning`. Offerings are
//! read only through `AcademicState::offerings`; students through `AcademicState::students`;
//! nothing in Domain C is written here (credit posting lives in `recognition`).

pub mod domain;
pub mod enrolment;
pub mod exception;
pub mod http;
pub mod kit;
pub mod opportunity;
pub mod policy;
pub mod provider;

use crate::shared::AcademicState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: AcademicState) -> OpenApiRouter {
    http::router(state)
}
