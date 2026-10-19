//! **Foundation-owned** building blocks shared by the Batch 6 slices (6A/6B/6C).
//!
//! Slices *use* these; they do not edit them. A change here is a contract change: propose it in
//! the slice PR description (see `docs/batch6-contracts.md` §2.2).
//!
//! * [`state`] — the one handler state every Domain I router shares (pool, ports, approvals).
//! * [`http`] — ETag / `If-Match` / actor helpers (same conventions as Domains B, C, D and F).
//! * [`scope`] — institution-bound execution scope from the session tenant + an institution id.
//! * [`guard`] — institution writable rule; operational-session rule (L2-I02 decision 4).
//! * [`numbering`] — gap-free per-institution document numbers (`EXR-000001`, `HT-…`, `GC-…`).
//! * [`rows`] — row decoders of the foundation tables (sessions, papers, registrations, marks).
//! * [`lifecycle`] — the **only** writers of session / paper / registration status, plus the
//!   append-only `exams.status_history` for every aggregate.
//! * [`registration`] — the **only** inserter of registrations / registered papers (creates the
//!   candidate's `paper_mark` rows at the same time).
//! * [`marks`] — the **only** writer of `exams.paper_mark` (6B; 6C via [`marks::revise_final`]),
//!   with the append-only mark history.
//! * [`results`] — the **only** writer of `exams.term_result(_line)` (6C).
//! * [`grading`] — pure grading / SGPA / CGPA arithmetic (`rust_decimal`, half-up to 2 places).
//! * [`me`] — student self-service resolution from the session subject.
//! * [`publish`] — outbox + audit in the owner-local transaction (ADR-L5-0006/0007).
//! * [`approval`] — WorkflowPort approval steps (auto-approve in Batch 6).
//! * [`time`] — business-local "today" in the institution timezone.

pub mod approval;
pub mod grading;
pub mod guard;
pub mod http;
pub mod lifecycle;
pub mod marks;
pub mod me;
pub mod numbering;
pub mod publish;
pub mod registration;
pub mod results;
pub mod rows;
pub mod scope;
pub mod state;
pub mod time;

pub use state::ExamsState;

/// Context of the caller performing a mutation.
#[derive(Debug, Clone, Default)]
pub struct Actor {
    pub id: Option<String>,
    pub correlation_id: Option<String>,
}

impl Actor {
    /// The system actor used for automatic steps (auto-approval, imports, sweeps).
    pub fn system(correlation_id: Option<String>) -> Self {
        Self {
            id: Some(approval::AUTO_APPROVER.to_owned()),
            correlation_id,
        }
    }
}
