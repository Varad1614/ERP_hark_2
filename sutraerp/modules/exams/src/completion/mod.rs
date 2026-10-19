//! **6C** — Academic completion evaluation over current published results (CGPA, credits earned vs curriculum; re-evaluation after a revised result — L3-I-145, 150), governed completion evidence (L3-I-153), session closure and finalisation (`closing → closed`, L3-I-187) and archival (L3-I-192).
//!
//! Tag `exam-completion`; operation id prefixes `exam_completion_`, `exam_closure_` (contract §5.2). The router mount point is
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
