//! Domain F — Finance contracts.
//!
//! Finance lands in Batch 5. Batch 3 (Domain C) needs Finance-owned *facts* (admission financial
//! clearance evidence — L2-C01 amendment 2026-10-05 —, outstanding dues for registration holds
//! and exit clearance). They are read through [`FinanceClearancePort`]; until Batch 5 the
//! composition root wires `sutra_finance::StubFinanceClearance`, which reports
//! [`FinanceEvidence::NotApplicable`] ("no Finance obligation is configured"). Domain C never
//! imports the finance crate and never calculates money (BR-C01-025).
//!
//! Scholarship hand-off to Financial Aid (L3-C-071) is **event only**:
//! `sutraerp.student.scholarship_eligibility.handed_off`; Finance subscribes in Batch 5.

use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use uuid::Uuid;

/// Finance-owned evidence about one subject, as Finance publishes it. Domain C decides.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum FinanceEvidence {
    /// Every applicable obligation is covered (allocation/settlement verified by Finance).
    Cleared {
        /// Finance reference of the evidence (receipt / allocation id).
        reference: String,
    },
    /// Obligations exist and are not covered; `detail` is safe to show to staff.
    Outstanding { detail: String },
    /// Finance cannot state the fact yet (unknown settlement, reversal in progress, …).
    Pending { detail: String },
    /// No Finance obligation applies / is configured (the Batch 3 stub always says this).
    NotApplicable,
}

impl FinanceEvidence {
    /// `true` when the evidence allows a clearance decision (cleared or nothing applies).
    pub fn permits_clearance(&self) -> bool {
        matches!(self, Self::Cleared { .. } | Self::NotApplicable)
    }
}

/// Subject of an admission clearance query (same Person/Admission/Tenant/Institute, CE-004).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmissionFinanceQuery {
    pub institution_id: InstitutionId,
    pub admission_id: Uuid,
    pub programme_id: Uuid,
    /// Set once a Student exists (readmission).
    pub student_id: Option<Uuid>,
}

/// Subject of a student dues query (registration holds, exit clearance).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StudentFinanceQuery {
    pub institution_id: InstitutionId,
    pub student_id: Uuid,
}

/// Read port over Finance-owned clearance facts.
#[async_trait::async_trait]
pub trait FinanceClearancePort: Send + Sync + 'static {
    /// Evidence for an admission's financial clearance (3A, WF-C01-04).
    async fn admission_clearance(
        &self,
        scope: &ExecutionScope,
        query: AdmissionFinanceQuery,
    ) -> Result<FinanceEvidence, KernelError>;

    /// Evidence about a student's outstanding dues (3B finance holds, 3C exit clearance).
    async fn student_dues(
        &self,
        scope: &ExecutionScope,
        query: StudentFinanceQuery,
    ) -> Result<FinanceEvidence, KernelError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_cleared_or_not_applicable_permit_clearance() {
        assert!(FinanceEvidence::NotApplicable.permits_clearance());
        assert!(FinanceEvidence::Cleared {
            reference: "r".into()
        }
        .permits_clearance());
        assert!(!FinanceEvidence::Pending { detail: "d".into() }.permits_clearance());
        assert!(!FinanceEvidence::Outstanding { detail: "d".into() }.permits_clearance());
    }
}
