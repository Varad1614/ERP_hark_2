//! [`ResultsPort`] adapters for Domain C (Batch 3 progression evidence).
//!
//! * [`PgResultsPort`] — **real since the Batch 6 foundation**: the student's current
//!   **published** term result of `exams.term_result` (6C writes it). Before anything is
//!   published it answers `None`, exactly like the former stub, so 3B keeps recording evidence
//!   manually. Decimals are converted to the frozen Batch 3 `f64` members only here.
//! * [`NoPublishedResults`] — the Batch 3 stub (always `None`), kept for tests.

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use sqlx::{PgPool, Row};
use sutra_contracts::exams::{ResultsPort, StudentTermResultQuery, TermResultSummary};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;
use sutra_persistence::map_db_error;
use uuid::Uuid;

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

#[derive(Clone)]
pub struct PgResultsPort {
    pool: PgPool,
}

impl PgResultsPort {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn f(d: Decimal) -> f64 {
    d.to_f64().unwrap_or_default()
}

#[async_trait::async_trait]
impl ResultsPort for PgResultsPort {
    async fn term_result(
        &self,
        scope: &ExecutionScope,
        query: StudentTermResultQuery,
    ) -> Result<Option<TermResultSummary>, KernelError> {
        let Some(r) = sqlx::query(
            "SELECT id, sgpa, cgpa, credits_attempted, credits_earned, revision, published_at \
             FROM exams.term_result WHERE tenant_id = $1 AND institution_id = $2 \
             AND student_id = $3 AND academic_term_id = $4 AND status = 'published' \
             ORDER BY published_at DESC NULLS LAST, revision DESC LIMIT 1",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(query.institution_id.as_uuid())
        .bind(query.student_id)
        .bind(query.academic_term_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_db_error)?
        else {
            return Ok(None);
        };
        let id: Uuid = r.try_get("id").map_err(map_db_error)?;
        let failed: Vec<Uuid> = sqlx::query_scalar(
            "SELECT curriculum_component_id FROM exams.term_result_line \
             WHERE result_id = $1 AND NOT passed ORDER BY line_no",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .map_err(map_db_error)?;
        let summary = (|| -> Result<TermResultSummary, sqlx::Error> {
            Ok(TermResultSummary {
                student_id: query.student_id,
                academic_term_id: query.academic_term_id,
                sgpa: r.try_get::<Option<Decimal>, _>("sgpa")?.map(f),
                cgpa: r.try_get::<Option<Decimal>, _>("cgpa")?.map(f),
                credits_attempted: f(r.try_get("credits_attempted")?),
                credits_earned: f(r.try_get("credits_earned")?),
                failed_component_ids: failed,
                revision: r.try_get("revision")?,
                published_at: r
                    .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("published_at")?
                    .unwrap_or_else(chrono::Utc::now),
            })
        })()
        .map_err(map_db_error)?;
        Ok(Some(summary))
    }
}
