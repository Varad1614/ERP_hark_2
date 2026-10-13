//! **7A** — Employee profile: qualifications (L3-E-060) and documents (L3-E-064) with verification, workforce → profile synchronisation records (L3-E-066), `/me/profile` (own record, engagement, faculty profile, qualifications, documents, certificate requests).
//!
//! Tag `people-workforce`; operation id prefix `people_profile_` (incl. `people_profile_self_`)
//! (contract §5.2).

pub mod http;

use crate::shared::PeopleState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: PeopleState) -> OpenApiRouter {
    http::router(state)
}
