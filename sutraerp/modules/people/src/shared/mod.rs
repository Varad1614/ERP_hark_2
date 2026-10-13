//! **Foundation-owned** building blocks shared by the Batch 7 slices (7A/7B/7C).
//!
//! Slices *use* these; they do not edit them. A change here is a contract change: propose it in
//! the slice PR description (see `docs/batch7-contracts.md` §2.2).
//!
//! * [`state`] — the one handler state every Domain E router shares (pool, ports, approvals).
//! * [`http`] — ETag / `If-Match` / actor helpers (same conventions as Domains B, C, D, F and I).
//! * [`scope`] — institution-bound execution scope from the session tenant + an institution id.
//! * [`guard`] — institution writable rule; live-engagement rule.
//! * [`numbering`] — gap-free per-institution document numbers (`EMP-000001`, `LV-…`, `PS-…`).
//! * [`rows`] — row decoders of the foundation tables (staff records, engagements) + locks.
//! * [`lifecycle`] — the **only** writers of staff / engagement status, plus the append-only
//!   `people.status_history` for every aggregate.
//! * [`staff`] — the **only** inserters of staff records and engagements.
//! * [`leave`] — the **only** writer of the append-only leave ledger (+ balance projection).
//! * [`attendance`] — the **only** writer of attendance days.
//! * [`decimal`] — money / days rounding and string formatting (`rust_decimal`).
//! * [`me`] — staff self-service resolution from the session subject.
//! * [`publish`] — outbox + audit in the owner-local transaction (ADR-L5-0006/0007).
//! * [`approval`] — WorkflowPort approval steps (auto-approve in Batch 7).
//! * [`time`] — business-local "today" in the institution timezone.

pub mod approval;
pub mod attendance;
pub mod decimal;
pub mod guard;
pub mod http;
pub mod leave;
pub mod lifecycle;
pub mod me;
pub mod numbering;
pub mod publish;
pub mod rows;
pub mod scope;
pub mod staff;
pub mod state;
pub mod time;

pub use state::PeopleState;

/// Context of the caller performing a mutation.
#[derive(Debug, Clone, Default)]
pub struct Actor {
    pub id: Option<String>,
    pub correlation_id: Option<String>,
}

impl Actor {
    /// The system actor used for automatic steps (auto-approval, accrual runs, sweeps).
    pub fn system(correlation_id: Option<String>) -> Self {
        Self {
            id: Some(approval::AUTO_APPROVER.to_owned()),
            correlation_id,
        }
    }
}
