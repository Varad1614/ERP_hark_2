//! [`OperationalMetricsPort`] adapter of Domain G (foundation): campus figures for Domain H
//! dashboards and regulatory snapshots, read from the `campus` schema only. Slices may ask the
//! integrator to add figures of their own tables here (contract change, §6.2).

use rust_decimal::Decimal;
use sqlx::PgPool;
use sutra_contracts::services::ports::{MetricValue, OperationalMetricsPort};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;

#[derive(Clone)]
pub struct PgCampusMetrics {
    pool: PgPool,
}

impl PgCampusMetrics {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl OperationalMetricsPort for PgCampusMetrics {
    fn domain(&self) -> &'static str {
        "campus"
    }

    async fn metrics(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
    ) -> Result<Vec<MetricValue>, KernelError> {
        let t = scope.tenant_id.as_uuid();
        let i = institution_id.as_uuid();
        let (spaces, seats): (i64, Option<i64>) = sqlx::query_as(
            "SELECT count(*), sum(capacity)::bigint FROM campus.infrastructure_node \
             WHERE tenant_id = $1 AND institution_id = $2 AND node_type = 'space' AND status = 'active'",
        )
        .bind(t)
        .bind(i)
        .fetch_one(&self.pool)
        .await
        .map_err(map_db_error)?;
        let buildings: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM campus.infrastructure_node WHERE tenant_id = $1 \
             AND institution_id = $2 AND node_type = 'building' AND status = 'active'",
        )
        .bind(t)
        .bind(i)
        .fetch_one(&self.pool)
        .await
        .map_err(map_db_error)?;
        let (open, breached): (i64, i64) = sqlx::query_as(
            "SELECT count(*), count(*) FILTER (WHERE sla_breached OR sla_due_at < now()) \
             FROM campus.service_ticket WHERE tenant_id = $1 AND institution_id = $2 \
             AND status NOT IN ('closed', 'cancelled', 'merged')",
        )
        .bind(t)
        .bind(i)
        .fetch_one(&self.pool)
        .await
        .map_err(map_db_error)?;
        let mut out = vec![
            MetricValue::count("campus", "active_buildings", "Active buildings", buildings),
            MetricValue::count("campus", "active_spaces", "Active spaces", spaces),
            MetricValue::new(
                "campus",
                "seating_capacity",
                "Seating / bed capacity of active spaces",
                Decimal::from(seats.unwrap_or(0)),
                "count",
            ),
            MetricValue::count("campus", "open_tickets", "Open service tickets", open),
            MetricValue::count(
                "campus",
                "tickets_past_sla",
                "Open tickets past their SLA",
                breached,
            ),
        ];
        // 8C: library figures (contract change, append-only; own `campus` library tables).
        out.extend(library_metrics(&self.pool, t, i).await?);
        // Integration: facilities, hostel and transport figures (8A / 8B tables, own schema).
        out.extend(residence_metrics(&self.pool, t, i).await?);
        Ok(out)
    }
}

// 8C: library figures for G-154 operational reporting (contract change, append-only).
async fn library_metrics(
    pool: &PgPool,
    t: uuid::Uuid,
    i: uuid::Uuid,
) -> Result<Vec<MetricValue>, KernelError> {
    use sqlx::Row;
    let r = sqlx::query(
        "SELECT          (SELECT count(*) FROM campus.library_membership WHERE tenant_id = $1 AND institution_id = $2 AND status = 'active') AS members,          (SELECT count(*) FROM campus.catalogue_record WHERE tenant_id = $1 AND institution_id = $2 AND status = 'published') AS titles,          (SELECT count(*) FROM campus.library_copy WHERE tenant_id = $1 AND institution_id = $2 AND status <> 'withdrawn') AS copies,          (SELECT count(*) FROM campus.library_copy WHERE tenant_id = $1 AND institution_id = $2 AND status = 'available') AS available,          (SELECT count(*) FROM campus.library_loan WHERE tenant_id = $1 AND institution_id = $2 AND status IN ('active', 'overdue')) AS loans,          (SELECT count(*) FROM campus.library_loan WHERE tenant_id = $1 AND institution_id = $2 AND status = 'overdue') AS overdue,          (SELECT count(*) FROM campus.library_reservation WHERE tenant_id = $1 AND institution_id = $2 AND status IN ('queued', 'ready')) AS reservations,          (SELECT COALESCE(sum(amount), 0) FROM campus.library_fine WHERE tenant_id = $1 AND institution_id = $2           AND status IN ('assessed', 'waiver_requested', 'billed')) AS unsettled",
    )
    .bind(t)
    .bind(i)
    .fetch_one(pool)
    .await
    .map_err(map_db_error)?;
    let n = |c: &str| r.try_get::<i64, _>(c).map_err(map_db_error);
    Ok(vec![
        MetricValue::count(
            "campus",
            "library_members",
            "Active library members",
            n("members")?,
        ),
        MetricValue::count(
            "campus",
            "library_titles",
            "Published catalogue titles",
            n("titles")?,
        ),
        MetricValue::count(
            "campus",
            "library_copies",
            "Library copies in stock",
            n("copies")?,
        ),
        MetricValue::count(
            "campus",
            "library_copies_available",
            "Library copies on the shelf",
            n("available")?,
        ),
        MetricValue::count(
            "campus",
            "library_loans_active",
            "Library loans out",
            n("loans")?,
        ),
        MetricValue::count(
            "campus",
            "library_loans_overdue",
            "Overdue library loans",
            n("overdue")?,
        ),
        MetricValue::count(
            "campus",
            "library_reservations_waiting",
            "Library reservations waiting",
            n("reservations")?,
        ),
        MetricValue::new(
            "campus",
            "library_fines_unsettled",
            "Unsettled library fines (assessed or billed)",
            r.try_get::<Decimal, _>("unsettled").map_err(map_db_error)?,
            "inr",
        ),
    ])
}

