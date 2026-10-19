//! **Foundation-owned** building blocks shared by the Batch 2 slices (2A/2B/2C).
//!
//! Slices *use* these; they do not edit them. A change here is a contract change: make it on
//! `batch2/foundation` (or propose it in the slice PR description) — see
//! `docs/batch2-contracts.md` §4.
//!
//! * [`state`] — the one handler state every Domain B router shares.
//! * [`http`] — ETag / `If-Match` / actor helpers (same conventions as the reference aggregate).
//! * [`scope`] — institution-bound execution scope from the session tenant + path id.
//! * [`guard`] — institution status lookup and the "structure editable" rule.
//! * [`publish`] — outbox + audit in the owner-local transaction (ADR-L5-0006/0007).
//! * [`approval`] — WorkflowPort approval steps (auto-approve in Batch 2).
//! * [`readiness`] — activation readiness item types + the aggregating `evaluate`.

pub mod approval;
pub mod guard;
pub mod http;
pub mod publish;
pub mod readiness;
pub mod scope;
pub mod state;

pub use state::InstitutionState;

/// Context of the caller performing a mutation.
#[derive(Debug, Clone, Default)]
pub struct Actor {
    pub id: Option<String>,
    pub correlation_id: Option<String>,
}

impl Actor {
    /// The system actor used for automatic steps (e.g. Batch 2 auto-approval).
    pub fn system(correlation_id: Option<String>) -> Self {
        Self {
            id: Some(approval::AUTO_APPROVER.to_owned()),
            correlation_id,
        }
    }
}
