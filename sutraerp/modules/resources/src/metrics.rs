//! [`OperationalMetricsPort`] adapter of Domain J (foundation): resource figures for Domain H,
//! read from the `resources` schema only (foundation tables; 8D may ask the integrator to add
//! figures of its own tables — contract change, §6.2).

use rust_decimal::Decimal;
use sqlx::{PgPool, Row};
use sutra_contracts::services::ports::{MetricValue, OperationalMetricsPort};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;

#[derive(Clone)]
pub struct PgResourceMetrics {
    pool: PgPool,
}

impl PgResourceMetrics {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl OperationalMetricsPort for PgResourceMetrics {
    fn domain(&self) -> &'static str {
        "resources"
    }

    async fn metrics(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
    ) -> Result<Vec<MetricValue>, KernelError> {
        let t = scope.tenant_id.as_uuid();
        let i = institution_id.as_uuid();
        let items: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM resources.item WHERE tenant_id = $1 AND institution_id = $2 \
             AND status = 'active'",
        )
        .bind(t)
        .bind(i)
        .fetch_one(&self.pool)
        .await
        .map_err(map_db_error)?;
        let stores: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM resources.store_location WHERE tenant_id = $1 \
             AND institution_id = $2 AND parent_id IS NULL AND status = 'active'",
        )
        .bind(t)
        .bind(i)
        .fetch_one(&self.pool)
        .await
        .map_err(map_db_error)?;
        let below: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM resources.item it WHERE it.tenant_id = $1 AND it.institution_id = $2 \
             AND it.status = 'active' AND it.reorder_level IS NOT NULL \
             AND COALESCE((SELECT sum(e.quantity) FROM resources.stock_ledger_entry e \
                           WHERE e.tenant_id = it.tenant_id AND e.item_id = it.id), 0) <= it.reorder_level",
        )
        .bind(t)
        .bind(i)
        .fetch_one(&self.pool)
        .await
        .map_err(map_db_error)?;
        // Integration (Batch 8): procurement, asset and payables figures (8D tables).
        let r = sqlx::query(
            "SELECT \
             (SELECT count(*) FROM resources.supplier WHERE tenant_id = $1 AND institution_id = $2 \
              AND status = 'active') AS suppliers, \
             (SELECT count(*) FROM resources.purchase_order WHERE tenant_id = $1 AND institution_id = $2 \
              AND status IN ('issued', 'acknowledged', 'partially_received')) AS open_pos, \
             (SELECT COALESCE(sum(total_amount), 0) FROM resources.purchase_order WHERE tenant_id = $1 \
              AND institution_id = $2 AND status IN ('issued', 'acknowledged', 'partially_received')) AS open_po_value, \
             (SELECT count(*) FROM resources.asset WHERE tenant_id = $1 AND institution_id = $2 \
              AND status <> 'retired') AS assets, \
             (SELECT COALESCE(sum(acquisition_cost), 0) FROM resources.asset WHERE tenant_id = $1 \
              AND institution_id = $2 AND status <> 'retired') AS asset_value, \
             (SELECT COALESCE(sum(total_amount - paid_amount), 0) FROM resources.vendor_invoice \
              WHERE tenant_id = $1 AND institution_id = $2 AND status = 'posted') AS payables, \
             (SELECT count(*) FROM resources.contract WHERE tenant_id = $1 AND institution_id = $2 \
              AND status = 'active') AS contracts, \
             (SELECT count(DISTINCT a.contract_id) FROM resources.contract_activity a \
              JOIN resources.contract c ON c.id = a.contract_id AND c.tenant_id = a.tenant_id \
              WHERE a.tenant_id = $1 AND a.institution_id = $2 AND c.kind IN ('mou_academic', \
              'mou_industry', 'mou_research') \
              AND c.status <> 'draft' AND a.held_on > current_date - 365) AS functional_mous",
        )
        .bind(t)
        .bind(i)
        .fetch_one(&self.pool)
        .await
        .map_err(map_db_error)?;
        let n = |c: &str| r.try_get::<i64, _>(c).map_err(map_db_error);
        let d = |c: &str| r.try_get::<Decimal, _>(c).map_err(map_db_error);
        Ok(vec![
            MetricValue::count("resources", "active_items", "Active items", items),
            MetricValue::count("resources", "active_stores", "Active stores", stores),
            MetricValue::count(
                "resources",
                "items_at_reorder_level",
                "Items at or below their reorder level",
                below,
            ),
            MetricValue::count(
                "resources",
                "active_suppliers",
                "Active suppliers",
                n("suppliers")?,
            ),
            MetricValue::count(
                "resources",
                "open_purchase_orders",
                "Open purchase orders",
                n("open_pos")?,
            ),
            MetricValue::new(
                "resources",
                "open_purchase_order_value",
                "Value of open purchase orders",
                d("open_po_value")?,
                "inr",
            ),
            MetricValue::count(
                "resources",
                "assets_in_use",
                "Registered assets",
                n("assets")?,
            ),
            MetricValue::new(
                "resources",
                "asset_value",
                "Acquisition cost of registered assets",
                d("asset_value")?,
                "inr",
            ),
            MetricValue::new(
                "resources",
                "payables_outstanding",
                "Vendor invoices posted and unpaid",
                d("payables")?,
                "inr",
            ),
            // ERP_EXTENSIONS 3C: contracts and MoUs (NAAC 3.7 evidence).
            MetricValue::count(
                "resources",
                "active_contracts",
                "Active contracts and MoUs",
                n("contracts")?,
            ),
            MetricValue::count(
                "resources",
                "functional_mous",
                "MoUs with an activity in the last 12 months",
                n("functional_mous")?,
            ),
        ])
    }
}
