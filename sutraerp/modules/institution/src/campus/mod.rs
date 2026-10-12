//! **2B** — institutional campuses (register / activation decision / modify / suspend / restore / decommission / archive).
//!
//! Paths: `/api/v1/institution/institutions/{id}/campuses`, `/api/v1/institution/campuses/...`.
//! operation_id prefix: `campus_`.
//!
//! Owned by slice 2B (see `docs/batch2-contracts.md`). Add `domain.rs`, `repo.rs`, `http.rs`
//! (or finer submodules) inside this directory only. Handlers take
//! [`InstitutionState`](crate::shared::InstitutionState); every `#[utoipa::path]` sets an explicit
//! `operation_id` with this slice's prefix and `tag = "campuses"`.

pub mod domain;
pub mod http;
pub mod readiness;
pub mod repo;

use crate::shared::InstitutionState;
use utoipa_axum::{router::OpenApiRouter, routes};

/// Routes of this area. Mounted by `crate::router` (already registered — do not edit lib.rs).
pub fn router(state: InstitutionState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(http::register, http::list))
        .routes(routes!(http::get, http::modify))
        .routes(routes!(http::revisions))
        .routes(routes!(http::activate))
        .routes(routes!(http::decision))
        .routes(routes!(http::suspend))
        .routes(routes!(http::restore))
        .routes(routes!(http::decommission))
        .routes(routes!(http::archive))
        .with_state(state)
}
