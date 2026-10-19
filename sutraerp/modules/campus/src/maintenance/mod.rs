//! **8A** — maintenance: corrective work orders, technician assignment / reassignment, completion → verification, reopening, preventive plans → generated cycles + work orders, deferral / rescheduling (L3-G-020…024, 026). Tag `campus-facilities`, operation ids `campus_workorder_*` / `campus_maintenance_*`.
//!
//! Owned by slice 8A (`docs/batch8-contracts.md` §2.1). The mount point is pre-registered in
//! `crate::router_with_state`.
//!
//! | File | Content |
//! |------|---------|
//! | [`workorders`] | corrective / preventive work orders, assignment history, verification |
//! | [`plans`] | preventive plans (`MAINTENANCE_PLAN_APPROVAL`), cycle generation, deferral |

pub mod plans;
pub mod workorders;

use crate::shared::CampusState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: CampusState) -> OpenApiRouter {
    OpenApiRouter::new()
        .merge(workorders::router(state.clone()))
        .merge(plans::router(state))
}
