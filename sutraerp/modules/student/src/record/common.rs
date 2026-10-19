//! Student-record helpers shared by the 3A record handlers: loading the view, choosing the
//! context a command acts on, and context placement updates with history.

use super::dto::Student;
use super::support::record_history;
use crate::shared::guard::{require_institution_writable, require_student_writable};
use crate::shared::http::with_etag;
use crate::shared::scope::institution_scope;
use crate::shared::student::{
    context_from_row, contexts_of, current_context, load_student, StudentContextRow, StudentRow,
    CONTEXT_COLUMNS,
};
use crate::shared::{Actor, StudentState};
use axum::http::StatusCode;
use axum::response::Response;
use chrono::NaiveDate;
use sqlx::{PgConnection, PgExecutor};
use sutra_contracts::student::ids::{StudentContextId, StudentId};
use sutra_kernel::auth::AuthContext;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// The full Student view (persona + current context + every context).
pub async fn view(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: StudentId,
) -> Result<Student, KernelError> {
    let s = load_student(&mut *conn, tenant, id).await?;
    view_of(conn, &s).await
}

/// View of a loaded persona.
pub async fn view_of(conn: &mut PgConnection, s: &StudentRow) -> Result<Student, KernelError> {
    let all = contexts_of(&mut *conn, s.tenant_id, s.id).await?;
    let current = all
        .iter()
        .find(|c| c.status.is_live())
        .or_else(|| all.first())
        .cloned();
    Ok(Student::from_rows(s, current.as_ref(), &all))
}

/// The institution a persona-level change is scoped to: the current live context's, else the
/// latest context's. A persona without any context is a 409 (no institution scope).
pub async fn home_institution<'e, E: PgExecutor<'e> + Copy>(
    executor: E,
    s: &StudentRow,
) -> Result<InstitutionId, KernelError> {
    if let Some(c) = current_context(executor, s.tenant_id, s.id).await? {
        return Ok(c.institution_id);
    }
    contexts_of(executor, s.tenant_id, s.id)
        .await?
        .first()
        .map(|c| c.institution_id)
        .ok_or_else(|| KernelError::Conflict("the student has no institution context".into()))
}

/// The context a command acts on: `context_id` when given (must belong to the student), else
/// the student's current live context (409 when none).
pub async fn pick_context(
    conn: &mut PgConnection,
    s: &StudentRow,
    context_id: Option<StudentContextId>,
) -> Result<StudentContextRow, KernelError> {
    let all = contexts_of(&mut *conn, s.tenant_id, s.id).await?;
    match context_id {
        Some(id) => all
            .into_iter()
            .find(|c| c.id == id)
            .ok_or(KernelError::NotFound {
                entity: "student context",
            }),
        None => all
            .into_iter()
            .find(|c| c.status.is_live())
            .ok_or_else(|| KernelError::Conflict("the student has no live context".into())),
    }
}

/// Staff command preparation: persona (404), writable persona (409), context, institution
/// scope and writable institution.
pub async fn prepare_context(
    st: &StudentState,
    ctx: &AuthContext,
    id: StudentId,
    context_id: Option<StudentContextId>,
) -> Result<(StudentRow, StudentContextRow, ExecutionScope), KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let s = load_student(&mut *conn, ctx.scope.tenant_id, id).await?;
    require_student_writable(&s)?;
    let c = pick_context(&mut conn, &s, context_id).await?;
    drop(conn);
    let scope = institution_scope(ctx, c.institution_id);
    require_institution_writable(st, &scope, c.institution_id).await?;
    Ok((s, c, scope))
}

/// Student response with `ETag` = the given version (persona or context, per endpoint).
pub fn respond(status: StatusCode, s: &Student, etag: Version) -> Response {
    with_etag(status, s, etag)
}

/// Placement attributes of a context that 3A maintains (§3.2 write rules).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Placement {
    pub campus_id: Option<Uuid>,
    pub org_unit_id: Option<Uuid>,
    pub admission_term_id: Option<Uuid>,
    pub admission_year_id: Option<Uuid>,
    pub batch_code: Option<String>,
    pub section_code: Option<String>,
    pub enrolment_number: Option<String>,
    pub roll_number: Option<String>,
    pub enrolment_type: String,
    pub enrolled_on: Option<NaiveDate>,
}

