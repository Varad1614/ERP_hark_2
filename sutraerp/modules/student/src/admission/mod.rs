//! **3A** — admissions: applications, review, offers/acceptance, admission documents + verification, the admission approval decision through the WorkflowPort (first BPMN-style approval), financial clearance from Finance evidence, confirmation, student creation, cancellation, withdrawal, revocation, offer expiry, readmission (L3-C-001…007).
//!
//! Paths: `/api/v1/student/institutions/{id}/admission-applications`, `/api/v1/student/admission-applications/{id}[/…]`, `/api/v1/student/institutions/{id}/admissions`, `/api/v1/student/admissions/{id}[/…]`.
//! operation_id prefixes: `admission_application_`, `admission_`. Tag: `admissions`.
//!
//! * [`domain`] — state machines (pure, unit-tested) and wire types.
//! * [`application`] — admission applications and the offer (Admission Record initiation).
//! * [`flow`] — the Admission Record lifecycle up to student creation, plus cancellation,
//!   withdrawal, revocation, expiry and readmission.

pub mod application;
pub mod domain;
pub mod duplicates;
pub mod flow;
pub mod funnel;
pub mod funnel_http;

use crate::shared::StudentState;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

/// Routes of this area. Mounted by `crate::router` (already registered — do not edit lib.rs).
pub fn router(state: StudentState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(application::create, application::list))
        .routes(routes!(application::get, application::update))
        .routes(routes!(application::submit))
        .routes(routes!(application::review))
        .routes(routes!(application::offer))
        .routes(routes!(application::reject))
        .routes(routes!(application::withdraw))
        .routes(routes!(flow::list))
        .routes(routes!(flow::expire_offers))
        .routes(routes!(flow::get))
        .routes(routes!(flow::get_history))
        .routes(routes!(flow::list_documents, flow::add_document))
        .routes(routes!(flow::verify_document))
        .routes(routes!(flow::accept))
        .routes(routes!(flow::submit_documents))
        .routes(routes!(flow::submit_for_approval))
        .routes(routes!(flow::decide))
        .routes(routes!(flow::financial_clearance))
        .routes(routes!(flow::confirm))
        .routes(routes!(flow::create_student))
        .routes(routes!(flow::cancel))
        .routes(routes!(flow::withdraw))
        .routes(routes!(flow::revoke))
        .routes(routes!(flow::expire))
        .routes(routes!(flow::readmit))
        // ERP_EXTENSIONS 2A — admissions funnel.
        .routes(routes!(funnel_http::list_enquiries, funnel_http::create_enquiry))
        .routes(routes!(funnel_http::get_enquiry, funnel_http::update_enquiry))
        .routes(routes!(funnel_http::link_application))
        .routes(routes!(funnel_http::report))
        .routes(routes!(funnel_http::forecast))
        .routes(routes!(funnel_http::duplicates))
        .routes(routes!(funnel_http::get_settings, funnel_http::put_settings))
        .with_state(state)
}
