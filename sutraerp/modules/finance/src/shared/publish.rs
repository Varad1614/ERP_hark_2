//! Outbox intent + audit evidence inside the owner-local transaction (ADR-L5-0006/0007).
//! Every Domain F mutation calls [`event`] and [`audit`] on the same transaction as its state
//! change.

use crate::shared::Actor;
use sqlx::PgConnection;
use sutra_contracts::finance::events::SOURCE;
use sutra_kernel::audit::{ActorKind, AuditEntry, AuditOutcome};
use sutra_kernel::error::KernelError;
use sutra_kernel::events::{EventEnvelope, NewEvent};
use sutra_kernel::scope::ExecutionScope;
use sutra_persistence::{audit as audit_store, map_db_error, outbox};

/// Enqueues one Domain F event (`source = /sutraerp/finance`). `scope` must be the institution
/// scope of the offering / aggregate; `subject` is the aggregate id.
pub async fn event<T: serde::Serialize>(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    event_type: &'static str,
    dataschema: &'static str,
    subject: String,
    data: &T,
    actor: &Actor,
) -> Result<(), KernelError> {
    let env = EventEnvelope::new(
        scope,
        NewEvent {
            source: SOURCE,
            event_type,
            dataschema,
            subject: Some(subject),
            data: serde_json::to_value(data).map_err(KernelError::internal)?,
        },
    )
    .with_correlation(actor.correlation_id.clone())
    .with_actor(actor.id.clone());
    outbox::enqueue(&mut *conn, &env)
        .await
        .map_err(map_db_error)
}

/// Writes one succeeded audit entry. `action` is `finance.<aggregate>.<command>` (e.g.
/// `finance.charge.publish`); `target_type` is the aggregate name (e.g. `charge`).
pub async fn audit(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    action: &str,
    target_type: &str,
    target_id: String,
    details: serde_json::Value,
    actor: &Actor,
) -> Result<(), KernelError> {
    let kind = if actor
        .id
        .as_deref()
        .is_some_and(|a| a.starts_with("system:"))
    {
        ActorKind::System
    } else {
        ActorKind::User
    };
    audit_store::insert(
        &mut *conn,
        &AuditEntry::new(*scope, action, AuditOutcome::Succeeded)
            .actor(kind, actor.id.clone())
            .target(target_type, target_id)
            .correlation(actor.correlation_id.clone())
            .details(details),
    )
    .await
    .map_err(map_db_error)
}