impl From<&StudentContextRow> for Placement {
    fn from(c: &StudentContextRow) -> Self {
        Self {
            campus_id: c.campus_id,
            org_unit_id: c.org_unit_id,
            admission_term_id: c.admission_term_id,
            admission_year_id: c.admission_year_id,
            batch_code: c.batch_code.clone(),
            section_code: c.section_code.clone(),
            enrolment_number: c.enrolment_number.clone(),
            roll_number: c.roll_number.clone(),
            enrolment_type: c.enrolment_type.clone(),
            enrolled_on: c.enrolled_on,
        }
    }
}

impl Placement {
    /// camelCase names of the members that differ.
    pub fn changed(&self, other: &Placement) -> Vec<String> {
        let a = serde_json::to_value(self).unwrap_or_default();
        let b = serde_json::to_value(other).unwrap_or_default();
        let (Some(a), Some(b)) = (a.as_object(), b.as_object()) else {
            return vec![];
        };
        a.iter()
            .filter(|(k, v)| b.get(*k) != Some(v))
            .map(|(k, _)| k.clone())
            .collect()
    }
}

/// Writes `p` onto a locked context (optimistic on its version), appends a record-history row
/// of `kind` with before/after values and returns the updated row.
pub async fn update_placement(
    conn: &mut PgConnection,
    c: &StudentContextRow,
    p: &Placement,
    kind: &str,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<StudentContextRow, KernelError> {
    let sql = format!(
        "UPDATE student.student_context SET campus_id = $3, org_unit_id = $4, \
         admission_term_id = $5, admission_year_id = $6, batch_code = $7, section_code = $8, \
         enrolment_number = $9, roll_number = $10, enrolment_type = $11, enrolled_on = $12, \
         version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 AND version = $13 RETURNING {CONTEXT_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(c.id.as_uuid())
        .bind(c.tenant_id.as_uuid())
        .bind(p.campus_id)
        .bind(p.org_unit_id)
        .bind(p.admission_term_id)
        .bind(p.admission_year_id)
        .bind(&p.batch_code)
        .bind(&p.section_code)
        .bind(&p.enrolment_number)
        .bind(&p.roll_number)
        .bind(&p.enrolment_type)
        .bind(p.enrolled_on)
        .bind(c.version.0)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(d)
                if d.constraint() == Some("student_context_enrolment_number_unique") =>
            {
                KernelError::Conflict(
                    "the enrolment number is already used in this institution".into(),
                )
            }
            _ => map_db_error(e),
        })?
        .ok_or_else(|| KernelError::Conflict("student context changed concurrently".into()))?;
    let next = context_from_row(&row).map_err(map_db_error)?;
    let before = Placement::from(c);
    record_history(
        conn,
        c.tenant_id,
        c.student_id.as_uuid(),
        Some(c.id.as_uuid()),
        kind,
        serde_json::to_value(&before).map_err(KernelError::internal)?,
        serde_json::to_value(p).map_err(KernelError::internal)?,
        reason,
        actor,
    )
    .await?;
    Ok(next)
}

/// Validates batch / section codes (`A-Z0-9-_/`, 1–20 characters).
pub fn validate_code(
    field: &str,
    v: Option<&str>,
    out: &mut Vec<sutra_kernel::error::FieldViolation>,
) {
    if let Some(c) = v.map(str::trim).filter(|c| !c.is_empty()) {
        let ok = c.len() <= 20
            && c.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'/'));
        if !ok {
            out.push(sutra_kernel::error::FieldViolation::new(
                field,
                "format",
                format!("{field} must be 1-20 characters of letters, digits, '-', '_' or '/'"),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_diff_names_changed_members() {
        let a = Placement {
            campus_id: None,
            org_unit_id: None,
            admission_term_id: None,
            admission_year_id: None,
            batch_code: Some("2026".into()),
            section_code: Some("A".into()),
            enrolment_number: None,
            roll_number: None,
            enrolment_type: "regular".into(),
            enrolled_on: None,
        };
        let mut b = a.clone();
        b.section_code = Some("B".into());
        assert_eq!(a.changed(&b), vec!["sectionCode".to_owned()]);
        let mut v = vec![];
        validate_code("batchCode", Some("2026-A/1"), &mut v);
        assert!(v.is_empty());
        validate_code("batchCode", Some("bad code"), &mut v);
        assert_eq!(v.len(), 1);
    }
}
