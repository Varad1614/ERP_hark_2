//! **6C** — Revaluation / rechecking requests (submission with the Finance fee through `ExamFeePort` and settlement read through `ChargeDirectory`, review, outcome verification, revised marks via `shared::marks::revise_final`, withdrawal, duplicates, conflicting requests, finalised correction — L3-I-118…121, 128…130, 132) and subsequent attempts (eligibility and attempt authorisation, opportunity → supplementary session, duplicate attempts, best-result position, exhausted attempts — L3-I-133, 134, 138, 139, 142).
//!
//! Tag `exam-revaluation`; operation id prefixes `exam_revaluation_` (incl. `exam_revaluation_self_`), `exam_supplementary_` (contract §5.2). The router mount point is
//! already registered in `crate::router_with_state`; 6C fills in this `router` (and its own
//! files under this folder) and never edits `lib.rs`.

pub mod domain;
pub mod http;
pub mod repo;

use crate::shared::ExamsState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: ExamsState) -> OpenApiRouter {
    http::router(state)
}
