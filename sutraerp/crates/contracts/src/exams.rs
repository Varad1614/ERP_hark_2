//! Domain I — Examinations contracts.
//!
//! Examinations lands in Batch 6. Batch 3 progression (3B) reads published term results through
//! [`ResultsPort`]; until Batch 6 the composition root wires `sutra_exams::NoPublishedResults`,
//! which always answers `None` — 3B then records the progression evidence manually (an
//! administrator enters the outcome and credits with a reason). Domain C never imports the exams
//! crate. Re-evaluation outcomes (L3-C-038) arrive later as Exams events; 3B exposes a manual
//! revision command in Batch 3.

use chrono::{DateTime, Utc};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use uuid::Uuid;

/// Query for one student's published result in one academic term.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StudentTermResultQuery {
    pub institution_id: InstitutionId,
    pub student_id: Uuid,
    pub academic_term_id: Uuid,
}

/// Published, Exams-owned result summary (no marks: Domain C only needs the outcome facts).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TermResultSummary {
    pub student_id: Uuid,
    pub academic_term_id: Uuid,
    pub sgpa: Option<f64>,
    pub cgpa: Option<f64>,
    pub credits_attempted: f64,
    pub credits_earned: f64,
    /// Curriculum components not passed (backlogs).
    pub failed_component_ids: Vec<Uuid>,
    /// Increments on re-evaluation / revision.
    pub revision: i32,
    pub published_at: DateTime<Utc>,
}

/// Read port over published Exams results.
#[async_trait::async_trait]
pub trait ResultsPort: Send + Sync + 'static {
    async fn term_result(
        &self,
        scope: &ExecutionScope,
        query: StudentTermResultQuery,
    ) -> Result<Option<TermResultSummary>, KernelError>;
}
