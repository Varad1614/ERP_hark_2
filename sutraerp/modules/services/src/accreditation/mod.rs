//! **9E** — accreditation (Domain H09, L3-H-061…071): versioned accreditation frameworks and
//! criteria, cycles → assessment plans, compliance obligations, evidence requests → approved evidence
//! records, internal audits, external assessments, findings → CAPA, quality improvement, readiness
//! decisions, periodic compliance reviews and self-study reports. Figures only through
//! `state.metrics`. Tag `services-accreditation`, operation ids `services_accreditation_*`,
//! permission `services.accreditation.manage` (decisions `services.report.approve`, step-up).
//!
//! Owned by slice 9E (`docs/batch9-contracts.md` §2). The mount point is pre-registered in
//! `crate::router_with_state`.
//!
//! | File | Scenarios |
//! |------|-----------|
//! | [`domain`] | state machines + the pure decision rules |
//! | [`frameworks`] | H-061 (`ACCREDITATION_FRAMEWORK_APPROVAL`) |
//! | [`cycles`] | H-062 (`ACCREDITATION_PLAN_APPROVAL`), cycle close after H-071 |
//! | [`records`] | H-063…071 (`ACCREDITATION_DECISION`), self-study hand-off |

pub mod cycles;
pub mod domain;
pub mod frameworks;
pub mod records;

use crate::shared::ServicesState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: ServicesState) -> OpenApiRouter {
    OpenApiRouter::new()
        .merge(frameworks::router(state.clone()))
        .merge(cycles::router(state.clone()))
        .merge(records::router(state))
}
