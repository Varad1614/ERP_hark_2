//! **5C** — ledger, periods, treasury (L2-F05/F09/F10, L3-F-025…032, 051…061).
//!
//! Owned by slice 5C: add events here (constant + `EVENT_TYPES` entry + payload) and list
//! them in `docs/batch5-contracts.md` §4.3 in the PR. Payload: [`super::StatusChangedV1`] unless
//! noted.

use super::event_type;
event_type!(
    /// (L3-F-026).
    MANUAL_JOURNAL_SUBMITTED, MANUAL_JOURNAL_SUBMITTED_SCHEMA, "manual_journal.submitted");
event_type!(
    /// (L3-F-026).
    MANUAL_JOURNAL_POSTED, MANUAL_JOURNAL_POSTED_SCHEMA, "manual_journal.posted");
event_type!(
    /// (L3-F-027).
    ACCOUNTING_BATCH_POSTED, ACCOUNTING_BATCH_POSTED_SCHEMA, "accounting_batch.posted");
event_type!(
    /// (L3-F-029).
    LEDGER_RECONCILIATION_COMPLETED, LEDGER_RECONCILIATION_COMPLETED_SCHEMA, "ledger_reconciliation.completed");
event_type!(
    /// (L3-F-030).
    TRIAL_BALANCE_GENERATED, TRIAL_BALANCE_GENERATED_SCHEMA, "trial_balance.generated");
event_type!(
    /// (L3-F-051).
    FINANCIAL_PERIOD_OPENED, FINANCIAL_PERIOD_OPENED_SCHEMA, "financial_period.opened");
event_type!(
    /// (L3-F-052).
    FINANCIAL_PERIOD_LOCKED, FINANCIAL_PERIOD_LOCKED_SCHEMA, "financial_period.locked");
event_type!(
    /// (L3-F-031/052).
    FINANCIAL_PERIOD_CLOSED, FINANCIAL_PERIOD_CLOSED_SCHEMA, "financial_period.closed");
event_type!(
    /// (L3-F-054).
    FINANCIAL_PERIOD_REOPENED, FINANCIAL_PERIOD_REOPENED_SCHEMA, "financial_period.reopened");
event_type!(
    /// (L3-F-053).
    FINANCIAL_YEAR_CLOSED, FINANCIAL_YEAR_CLOSED_SCHEMA, "financial_year.closed");
event_type!(
    /// (L3-F-056).
    BANK_ACCOUNT_ACTIVATED, BANK_ACCOUNT_ACTIVATED_SCHEMA, "bank_account.activated");
event_type!(
    /// (L3-F-058).
    BANK_STATEMENT_RECONCILED, BANK_STATEMENT_RECONCILED_SCHEMA, "bank_statement.reconciled");
event_type!(
    /// (L3-F-060).
    FUND_TRANSFER_SETTLED, FUND_TRANSFER_SETTLED_SCHEMA, "fund_transfer.settled");
event_type!(
    /// Failed / returned / uncertain bank transaction (L3-F-061).
    BANK_TRANSACTION_FAILED, BANK_TRANSACTION_FAILED_SCHEMA, "bank_transaction.failed");

/// Event types of this slice.
pub const EVENT_TYPES: &[&str] = &[
    MANUAL_JOURNAL_SUBMITTED,
    MANUAL_JOURNAL_POSTED,
    ACCOUNTING_BATCH_POSTED,
    LEDGER_RECONCILIATION_COMPLETED,
    TRIAL_BALANCE_GENERATED,
    FINANCIAL_PERIOD_OPENED,
    FINANCIAL_PERIOD_LOCKED,
    FINANCIAL_PERIOD_CLOSED,
    FINANCIAL_PERIOD_REOPENED,
    FINANCIAL_YEAR_CLOSED,
    BANK_ACCOUNT_ACTIVATED,
    BANK_STATEMENT_RECONCILED,
    FUND_TRANSFER_SETTLED,
    BANK_TRANSACTION_FAILED,
];
