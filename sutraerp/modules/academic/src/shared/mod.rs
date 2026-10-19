//! **Foundation-owned** building blocks shared by the Batch 4 slices (4A/4B/4C).
//!
//! Slices *use* these; they do not edit them. A change here is a contract change: propose it in
//! the slice PR description (see `docs/batch4-contracts.md` §2.2).
//!
//! * [`state`] — the one handler state every Domain D router shares (pool, ports, approvals).
//! * [`http`] — ETag / `If-Match` / actor helpers (same conventions as Domains B and C).
//! * [`scope`] — institution-bound execution scope from the session tenant + an institution id.
//! * [`guard`] — institution writable rule, offering changeable / deliverable gates.
//! * [`offering`] — row types + loaders of the shared delivery tables (offering, section,
//!   faculty assignment, section enrolment).
//! * [`lifecycle`] — the **only** code that changes the status of those four tables (4A).
//! * [`faculty`] — faculty-scoped access: `academic.teach` + an active assignment on the section.
//! * [`me`] — student self-service resolution from the session subject.
//! * [`publish`] — outbox + audit in the owner-local transaction (ADR-L5-0006/0007).
//! * [`approval`] — WorkflowPort approval steps (auto-approve in Batch 4).
//! * [`time`] — business-local "today" in the institution timezone.

pub mod approval;
pub mod faculty;
pub mod guard;
pub mod http;
pub mod lifecycle;
pub mod me;
pub mod offering;
pub mod publish;
pub mod scope;
pub mod state;
pub mod time;

pub use state::AcademicState;

/// Context of the caller performing a mutation.
#[derive(Debug, Clone, Default)]
pub struct Actor {
    pub id: Option<String>,
    pub correlation_id: Option<String>,
}

impl Actor {
    /// The system actor used for automatic steps (auto-approval, sweeps, threshold evaluation).
    pub fn system(correlation_id: Option<String>) -> Self {
        Self {
            id: Some(approval::AUTO_APPROVER.to_owned()),
            correlation_id,
        }
    }
}
