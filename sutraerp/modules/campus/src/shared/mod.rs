//! **Foundation-owned** building blocks shared by the Batch 8 Campus slices (8A / 8B / 8C).
//!
//! Slices *use* these; they do not edit them. A change here is a contract change: propose it in
//! the slice PR description (see `docs/batch8-contracts.md` §2.2).
//!
//! * [`state`] — the one handler state every Domain G router shares (pool, ports, approvals).
//! * [`http`] — ETag / `If-Match` / actor helpers (same conventions as Domains B…F, I).
//! * [`scope`] — institution-bound execution scope from the session tenant + an institution id.
//! * [`guard`] — institution writable rule.
//! * [`numbering`] — gap-free per-institution document numbers (`SR-000001`, `WO-…`, `LN-…`).
//! * [`rows`] — row decoders + locks of the foundation tables (infrastructure nodes, tickets).
//! * [`infrastructure`] — the **only** inserter / status writer of infrastructure nodes (8A).
//! * [`tickets`] — the **only** opener (any slice) / status writer (8A) of service tickets.
//! * [`lifecycle`] — the append-only `campus.status_history` for every aggregate.
//! * [`me`] — self-service person resolution (student or staff) from the session subject.
//! * [`decimal`] — money rounding and string formatting (`rust_decimal`).
//! * [`publish`] — outbox + audit in the owner-local transaction (ADR-L5-0006/0007).
//! * [`approval`] — WorkflowPort approval steps (auto-approve in Batch 8).
//! * [`time`] — business-local "today" in the institution timezone.

pub mod approval;
pub mod decimal;
pub mod guard;
pub mod http;
pub mod infrastructure;
pub mod lifecycle;
pub mod me;
pub mod numbering;
pub mod publish;
pub mod rows;
pub mod scope;
pub mod state;
pub mod tickets;
pub mod time;

pub use state::CampusState;

/// Context of the caller performing a mutation.
#[derive(Debug, Clone, Default)]
pub struct Actor {
    pub id: Option<String>,
    pub correlation_id: Option<String>,
}

impl Actor {
    /// The system actor used for automatic steps (auto-approval, overdue sweeps, SLA checks).
    pub fn system(correlation_id: Option<String>) -> Self {
        Self {
            id: Some(approval::AUTO_APPROVER.to_owned()),
            correlation_id,
        }
    }
}
