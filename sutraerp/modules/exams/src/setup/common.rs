//! Slice-6A helpers shared by `setup`, `scheduling` and `registration` (6A-owned; not part of
//! the foundation `shared` module).
//!
//! * request bodies (`ExamSetupDecisionBody`), validation helpers (`invalid`, `parse_dec`,
//!   `valid_code`, `reason_of`, `require_reason`, `check_version`);
//! * [`move_status`] — the one status writer of the **slice-owned** 6A tables (policy,
//!   schedule, sitting, seating plan, invigilation duty, conduct record, exception, hall ticket,
//!   registration override): optimistic version check, `status_reason`, optional
//!   `process_instance_id`, and the append-only `exams.status_history` row;
//! * [`status_event`] — the shared `StatusChangedV1` payload builder.

use crate::shared::lifecycle::{record_history, HistoryEntry, StatusChange};
use crate::shared::publish::event;
use crate::shared::Actor;
use axum::Json;
use rust_decimal::Decimal;
use serde::Deserialize;
use sqlx::PgConnection;
use std::str::FromStr;
use sutra_contracts::exams::events::StatusChangedV1;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use utoipa::ToSchema;
use uuid::Uuid;

pub use crate::grading::http::ReasonBody;

/// Decision of a governed 6A approval step (`POST …/{id}/decision`, `exam.approve`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExamSetupDecisionKind {
    Approve,
    Reject,
}

/// Body of a 6A decision endpoint.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ExamSetupDecisionBody {
    pub decision: ExamSetupDecisionKind,
    pub reason: Option<String>,
}

/// Summary of a 6A bulk command (`{processed, skipped, failed[]}`, contract §5.1).
#[derive(Debug, Clone, Default, serde::Serialize, ToSchema)]
pub struct ExamSetupBulkSummary {
    pub processed: i64,
    pub skipped: i64,
    pub failed: Vec<ExamSetupBulkFailure>,
}

/// One item a bulk command could not process.
#[derive(Debug, Clone, serde::Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamSetupBulkFailure {
    /// The item (student, offering, registration …) concerned.
    pub id: Option<Uuid>,
    pub reason: String,
}

pub fn invalid(field: &str, code: &str, message: &str) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, code, message)])
}

pub fn parse_dec(field: &str, raw: &str) -> Result<Decimal, KernelError> {
    Decimal::from_str(raw.trim()).map_err(|_| {
        invalid(
            field,
            "invalid_number",
            "must be a decimal string such as \"40.00\"",
        )
    })
}

pub fn parse_opt_dec(field: &str, raw: Option<&str>) -> Result<Option<Decimal>, KernelError> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| parse_dec(field, s))
        .transpose()
}

