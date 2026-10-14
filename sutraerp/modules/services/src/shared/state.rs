//! Shared handler state of every Domain H router.

use crate::shared::approval::ApprovalMode;
use sqlx::PgPool;
use std::sync::Arc;
use sutra_contracts::institution::reference::InstitutionReferencePort;
use sutra_contracts::platform::workflow::WorkflowPort;
use sutra_contracts::services::ports::{MetricValue, OperationalMetricsPort};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};

/// Handler state: the owner's pool plus every port Domain H calls. Cheap to clone.
#[derive(Clone)]
pub struct ServicesState {
    pub pool: PgPool,
    /// Approval steps (ADR-L5-0023; `NoopWorkflowAdapter` in Batch 8).
    pub workflow: Arc<dyn WorkflowPort>,
    pub approvals: ApprovalMode,
    /// Domain B reference data (institution + timezone).
    pub references: Arc<dyn InstitutionReferencePort>,
    /// One `OperationalMetricsPort` per domain (student, people, finance, campus, resources).
    pub metrics: Arc<[Arc<dyn OperationalMetricsPort>]>,
}

impl ServicesState {
    /// Every figure of every domain for one institution, in port order (the only way 8C reads
    /// other domains).
    pub async fn all_metrics(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
    ) -> Result<Vec<MetricValue>, KernelError> {
        let mut out = Vec::new();
        for port in self.metrics.iter() {
            out.extend(port.metrics(scope, institution_id).await?);
        }
        Ok(out)
    }
}
