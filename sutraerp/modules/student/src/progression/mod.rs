//! **3B** — progression decisions, credit ledger, conditional promotion, probation, detention/repeat, credit transfer, programme migration, academic break/rejoin, qualification eligibility, ABC sync (AbcRegistryPort, no-op), decision revision, progression policy lifecycle (L3-C-027…038, C-075).
//!
//! Paths: `/api/v1/student/institutions/{id}/progression-policies`, `/api/v1/student/progression-policies/{id}[/…]`, `/api/v1/student/students/{id}/progression-decisions`, `/api/v1/student/progression-decisions/{id}[/…]`, `/api/v1/student/students/{id}/credit-ledger`, `/api/v1/student/progression-conditions/{id}[/…]`, `/api/v1/student/probation-cases/{id}[/…]`, `/api/v1/student/students/{id}/credit-transfers`, `/api/v1/student/credit-transfers/{id}[/…]`, `/api/v1/student/students/{id}/programme-migrations`, `/api/v1/student/programme-migrations/{id}[/…]`, `/api/v1/student/students/{id}/academic-breaks`, `/api/v1/student/academic-breaks/{id}[/…]`, `/api/v1/student/students/{id}/qualification-eligibility`, `/api/v1/student/students/{id}/abc-syncs``.
//! operation_id prefixes: `progression_policy_`, `progression_`, `credit_ledger_`, `progression_condition_`, `probation_`, `detention_`, `credit_transfer_`, `programme_migration_`, `academic_break_`, `qualification_eligibility_`, `abc_sync_`. Tag: `progression`.
//!
//! Owned by slice 3B (see `docs/batch3-contracts.md`). Add `domain.rs`, `repo.rs`, `http.rs`
//! (or finer submodules) inside this directory only. Handlers take
//! [`StudentState`](crate::shared::StudentState); every `#[utoipa::path]` sets an explicit
//! `operation_id` with this slice's prefix and `tag = "progression"`. The first path parameter after a
//! collection segment is always named `{id}` (contract doc §5.2).

pub mod domain;
pub mod eligibility;
pub mod http;
pub mod mobility;
pub mod repo;

use crate::shared::StudentState;
use utoipa_axum::{router::OpenApiRouter, routes};

/// Routes of this area. Mounted by `crate::router` (already registered — do not edit lib.rs).
pub fn router(state: StudentState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(http::policy_create, http::policy_list))
        .routes(routes!(http::policy_get, http::policy_update))
        .routes(routes!(http::policy_submit))
        .routes(routes!(http::policy_decision))
        .routes(routes!(http::policy_retire))
        .routes(routes!(http::policy_revise))
        .routes(routes!(http::summary))
        .routes(routes!(http::decision_create, http::decision_list))
        .routes(routes!(http::decision_get))
        .routes(routes!(http::decision_submit))
        .routes(routes!(http::decision_decide))
        .routes(routes!(http::decision_discard))
        .routes(routes!(http::revision_create, http::revision_list))
        .routes(routes!(http::condition_get))
        .routes(routes!(http::condition_fulfil))
        .routes(routes!(http::condition_fail))
        .routes(routes!(http::condition_waive))
        .routes(routes!(http::probation_get))
        .routes(routes!(http::probation_clear))
        .routes(routes!(http::probation_escalate))
        .routes(routes!(http::detention_get))
        .routes(routes!(http::detention_determine))
        .routes(routes!(http::ledger_get, http::ledger_append))
        .routes(routes!(http::transfer_create, http::transfer_list))
        .routes(routes!(http::transfer_get, http::transfer_update))
        .routes(routes!(http::transfer_submit))
        .routes(routes!(http::transfer_decision))
        .routes(routes!(http::transfer_withdraw))
        .routes(routes!(http::migration_create, http::migration_list))
        .routes(routes!(http::migration_get))
        .routes(routes!(http::migration_submit))
        .routes(routes!(http::migration_decision))
        .routes(routes!(http::migration_withdraw))
        .routes(routes!(http::break_create, http::break_list))
        .routes(routes!(http::break_get))
        .routes(routes!(http::break_decision))
        .routes(routes!(http::break_start))
        .routes(routes!(http::break_rejoin))
        .routes(routes!(http::break_withdraw))
        .routes(routes!(http::eligibility_evaluate, http::eligibility_get))
        .routes(routes!(http::eligibility_history))
        .routes(routes!(http::abc_create, http::abc_list))
        .routes(routes!(http::abc_get))
        .with_state(state)
}
