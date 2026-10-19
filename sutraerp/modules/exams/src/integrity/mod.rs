//! **6B** — Malpractice basics: suspected malpractice → investigation → committee decision with penalty (L3-I-170), appeal review (uphold / modify / overturn, restored outcome — L3-I-162…167), duplicates (L3-I-168), closure and archival (L3-I-171). Debarment sets the registered paper to `debarred` (`shared::registration::set_paper_status`); 6C reads decided cases read-only for result consequences.
//!
//! Tag `exam-integrity`; operation id prefixes `exam_malpractice_` (contract §5.2). The router mount point is
//! already registered in `crate::router_with_state`; 6B fills in this `router` (and its own
//! files under this folder) and never edits `lib.rs`.

pub mod domain;
pub mod http;
pub mod repo;

use crate::shared::ExamsState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area (contract §5.2, tag `exam-integrity`).
pub fn router(state: ExamsState) -> OpenApiRouter {
    http::router(state)
}
