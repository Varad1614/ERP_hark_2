//! **7A** — Recruitment → onboarding (L3-E-033), probation review (L3-E-043), separation case → clearance → exit (L3-E-041), certificate requests (L3-E-039).
//!
//! Tag `people-hr`; operation id prefixes `people_recruitment_`, `people_onboarding_`,
//! `people_probation_`, `people_separation_`, `people_certificate_` (contract §5.2).

pub mod careers; // ERP_EXTENSIONS 5A
pub mod careers_http; // ERP_EXTENSIONS 5A
pub mod certificate;
pub mod hiring_analytics; // ERP_EXTENSIONS 5A
pub mod hiring_insights_http; // ERP_EXTENSIONS 5A
pub mod offers; // ERP_EXTENSIONS 5A
pub mod offers_http; // ERP_EXTENSIONS 5A
pub mod onboarding;
pub mod probation;
pub mod recruitment;
pub mod scorecards; // ERP_EXTENSIONS 3B
pub mod scorecards_http; // ERP_EXTENSIONS 3B
pub mod separation;

use crate::shared::PeopleState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .merge(recruitment::router(state.clone()))
        .merge(scorecards_http::router(state.clone())) // ERP_EXTENSIONS 3B
        .merge(offers_http::router(state.clone())) // ERP_EXTENSIONS 5A
        .merge(hiring_insights_http::router(state.clone())) // ERP_EXTENSIONS 5A
        .merge(careers_http::router(state.clone())) // ERP_EXTENSIONS 5A
        .merge(onboarding::router(state.clone()))
        .merge(probation::router(state.clone()))
        .merge(separation::router(state.clone()))
        .merge(certificate::router(state))
}
