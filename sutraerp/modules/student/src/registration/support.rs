//! Slice-3B helpers shared by `registration` and `progression` (request bodies, student
//! resolution with the Domain C write guards, status events, the academic-standing projection).

use crate::shared::guard::{
    require_institution_writable, require_live_context, require_student_writable,
};
use crate::shared::student::{current_context, load_student, StudentContextRow, StudentRow};
use crate::shared::{publish, Actor, StudentState};
use axum::body::Bytes;
use serde::de::DeserializeOwned;
use sqlx::{PgConnection, PgExecutor};
use sutra_contracts::student::events::StatusChangedV1;
use sutra_contracts::student::ids::StudentId;
use sutra_contracts::student::status::{AcademicStanding, StudentContextStatus};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Parses an optional JSON body (empty body → `T::default()`).
pub fn body<T: DeserializeOwned + Default>(raw: &Bytes) -> Result<T, KernelError> {
    if raw.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(raw)
        .map_err(|e| KernelError::BadRequest(format!("invalid JSON body: {e}")))
}

/// `{ "reason": "…" }` of state commands.
#[derive(Debug, Default, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StepReason {
    pub reason: Option<String>,
}

impl StepReason {
    /// Trimmed reason; `required` → 422 when missing.
    pub fn validated(self, required: bool) -> Result<Option<String>, KernelError> {
        let r = self
            .reason
            .map(|r| r.trim().to_owned())
            .filter(|r| !r.is_empty());
        if required && r.is_none() {
            return Err(invalid("reason", "required", "a reason is required"));
        }
        if r.as_ref().is_some_and(|r| r.chars().count() > 1000) {
            return Err(invalid("reason", "too_long", "at most 1000 characters"));
        }
        Ok(r)
    }
}

/// Approve / reject decision of a pending approval step.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum StepDecision {
    Approve,
    Reject,
}

/// Body of the explicit decision endpoints (`AwaitDecision`).
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StepDecisionBody {
    pub decision: StepDecision,
    pub reason: Option<String>,
}

/// One field violation as a 422.
pub fn invalid(field: &str, code: &str, message: impl Into<String>) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, code, message)])
}

/// Optimistic concurrency check (412 on mismatch).
pub fn check_version(actual: i64, expected: Version) -> Result<(), KernelError> {
    Version(actual).expect(expected)
}

pub fn conflict(msg: impl Into<String>) -> KernelError {
    KernelError::Conflict(msg.into())
}

/// A student with its current live context and the institution scope of that context.
#[derive(Clone, Debug)]
pub struct StudentScope {
    pub student: StudentRow,
    pub context: StudentContextRow,
    pub scope: ExecutionScope,
}

impl StudentScope {
    pub fn subject_ref(&self) -> Option<String> {
        self.student.subject_ref.clone()
    }
}

/// Student + current live context for reads (404 when the student is missing; `None` context
/// when the student has no live context).
pub async fn read_student(
    state: &StudentState,
    tenant: TenantId,
    student_id: StudentId,
) -> Result<(StudentRow, Option<StudentContextRow>), KernelError> {
    let s = load_student(&state.pool, tenant, student_id).await?;
    let c = current_context(&state.pool, tenant, student_id).await?;
    Ok((s, c))
}

/// Student + live context for a 3B command, with the Domain C write guards: institution
/// writable (404/409), persona not archived (409), live context (409). `require_active` also
/// rejects `pending_activation` / `suspended` contexts (registration preconditions, §7).
pub async fn write_student(
    state: &StudentState,
    tenant: TenantId,
    student_id: StudentId,
    require_active: bool,
) -> Result<StudentScope, KernelError> {
    let student = load_student(&state.pool, tenant, student_id).await?;
    require_student_writable(&student)?;
    let context = current_context(&state.pool, tenant, student_id)
        .await?
        .ok_or_else(|| conflict("the student has no live student context"))?;
    require_live_context(&context)?;
    if require_active && context.status != StudentContextStatus::Active {
        return Err(conflict(format!(
            "the student context is {}; this action needs an active context",
            context.status
        )));
    }
    let scope = ExecutionScope::institution(tenant, context.institution_id);
    require_institution_writable(state, &scope, context.institution_id).await?;
    Ok(StudentScope {
        student,
        context,
        scope,
    })
}

