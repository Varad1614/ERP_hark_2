//! Shared handler state of every Domain G router.

use crate::shared::approval::ApprovalMode;
use sqlx::PgPool;
use std::sync::Arc;
use sutra_contracts::finance::ports::{CampusChargePort, ChargeDirectory};
use sutra_contracts::institution::reference::InstitutionReferencePort;
use sutra_contracts::people::ports::WorkforceDirectory;
use sutra_contracts::platform::workflow::WorkflowPort;
use sutra_contracts::student::ports::StudentDirectory;

/// Handler state: the owner's pool plus every port Domain G calls. Cheap to clone.
#[derive(Clone)]
pub struct CampusState {
    pub pool: PgPool,
    /// Approval steps (ADR-L5-0023; `NoopWorkflowAdapter` in Batch 8).
    pub workflow: Arc<dyn WorkflowPort>,
    pub approvals: ApprovalMode,
    /// Domain B reference data (institution + timezone, campuses).
    pub references: Arc<dyn InstitutionReferencePort>,
    /// Domain C students (residents, passengers, borrowers, requesters).
    pub students: Arc<dyn StudentDirectory>,
    /// Domain E staff records (staff residents, passengers, borrowers, requesters).
    pub workforce: Arc<dyn WorkforceDirectory>,
    /// Domain F charges: library fines (8C), hostel fees / damage and transport pass fees (8B).
    pub charges: Arc<dyn CampusChargePort>,
    /// Domain F charge reads: settlement of the charges raised above (`outstanding_amount == 0`
    /// = paid) for clearance / vacating checks (8B, 8C).
    pub finance: Arc<dyn ChargeDirectory>,
}