/// `^[A-Z0-9][A-Z0-9_-]{1,31}$`.
pub fn valid_code(code: &str) -> bool {
    let b = code.as_bytes();
    (2..=32).contains(&b.len())
        && (b[0].is_ascii_uppercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

/// Upper-cases and strips a free text into a code candidate (`CS 101` → `CS101`).
pub fn code_from(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .map(|c| c.to_ascii_uppercase())
        .take(32)
        .collect()
}

pub fn require_code(field: &str, raw: &str) -> Result<String, KernelError> {
    let code = raw.trim().to_owned();
    if valid_code(&code) {
        Ok(code)
    } else {
        Err(invalid(
            field,
            "invalid_code",
            "must match ^[A-Z0-9][A-Z0-9_-]{1,31}$",
        ))
    }
}

pub fn require_name(field: &str, raw: &str) -> Result<String, KernelError> {
    let name = raw.trim().to_owned();
    if name.is_empty() || name.chars().count() > 200 {
        Err(invalid(
            field,
            "invalid_length",
            "must be 1 … 200 characters",
        ))
    } else {
        Ok(name)
    }
}

/// Trimmed, non-empty optional reason of a state command body.
pub fn reason_of(body: Option<Json<ReasonBody>>) -> Option<String> {
    body.and_then(|Json(b)| b.reason)
        .map(|r| r.trim().to_owned())
        .filter(|r| !r.is_empty())
}

pub fn clean(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// 422 unless a reason was given (governed revisions, withdrawals, suspensions, …).
pub fn require_reason(reason: &Option<String>) -> Result<&str, KernelError> {
    reason.as_deref().ok_or_else(|| {
        invalid(
            "reason",
            "required",
            "a reason is required for this command",
        )
    })
}

/// 412 unless the caller's `If-Match` matches the stored version.
pub fn check_version(expected: Version, actual: i64) -> Result<(), KernelError> {
    if expected.0 == actual {
        Ok(())
    } else {
        Err(KernelError::VersionConflict {
            expected: expected.0,
            actual,
        })
    }
}

/// `409` for an illegal move of a slice-owned machine.
pub fn illegal(what: &str, from: &str, to: &str) -> KernelError {
    KernelError::Conflict(format!("{what} is {from}; it cannot become {to}"))
}

/// One status move of a slice-owned 6A aggregate.
pub struct Move<'a> {
    /// Qualified table (`exams.exam_policy`).
    pub table: &'static str,
    /// Aggregate name for the status history (`exam_policy`).
    pub aggregate: &'static str,
    /// Whether the table has a `process_instance_id` column.
    pub has_process: bool,
    pub tenant: TenantId,
    pub institution: InstitutionId,
    pub id: Uuid,
    /// The version the caller read (optimistic check).
    pub version: i64,
    pub from: &'a str,
    pub to: &'a str,
}

/// Moves a slice-owned aggregate (the caller already checked the transition is legal) and
/// appends its status history row; returns the new version.
pub async fn move_status(
    conn: &mut PgConnection,
    m: Move<'_>,
    change: &StatusChange<'_>,
    actor: &Actor,
) -> Result<i64, KernelError> {
    let pid = if m.has_process {
        ", process_instance_id = COALESCE($6, process_instance_id)"
    } else {
        ""
    };
    let sql = format!(
        "UPDATE {} SET status = $3, status_reason = $4, version = version + 1, updated_at = now(){pid} \
         WHERE id = $1 AND tenant_id = $2 AND version = $5 RETURNING version",
        m.table
    );
    let mut q = sqlx::query_scalar::<_, i64>(&sql)
        .bind(m.id)
        .bind(m.tenant.as_uuid())
        .bind(m.to)
        .bind(change.reason)
        .bind(m.version);
    if m.has_process {
        q = q.bind(change.process_instance_id);
    }
    let version = q
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| KernelError::Conflict(format!("{} changed concurrently", m.aggregate)))?;
    record_history(
        conn,
        HistoryEntry {
            tenant: m.tenant,
            institution: m.institution,
            aggregate_type: m.aggregate,
            aggregate_id: m.id,
            from: Some(m.from),
            to: m.to,
        },
        change,
        actor,
    )
    .await?;
    Ok(version)
}

/// `StatusChangedV1` with the mandatory members; callers fill the optional ones.
pub fn status_event(
    aggregate_id: Uuid,
    institution: InstitutionId,
    from: Option<&str>,
    to: &str,
    version: i64,
) -> StatusChangedV1 {
    StatusChangedV1 {
        aggregate_id,
        institution_id: institution.as_uuid(),
        session_id: None,
        paper_id: None,
        registration_id: None,
        student_id: None,
        from_status: from.map(str::to_owned),
        to_status: to.to_owned(),
        version,
        reason: None,
        process_instance_id: None,
        student_subject_ref: None,
    }
}

/// Emits a 6A status event (`(type, dataschema)` pair) on the aggregate.
pub async fn emit(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    kind: (&'static str, &'static str),
    payload: &StatusChangedV1,
    actor: &Actor,
) -> Result<(), KernelError> {
    event(
        conn,
        scope,
        kind.0,
        kind.1,
        payload.aggregate_id.to_string(),
        payload,
        actor,
    )
    .await
}

/// `f64` credits of the frozen Domain D offering contract → two-decimal `Decimal`.
pub fn credits_of(raw: f64) -> Decimal {
    Decimal::try_from(raw)
        .map(crate::shared::grading::round2)
        .unwrap_or(Decimal::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_reasons() {
        assert!(valid_code("ESE-2026") && !valid_code("e"));
        assert_eq!(code_from("cs 101-a"), "CS101-A");
        assert!(require_reason(&None).is_err());
        assert_eq!(require_reason(&Some("x".into())).ok(), Some("x"));
        assert!(check_version(Version(2), 2).is_ok());
        assert!(check_version(Version(1), 2).is_err());
        assert_eq!(credits_of(4.0).to_string(), "4.00");
        assert_eq!(parse_opt_dec("x", Some(" ")).ok(), Some(None));
    }
}
