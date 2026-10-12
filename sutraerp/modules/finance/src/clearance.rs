//! Batch 3 stub of [`FinanceClearancePort`] (Finance itself lands in Batch 5).
//!
//! Always reports [`FinanceEvidence::NotApplicable`]: no Finance obligation is configured yet,
//! so Domain C's admission clearance, finance-sourced registration holds and exit clearance
//! proceed on their own policy. Batch 5 replaces this with the real F01/F02 evidence reader.

use sutra_contracts::finance::{
    AdmissionFinanceQuery, FinanceClearancePort, FinanceEvidence, StudentFinanceQuery,
};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;

#[derive(Clone, Copy, Debug, Default)]
pub struct StubFinanceClearance;

#[async_trait::async_trait]
impl FinanceClearancePort for StubFinanceClearance {
    async fn admission_clearance(
        &self,
        _scope: &ExecutionScope,
        _query: AdmissionFinanceQuery,
    ) -> Result<FinanceEvidence, KernelError> {
        Ok(FinanceEvidence::NotApplicable)
    }

    async fn student_dues(
        &self,
        _scope: &ExecutionScope,
        _query: StudentFinanceQuery,
    ) -> Result<FinanceEvidence, KernelError> {
        Ok(FinanceEvidence::NotApplicable)
    }
}
