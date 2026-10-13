//! The **only** writers of the shared session / paper / registration status columns (contract doc
//! §3.3), plus the append-only `exams.status_history` every slice uses for its own aggregates.
//!
//! Inside the caller's owner-local transaction and on a row the caller locked
//! (`shared::rows::lock_*`), each `change_*` function:
//!
//! 1. checks the legal transition (`can_transition` of the shared machine in
//!    `sutra_contracts::exams::status`) → `409 conflict` otherwise;
//! 2. updates status, reason (and `process_instance_id` when given), bumps `version` (optimistic:
//!    the row's version must still match);
//! 3. appends an `exams.status_history` row.
//!
//! The slice still emits its own business event (e.g. `exam_session.activated`) and audit entry
//! in the same transaction. Who may call which move is fixed in the contract (§3.3): sessions —
//! 6A up to `reopened`, 6C `closing → closed → archived`; papers — 6A up to `conducted` /
//! `cancelled`, 6B `evaluation` / `marks_finalised`; registrations — 6A (6C `completed`).

use crate::shared::rows::{
    paper_from_row, registration_from_row, session_from_row, PAPER_COLUMNS, REGISTRATION_COLUMNS,
    SESSION_COLUMNS,
};
use crate::shared::Actor;
use sqlx::PgConnection;
use sutra_contracts::exams::ports::{CandidateSummary, ExamPaperSummary, ExamSessionSummary};
use sutra_contracts::exams::status::{ExamPaperStatus, ExamRegistrationStatus, ExamSessionStatus};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Why a status changes.
#[derive(Clone, Debug, Default)]
pub struct StatusChange<'a> {
    /// The slice business event type that causes the change (e.g. `EXAM_SESSION_ACTIVATED`).
    pub cause_event_type: &'static str,
    pub reason: Option<&'a str>,
    pub process_instance_id: Option<&'a str>,
}

/// One status history entry of any Domain I aggregate.
pub struct HistoryEntry<'a> {
    pub tenant: TenantId,
    pub institution: InstitutionId,
    /// Table / aggregate name (`exam_session`, `hall_ticket`, `mark_sheet`, …).
    pub aggregate_type: &'static str,
    pub aggregate_id: Uuid,
    pub from: Option<&'a str>,
    pub to: &'a str,
}

/// Appends an `exams.status_history` row (append-only). Slices use it for their own
/// aggregates' transitions too.
pub async fn record_history(
    conn: &mut PgConnection,
    h: HistoryEntry<'_>,
    change: &StatusChange<'_>,
    actor: &Actor,
) -> Result<(), KernelError> {
    sqlx::query(
        "INSERT INTO exams.status_history (tenant_id, institution_id, aggregate_type, \
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

/// Moves a locked session to `to`.
pub async fn change_session_status(
    conn: &mut PgConnection,
    tenant: TenantId,
    s: &ExamSessionSummary,
    to: ExamSessionStatus,
    change: &StatusChange<'_>,
    actor: &Actor,
) -> Result<ExamSessionSummary, KernelError> {
    if !s.status.can_transition(to) {
        return Err(conflict(
            "examination session",
            &s.code,
            s.status.as_str(),
            to.as_str(),
        ));
    }
    let row = sqlx::query(&format!(
        "UPDATE exams.exam_session SET status = $3, status_reason = $4, status_changed_at = now(), \
         process_instance_id = COALESCE($5, process_instance_id), version = version + 1, \
         updated_at = now() WHERE id = $1 AND tenant_id = $2 AND version = $6 \
         RETURNING {SESSION_COLUMNS}"
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
    .ok_or_else(|| KernelError::Conflict("examination session changed concurrently".into()))?;
    record_history(
        conn,
        HistoryEntry {
            tenant,
            institution: s.institution_id,
            aggregate_type: "exam_session",
            aggregate_id: s.id.as_uuid(),
            from: Some(s.status.as_str()),
            to: to.as_str(),
        },
        change,
        actor,
    )
    .await?;
    session_from_row(&row).map_err(map_db_error)
}

/// Moves a locked paper to `to`.
pub async fn change_paper_status(
    conn: &mut PgConnection,
    tenant: TenantId,
    p: &ExamPaperSummary,
    to: ExamPaperStatus,
    change: &StatusChange<'_>,
    actor: &Actor,
) -> Result<ExamPaperSummary, KernelError> {
    if !p.status.can_transition(to) {
        return Err(conflict(
            "paper",
            &p.paper_code,
            p.status.as_str(),
            to.as_str(),
        ));
    }
    let row = sqlx::query(&format!(
        "UPDATE exams.exam_paper SET status = $3, status_reason = $4, version = version + 1, \
         updated_at = now() WHERE id = $1 AND tenant_id = $2 AND version = $5 \
         RETURNING {PAPER_COLUMNS}"
    ))
    .bind(p.id.as_uuid())
    .bind(tenant.as_uuid())
    .bind(to.as_str())
    .bind(change.reason)
    .bind(p.version)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| KernelError::Conflict("paper changed concurrently".into()))?;
    record_history(
        conn,
        HistoryEntry {
            tenant,
            institution: p.institution_id,
            aggregate_type: "exam_paper",
            aggregate_id: p.id.as_uuid(),
            from: Some(p.status.as_str()),
            to: to.as_str(),
        },
        change,
        actor,
    )
    .await?;
    paper_from_row(&row).map_err(map_db_error)
}

/// Moves a locked registration to `to` (papers are kept as loaded).
pub async fn change_registration_status(
    conn: &mut PgConnection,
    tenant: TenantId,
    r: &CandidateSummary,
    to: ExamRegistrationStatus,
    change: &StatusChange<'_>,
    actor: &Actor,
) -> Result<CandidateSummary, KernelError> {
    if !r.status.can_transition(to) {
        return Err(conflict(
            "registration",
            &r.registration_number,
            r.status.as_str(),
            to.as_str(),
        ));
    }
    let row = sqlx::query(&format!(
        "UPDATE exams.exam_registration SET status = $3, status_reason = $4, \
         status_changed_at = now(), process_instance_id = COALESCE($5, process_instance_id), \
         version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 AND version = $6 RETURNING {REGISTRATION_COLUMNS}"
    ))
    .bind(r.registration_id.as_uuid())
    .bind(tenant.as_uuid())
    .bind(to.as_str())
    .bind(change.reason)
    .bind(change.process_instance_id)
    .bind(r.version)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| KernelError::Conflict("registration changed concurrently".into()))?;
    record_history(
        conn,
        HistoryEntry {
            tenant,
            institution: r.institution_id,
            aggregate_type: "exam_registration",
            aggregate_id: r.registration_id.as_uuid(),
            from: Some(r.status.as_str()),
            to: to.as_str(),
        },
        change,
        actor,
    )
    .await?;
    let mut updated = registration_from_row(&row).map_err(map_db_error)?;
    updated.papers.clone_from(&r.papers);
    Ok(updated)
}
