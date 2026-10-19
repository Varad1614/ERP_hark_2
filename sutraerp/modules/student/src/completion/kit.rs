//! **3C** persistence + HTTP kit shared by the 3C areas (`completion`, `guardian`,
//! `scholarship`, `alumni`). Slice-owned (lives in `completion/`), not foundation code.
//!
//! Rows are read as `to_jsonb(row)` and decoded into the API DTOs after converting the
//! top-level snake_case column names to camelCase ([`camelize`]); writes go through
//! `jsonb_populate_record(NULL::student.<table>, $json)` so one generic [`insert`] / [`patch`]
//! serves every 3C table with the database doing the typing. Table and column names are
//! compile-time constants of this slice, never client input.

use crate::shared::guard::{
    require_institution_writable, require_live_context, require_student_writable,
};
use crate::shared::publish;
use crate::shared::scope::institution_scope;
use crate::shared::student::{current_context, load_student, StudentContextRow, StudentRow};
use crate::shared::{Actor, StudentState};
use axum::http::StatusCode;
use axum::response::Response;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use sqlx::{PgConnection, PgExecutor};
use sutra_contracts::student::events::{ModifiedV1, StatusChangedV1};
use sutra_contracts::student::ids::StudentId;
use sutra_kernel::auth::AuthContext;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Returns the 428 response of a missing `If-Match` early from a handler.
macro_rules! expect_version {
    ($headers:expr) => {
        match crate::shared::http::if_match(&$headers)? {
            Ok(v) => v,
            Err(res) => return Ok(res),
        }
    };
}
pub(crate) use expect_version;

/// `200` + `ETag`.
pub fn ok<T: serde::Serialize>(body: &T, version: i64) -> Response {
    crate::shared::http::with_etag(StatusCode::OK, body, Version(version))
}

/// `201` + `ETag`.
pub fn created<T: serde::Serialize>(body: &T, version: i64) -> Response {
    crate::shared::http::with_etag(StatusCode::CREATED, body, Version(version))
}

/// `snake_case` → `camelCase`.
pub fn camel_key(k: &str) -> String {
    let mut out = String::with_capacity(k.len());
    let mut upper = false;
    for c in k.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// Converts the top-level keys of a row object to camelCase (nested JSON is stored camelCase).
pub fn camelize(v: Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.into_iter()
                .map(|(k, v)| (camel_key(&k), v))
                .collect::<Map<_, _>>(),
        ),
        other => other,
    }
}

/// Decodes a `to_jsonb(row)` value into a DTO.
pub fn decode<T: DeserializeOwned>(v: Value) -> Result<T, KernelError> {
    serde_json::from_value(camelize(v)).map_err(KernelError::internal)
}

fn not_found(entity: &'static str) -> KernelError {
    KernelError::NotFound { entity }
}

/// One row of `student.<table>` of the tenant (404 when missing / foreign).
pub async fn fetch<'e, T: DeserializeOwned, E: PgExecutor<'e>>(
    executor: E,
    table: &str,
    entity: &'static str,
    tenant: TenantId,
    id: Uuid,
) -> Result<T, KernelError> {
    let sql =
        format!("SELECT to_jsonb(t) FROM student.{table} t WHERE t.id = $1 AND t.tenant_id = $2");
    let v: Value = sqlx::query_scalar(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(executor)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| not_found(entity))?;
    decode(v)
}

/// Same as [`fetch`] with `FOR UPDATE`.
pub async fn lock<T: DeserializeOwned>(
    conn: &mut PgConnection,
    table: &str,
    entity: &'static str,
    tenant: TenantId,
    id: Uuid,
) -> Result<T, KernelError> {
    let sql = format!(
        "SELECT to_jsonb(t) FROM student.{table} t WHERE t.id = $1 AND t.tenant_id = $2 FOR UPDATE"
    );
    let v: Value = sqlx::query_scalar(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| not_found(entity))?;
    decode(v)
}

/// Rows of `student.<table>` where `column = value`, optional `status`, newest first (≤ 500).
pub async fn list_by<'e, T: DeserializeOwned, E: PgExecutor<'e>>(
    executor: E,
    table: &str,
    tenant: TenantId,
    column: &str,
    value: Uuid,
    status: Option<&str>,
) -> Result<Vec<T>, KernelError> {
    let sql = format!(
        "SELECT to_jsonb(t) FROM student.{table} t WHERE t.tenant_id = $1 AND t.{column} = $2 \
         AND ($3::text IS NULL OR t.status = $3) ORDER BY t.created_at DESC, t.id DESC LIMIT 500"
    );
    let rows: Vec<Value> = sqlx::query_scalar(&sql)
        .bind(tenant.as_uuid())
        .bind(value)
        .bind(status)
        .fetch_all(executor)
        .await
        .map_err(map_db_error)?;
    rows.into_iter().map(decode).collect()
}

