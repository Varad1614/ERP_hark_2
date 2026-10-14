//! **Foundation** — read-only Campus directory: the infrastructure tree of an institution (what
//! every slice and screen picks spaces from) and the caller's campus person (student or staff).
//! Tag `campus-directory`, operation ids `campus_directory_*`. Writes to nodes are 8A's.

pub mod http;

use crate::shared::CampusState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: CampusState) -> OpenApiRouter {
    http::router(state)
}
