//! Shared handler state of every Domain C router.

use crate::shared::approval::ApprovalMode;
use sqlx::PgPool;
use std::sync::Arc;
use sutra_contracts::exams::ResultsPort;
use sutra_contracts::finance::FinanceClearancePort;
use sutra_contracts::institution::reference::InstitutionReferencePort;
use sutra_contracts::platform::documents::DocumentVault;
use sutra_contracts::platform::workflow::WorkflowPort;
use sutra_contracts::student::ports::{AbcRegistryPort, ExternalScholarshipPort};

/// Handler state: the owner's pool plus every port Domain C calls. Cheap to clone.
#[derive(Clone)]
pub struct StudentState {
    pub pool: PgPool,
    /// Approval steps (ADR-L5-0023; `NoopWorkflowAdapter` in Batch 3).
    pub workflow: Arc<dyn WorkflowPort>,
    pub approvals: ApprovalMode,
    /// Domain B reference data (institution, campus, programme, curriculum, terms).
    pub references: Arc<dyn InstitutionReferencePort>,
    /// Finance-owned clearance facts (stub until Batch 5).
    pub finance: Arc<dyn FinanceClearancePort>,
    /// Published Exams results (stub until Batch 6).
    pub results: Arc<dyn ResultsPort>,
    /// Academic Bank of Credits (no-op adapter).
    pub abc: Arc<dyn AbcRegistryPort>,
    /// External scholarship ecosystems (no-op adapter).
    pub external_scholarships: Arc<dyn ExternalScholarshipPort>,
    /// Part 2 · S4-docs: platform document service (files of admission / record documents).
    pub documents: Arc<dyn DocumentVault>,
}
