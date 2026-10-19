//! Shared handler state of every Domain E router.

use crate::shared::approval::ApprovalMode;
use sqlx::PgPool;
use std::sync::Arc;
use sutra_contracts::academic::ports::FacultyWorkloadPort;
use sutra_contracts::finance::ports::PayrollPostingPort;
use sutra_contracts::institution::reference::InstitutionReferencePort;
use sutra_contracts::people::ports::{LeaveReader, WorkforceDirectory};
use sutra_contracts::platform::workflow::WorkflowPort;

/// Handler state: the owner's pool plus every port Domain E calls. Cheap to clone.
#[derive(Clone)]
pub struct PeopleState {
    pub pool: PgPool,
    /// Approval steps (ADR-L5-0023; `NoopWorkflowAdapter` in Batch 7).
    pub workflow: Arc<dyn WorkflowPort>,
    pub approvals: ApprovalMode,
    /// Domain B reference data (institution + timezone, org units, campuses).
    pub references: Arc<dyn InstitutionReferencePort>,
    /// Staff records and engagements (foundation tables) — "7A → 7B / 7C".
    pub directory: Arc<dyn WorkforceDirectory>,
    /// Leave balances and payroll inputs (foundation ledger + attendance) — "7B → 7C".
    pub leave: Arc<dyn LeaveReader>,
    /// Domain D (4A) teaching load of a faculty member — 7B workload plans.
    pub workload: Arc<dyn FacultyWorkloadPort>,
    /// Domain F payroll liability postings — 7C finalisation / adjustments / settlements.
    pub postings: Arc<dyn PayrollPostingPort>,
}
