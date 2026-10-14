//! **Foundation** — the operational metrics catalogue: every figure the domains expose through
//! `OperationalMetricsPort`, for one institution (what dashboards and regulatory snapshots are
//! built from). Tag `services-directory`, operation ids `services_directory_*`.

pub mod http;

use crate::shared::ServicesState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: ServicesState) -> OpenApiRouter {
    http::router(state)
}
