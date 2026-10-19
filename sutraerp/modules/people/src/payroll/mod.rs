//! **7C** — Payroll configuration (components, statutory rules, L3-E-074), salary structures /
//! assignments, periods, runs (calculation → validation → approval → finalisation → Finance
//! posting through `state.postings`, L3-E-053), adjustments (054), retro pay (055), final
//! settlement (056), off-cycle runs (057).
//!
//! Tag `people-payroll`; operation id prefixes `people_payroll_`, `people_pay_`,
//! `people_salary_`, `people_settlement_` (contract §5.2). Inputs come only from
//! `WorkforceDirectory::payable_engagements` and `LeaveReader::payroll_inputs`; money is
//! `rust_decimal` end to end.
//!
//! * [`domain`] — pure rules: transitions, configuration validation, the calculation engine.
//! * [`rows`] — row types and loaders of the 7C tables.
//! * [`config`] — configuration versions, salary structures / assignments, periods.
//! * [`runs`] — runs: create, calculate, validate, submit, decide, finalise, post, close.
//! * [`adjust`] — corrective adjustments on finalised results.
//! * [`settlement`] — separation final settlements.
//! * [`views`] / [`http`] — the API.
//! * [`disbursement`] / [`disbursement_http`] — payroll disbursement (Part 1, DEF-E-007): bank
//!   transfer batches of net pay and statutory remittances, settled in Finance.

pub mod adjust;
pub mod common;
pub mod config;
pub mod disbursement; // Part 1 (DEF-E-007)
pub mod disbursement_http; // Part 1 (DEF-E-007)
pub mod domain;
pub mod http;
pub mod rows;
pub mod runs;
pub mod settlement;
pub mod views;

use crate::shared::PeopleState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: PeopleState) -> OpenApiRouter {
    http::router(state.clone()).merge(disbursement_http::router(state))
}
