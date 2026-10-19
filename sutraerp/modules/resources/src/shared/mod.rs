//! **Foundation-owned** building blocks of Domain J (Batch 8, slice 8D). Slices *use* these;
//! they do not edit them (`docs/batch8-contracts.md` §2.2).
//!
//! * [`state`] — handler state (pool, ports incl. Finance `ProcurementPostingPort`).
//! * [`stock`] — the **only** writer of the append-only stock ledger + on-hand projection.
//! * [`http`], [`scope`], [`guard`], [`numbering`] (`SUP-`, `PR-`, `PO-`, `GRN-` …),
//!   [`lifecycle`] (`resources.status_history`), [`decimal`], [`publish`], [`approval`],
//!   [`time`] — same conventions as Domains B…G.

pub mod approval;
pub mod decimal;
pub mod guard;
pub mod http;
pub mod lifecycle;
pub mod numbering;
pub mod publish;
pub mod scope;
pub mod state;
pub mod stock;
pub mod time;

pub use state::ResourcesState;

/// Context of the caller performing a mutation.
#[derive(Debug, Clone, Default)]
pub struct Actor {
    pub id: Option<String>,
    pub correlation_id: Option<String>,
}

impl Actor {
    /// The system actor used for automatic steps (auto-approval, replenishment sweeps).
    pub fn system(correlation_id: Option<String>) -> Self {
        Self {
            id: Some(approval::AUTO_APPROVER.to_owned()),
            correlation_id,
        }
    }
}
