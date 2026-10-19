//! No-op adapters of the external-integration ports Domain C calls (foundation-owned).
//! Real adapters (ABC / APAAR, NSP, …) land in `sutra-infra` in a later batch.

use sutra_contracts::student::ports::{
    AbcCreditSubmission, AbcRegistryPort, ExternalScholarshipPort, ExternalScholarshipSync,
    ExternalSyncOutcome,
};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;

/// Academic Bank of Credits: always [`ExternalSyncOutcome::NotConfigured`].
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopAbcRegistry;

#[async_trait::async_trait]
impl AbcRegistryPort for NoopAbcRegistry {
    async fn submit_credits(
        &self,
        _scope: &ExecutionScope,
        submission: AbcCreditSubmission,
    ) -> Result<ExternalSyncOutcome, KernelError> {
        tracing::debug!(
            student = %submission.student_id,
            entries = submission.ledger_entry_ids.len(),
            "ABC registry not configured; submission skipped"
        );
        Ok(ExternalSyncOutcome::NotConfigured)
    }
}

/// External scholarship ecosystems: always [`ExternalSyncOutcome::NotConfigured`].
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopExternalScholarships;

#[async_trait::async_trait]
impl ExternalScholarshipPort for NoopExternalScholarships {
    async fn synchronise(
        &self,
        _scope: &ExecutionScope,
        request: ExternalScholarshipSync,
    ) -> Result<ExternalSyncOutcome, KernelError> {
        tracing::debug!(
            scheme = %request.external_scheme,
            students = request.student_ids.len(),
            "external scholarship ecosystem not configured; sync skipped"
        );
        Ok(ExternalSyncOutcome::NotConfigured)
    }
}