/// Builds the shared [`StatusChangedV1`] payload.
#[allow(clippy::too_many_arguments)]
pub fn status_changed(
    aggregate_id: Uuid,
    scope: &ExecutionScope,
    student_id: Option<Uuid>,
    from: Option<&str>,
    to: &str,
    version: i64,
    reason: Option<&str>,
    process_instance_id: Option<&str>,
    student_subject_ref: Option<String>,
) -> StatusChangedV1 {
    StatusChangedV1 {
        aggregate_id,
        institution_id: scope
            .institution_id
            .map(|i| i.as_uuid())
            .unwrap_or_default(),
        student_id,
        from_status: from.map(str::to_owned),
        to_status: to.to_owned(),
        version,
        reason: reason.map(str::to_owned),
        process_instance_id: process_instance_id.map(str::to_owned),
        student_subject_ref,
    }
}

/// Enqueues one event (thin wrapper over [`publish::event`]).
pub async fn emit<T: serde::Serialize>(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    event_type: &'static str,
    dataschema: &'static str,
    subject: Uuid,
    data: &T,
    actor: &Actor,
) -> Result<(), KernelError> {
    publish::event(
        conn,
        scope,
        event_type,
        dataschema,
        subject.to_string(),
        data,
        actor,
    )
    .await
}

/// Writes one audit entry (thin wrapper over [`publish::audit`]).
pub async fn audit(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    action: &str,
    target_type: &str,
    target_id: Uuid,
    details: serde_json::Value,
    actor: &Actor,
) -> Result<(), KernelError> {
    publish::audit(
        conn,
        scope,
        action,
        target_type,
        target_id.to_string(),
        details,
        actor,
    )
    .await
}

/// Current academic standing of a context (`good_standing` when never set).
pub async fn standing_of<'e, E: PgExecutor<'e>>(
    executor: E,
    tenant: TenantId,
    context_id: Uuid,
) -> Result<AcademicStanding, KernelError> {
    let raw: Option<String> = sqlx::query_scalar(
        "SELECT standing FROM student.academic_standing WHERE student_context_id = $1 AND tenant_id = $2",
    )
    .bind(context_id)
    .bind(tenant.as_uuid())
    .fetch_optional(executor)
    .await
    .map_err(map_db_error)?;
    Ok(raw
        .as_deref()
        .and_then(AcademicStanding::parse)
        .unwrap_or(AcademicStanding::GoodStanding))
}

/// Upserts the standing projection of a context (progression only).
pub async fn set_standing(
    conn: &mut PgConnection,
    ctx: &StudentContextRow,
    standing: AcademicStanding,
    source_type: &str,
    source_id: Uuid,
) -> Result<(), KernelError> {
    sqlx::query(
        "INSERT INTO student.academic_standing (student_context_id, tenant_id, institution_id, \
         student_id, standing, source_type, source_id) VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (student_context_id) DO UPDATE SET standing = EXCLUDED.standing, \
         source_type = EXCLUDED.source_type, source_id = EXCLUDED.source_id, \
         version = student.academic_standing.version + 1, updated_at = now()",
    )
    .bind(ctx.id.as_uuid())
    .bind(ctx.tenant_id.as_uuid())
    .bind(ctx.institution_id.as_uuid())
    .bind(ctx.student_id.as_uuid())
    .bind(standing.as_str())
    .bind(source_type)
    .bind(source_id)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

/// Decodes a stored enum value.
pub fn parse_enum<T>(
    raw: &str,
    parse: fn(&str) -> Option<T>,
    what: &str,
) -> Result<T, KernelError> {
    parse(raw).ok_or_else(|| KernelError::internal_msg(format!("unknown {what} `{raw}`")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_body_is_default_and_reason_is_trimmed() {
        let b: StepReason = body(&Bytes::from_static(b"  ")).unwrap_or_default();
        assert!(b.reason.is_none());
        let b: StepReason = body(&Bytes::from_static(br#"{"reason":"  x "}"#)).unwrap_or_default();
        assert_eq!(b.validated(true).ok().flatten().as_deref(), Some("x"));
        assert!(StepReason::default().validated(true).is_err());
        assert!(check_version(2, Version(1)).is_err());
        assert!(check_version(2, Version(2)).is_ok());
    }
}
