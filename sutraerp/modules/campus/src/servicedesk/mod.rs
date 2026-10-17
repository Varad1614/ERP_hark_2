//! **8A** — service desk: service requests and complaints → triage → assignment → resolution → verified closure, duplicates, SLA monitoring, escalation, administrative closure, `/me/tickets` (L3-G-016…018, 041, 042, 046). Tag `campus-facilities`, operation ids `campus_ticket_*`.
//!
//! Owned by slice 8A (`docs/batch8-contracts.md` §2.1). The mount point is pre-registered in
//! `crate::router_with_state`; [`http`] holds the handlers.

pub mod http;
pub mod rules;

use crate::shared::CampusState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: CampusState) -> OpenApiRouter {
    http::router(state)
}
