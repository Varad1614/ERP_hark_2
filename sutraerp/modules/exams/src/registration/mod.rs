//! **6A** — Examination registration (self-service, bulk, late, deadline override, amendment / correction, withdrawal / reinstatement, reopening, rejection, scope change after a 3B registration change, special arrangements, supplementary / backlog / improvement registration, duplicates, administrative registration, population finalisation — L3-I-017…027, 029, 030, 032, 034, 036) through `shared::registration`, and hall tickets (issuance with the finance-dues and attendance entry conditions, withholding, invalidation, special arrangements, duplicates, population finalisation — L3-I-037…042).
//!
//! Tag `exam-registration`; operation id prefixes `exam_registration_` (incl. `exam_registration_self_`), `exam_hall_ticket_` (incl. `exam_hall_ticket_self_`) (contract §5.2). The router mount point is
//! already registered in `crate::router_with_state`; 6A fills in this `router` (and its own
//! files under this folder) and never edits `lib.rs`.

pub mod base;
pub mod hall_ticket;
pub mod me;
pub mod staff;

use crate::shared::ExamsState;
use hall_ticket::*;
use me::*;
use staff::*;
use utoipa_axum::{router::OpenApiRouter, routes};

/// Routes of this area.
pub fn router(state: ExamsState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(registration_list, registration_create))
        .routes(routes!(registration_bulk))
        .routes(routes!(registration_get))
        .routes(routes!(registration_decision))
        .routes(routes!(registration_confirm))
        .routes(routes!(registration_amend))
        .routes(routes!(registration_add_paper))
        .routes(routes!(registration_scope_sync))
        .routes(routes!(registration_withdraw))
        .routes(routes!(registration_reinstate))
        .routes(routes!(arrangement_create))
        .routes(routes!(override_list, override_create))
        .routes(routes!(override_decision))
        .routes(routes!(registration_reopen))
        .routes(routes!(population_finalise))
        .routes(routes!(hall_ticket_list))
        .routes(routes!(hall_ticket_issue))
        .routes(routes!(hall_ticket_finalise))
        .routes(routes!(hall_ticket_get))
        .routes(routes!(hall_ticket_withhold))
        .routes(routes!(hall_ticket_release))
        .routes(routes!(hall_ticket_withdraw))
        .routes(routes!(hall_ticket_reissue))
        .routes(routes!(my_registration_list, my_registration_create))
        .routes(routes!(my_registration_options))
        .routes(routes!(my_registration_withdraw))
        .routes(routes!(my_hall_tickets))
        .with_state(state)
}
