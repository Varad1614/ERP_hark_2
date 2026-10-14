//! **8C** — regulatory reporting (H08): frameworks (AISHE / NIRF / NAAC data definitions), reporting cycles + collection plans, snapshots from the metrics port, validation, submission packages, approval, acknowledgement, correction, closure (L3-H-055…060). Tag `services-regulatory`, operation ids `services_regulatory_*`.
//!
//! Owned by slice 8C (`docs/batch8-contracts.md` §2.1). The mount point is pre-registered in
//! `crate::router_with_state`; the slice fills [`router`] and adds files in this folder.
//!
//! | File | Scenarios |
//! |------|-----------|
//! | [`frameworks`] | H-055 framework change → active reporting definition (`REPORTING_FRAMEWORK_APPROVAL`) |
//! | [`cycles`] | H-056 cycle + plan, H-057 snapshot, H-058 package (`SUBMISSION_APPROVAL`), H-059 acknowledgement, H-060 correction / closure |

pub mod cycles;
pub mod domain;
pub mod frameworks;

use crate::shared::ServicesState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: ServicesState) -> OpenApiRouter {
    OpenApiRouter::new()
        .merge(frameworks::router(state.clone()))
        .merge(cycles::router(state))
}