fn columns(row: &Value) -> Result<Vec<String>, KernelError> {
    let obj = row
        .as_object()
        .ok_or_else(|| KernelError::internal_msg("row must be a JSON object"))?;
    let cols: Vec<String> = obj.keys().cloned().collect();
    if cols.iter().any(|c| {
        c.is_empty()
            || !c
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'_' || b.is_ascii_digit())
    }) {
        return Err(KernelError::internal_msg("invalid column name"));
    }
    Ok(cols)
}

/// Inserts one row (snake_case keys) and returns the stored row decoded.
pub async fn insert<T: DeserializeOwned>(
    conn: &mut PgConnection,
    table: &str,
    row: Value,
) -> Result<T, KernelError> {
    let cols = columns(&row)?.join(", ");
    let sql = format!(
        "INSERT INTO student.{table} AS t ({cols}) \
         SELECT {cols} FROM jsonb_populate_record(NULL::student.{table}, $1) RETURNING to_jsonb(t)"
    );
    let v: Value = sqlx::query_scalar(&sql)
        .bind(&row)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
    decode(v)
}

/// Updates the given columns of a (locked) row, bumps `version` and `updated_at`.
pub async fn patch<T: DeserializeOwned>(
    conn: &mut PgConnection,
    table: &str,
    tenant: TenantId,
    id: Uuid,
    changes: Value,
) -> Result<T, KernelError> {
    let set = columns(&changes)?
        .iter()
        .map(|c| format!("{c} = r.{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "UPDATE student.{table} t SET {set}, version = t.version + 1, updated_at = now() \
         FROM jsonb_populate_record(NULL::student.{table}, $3) r \
         WHERE t.id = $1 AND t.tenant_id = $2 RETURNING to_jsonb(t)"
    );
    let v: Value = sqlx::query_scalar(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .bind(&changes)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| KernelError::Conflict(format!("{table} changed concurrently")))?;
    decode(v)
}

/// Appends a row to an append-only history table (no `version` / `updated_at`).
pub async fn append(conn: &mut PgConnection, table: &str, row: Value) -> Result<(), KernelError> {
    let cols = columns(&row)?.join(", ");
    let sql = format!(
        "INSERT INTO student.{table} ({cols}) SELECT {cols} FROM jsonb_populate_record(NULL::student.{table}, $1)"
    );
    sqlx::query(&sql)
        .bind(&row)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    Ok(())
}

/// `If-Match` check against the locked row's version (412 on mismatch).
pub fn check_version(actual: i64, expected: Version) -> Result<(), KernelError> {
    Version(actual).expect(expected)
}

/// 409 for an illegal transition of a slice-owned machine.
pub fn illegal(
    what: &str,
    from: impl std::fmt::Display,
    to: impl std::fmt::Display,
) -> KernelError {
    KernelError::Conflict(format!("{what} is {from}; it cannot become {to}"))
}

/// 422 on one field.
pub fn invalid(field: &str, code: &str, message: impl Into<String>) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, code, message)])
}

/// Requires a non-blank text value (422 otherwise); returns it trimmed.
pub fn required_text(field: &str, v: Option<&str>) -> Result<String, KernelError> {
    match v.map(str::trim) {
        Some(s) if !s.is_empty() => Ok(s.to_owned()),
        _ => Err(invalid(field, "required", format!("{field} is required"))),
    }
}

/// A student plus the live context that 3C commands act on, with the write guards applied
/// (persona not archived, live context, institution writable).
pub struct StudentTarget {
    pub student: StudentRow,
    pub context: StudentContextRow,
    pub scope: ExecutionScope,
}

impl StudentTarget {
    pub fn institution_id(&self) -> InstitutionId {
        self.context.institution_id
    }
}

/// Loads the student (404) and its current live context (409 when none) and applies the write guards.
pub async fn writable_student(
    state: &StudentState,
    ctx: &AuthContext,
    student_id: StudentId,
) -> Result<StudentTarget, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let student = load_student(&state.pool, tenant, student_id).await?;
    require_student_writable(&student)?;
    let context = current_context(&state.pool, tenant, student_id)
        .await?
        .ok_or_else(|| KernelError::Conflict("the student has no live student context".into()))?;
    require_live_context(&context)?;
    let scope = institution_scope(ctx, context.institution_id);
    require_institution_writable(state, &scope, context.institution_id).await?;
    Ok(StudentTarget {
        student,
        context,
        scope,
    })
}

