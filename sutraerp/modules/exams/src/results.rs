//! Batch 3 stub of [`ResultsPort`] (Examinations lands in Batch 6): no result is ever
//! published, so Domain C progression records its evidence manually until then.

use sutra_contracts::exams::{ResultsPort, StudentTermResultQuery, TermResultSummary};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;

#[derive(Clone, Copy, Debug, Default)]
pub struct NoPublishedResults;

#[async_trait::async_trait]
impl ResultsPort for NoPublishedResults {
    async fn term_result(
        &self,
        _scope: &ExecutionScope,
        _query: StudentTermResultQuery,
    ) -> Result<Option<TermResultSummary>, KernelError> {
        Ok(None)
    }
}
