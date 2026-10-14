//! **Foundation-owned** building blocks of Domain H (Batch 8, slice 8C reports). Slices *use*
//! these; they do not edit them (`docs/batch8-contracts.md` §2.2).
//!
//! * [`state`] — handler state (pool, ports incl. every domain's `OperationalMetricsPort`).
//! * [`http`], [`scope`], [`guard`], [`numbering`] (`RC-…`, `SUB-…`), [`lifecycle`]
//!   (`services.status_history`), [`decimal`], [`publish`], [`approval`], [`time`] — same
//!   conventions as Domains B…G.

pub mod approval;
pub mod decimal;
pub mod guard;
pub mod http;
pub mod lifecycle;
pub mod numbering;
pub mod publish;
pub mod scope;
pub mod state;
pub mod time;

pub use state::ServicesState;

/// Context of the caller performing a mutation.
#[derive(Debug, Clone, Default)]
pub struct Actor {
    pub id: Option<String>,
    pub correlation_id: Option<String>,
}

impl Actor {
    /// The system actor used for automatic steps (auto-approval, scheduled snapshots).
    pub fn system(correlation_id: Option<String>) -> Self {
        Self {
            id: Some(approval::AUTO_APPROVER.to_owned()),
            correlation_id,
        }
    }
}
