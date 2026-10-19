//! The **only** writers of the shared staff / engagement status columns (contract doc §3.3), plus
//! the append-only `people.status_history` every slice uses for its own aggregates.
//!
//! Inside the caller's owner-local transaction and on a row the caller locked
//! (`shared::rows::lock_*`), each `change_*` function:
//!
//! 1. checks the legal transition (`can_transition` of the shared machine in
//!    `sutra_contracts::people::status`) → `409 conflict` otherwise;
//! 2. updates status, reason (and `process_instance_id` when given), bumps `version` (optimistic:
//!    the row's version must still match);
//! 3. appends a `people.status_history` row.
//!
//! The slice still emits its own business event (e.g. `staff_engagement.activated`) and audit
//! entry in the same transaction. Who may call which move is fixed in the contract (§3.3): staff
//! records — 7A; engagements — 7A every move, 7B only `active → long_leave → active`.

use crate::shared::rows::{engagement_from_row, staff_from_row, ENGAGEMENT_COLUMNS, STAFF_COLUMNS};
use crate::shared::Actor;
use sqlx::PgConnection;
use sutra_contracts::people::ports::{EngagementSummary, StaffSummary};
use sutra_contracts::people::status::{EngagementStatus, StaffStatus};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Why a status changes.
#[derive(Clone, Debug, Default)]
pub struct StatusChange<'a> {
    /// The slice business event type that causes the change (e.g. `ENGAGEMENT_ACTIVATED`).
    pub cause_event_type: &'static str,
    pub reason: Option<&'a str>,
    pub process_instance_id: Option<&'a str>,
}

/// One status history entry of any Domain E aggregate.
pub struct HistoryEntry<'a> {
    pub tenant: TenantId,
    pub institution: InstitutionId,
    /// Table / aggregate name (`staff_engagement`, `leave_request`, `payroll_run`, …).
    pub aggregate_type: &'static str,
    pub aggregate_id: Uuid,
    pub from: Option<&'a str>,
    pub to: &'a str,
}

/// Appends a `people.status_history` row (append-only). Slices use it for their own
/// aggregates' transitions too.
pub async fn record_history(
    conn: &mut PgConnection,
    h: HistoryEntry<'_>,
    change: &StatusChange<'_>,
    actor: &Actor,
) -> Result<(), KernelError> {
    sqlx::query(
        "INSERT INTO people.status_history (tenant_id, institution_id, aggregate_type, \
         aggregate_id, from_status, to_status, reason, cause_event_type, process_instance_id, \
         actor, correlation_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(h.tenant.as_uuid())
    .bind(h.institution.as_uuid())
    .bind(h.aggregate_type)
    .bind(h.aggregate_id)
    .bind(h.from)
    .bind(h.to)
    .bind(change.reason)
    .bind(change.cause_event_type)
    .bind(change.process_instance_id)
    .bind(actor.id.as_deref())
    .bind(actor.correlation_id.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

fn conflict(what: &str, label: &str, from: &str, to: &str) -> KernelError {
    KernelError::Conflict(format!("{what} {label} is {from}; it cannot become {to}"))
}

/// Moves a locked staff record to `to` (`verified` also stamps `verified_at`).
pub async fn change_staff_status(
    conn: &mut PgConnection,
    tenant: TenantId,
    s: &StaffSummary,
    to: StaffStatus,
    change: &StatusChange<'_>,
    actor: &Actor,
) -> Result<StaffSummary, KernelError> {
    if !s.status.can_transition(to) {
        return Err(conflict(
            "staff record",
            &s.staff_number,
            s.status.as_str(),
            to.as_str(),
        ));
    }
    let row = sqlx::query(&format!(
        "UPDATE people.staff_member SET status = $3, status_reason = $4, status_changed_at = now(), \
         process_instance_id = COALESCE($5, process_instance_id), \
         verified_at = CASE WHEN $3 = 'verified' THEN now() ELSE verified_at END, \
         version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 AND version = $6 RETURNING {STAFF_COLUMNS}"
    ))
    .bind(s.id.as_uuid())
    .bind(tenant.as_uuid())
    .bind(to.as_str())
    .bind(change.reason)
    .bind(change.process_instance_id)
    .bind(s.version)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| KernelError::Conflict("staff record changed concurrently".into()))?;
    record_history(
        conn,
        HistoryEntry {
            tenant,
            institution: s.institution_id,
            aggregate_type: "staff_member",
            aggregate_id: s.id.as_uuid(),
            from: Some(s.status.as_str()),
            to: to.as_str(),
        },
        change,
        actor,
    )
    .await?;
    staff_from_row(&row).map_err(map_db_error)
}

/// Moves a locked engagement to `to`.
pub async fn change_engagement_status(
    conn: &mut PgConnection,
    tenant: TenantId,
    e: &EngagementSummary,
    to: EngagementStatus,
    change: &StatusChange<'_>,
    actor: &Actor,
) -> Result<EngagementSummary, KernelError> {
    if !e.status.can_transition(to) {
        return Err(conflict(
            "engagement",
            &e.engagement_number,
            e.status.as_str(),
            to.as_str(),
        ));
    }
    let row = sqlx::query(&format!(
        "UPDATE people.staff_engagement SET status = $3, status_reason = $4, \
         status_changed_at = now(), process_instance_id = COALESCE($5, process_instance_id), \
         version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 AND version = $6 RETURNING {ENGAGEMENT_COLUMNS}"
    ))
    .bind(e.id.as_uuid())
    .bind(tenant.as_uuid())
    .bind(to.as_str())
    .bind(change.reason)
    .bind(change.process_instance_id)
    .bind(e.version)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| KernelError::Conflict("engagement changed concurrently".into()))?;
    record_history(
        conn,
        HistoryEntry {
            tenant,
            institution: e.institution_id,
            aggregate_type: "staff_engagement",
            aggregate_id: e.id.as_uuid(),
            from: Some(e.status.as_str()),
            to: to.as_str(),
        },
        change,
        actor,
    )
    .await?;
    engagement_from_row(&row).map_err(map_db_error)
}
