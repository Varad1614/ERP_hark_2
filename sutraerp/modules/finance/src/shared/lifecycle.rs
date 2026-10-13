//! The **only** writer of `finance.charge.status` (contract doc §3.3).
//!
//! Inside the caller's owner-local transaction and on a charge the caller locked
//! (`shared::charge::lock_charge`):
//!
//! 1. checks the legal transition (`ChargeStatus::can_transition`) → `409 conflict` otherwise;
//! 2. updates status, reason, `status_changed_at`, `process_instance_id` (when given) and
//!    `published_at` (on publication), bumps `version` (optimistic: the row's version must
//!    still match);
//! 3. appends a `finance.status_history` row.
//!
//! The slice still emits its own business event (e.g. `charge.published`) and audit entry in the
//! same transaction. Only slice **5A** calls this (plus `shared::charge::apply_settlement`'s
//! automatic `published → active` / `closed → active`). Slices record the status history of
//! their **own** aggregates with [`record_history`].

use crate::shared::charge::{charge_from_row, ChargeRow, CHARGE_COLUMNS};
use crate::shared::Actor;
use sqlx::PgConnection;
use sutra_contracts::finance::status::ChargeStatus;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Why a status changes.
#[derive(Clone, Debug, Default)]
pub struct StatusChange<'a> {
    /// The slice business event type that causes the change (e.g. `CHARGE_PUBLISHED`).
    pub cause_event_type: &'static str,
    pub reason: Option<&'a str>,
    pub process_instance_id: Option<&'a str>,
}

/// One status history entry of any Domain F aggregate.
pub struct HistoryEntry<'a> {
    pub tenant: TenantId,
    pub institution: InstitutionId,
    /// Table / aggregate name (`charge`, `refund`, `manual_journal`, …).
    pub aggregate_type: &'static str,
    pub aggregate_id: Uuid,
    pub from: Option<&'a str>,
    pub to: &'a str,
}

/// Appends a `finance.status_history` row (append-only). Slices use it for their own
/// aggregates' transitions too.
pub async fn record_history(
    conn: &mut PgConnection,
    h: HistoryEntry<'_>,
    change: &StatusChange<'_>,
    actor: &Actor,
) -> Result<(), KernelError> {
    sqlx::query(
        "INSERT INTO finance.status_history (tenant_id, institution_id, aggregate_type, \
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

/// Moves a locked charge to `to`.
pub async fn change_charge_status(
    conn: &mut PgConnection,
    c: &ChargeRow,
    to: ChargeStatus,
    change: &StatusChange<'_>,
    actor: &Actor,
) -> Result<ChargeRow, KernelError> {
    if !c.status.can_transition(to) {
        return Err(KernelError::Conflict(format!(
            "charge {} is {}; it cannot become {to}",
            c.charge_number, c.status
        )));
    }
    let row = sqlx::query(&format!(
        "UPDATE finance.charge SET status = $3, status_reason = $4, status_changed_at = now(), \
         process_instance_id = COALESCE($5, process_instance_id), \
         published_at = CASE WHEN $3 = 'published' THEN now() ELSE published_at END, \
         version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 AND version = $6 RETURNING {CHARGE_COLUMNS}"
    ))
    .bind(c.id.as_uuid())
    .bind(c.tenant_id.as_uuid())
    .bind(to.as_str())
    .bind(change.reason)
    .bind(change.process_instance_id)
    .bind(c.version)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| KernelError::Conflict("charge changed concurrently".into()))?;
    record_history(
        conn,
        HistoryEntry {
            tenant: c.tenant_id,
            institution: c.institution_id,
            aggregate_type: "charge",
            aggregate_id: c.id.as_uuid(),
            from: Some(c.status.as_str()),
            to: to.as_str(),
        },
        change,
        actor,
    )
    .await?;
    charge_from_row(&row).map_err(map_db_error)
}
