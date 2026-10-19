//! The **only** writer of `student.status` and `student_context.status` (contract doc §4.2).
//!
//! Each call, inside the caller's owner-local transaction and on a row the caller locked
//! ([`crate::shared::student::lock_student`] / [`lock_context`](crate::shared::student::lock_context)):
//!
//! 1. checks the legal transition (`StudentStatus::can_transition` /
//!    `StudentContextStatus::can_transition`) → `409 conflict` otherwise;
//! 2. updates status, reason, `status_changed_at`, `closed_on` (terminal contexts) and bumps
//!    `version` (optimistic: the row's version must still match);
//! 3. appends a `student.student_status_history` row;
//! 4. enqueues `sutraerp.student.student.status_changed` ([`StudentStatusChangedV1`]).
//!
//! The slice still emits its own business event (e.g. `student.suspended`) and audit entry.

use crate::shared::publish;
use crate::shared::student::{
    context_from_row, student_from_row, StudentContextRow, StudentRow, CONTEXT_COLUMNS,
    STUDENT_COLUMNS,
};
use crate::shared::Actor;
use chrono::NaiveDate;
use sqlx::PgConnection;
use sutra_contracts::student::events::{
    StudentStatusChangedV1, STUDENT_STATUS_CHANGED, STUDENT_STATUS_CHANGED_SCHEMA,
};
use sutra_contracts::student::status::{StudentContextStatus, StudentStatus};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Why a status changes.
#[derive(Clone, Debug, Default)]
pub struct StatusChange<'a> {
    /// The slice business event type that causes the change (e.g. `STUDENT_SUSPENDED`).
    pub cause_event_type: &'static str,
    pub reason: Option<&'a str>,
    pub process_instance_id: Option<&'a str>,
    /// Business-local date for a terminal context (defaults to the database `current_date`).
    pub closed_on: Option<NaiveDate>,
}

#[allow(clippy::too_many_arguments)]
async fn history(
    conn: &mut PgConnection,
    tenant: Uuid,
    student_id: Uuid,
    context_id: Option<Uuid>,
    institution_id: InstitutionId,
    from: &str,
    to: &str,
    change: &StatusChange<'_>,
    actor: &Actor,
) -> Result<(), KernelError> {
    sqlx::query(
        "INSERT INTO student.student_status_history (tenant_id, student_id, student_context_id, \
         institution_id, scope, from_status, to_status, reason, cause_event_type, \
         process_instance_id, actor, correlation_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(tenant)
    .bind(student_id)
    .bind(context_id)
    .bind(institution_id.as_uuid())
    .bind(if context_id.is_some() {
        "context"
    } else {
        "student"
    })
    .bind(from)
    .bind(to)
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

/// Moves a locked StudentContext to `to`.
pub async fn change_context_status(
    conn: &mut PgConnection,
    ctx: &StudentContextRow,
    to: StudentContextStatus,
    change: &StatusChange<'_>,
    actor: &Actor,
) -> Result<StudentContextRow, KernelError> {
    if !ctx.status.can_transition(to) {
        return Err(KernelError::Conflict(format!(
            "student context is {}; it cannot become {to}",
            ctx.status
        )));
    }
    let sql = format!(
        "UPDATE student.student_context SET status = $3, status_reason = $4, \
         status_changed_at = now(), \
         closed_on = CASE WHEN $5 THEN COALESCE($6, current_date) ELSE NULL END, \
         version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 AND version = $7 RETURNING {CONTEXT_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(ctx.id.as_uuid())
        .bind(ctx.tenant_id.as_uuid())
        .bind(to.as_str())
        .bind(change.reason)
        .bind(to.is_terminal())
        .bind(change.closed_on)
        .bind(ctx.version.0)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| KernelError::Conflict("student context changed concurrently".into()))?;
    let updated = context_from_row(&row).map_err(map_db_error)?;
    history(
        conn,
        ctx.tenant_id.as_uuid(),
        ctx.student_id.as_uuid(),
        Some(ctx.id.as_uuid()),
        ctx.institution_id,
        ctx.status.as_str(),
        to.as_str(),
        change,
        actor,
    )
    .await?;
    let scope = ExecutionScope::institution(ctx.tenant_id, ctx.institution_id);
    publish::event(
        conn,
        &scope,
        STUDENT_STATUS_CHANGED,
        STUDENT_STATUS_CHANGED_SCHEMA,
        ctx.student_id.to_string(),
        &StudentStatusChangedV1 {
            student_id: ctx.student_id.as_uuid(),
            student_context_id: Some(ctx.id.as_uuid()),
            institution_id: ctx.institution_id.as_uuid(),
            scope: "context".into(),
            from_status: Some(ctx.status.as_str().into()),
            to_status: to.as_str().into(),
            cause_event_type: change.cause_event_type.into(),
            reason: change.reason.map(str::to_owned),
            version: updated.version.0,
        },
        actor,
    )
    .await?;
    Ok(updated)
}

/// Moves a locked persona to `to`. `institution_id` is the event/history scope (the institution
/// of the context that caused it). `archived` requires that no live context remains.
pub async fn change_student_status(
    conn: &mut PgConnection,
    student: &StudentRow,
    institution_id: InstitutionId,
    to: StudentStatus,
    change: &StatusChange<'_>,
    actor: &Actor,
) -> Result<StudentRow, KernelError> {
    if !student.status.can_transition(to) {
        return Err(KernelError::Conflict(format!(
            "student is {}; it cannot become {to}",
            student.status
        )));
    }
    if to == StudentStatus::Archived {
        let live: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM student.student_context WHERE student_id = $1 AND tenant_id = $2 \
             AND status IN ('pending_activation', 'active', 'suspended')",
        )
        .bind(student.id.as_uuid())
        .bind(student.tenant_id.as_uuid())
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
        if live > 0 {
            return Err(KernelError::Conflict(
                "the student still has a live context; close it before archiving".into(),
            ));
        }
    }
    let sql = format!(
        "UPDATE student.student SET status = $3, status_changed_at = now(), \
         version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 AND version = $4 RETURNING {STUDENT_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(student.id.as_uuid())
        .bind(student.tenant_id.as_uuid())
        .bind(to.as_str())
        .bind(student.version.0)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| KernelError::Conflict("student changed concurrently".into()))?;
    let updated = student_from_row(&row).map_err(map_db_error)?;
    history(
        conn,
        student.tenant_id.as_uuid(),
        student.id.as_uuid(),
        None,
        institution_id,
        student.status.as_str(),
        to.as_str(),
        change,
        actor,
    )
    .await?;
    let scope = ExecutionScope::institution(student.tenant_id, institution_id);
    publish::event(
        conn,
        &scope,
        STUDENT_STATUS_CHANGED,
        STUDENT_STATUS_CHANGED_SCHEMA,
        student.id.to_string(),
        &StudentStatusChangedV1 {
            student_id: student.id.as_uuid(),
            student_context_id: None,
            institution_id: institution_id.as_uuid(),
            scope: "student".into(),
            from_status: Some(student.status.as_str().into()),
            to_status: to.as_str().into(),
            cause_event_type: change.cause_event_type.into(),
            reason: change.reason.map(str::to_owned),
            version: updated.version.0,
        },
        actor,
    )
    .await?;
    Ok(updated)
}
