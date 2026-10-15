//! **4A** — Course offerings (L2-D01, L3-D-001…016): establishment → approval (WorkflowPort, the reference approval of Batch 4) → publication → enrolment window → activation, suspension/reactivation, cancellation, capacity and quota governance, enrolment eligibility (gated through `RegistrationReader`), waitlist → seat, cross-campus enrolment, revision/replacement/rollback, archive, merge/split, template generation, freeze/unfreeze, change simulation.
//!
//! Owned by slice 4A (`docs/batch4-contracts.md` §1/§2). Tag `offerings`; paths under
//! `/api/v1/academic` (§5.2). Writes the foundation tables `course_offering`,
//! `offering_section`, `section_enrolment` (status only via `crate::shared::lifecycle`) and
//! 4A's own tables (migration `20261025000001`).
//!
//! | File | Scenarios |
//! |------|-----------|
//! | [`lifecycle`] | D-001 establishment → approval → publication → enrolment → activation; D-002/003 suspension / reactivation; D-010 archive; D-013 freeze |
//! | [`change`] | D-004 cancellation, D-013 governed unfreeze, D-009 revision / replacement / rollback, D-011 merge / split |
//! | [`sections`] | sections, D-005 capacity governance |
//! | [`enrolment`] | section enrolment gated on 3B registrations, D-006/008 gate, D-007 waitlist → seat, student self-service |
//! | [`governance`] | D-015 quotas, D-014 enrolment windows, D-006/008 restrictions and permits |
//! | [`templates`] | D-012 template generation, D-016 change simulation → impact decision |

pub mod change;
pub mod enrolment;
pub mod governance;
pub mod lifecycle;
pub mod sections;
pub mod support;
pub mod templates;

use crate::shared::AcademicState;
use utoipa_axum::{router::OpenApiRouter, routes};

/// Routes of this area.
pub fn router(state: AcademicState) -> OpenApiRouter {
    OpenApiRouter::new()
        // lifecycle
        .routes(routes!(lifecycle::create, lifecycle::list))
        .routes(routes!(lifecycle::get, lifecycle::update))
        .routes(routes!(lifecycle::get_readiness))
        .routes(routes!(lifecycle::submit))
        .routes(routes!(lifecycle::decision))
        .routes(routes!(lifecycle::publish))
        .routes(routes!(lifecycle::open_enrolment_cmd))
        .routes(routes!(lifecycle::close_enrolment_cmd))
        .routes(routes!(lifecycle::activate_cmd))
        .routes(routes!(lifecycle::suspend_cmd))
        .routes(routes!(lifecycle::reactivate_cmd))
        .routes(routes!(lifecycle::freeze_cmd))
        .routes(routes!(lifecycle::complete_cmd))
        .routes(routes!(lifecycle::archive_cmd))
        // governed changes
        .routes(routes!(change::cancel))
        .routes(routes!(change::unfreeze))
        .routes(routes!(change::revision_create, change::revision_list))
        .routes(routes!(change::revision_decision))
        .routes(routes!(change::revision_withdraw))
        .routes(routes!(change::merge))
        .routes(routes!(change::split))
        .routes(routes!(change::reorganisation_list))
        .routes(routes!(change::reorganisation_decision))
        // sections + capacity
        .routes(routes!(sections::create, sections::list))
        .routes(routes!(sections::get, sections::update))
        .routes(routes!(sections::activate))
        .routes(routes!(sections::cancel))
        .routes(routes!(
            sections::change_capacity,
            sections::capacity_history
        ))
        // enrolment + waitlist
        .routes(routes!(enrolment::create, enrolment::list))
        .routes(routes!(enrolment::get))
        // Part 1 · S3-appr (DEF-P1-DEMO-006): governed eligibility overrides.
        .routes(routes!(enrolment::override_request))
        .routes(routes!(enrolment::override_get))
        .routes(routes!(enrolment::override_decision))
        .routes(routes!(enrolment::drop))
        .routes(routes!(enrolment::withdraw))
        .routes(routes!(enrolment::transfer))
        .routes(routes!(enrolment::waitlist_join, enrolment::waitlist_list))
        .routes(routes!(enrolment::waitlist_promote))
        .routes(routes!(enrolment::waitlist_accept))
        .routes(routes!(enrolment::waitlist_remove))
        // self-service
        .routes(routes!(enrolment::my_courses))
        .routes(routes!(enrolment::available))
        .routes(routes!(enrolment::self_enrol, enrolment::my_enrolments))
        .routes(routes!(enrolment::self_drop))
        .routes(routes!(enrolment::self_waitlist))
        .routes(routes!(enrolment::self_waitlist_accept))
        // governance
        .routes(routes!(governance::quota_create, governance::quota_list))
        .routes(routes!(governance::quota_retire))
        .routes(routes!(governance::window_create, governance::window_list))
        .routes(routes!(governance::window_open))
        .routes(routes!(governance::window_suspend))
        .routes(routes!(governance::window_resume))
        .routes(routes!(governance::window_extend))
        .routes(routes!(governance::window_close))
        .routes(routes!(governance::window_close_permanently))
        .routes(routes!(
            governance::restriction_create,
            governance::restriction_list
        ))
        .routes(routes!(governance::restriction_retire))
        // templates + simulations
        .routes(routes!(
            templates::template_create,
            templates::template_list
        ))
        .routes(routes!(templates::template_get))
        .routes(routes!(templates::template_activate))
        .routes(routes!(templates::template_retire))
        .routes(routes!(templates::generate))
        .routes(routes!(
            templates::simulation_create,
            templates::simulation_list
        ))
        .routes(routes!(templates::simulation_decision))
        .with_state(state)
}
