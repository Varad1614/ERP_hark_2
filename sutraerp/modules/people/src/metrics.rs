//! [`OperationalMetricsPort`] adapter of Domain E (Batch 8 foundation, contract change recorded
//! in `docs/batch8-contracts.md` §6.2): workforce figures for Domain H dashboards and regulatory
//! snapshots (AISHE / NIRF staff counts), read from the `people` schema only.

use sqlx::PgPool;
use sutra_contracts::services::ports::{MetricValue, OperationalMetricsPort};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;

#[derive(Clone)]
pub struct PgPeopleMetrics {
    pool: PgPool,
}

impl PgPeopleMetrics {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl OperationalMetricsPort for PgPeopleMetrics {
    fn domain(&self) -> &'static str {
        "people"
    }

    async fn metrics(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
    ) -> Result<Vec<MetricValue>, KernelError> {
        let (working, teaching): (i64, i64) = sqlx::query_as(
            "SELECT count(DISTINCT staff_id), \
             count(DISTINCT staff_id) FILTER (WHERE staff_category = 'teaching') \
             FROM people.staff_engagement WHERE tenant_id = $1 AND institution_id = $2 \
             AND status = 'active'",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(institution_id.as_uuid())
        .fetch_one(&self.pool)
        .await
        .map_err(map_db_error)?;
        Ok(vec![
            MetricValue::count(
                "people",
                "working_staff",
                "Staff with an active engagement",
                working,
            ),
            MetricValue::count(
                "people",
                "teaching_staff",
                "Teaching staff (active)",
                teaching,
            ),
        ])
    }
}
