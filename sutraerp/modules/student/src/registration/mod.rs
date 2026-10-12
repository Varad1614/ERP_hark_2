//! **3B** — academic registration: windows, registration, add/drop, cancellation, backlog/repeat lines, finalisation and publication, holds (incl. finance-sourced holds via FinanceClearancePort), student self-registration (L3-C-020…026).
//!
//! Paths: `/api/v1/student/institutions/{id}/registration-windows`, `/api/v1/student/registration-windows/{id}[/…]`, `/api/v1/student/students/{id}/registrations`, `/api/v1/student/registrations/{id}[/…]`, `/api/v1/student/students/{id}/holds`, `/api/v1/student/registration-holds/{id}[/…]`, `/api/v1/student/me/registrations[/…]`, `/api/v1/student/me/registration-windows`.
//! operation_id prefixes: `registration_window_`, `registration_`, `registration_hold_`, `registration_self_`. Tag: `registration`.
//!
//! Owned by slice 3B (see `docs/batch3-contracts.md`). Add `domain.rs`, `repo.rs`, `http.rs`
//! (or finer submodules) inside this directory only. Handlers take
//! [`StudentState`](crate::shared::StudentState); every `#[utoipa::path]` sets an explicit
//! `operation_id` with this slice's prefix and `tag = "registration"`. The first path parameter after a
//! collection segment is always named `{id}` (contract doc §5.2).

pub mod domain;
pub mod http;
pub mod repo;
pub mod support;

use crate::shared::StudentState;
use utoipa_axum::{router::OpenApiRouter, routes};

/// Routes of this area. Mounted by `crate::router` (already registered — do not edit lib.rs).
pub fn router(state: StudentState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(http::window_create, http::window_list))
        .routes(routes!(http::window_get, http::window_update))
        .routes(routes!(http::window_open))
        .routes(routes!(http::window_close))
        .routes(routes!(http::window_cancel))
        .routes(routes!(http::window_finalise))
        .routes(routes!(http::options))
        .routes(routes!(http::create, http::list))
        .routes(routes!(http::get))
        .routes(routes!(http::history))
        .routes(routes!(http::add_line))
        .routes(routes!(http::drop_line))
        .routes(routes!(http::submit))
        .routes(routes!(http::decision))
        .routes(routes!(http::cancel))
        .routes(routes!(http::finalise))
        .routes(routes!(http::hold_place, http::hold_list))
        .routes(routes!(http::hold_get))
        .routes(routes!(http::hold_release))
        .routes(routes!(http::self_options))
        .routes(routes!(http::self_create, http::self_list))
        .routes(routes!(http::self_get))
        .routes(routes!(http::self_add_line))
        .routes(routes!(http::self_drop_line))
        .routes(routes!(http::self_submit))
        .routes(routes!(http::self_cancel))
        .with_state(state)
}
