//! **8A** — space reservations / room booking: request → confirmed (`RESERVATION_APPROVAL`) → reschedule → cancel, availability and conflicts (L3-G-291…293). Tag `campus-infrastructure`, operation ids `campus_reservation_*`.
//!
//! Owned by slice 8A (`docs/batch8-contracts.md` §2.1). The mount point is pre-registered in
//! `crate::router_with_state`; [`http`] holds the handlers.

pub mod http;

use crate::shared::CampusState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: CampusState) -> OpenApiRouter {
    http::router(state)
}
