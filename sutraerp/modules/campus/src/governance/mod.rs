//! **9E** — campus governance (Domain G00, L3-G-001…015): readiness / health / maturity
//! assessments, operational and executive reviews, the campus risk register, multi-domain action
//! plans, SLA deviations, KPI reviews, findings → corrective actions, escalations, evidence /
//! dependency exceptions, versioned governance configuration and governance evidence for
//! accreditation. Tag `campus-governance`, operation ids `campus_governance_*`, permission
//! `campus.governance.manage` (decisions `campus.approve`, step-up).
//!
//! Owned by slice 9E (`docs/batch9-contracts.md` §2). The mount point is pre-registered in
//! `crate::router_with_state`.
//!
//! | File | Scenarios |
//! |------|-----------|
//! | [`domain`] | state machines + the pure closure rules of every case kind |
//! | [`cases`] | G-001, 003…013 (cases, items, submit → `GOVERNANCE_DECISION`, escalation) |
//! | [`config`] | G-014 versioned configuration (`GOVERNANCE_CONFIG_APPROVAL`) |
//! | [`evidence`] | G-002 / G-015 governance evidence → `governance_evidence.published` |
//! | [`policy`] | effective policy read by the closure rules |

pub mod cases;
pub mod config;
pub mod domain;
pub mod evidence;
pub mod policy;

use crate::shared::CampusState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: CampusState) -> OpenApiRouter {
    OpenApiRouter::new()
        .merge(cases::router(state.clone()))
        .merge(config::router(state.clone()))
        .merge(evidence::router(state))
}
