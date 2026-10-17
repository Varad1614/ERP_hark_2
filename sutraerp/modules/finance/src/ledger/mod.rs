//! **5C** — Balanced postings on top of `shared::posting`, manual journals, accounting batches, journal reversal, source ↔ ledger reconciliation, trial balance, accounting close, auditable history (L3-F-025…032); chart-of-accounts management.
//!
//! Owned by slice 5C (`docs/batch5-contracts.md` §1/§2). Tag `finance-ledger`; every operation id
//! starts with the prefixes of §5.2 (enforced by `apps/api/tests/architecture.rs`).
//!
//! * [`accounts`] — chart of accounts (list with balances, user accounts, rename / deactivate).
//! * [`journals`] — journal entries (read + manual-journal reversal), manual journals
//!   (draft → submit → approval through the WorkflowPort → posting), trial balance,
//!   receivables reconciliation.
//! * [`trial`] — the trial balance computation (also snapshotted by the period close).
//! * [`common`] — helpers shared by the 5C areas (`periods`, `treasury` use them too).
//!
//! The ledger has ONE write path: `shared::posting::{post, reverse}`.

pub mod accounts;
pub mod common;
pub mod journals;
pub mod statements;
pub mod trial;

use crate::shared::FinanceState;
use utoipa_axum::{router::OpenApiRouter, routes};

/// Routes of this area.
pub fn router(state: FinanceState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(
            accounts::ledger_account_list,
            accounts::ledger_account_create
        ))
        .routes(routes!(
            accounts::ledger_account_get,
            accounts::ledger_account_update
        ))
        .routes(routes!(journals::journal_list))
        .routes(routes!(journals::journal_get))
        .routes(routes!(journals::journal_reverse))
        .routes(routes!(
            journals::manual_journal_list,
            journals::manual_journal_create
        ))
        .routes(routes!(
            journals::manual_journal_get,
            journals::manual_journal_update
        ))
        .routes(routes!(journals::manual_journal_submit))
        .routes(routes!(journals::manual_journal_decide))
        .routes(routes!(journals::trial_balance_get))
        .routes(routes!(
            journals::reconciliation_list,
            journals::reconciliation_run
        ))
        .routes(routes!(journals::reconciliation_get))
        .with_state(state)
}
