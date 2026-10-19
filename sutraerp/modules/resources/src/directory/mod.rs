//! **Foundation** — stock on hand per item and location (projection of the append-only stock
//! ledger). Tag `resources-directory`, operation ids `resources_directory_*`. Items, stores and
//! every movement are 8D's.

pub mod http;

use crate::shared::ResourcesState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: ResourcesState) -> OpenApiRouter {
    http::router(state)
}
