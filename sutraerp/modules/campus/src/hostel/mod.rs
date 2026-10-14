//! **8B** — hostel: policy, beds, admission applications → decision / waiting list → allocation (residency, hostel fee through `CampusChargePort`) → resident leave, room transfer, inspection (damage charge), mess, vacating; hostel complaints open tickets through `shared::tickets::open_ticket` (L3-G-078…082, 087…092). Tag `campus-hostel`, operation ids `campus_hostel_*`.
//!
//! Owned by slice 8B (`docs/batch8-contracts.md` §2.1). The mount point is pre-registered in
//! `crate::router_with_state`; the slice fills [`router`] and adds files in this folder.
//!
//! | File | Scenarios |
//! |------|-----------|
//! | [`policy`] | G-092 policy versions (`HOSTEL_POLICY_APPROVAL`), beds of `hostel_room` spaces |
//! | [`admission`] | G-078 application → decision (`HOSTEL_ADMISSION`), G-079 allocation + `HOSTEL-FEE`, G-080 waiting list / vacancy offer |
//! | [`residency`] | G-082 resident leave, G-087 room transfer, G-089 inspection + `HOSTEL-DAMAGE`, G-081 vacating → clearance → closed, G-088 complaints |
//! | [`mess`] | G-090 mess allocation, G-091 mess change |
//! | [`selfservice`] | `/me/hostel` (application, withdrawal, leave, complaints) |
//! | [`common`] | helpers shared with `transport/` |

pub mod admission;
pub mod common;
pub mod domain;
pub mod mess;
pub mod policy;
pub mod residency;
pub mod selfservice;

use crate::shared::CampusState;
use utoipa_axum::{router::OpenApiRouter, routes};

/// Routes of this area.
pub fn router(state: CampusState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(policy::policy_list, policy::policy_create))
        .routes(routes!(policy::policy_get))
        .routes(routes!(policy::policy_submit))
        .routes(routes!(policy::policy_decide))
        .routes(routes!(policy::policy_activate))
        .routes(routes!(policy::bed_list, policy::bed_create))
        .routes(routes!(policy::bed_occupancy))
        .routes(routes!(
            admission::application_list,
            admission::application_create
        ))
        .routes(routes!(admission::application_get))
        .routes(routes!(admission::application_decide))
        .routes(routes!(admission::application_allocate))
        .routes(routes!(admission::application_withdraw))
        .routes(routes!(admission::allocation_list))
        .routes(routes!(admission::allocation_get))
        .routes(routes!(residency::leave_list))
        .routes(routes!(residency::leave_queue))
        .routes(routes!(residency::leave_request))
        .routes(routes!(residency::leave_decide))
        .routes(routes!(residency::leave_depart))
        .routes(routes!(residency::leave_return))
        .routes(routes!(residency::leave_overdue_check))
        .routes(routes!(residency::transfer_list))
        .routes(routes!(residency::transfer_request))
        .routes(routes!(residency::transfer_decide))
        .routes(routes!(
            residency::inspection_list,
            residency::inspection_create
        ))
        .routes(routes!(residency::inspection_queue))
        .routes(routes!(residency::inspection_complete))
        .routes(routes!(residency::inspection_close))
        .routes(routes!(residency::allocation_vacate))
        .routes(routes!(residency::allocation_close))
        .routes(routes!(
            residency::complaint_list,
            residency::complaint_create
        ))
        .routes(routes!(mess::mess_list, mess::mess_create))
        .routes(routes!(
            mess::mess_allocation_list,
            mess::mess_allocation_create
        ))
        .routes(routes!(mess::mess_allocation_change))
        .routes(routes!(selfservice::me_hostel))
        .routes(routes!(selfservice::me_apply))
        .routes(routes!(selfservice::me_withdraw))
        .routes(routes!(selfservice::me_leave))
        .routes(routes!(selfservice::me_complaint))
        .with_state(state)
}
