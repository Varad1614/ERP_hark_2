//! **Foundation** — the staff directory (read-only): HR's staff list and staff detail with the
//! current engagement, the basic staff directory every staff member may read, and the caller's
//! own staff record. Tag `people-directory`, operation ids `people_directory_*`. Writes to staff
//! records and engagements are 7A's (`workforce/`).

pub mod http;

use crate::shared::PeopleState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: PeopleState) -> OpenApiRouter {
    http::router(state)
}
