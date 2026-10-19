//! **8A** — infrastructure hierarchy (campus → building → floor → space / zone), classification, capabilities, capacity and availability profiles, suspension / restoration, decommissioning (L3-G-278, 281…284, 286…290, 315). Tag `campus-infrastructure`, operation ids `campus_infra_*` / `campus_space_*`.
//!
//! Owned by slice 8A (`docs/batch8-contracts.md` §2.1). The mount point is pre-registered in
//! `crate::router_with_state`.
//!
//! | File | Content |
//! |------|---------|
//! | [`domain`] | pure 8A rules: slice enums + transitions, availability, free / busy, lab readiness, PM periods |
//! | [`common`] | bodies, `If-Match`, history + `StatusChangedV1` / `ModifiedV1` events, audit (shared by every 8A area) |
//! | [`nodes`] | infrastructure nodes, capabilities, availability profiles, space calendar, state commands |

pub mod common;
pub mod domain;
pub mod nodes;

use crate::shared::CampusState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: CampusState) -> OpenApiRouter {
    nodes::router(state)
}
