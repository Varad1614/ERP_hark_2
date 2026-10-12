//! Shared handler state of every Domain B router.

use crate::shared::approval::ApprovalMode;
use sqlx::PgPool;
use std::sync::Arc;
use sutra_contracts::platform::workflow::WorkflowPort;
use sutra_kernel::cache::AdvisoryCache;

/// Handler state: the owner's pool, the advisory read-through cache (ADR-L5-0037) and the
/// workflow port for approval steps (ADR-L5-0023). Cheap to clone.
#[derive(Clone)]
pub struct InstitutionState {
    pub pool: PgPool,
    pub cache: AdvisoryCache,
    pub workflow: Arc<dyn WorkflowPort>,
    pub approvals: ApprovalMode,
}

impl InstitutionState {
    /// State with the Batch 2 defaults: auto-approving approval steps.
    pub fn new(pool: PgPool, cache: AdvisoryCache, workflow: Arc<dyn WorkflowPort>) -> Self {
        Self {
            pool,
            cache,
            workflow,
            approvals: ApprovalMode::AutoApprove,
        }
    }
}
