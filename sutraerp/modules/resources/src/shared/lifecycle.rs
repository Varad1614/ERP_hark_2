//! Append-only `resources.status_history` every slice uses for its own aggregates' transitions
//! (contract `docs/batch8-contracts.md` §3.3). The slice still emits its own business event and
//! audit entry in the same transaction.

use crate::shared::Actor;
use sqlx::PgConnection;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Why a status changes.
#[derive(Clone, Debug, Default)]
pub struct StatusChange<'a> {
    /// The slice business event type that causes the change.
    pub cause_event_type: &'static str,
    pub reason: Option<&'a str>,
    pub process_instance_id: Option<&'a str>,
}

/// One status history entry of any Domain J aggregate.
pub struct HistoryEntry<'a> {
    pub tenant: TenantId,
    pub institution: InstitutionId,
    /// Table / aggregate name.
    pub aggregate_type: &'static str,
    pub aggregate_id: Uuid,
    pub from: Option<&'a str>,
    pub to: &'a str,
}

/// Appends a `resources.status_history` row (append-only).
pub async fn record_history(
    conn: &mut PgConnection,
    h: HistoryEntry<'_>,
    change: &StatusChange<'_>,
    actor: &Actor,
) -> Result<(), KernelError> {
    sqlx::query(
        "INSERT INTO resources.status_history (tenant_id, institution_id, aggregate_type, \
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

/// `409` for an illegal transition of `what` (`label` = number / code).
pub fn illegal(what: &str, label: &str, from: &str, to: &str) -> KernelError {
    KernelError::Conflict(format!("{what} {label} is {from}; it cannot become {to}"))
}
