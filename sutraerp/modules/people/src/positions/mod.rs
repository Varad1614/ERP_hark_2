//! **7A** — Institutional positions per org unit (draft → active → frozen → retired), assignments of an engagement to a position (`POSITION_ASSIGNMENT`), change / reassignment and completion / termination (L3-E-011, 012, 013). Reporting managers live on the engagement (`workforce::engagement`, L3-E-018).
//!
//! Tag `people-workforce`; operation id prefixes `people_position_` (+ `people_reporting_` in
//! `workforce`) (contract §5.2).

pub mod http;

use crate::shared::PeopleState;
use utoipa_axum::router::OpenApiRouter;

pub use http::end_for_engagement;

/// Routes of this area.
pub fn router(state: PeopleState) -> OpenApiRouter {
    http::router(state)
}
