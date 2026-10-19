//! [`OperationalMetricsPort`] adapter of Domain C (Batch 8 foundation, contract change recorded
//! in `docs/batch8-contracts.md` §6.2): student figures for Domain H dashboards and regulatory
//! snapshots (AISHE / NIRF enrolment), read from the `student` schema only.

use sqlx::PgPool;
use sutra_contracts::services::ports::{MetricValue, OperationalMetricsPort};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;

#[derive(Clone)]
pub struct PgStudentMetrics {
    pool: PgPool,
}

impl PgStudentMetrics {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl OperationalMetricsPort for PgStudentMetrics {
    fn domain(&self) -> &'static str {
        "student"
    }

    async fn metrics(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
    ) -> Result<Vec<MetricValue>, KernelError> {
        let (active, live): (i64, i64) = sqlx::query_as(
            "SELECT count(*) FILTER (WHERE status = 'active'), \
             count(*) FILTER (WHERE status IN ('pending_activation', 'active', 'suspended')) \
             FROM student.student_context WHERE tenant_id = $1 AND institution_id = $2",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(institution_id.as_uuid())
        .fetch_one(&self.pool)
        .await
        .map_err(map_db_error)?;
        Ok(vec![
            MetricValue::count("student", "active_students", "Active students", active),
            MetricValue::count(
                "student",
                "enrolled_students",
                "Enrolled students (pending, active or suspended)",
                live,
            ),
        ])
    }
}