// Integration (Batch 8): facilities (8A), hostel and transport (8B) figures for Domain H.
async fn residence_metrics(
    pool: &PgPool,
    t: uuid::Uuid,
    i: uuid::Uuid,
) -> Result<Vec<MetricValue>, KernelError> {
    use sqlx::Row;
    let r = sqlx::query(
        "SELECT \
         (SELECT count(*) FROM campus.space_reservation WHERE tenant_id = $1 AND institution_id = $2 \
          AND status IN ('requested', 'confirmed', 'rescheduled') AND reservation_date >= current_date) AS bookings, \
         (SELECT count(*) FROM campus.work_order WHERE tenant_id = $1 AND institution_id = $2 \
          AND status IN ('created', 'assigned', 'in_progress', 'on_hold', 'reopened')) AS work_orders, \
         (SELECT count(*) FROM campus.hostel_bed WHERE tenant_id = $1 AND institution_id = $2 \
          AND status <> 'out_of_service') AS beds, \
         (SELECT count(*) FROM campus.hostel_bed WHERE tenant_id = $1 AND institution_id = $2 \
          AND status = 'occupied') AS occupied, \
         (SELECT count(*) FROM campus.hostel_allocation WHERE tenant_id = $1 AND institution_id = $2 \
          AND status <> 'closed') AS residents, \
         (SELECT count(*) FROM campus.hostel_application WHERE tenant_id = $1 AND institution_id = $2 \
          AND status = 'waitlisted') AS waitlisted, \
         (SELECT count(*) FROM campus.transport_route WHERE tenant_id = $1 AND institution_id = $2 \
          AND status = 'active') AS routes, \
         (SELECT count(*) FROM campus.transport_pass WHERE tenant_id = $1 AND institution_id = $2 \
          AND status IN ('issued', 'active')) AS passes, \
         (SELECT count(*) FROM campus.vehicle WHERE tenant_id = $1 AND institution_id = $2 \
          AND status IN ('available', 'assigned')) AS vehicles",
    )
    .bind(t)
    .bind(i)
    .fetch_one(pool)
    .await
    .map_err(map_db_error)?;
    let n = |c: &str| r.try_get::<i64, _>(c).map_err(map_db_error);
    Ok(vec![
        MetricValue::count(
            "campus",
            "upcoming_reservations",
            "Upcoming room reservations",
            n("bookings")?,
        ),
        MetricValue::count(
            "campus",
            "open_work_orders",
            "Open maintenance work orders",
            n("work_orders")?,
        ),
        MetricValue::count(
            "campus",
            "hostel_beds",
            "Hostel beds in service",
            n("beds")?,
        ),
        MetricValue::count(
            "campus",
            "hostel_beds_occupied",
            "Hostel beds occupied",
            n("occupied")?,
        ),
        MetricValue::count(
            "campus",
            "hostel_residents",
            "Hostel residents",
            n("residents")?,
        ),
        MetricValue::count(
            "campus",
            "hostel_waiting_list",
            "Hostel applications on the waiting list",
            n("waitlisted")?,
        ),
        MetricValue::count(
            "campus",
            "transport_active_routes",
            "Active transport routes",
            n("routes")?,
        ),
        MetricValue::count(
            "campus",
            "transport_active_passes",
            "Transport passes in use",
            n("passes")?,
        ),
        MetricValue::count(
            "campus",
            "transport_vehicles_in_service",
            "Vehicles in service",
            n("vehicles")?,
        ),
    ])
}
