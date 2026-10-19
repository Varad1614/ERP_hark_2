//! Shared handler state of every Domain J router.

use crate::shared::approval::ApprovalMode;
use sqlx::PgPool;
use std::sync::Arc;
use sutra_contracts::finance::ports::ProcurementPostingPort;
use sutra_contracts::institution::reference::InstitutionReferencePort;
use sutra_contracts::people::ports::WorkforceDirectory;
use sutra_contracts::platform::workflow::WorkflowPort;

/// Handler state: the owner's pool plus every port Domain J calls. Cheap to clone.
#[derive(Clone)]
pub struct ResourcesState {
    pub pool: PgPool,
    /// Approval steps (ADR-L5-0023; `NoopWorkflowAdapter` in Batch 8).
    pub workflow: Arc<dyn WorkflowPort>,
    pub approvals: ApprovalMode,
    /// Domain B reference data (institution + timezone, org units, campuses).
    pub references: Arc<dyn InstitutionReferencePort>,
    /// Domain E staff records (asset custodians, requisition requesters).
    pub workforce: Arc<dyn WorkforceDirectory>,
    /// Domain F accounts payable: vendor invoices and payments (F-044 / 045 / 081 / 082).
    pub postings: Arc<dyn ProcurementPostingPort>,
    /// ERP_EXTENSIONS 3D: receipt reader for expense claims.
    pub ocr: Arc<dyn crate::expenses::ocr::ReceiptOcrPort>,
}
