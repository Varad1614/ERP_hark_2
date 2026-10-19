//! **8C** — library: policy, memberships, catalogue, copies, issue / return / renewal, reservations, overdue fines billed through `CampusChargePort`, waivers, lost copies, clearance, restrictions; `/me/library` (L3-G-124…139, 149, 152). Tag `campus-library`, operation ids `campus_library_*`.
//!
//! Owned by slice 8C (`docs/batch8-contracts.md` §2.1). The mount point is pre-registered in
//! `crate::router_with_state`; the slice fills [`router`] and adds files in this folder.
//!
//! | File | Scenarios |
//! |------|-----------|
//! | [`policy`] | borrowing policy versions (`LIBRARY_POLICY_APPROVAL`) |
//! | [`members`] | G-124 membership, G-125 suspension, G-126 closure, G-152 borrowing restriction |
//! | [`catalogue`] | G-127 / 128 catalogue records, G-129 / 130 copies |
//! | [`circulation`] | G-131 issue, G-132 return, G-133 renewal, G-134…136 reservations, G-137 overdue sweep, G-139 lost copy |
//! | [`fines`] | G-137 fines, G-138 waiver (`FINE_WAIVER`), billing through `CampusChargePort` |
//! | [`clearance`] | G-149 clearance (`LIBRARY_CLEARANCE`) |
//! | [`me`] | `/me/library` self-service + library complaints (`open_ticket`, source `library`) |

pub mod catalogue;
pub mod circulation;
pub mod clearance;
pub mod common;
pub mod domain;
pub mod fines;
pub mod me;
pub mod members;
pub mod policy;

use crate::shared::CampusState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: CampusState) -> OpenApiRouter {
    OpenApiRouter::new()
        .merge(policy::router(state.clone()))
        .merge(members::router(state.clone()))
        .merge(catalogue::router(state.clone()))
        .merge(circulation::router(state.clone()))
        .merge(fines::router(state.clone()))
        .merge(clearance::router(state.clone()))
        .merge(me::router(state))
}
