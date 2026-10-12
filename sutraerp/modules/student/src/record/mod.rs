//! **3A** — student record: activation + enrolment → active, enrolment updates, academic assignment change, suspension/restoration, intra/inter-institute transfer, profile, communication preferences, identity correction, duplicate resolution, student documents, document/certificate requests, archival, Student 360 and self-service `me` (L3-C-001, C-008…019).
//!
//! Paths: `/api/v1/student/institutions/{id}/students`, `/api/v1/student/students/{id}[/…]`, `/api/v1/student/document-requests/{id}[/…]`, `/api/v1/student/identity-corrections/{id}[/…]`, `/api/v1/student/duplicate-cases/{id}[/…]`, `/api/v1/student/transfers/{id}[/…]`, `/api/v1/student/me/student`, `/api/v1/student/me/document-requests`.
//! operation_id prefixes: `student_`, `student_document_`, `document_request_`, `identity_correction_`, `duplicate_case_`, `student_transfer_`, `student_self_`. Tag: `students`.
//!
//! * [`support`] / [`refs`] / [`common`] — slice-private helpers (also used by `admission`).
//! * [`creation`] — student establishment from a confirmed admission.
//! * [`student`] — directory, Student 360, enrolment, assignment, suspension, profile, archival.
//! * [`preferences`], [`documents`], [`identity`], [`transfer`] — the remaining record areas.

pub mod common;
pub mod creation;
pub mod documents;
pub mod dto;
pub mod identity;
pub mod preferences;
pub mod refs;
pub mod student;
pub mod support;
pub mod transfer;

use crate::shared::StudentState;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

/// Routes of this area. Mounted by `crate::router` (already registered — do not edit lib.rs).
pub fn router(state: StudentState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(student::directory))
        .routes(routes!(student::get))
        .routes(routes!(student::get_me))
        .routes(routes!(student::status_history))
        .routes(routes!(student::record_history_list))
        .routes(routes!(student::enrol))
        .routes(routes!(student::update_enrolment))
        .routes(routes!(student::change_assignment))
        .routes(routes!(student::suspend))
        .routes(routes!(student::restore))
        .routes(routes!(student::suspensions))
        .routes(routes!(student::archive))
        .routes(routes!(student::update_profile))
        .routes(routes!(student::duplicate_candidates))
        .routes(routes!(preferences::get_staff, preferences::put_staff))
        .routes(routes!(preferences::get_me, preferences::put_me))
        .routes(routes!(documents::list_documents, documents::add_document))
        .routes(routes!(documents::verify_document))
        .routes(routes!(documents::my_documents))
        .routes(routes!(
            documents::list_for_student,
            documents::create_for_student
        ))
        .routes(routes!(documents::list_for_institution))
        .routes(routes!(documents::my_requests, documents::request_mine))
        .routes(routes!(documents::get_request))
        .routes(routes!(documents::start))
        .routes(routes!(documents::issue))
        .routes(routes!(documents::reject))
        .routes(routes!(documents::cancel))
        .routes(routes!(documents::cancel_mine))
        .routes(routes!(
            identity::create_correction,
            identity::list_corrections
        ))
        .routes(routes!(identity::get_correction))
        .routes(routes!(identity::submit_correction))
        .routes(routes!(identity::decide_correction))
        .routes(routes!(identity::withdraw_correction))
        .routes(routes!(
            identity::create_duplicate,
            identity::list_duplicates
        ))
        .routes(routes!(identity::get_duplicate))
        .routes(routes!(identity::submit_duplicate))
        .routes(routes!(identity::decide_duplicate))
        .routes(routes!(identity::withdraw_duplicate))
        .routes(routes!(transfer::create, transfer::list))
        .routes(routes!(transfer::get))
        .routes(routes!(transfer::submit))
        .routes(routes!(transfer::decide))
        .routes(routes!(transfer::withdraw))
        .with_state(state)
}