/// Loads the student (404, read guard only) for aggregates that outlive the live context
/// (exits, conferred qualifications, alumni): persona not archived + institution writable.
pub async fn writable_institution(
    state: &StudentState,
    ctx: &AuthContext,
    institution_id: InstitutionId,
) -> Result<ExecutionScope, KernelError> {
    let scope = institution_scope(ctx, institution_id);
    require_institution_writable(state, &scope, institution_id).await?;
    Ok(scope)
}

/// Builder of a [`StatusChangedV1`] lifecycle event.
pub struct StatusEvent<'a> {
    pub event_type: &'static str,
    pub schema: &'static str,
    pub aggregate_id: Uuid,
    pub institution_id: Uuid,
    pub student: Option<&'a StudentRow>,
    pub from: Option<&'a str>,
    pub to: &'a str,
    pub version: i64,
    pub reason: Option<&'a str>,
    pub process_instance_id: Option<&'a str>,
}

impl StatusEvent<'_> {
    pub async fn emit(
        self,
        conn: &mut PgConnection,
        scope: &ExecutionScope,
        actor: &Actor,
    ) -> Result<(), KernelError> {
        publish::event(
            conn,
            scope,
            self.event_type,
            self.schema,
            self.aggregate_id.to_string(),
            &StatusChangedV1 {
                aggregate_id: self.aggregate_id,
                institution_id: self.institution_id,
                student_id: self.student.map(|s| s.id.as_uuid()),
                from_status: self.from.map(str::to_owned),
                to_status: self.to.to_owned(),
                version: self.version,
                reason: self.reason.map(str::to_owned),
                process_instance_id: self.process_instance_id.map(str::to_owned),
                student_subject_ref: self.student.and_then(|s| s.subject_ref.clone()),
            },
            actor,
        )
        .await
    }
}

/// Emits a [`ModifiedV1`] event.
#[allow(clippy::too_many_arguments)]
pub async fn modified_event(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    (event_type, schema): (&'static str, &'static str),
    aggregate_id: Uuid,
    student: Option<&StudentRow>,
    version: i64,
    changed_fields: Vec<String>,
    actor: &Actor,
) -> Result<(), KernelError> {
    publish::event(
        conn,
        scope,
        event_type,
        schema,
        aggregate_id.to_string(),
        &ModifiedV1 {
            aggregate_id,
            institution_id: scope
                .institution_id
                .map(|i| i.as_uuid())
                .unwrap_or_default(),
            student_id: student.map(|s| s.id.as_uuid()),
            version,
            changed_fields,
            student_subject_ref: student.and_then(|s| s.subject_ref.clone()),
        },
        actor,
    )
    .await
}

/// Audit entry for a 3C command (`student.<aggregate>.<command>`).
pub async fn audit(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    aggregate: &str,
    command: &str,
    id: Uuid,
    details: Value,
    actor: &Actor,
) -> Result<(), KernelError> {
    publish::audit(
        conn,
        scope,
        &format!("student.{aggregate}.{command}"),
        aggregate,
        id.to_string(),
        details,
        actor,
    )
    .await
}

/// Generic body of a state command.
#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleReason {
    pub reason: Option<String>,
}

/// Decision of an approval step (`AwaitDecision` mode).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleDecisionValue {
    Approve,
    Reject,
}

/// Body of an approval decision endpoint.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleDecision {
    pub decision: LifecycleDecisionValue,
    pub reason: Option<String>,
}

/// `reason` of an optional state-command body.
pub fn reason(body: Option<axum::Json<LifecycleReason>>) -> Option<String> {
    body.and_then(|axum::Json(b)| b.reason)
        .map(|r| r.trim().to_owned())
        .filter(|r| !r.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camel_case_keys() {
        assert_eq!(camel_key("student_context_id"), "studentContextId");
        assert_eq!(camel_key("id"), "id");
        let v = camelize(serde_json::json!({"is_primary": true, "nested": {"a_b": 1}}));
        assert_eq!(v["isPrimary"], true);
        assert_eq!(v["nested"]["a_b"], 1);
    }

    #[test]
    fn column_names_are_checked() {
        assert!(columns(&serde_json::json!({"status": "x", "valid_until": null})).is_ok());
        assert!(columns(&serde_json::json!({"x; drop": 1})).is_err());
        assert!(columns(&serde_json::json!([1])).is_err());
    }
}
