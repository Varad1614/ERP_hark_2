//! **4B** persistence + HTTP kit shared by the 4B areas (`timetable`, `attendance`,
//! `monitoring`, `assessment`). Slice-owned (lives in `timetable/`), not foundation code.
//!
//! Rows are read as `to_jsonb(row)` and decoded into the API DTOs after converting the top-level
//! snake_case column names to camelCase ([`camelize`]); writes go through
//! `jsonb_populate_record(NULL::academic.<table>, $json)` so one generic [`insert`] / [`patch`]
//! serves every 4B table with the database doing the typing. Table and column names are
//! compile-time constants of this slice, never client input.

use crate::shared::guard::{require_deliverable, require_institution_writable};
use crate::shared::scope::institution_scope;
use crate::shared::{publish, AcademicState, Actor};
use axum::http::StatusCode;
use axum::response::Response;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use sqlx::{PgConnection, PgExecutor};
use sutra_contracts::academic::events::{ModifiedV1, StatusChangedV1};
use sutra_contracts::academic::ids::OfferingSectionId;
use sutra_contracts::academic::ports::{OfferingSummary, SectionSummary};
use sutra_kernel::auth::AuthContext;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, TenantId};
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

/// `HH:MM:SS` → `HH:MM` (weekly times are exchanged as `HH:MM`, contract §5.1).
fn short_time(v: Value) -> Value {
    match v {
        Value::String(s) if s.len() == 8 && s.as_bytes()[2] == b':' && s.as_bytes()[5] == b':' => {
            Value::String(s[..5].to_owned())
        }
        other => other,
    }
}

