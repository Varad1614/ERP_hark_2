//! **Foundation-owned** building blocks shared by the Batch 5 slices (5A/5B/5C).
//!
//! Slices *use* these; they do not edit them. A change here is a contract change: propose it in
//! the slice PR description (see `docs/batch5-contracts.md` §2.2). **Exception:** slice 5C
//! replaces the *body* of [`period::require_open`] (signature frozen).
//!
//! * [`state`] — the one handler state every Domain F router shares (pool, ports, approvals).
//! * [`http`] — ETag / `If-Match` / actor helpers (same conventions as Domains B, C and D).
//! * [`scope`] — institution-bound execution scope from the session tenant + an institution id.
//! * [`guard`] — institution writable rule.
//! * [`money`] — decimal parsing / validation / formatting (NUMERIC(18,2), INR, no floats).
//! * [`numbering`] — gap-free per-institution document numbers (`CHG-000001`, `JE-…`, `RCT-…`).
//! * [`coa`] — the default chart of accounts seed + account resolution (`SystemAccount`).
//! * [`account`] — finance party accounts (`ensure_student_account`, `ensure_applicant_account`).
//! * [`charge`] — charge rows, loaders, `insert_charge`, and the only writers of
//!   `settled_amount` ([`charge::apply_settlement`], 5B) and `adjustment_amount`
//!   ([`charge::apply_adjustment`], 5A).
//! * [`lifecycle`] — the **only** code that changes a charge's status (5A; plus the automatic
//!   `published → active` / `closed → active` moves inside `apply_settlement`).
//! * [`posting`] — the **only** write path into the general ledger: balanced, idempotent,
//!   period-guarded double-entry postings and reversals (`journal_entry.posted`).
//! * [`period`] — the period guard (stub: always open; 5C fills it).
//! * [`me`] — student self-service resolution from the session subject.
//! * [`publish`] — outbox + audit in the owner-local transaction (ADR-L5-0006/0007).
//! * [`approval`] — WorkflowPort approval steps (auto-approve in Batch 5).
//! * [`time`] — business-local "today" in the institution timezone.

pub mod account;
pub mod approval;
pub mod charge;
pub mod coa;
pub mod guard;
pub mod http;
pub mod lifecycle;
pub mod me;
pub mod money;
pub mod numbering;
pub mod period;
pub mod posting;
pub mod publish;
pub mod scope;
pub mod state;
pub mod time;

pub use state::FinanceState;

/// Context of the caller performing a mutation.
#[derive(Debug, Clone, Default)]
pub struct Actor {
    pub id: Option<String>,
    pub correlation_id: Option<String>,
}

impl Actor {
    /// The system actor used for automatic steps (auto-approval, sweeps, webhooks, dunning runs).
    pub fn system(correlation_id: Option<String>) -> Self {
        Self {
            id: Some(approval::AUTO_APPROVER.to_owned()),
            correlation_id,
        }
    }
}
