//! [`OperationalMetricsPort`] adapter of Domain F (Batch 8 foundation, contract change recorded
//! in `docs/batch8-contracts.md` §6.2; Batch 5 deferral F-076 KPIs and dashboards): finance
//! figures for Domain H dashboards and regulatory snapshots, read from the `finance` schema only.

use rust_decimal::Decimal;
use sqlx::PgPool;
use sutra_contracts::services::ports::{MetricValue, OperationalMetricsPort};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;

#[derive(Clone)]
pub struct PgFinanceMetrics {
    pool: PgPool,
}

impl PgFinanceMetrics {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl OperationalMetricsPort for PgFinanceMetrics {
    fn domain(&self) -> &'static str {
        "finance"
    }

    async fn metrics(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
    ) -> Result<Vec<MetricValue>, KernelError> {
        let t = scope.tenant_id.as_uuid();
        let i = institution_id.as_uuid();
        let (open_charges, outstanding): (i64, Option<Decimal>) = sqlx::query_as(
            "SELECT count(*) FILTER (WHERE outstanding_amount > 0), sum(outstanding_amount) \
             FROM finance.charge WHERE tenant_id = $1 AND institution_id = $2 \
             AND status IN ('published', 'active', 'suspended', 'disputed')",
        )
        .bind(t)
        .bind(i)
        .fetch_one(&self.pool)
        .await
        .map_err(map_db_error)?;
        let collected: Option<Decimal> = sqlx::query_scalar(
            "SELECT sum(amount) FROM finance.payment WHERE tenant_id = $1 AND institution_id = $2 \
             AND status = 'completed'",
        )
        .bind(t)
        .bind(i)
        .fetch_one(&self.pool)
        .await
        .map_err(map_db_error)?;
        Ok(vec![
            MetricValue::count(
                "finance",
                "open_charges",
                "Charges with an outstanding balance",
                open_charges,
            ),
            MetricValue::new(
                "finance",
                "receivables_outstanding",
                "Receivables outstanding",
                outstanding.unwrap_or(Decimal::ZERO),
                "inr",
            ),
            MetricValue::new(
                "finance",
                "payments_collected",
                "Payments collected",
                collected.unwrap_or(Decimal::ZERO),
                "inr",
            ),
        ])
    }
}
