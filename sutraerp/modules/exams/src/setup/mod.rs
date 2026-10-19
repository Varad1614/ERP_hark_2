//! **6A** — Examination policy (versions, activation, revision / supersession, retirement — L3-I-001…003) and sessions (creation → review → approval → scheduling → activation, governed revision, suspension / reopening, duplicate resolution — L3-I-004…006) with their papers (one per offering / curriculum component; credits, mode, maximum and pass marks, internal weight).
//!
//! Tag `exam-setup`; operation id prefixes `exam_policy_`, `exam_session_`, `exam_paper_` (contract §5.2). The router mount point is
//! already registered in `crate::router_with_state`; 6A fills in this `router` (and its own
//! files under this folder) and never edits `lib.rs`.
//!
//! `common` and `domain` are shared by the three 6A areas (`setup`, `scheduling`,
//! `registration`).

pub mod common;
pub mod domain;
pub mod policy;
pub mod session;

use crate::shared::ExamsState;
use policy::*;
use session::*;
use utoipa_axum::{router::OpenApiRouter, routes};

/// Routes of this area.
pub fn router(state: ExamsState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(policy_list, policy_create))
        .routes(routes!(policy_get, policy_update))
        .routes(routes!(policy_submit))
        .routes(routes!(policy_decision))
        .routes(routes!(policy_activate))
        .routes(routes!(policy_supersede))
        .routes(routes!(policy_retire))
        .routes(routes!(session_list, session_create))
        .routes(routes!(session_get, session_update))
        .routes(routes!(session_submit))
        .routes(routes!(session_decision))
        .routes(routes!(session_schedule))
        .routes(routes!(session_activate))
        .routes(routes!(session_suspend))
        .routes(routes!(session_reopen))
        .routes(routes!(session_merge))
        .routes(routes!(paper_list, paper_create))
        .routes(routes!(paper_generate))
        .routes(routes!(paper_get, paper_update))
        .routes(routes!(paper_schedule))
        .routes(routes!(paper_ready))
        .routes(routes!(paper_conducted))
        .routes(routes!(paper_cancel))
        .with_state(state)
}
