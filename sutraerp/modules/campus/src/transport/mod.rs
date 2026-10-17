//! **8B** — transport: routes with effective-dated versions and stops (`ROUTE_ACTIVATION`), stops, vehicles (breakdown → replacement), driver assignments, passenger requests → allocations, passes (transport fee through `CampusChargePort`), trips (planned → ready → in progress → completed / cancelled); transport complaints open tickets (L3-G-093…097, 099…102, 104…106, 108, 109, 113…116). Tag `campus-transport`, operation ids `campus_transport_*`.
//!
//! Owned by slice 8B (`docs/batch8-contracts.md` §2.1). The mount point is pre-registered in
//! `crate::router_with_state`; the slice fills [`router`] and adds files in this folder.
//!
//! | File | Scenarios |
//! |------|-----------|
//! | [`network`] | G-093 route → activation, G-094 new effective version, G-095 suspension, G-096 / 097 stops |
//! | [`fleet`] | G-108 vehicles, G-109 breakdown, G-102 drivers |
//! | [`passengers`] | G-099 … 101 allocations, G-113 … 116 passes (`TRANSPORT-PASS`), complaints |
//! | [`trips`] | G-104 … 106 trips, G-109 replacement vehicle |
//! | [`selfservice`] | `/me/transport` (request, pass, complaints, routes) |

pub mod domain;
pub mod fleet;
pub mod maintenance;
pub mod network;
pub mod passengers;
pub mod selfservice;
pub mod trips;

use crate::shared::CampusState;
use utoipa_axum::{router::OpenApiRouter, routes};

/// Routes of this area.
pub fn router(state: CampusState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(network::route_list, network::route_create))
        .routes(routes!(network::route_get))
        .routes(routes!(network::version_list, network::version_create))
        .routes(routes!(network::version_stops))
        .routes(routes!(network::route_activate))
        .routes(routes!(network::route_decide))
        .routes(routes!(network::route_suspend))
        .routes(routes!(network::route_retire))
        .routes(routes!(network::stop_list, network::stop_create))
        .routes(routes!(network::stop_suspend))
        .routes(routes!(network::stop_restore))
        .routes(routes!(fleet::vehicle_list, fleet::vehicle_create))
        .routes(routes!(fleet::vehicle_status))
        .routes(routes!(fleet::vehicle_breakdown))
        .routes(routes!(fleet::driver_list, fleet::driver_assign))
        .routes(routes!(fleet::driver_end))
        .routes(routes!(
            passengers::allocation_list,
            passengers::allocation_create
        ))
        .routes(routes!(passengers::allocation_get))
        .routes(routes!(passengers::allocation_activate))
        .routes(routes!(passengers::allocation_change))
        .routes(routes!(passengers::allocation_withdraw))
        .routes(routes!(passengers::pass_list))
        .routes(routes!(passengers::pass_issue))
        .routes(routes!(passengers::pass_get))
        .routes(routes!(passengers::pass_renew))
        .routes(routes!(passengers::pass_replace))
        .routes(routes!(passengers::pass_suspend))
        .routes(routes!(passengers::pass_restore))
        .routes(routes!(
            passengers::complaint_list,
            passengers::complaint_create
        ))
        .routes(routes!(trips::trip_list, trips::trip_plan))
        .routes(routes!(trips::trip_board))
        .routes(routes!(trips::trip_ready))
        .routes(routes!(trips::trip_start))
        .routes(routes!(trips::trip_complete))
        .routes(routes!(trips::trip_cancel))
        .routes(routes!(trips::trip_replace_vehicle))
        .routes(routes!(selfservice::me_transport))
        .routes(routes!(selfservice::me_routes))
        .routes(routes!(selfservice::me_request))
        .routes(routes!(selfservice::me_pass))
        .routes(routes!(selfservice::me_complaint))
        .with_state(state)
}