/// Converts the top-level keys of a row object to camelCase (nested JSON is stored camelCase).
pub fn camelize(v: Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.into_iter()
                .map(|(k, v)| {
                    let v = if k == "starts_at" || k == "ends_at" {
                        short_time(v)
                    } else {
                        v
                    };
                    (camel_key(&k), v)
                })
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

/// One row of `academic.<table>` of the tenant (404 when missing / foreign).
pub async fn fetch<'e, T: DeserializeOwned, E: PgExecutor<'e>>(
    executor: E,
    table: &str,
    entity: &'static str,
    tenant: TenantId,
    id: Uuid,
) -> Result<T, KernelError> {
    let sql =
        format!("SELECT to_jsonb(t) FROM academic.{table} t WHERE t.id = $1 AND t.tenant_id = $2");
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
        "SELECT to_jsonb(t) FROM academic.{table} t WHERE t.id = $1 AND t.tenant_id = $2 FOR UPDATE"
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

/// Rows of `academic.<table>` where `column = value`, optional `status`, ordered by `order`
/// (a compile-time constant of the slice), at most `limit` rows.
#[allow(clippy::too_many_arguments)]
pub async fn list_by<'e, T: DeserializeOwned, E: PgExecutor<'e>>(
    executor: E,
    table: &str,
    tenant: TenantId,
    column: &str,
    value: Uuid,
    status: Option<&str>,
    order: &str,
    limit: i64,
) -> Result<Vec<T>, KernelError> {
    let sql = format!(
        "SELECT to_jsonb(t) FROM academic.{table} t WHERE t.tenant_id = $1 AND t.{column} = $2 \
         AND ($3::text IS NULL OR to_jsonb(t)->>'status' = $3) ORDER BY {order} LIMIT $4"
    );
    let rows: Vec<Value> = sqlx::query_scalar(&sql)
        .bind(tenant.as_uuid())
        .bind(value)
        .bind(status)
        .bind(limit)
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
        "INSERT INTO academic.{table} AS t ({cols}) \
         SELECT {cols} FROM jsonb_populate_record(NULL::academic.{table}, $1) RETURNING to_jsonb(t)"
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
    // An empty change set only bumps `version` / `updated_at` (a "touch" of the aggregate).
    let mut set = String::new();
    for c in columns(&changes)? {
        set.push_str(&c);
        set.push_str(" = r.");
        set.push_str(&c);
        set.push_str(", ");
    }
    let sql = format!(
        "UPDATE academic.{table} t SET {set}version = t.version + 1, updated_at = now() \
         FROM jsonb_populate_record(NULL::academic.{table}, $3) r \
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

/// Appends a row to a table without `version` / `updated_at` (histories, slots, topics).
pub async fn append(conn: &mut PgConnection, table: &str, row: Value) -> Result<(), KernelError> {
    let cols = columns(&row)?.join(", ");
    let sql = format!(
        "INSERT INTO academic.{table} ({cols}) SELECT {cols} FROM jsonb_populate_record(NULL::academic.{table}, $1)"
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
    KernelError::Conflict(format!("the {what} is {from}; it cannot become {to}"))
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

/// Trimmed optional text (blank = `None`).
pub fn opt_text(v: Option<&str>) -> Option<String> {
    v.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// A section with its offering, resolved through the offering read port, plus the institution
/// scope of events and audit.
#[derive(Clone, Debug)]
pub struct SectionCtx {
    pub section: SectionSummary,
    pub offering: OfferingSummary,
    pub scope: ExecutionScope,
}

impl SectionCtx {
    pub fn institution_uuid(&self) -> Uuid {
        self.section.institution_id.as_uuid()
    }
}

/// Resolves a section of the caller's tenant (404) and its offering through `state.offerings`.
pub async fn section_ctx(
    state: &AcademicState,
    ctx: &AuthContext,
    section_id: Uuid,
) -> Result<SectionCtx, KernelError> {
    let tscope = ExecutionScope::tenant(ctx.scope.tenant_id);
    let section = state
        .offerings
        .section(&tscope, OfferingSectionId(section_id))
        .await?
        .ok_or(KernelError::NotFound { entity: "section" })?;
    let offering = state
        .offerings
        .offering(&tscope, section.offering_id)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "course offering",
        })?;
    let scope = institution_scope(ctx, section.institution_id);
    Ok(SectionCtx {
        section,
        offering,
        scope,
    })
}

/// [`section_ctx`] + institution writable + offering deliverable + section active (409).
pub async fn deliverable_section(
    state: &AcademicState,
    ctx: &AuthContext,
    section_id: Uuid,
) -> Result<SectionCtx, KernelError> {
    let s = section_ctx(state, ctx, section_id).await?;
    require_institution_writable(state, &s.scope, s.section.institution_id).await?;
    require_deliverable(s.offering.status, s.section.status)?;
    Ok(s)
}

/// Builder of a [`StatusChangedV1`] lifecycle event.
#[derive(Default)]
pub struct StatusEvent<'a> {
    pub aggregate_id: Uuid,
    pub institution_id: Uuid,
    pub offering_id: Option<Uuid>,
    pub section_id: Option<Uuid>,
    pub student_id: Option<Uuid>,
    pub from: Option<&'a str>,
    pub to: &'a str,
    pub version: i64,
    pub reason: Option<&'a str>,
    pub process_instance_id: Option<&'a str>,
    pub student_subject_ref: Option<&'a str>,
    pub faculty_subject_ref: Option<&'a str>,
}

impl StatusEvent<'_> {
    pub async fn emit(
        self,
        conn: &mut PgConnection,
        scope: &ExecutionScope,
        (event_type, schema): (&'static str, &'static str),
        actor: &Actor,
    ) -> Result<(), KernelError> {
        publish::event(
            conn,
            scope,
            event_type,
            schema,
            self.aggregate_id.to_string(),
            &StatusChangedV1 {
                aggregate_id: self.aggregate_id,
                institution_id: self.institution_id,
                offering_id: self.offering_id,
                section_id: self.section_id,
                student_id: self.student_id,
                from_status: self.from.map(str::to_owned),
                to_status: self.to.to_owned(),
                version: self.version,
                reason: self.reason.map(str::to_owned),
                process_instance_id: self.process_instance_id.map(str::to_owned),
                student_subject_ref: self.student_subject_ref.map(str::to_owned),
                faculty_subject_ref: self.faculty_subject_ref.map(str::to_owned),
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
    offering_id: Option<Uuid>,
    section_id: Option<Uuid>,
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
            offering_id,
            section_id,
            version,
            changed_fields,
        },
        actor,
    )
    .await
}

/// Audit entry for a 4B command (`academic.<aggregate>.<command>`).
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
        &format!("academic.{aggregate}.{command}"),
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
pub struct DeliveryReason {
    pub reason: Option<String>,
}

/// Decision of an approval step (`AwaitDecision` mode).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryDecisionValue {
    Approve,
    Reject,
}

/// Body of an approval decision endpoint.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryDecision {
    pub decision: DeliveryDecisionValue,
    pub reason: Option<String>,
}

/// `reason` of an optional state-command body.
pub fn reason(body: Option<axum::Json<DeliveryReason>>) -> Option<String> {
    body.and_then(|axum::Json(b)| b.reason)
        .map(|r| r.trim().to_owned())
        .filter(|r| !r.is_empty())
}

/// `?limit=` clamp (default 50, max 200).
pub fn limit(v: Option<i64>) -> i64 {
    v.unwrap_or(50).clamp(1, 200)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camel_case_keys_and_short_times() {
        assert_eq!(camel_key("timetable_slot_id"), "timetableSlotId");
        let v = camelize(
            serde_json::json!({"starts_at": "09:00:00", "day_of_week": 1, "nested": {"a_b": 1}}),
        );
        assert_eq!(v["startsAt"], "09:00");
        assert_eq!(v["dayOfWeek"], 1);
        assert_eq!(v["nested"]["a_b"], 1);
    }

    #[test]
    fn column_names_are_checked() {
        assert!(columns(&serde_json::json!({"status": "x", "valid_until": null})).is_ok());
        assert!(columns(&serde_json::json!({"x; drop": 1})).is_err());
        assert!(columns(&serde_json::json!([1])).is_err());
        assert_eq!(limit(None), 50);
        assert_eq!(limit(Some(1000)), 200);
    }
}
