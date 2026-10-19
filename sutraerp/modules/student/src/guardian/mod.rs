//! **3C** — guardian relationship lifecycle, custody, consent, communication authorisation, emergency contacts, financial responsibility, pickup authorisation (L3-C-051…061).
//!
//! Paths: `/api/v1/student/institutions/{id}/guardians`, `/api/v1/student/guardians/{id}[/…]`, `/api/v1/student/students/{id}/guardian-relationships`, `/api/v1/student/guardian-relationships/{id}[/…]`, `/api/v1/student/students/{id}/emergency-contacts`, `/api/v1/student/emergency-contacts/{id}[/…]`, `/api/v1/student/students/{id}/pickup-authorisations`, `/api/v1/student/pickup-authorisations/{id}[/…]`, `/api/v1/student/students/{id}/financial-responsibilities`, `/api/v1/student/financial-responsibilities/{id}[/…]`.
//! operation_id prefixes: `guardian_`, `guardian_relationship_`, `guardian_consent_`, `emergency_contact_`, `pickup_authorisation_`, `financial_responsibility_`. Tag: `guardians`.
//!
//! Owned by slice 3C (see `docs/batch3-contracts.md`). Add `domain.rs`, `repo.rs`, `http.rs`
//! (or finer submodules) inside this directory only. Handlers take
//! [`StudentState`](crate::shared::StudentState); every `#[utoipa::path]` sets an explicit
//! `operation_id` with this slice's prefix and `tag = "guardians"`. The first path parameter after a
//! collection segment is always named `{id}` (contract doc §5.2).

pub mod domain;
pub mod http;
pub mod repo;

use crate::shared::StudentState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area. Mounted by `crate::router` (already registered — do not edit lib.rs).
pub fn router(state: StudentState) -> OpenApiRouter {
    http::router(state)
}
