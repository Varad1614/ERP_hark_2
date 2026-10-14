//! **8C** — dashboards (H07): requirement → governed published dashboard, versioning / rollback, widget definitions bound to operational metrics, role / org assignments (L3-H-048, 050, 052, 054); operational reports G-154 (library), G-320 (infrastructure portfolio). Tag `services-dashboards`, operation ids `services_dashboard_*`.
//!
//! Owned by slice 8C (`docs/batch8-contracts.md` §2.1). The mount point is pre-registered in
//! `crate::router_with_state`; the slice fills [`router`] and adds files in this folder.
//!
//! | File | Scenarios |
//! |------|-----------|
//! | [`widgets`] | H-054 widget definitions |
//! | [`board`] | H-048 dashboard requirement → published, H-050 assignments, H-052 versions / rollback, F-076 finance KPI dashboard, `/me/dashboards` |
//! | [`reports`] | G-154 library and G-320 infrastructure operational reports |
//! | [`common`], [`domain`] | 8C reporting helpers and state machines (also used by `crate::regulatory`) |

pub mod board;
pub mod common;
pub mod domain;
pub mod reports;
pub mod widgets;

use crate::shared::ServicesState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: ServicesState) -> OpenApiRouter {
    OpenApiRouter::new()
        .merge(widgets::router(state.clone()))
        .merge(board::router(state.clone()))
        .merge(reports::router(state))
}
