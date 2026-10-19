//! The Batch 3 [`ResultsPort`] Domain C reads (progression evidence).
//!
//! Batch 3 progression (3B) reads published term results through [`ResultsPort`]. Since the
//! Batch 6 foundation the composition root wires `sutra_exams::PgResultsPort` over
//! `exams.term_result` (only `published` rows are visible); before 6C publishes anything it
//! answers `None`, exactly like the former `NoPublishedResults` stub, so 3B keeps recording the
//! evidence manually. Domain C never imports the exams crate. Re-evaluation outcomes (L3-C-038)
//! arrive as a new `revision` of the same summary (and as `term_result.revised` events).
//! The summary keeps its Batch 3 `f64` members (a frozen contract); Exams computes with
//! `rust_decimal` and converts only at this boundary.

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
