//! **8A** — laboratories: readiness, practical sessions, batch allocation, equipment status (L3-G-050, 053, 057, 060). Tag `campus-facilities`, operation ids `campus_lab_*`.
//!
//! Owned by slice 8A (`docs/batch8-contracts.md` §2.1). The mount point is pre-registered in
//! `crate::router_with_state`; [`http`] holds the handlers.

pub mod http;

use crate::shared::CampusState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: CampusState) -> OpenApiRouter {
    http::router(state)
}
