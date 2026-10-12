//! **Foundation-owned** building blocks shared by the Batch 3 slices (3A/3B/3C).
//!
//! Slices *use* these; they do not edit them. A change here is a contract change: propose it in
//! the slice PR description (see `docs/batch3-contracts.md` §2.2).
//!
//! * [`state`] — the one handler state every Domain C router shares (pool, ports, approvals).
//! * [`http`] — ETag / `If-Match` / actor helpers (same conventions as Domain B).
//! * [`scope`] — institution-bound execution scope from the session tenant + an institution id.
//! * [`guard`] — Domain B institution status check (through the reference port) and the
//!   "student writable" rule.
//! * [`student`] — row types + loaders of the shared `student` / `student_context` tables,
//!   and self-service resolution (`me`).
//! * [`lifecycle`] — the **only** code that changes `student.status` / `student_context.status`
//!   (legal transition check, history row, `student.status_changed` event).
//! * [`publish`] — outbox + audit in the owner-local transaction (ADR-L5-0006/0007).
//! * [`approval`] — WorkflowPort approval steps (auto-approve in Batch 3).
//! * [`time`] — business-local "today" in the institution timezone.

pub mod approval;
pub mod guard;
pub mod http;
pub mod lifecycle;
pub mod publish;
pub mod scope;
pub mod state;
pub mod student;
pub mod time;

pub use state::StudentState;

/// Context of the caller performing a mutation.
#[derive(Debug, Clone, Default)]
pub struct Actor {
    pub id: Option<String>,
    pub correlation_id: Option<String>,
}

impl Actor {
    /// The system actor used for automatic steps (auto-approval, expiry sweeps).
    pub fn system(correlation_id: Option<String>) -> Self {
        Self {
            id: Some(approval::AUTO_APPROVER.to_owned()),
            correlation_id,
        }
    }
}
