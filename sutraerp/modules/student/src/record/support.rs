//! Helpers shared by the 3A areas (`admission`, `record`). Slice-private.

use crate::shared::Actor;
use sqlx::PgConnection;
use sutra_contracts::student::events::{ModifiedV1, StatusChangedV1};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::Problem;
use uuid::Uuid;

/// Extracts the required `If-Match` version or returns the ready 428 response.
macro_rules! expect_version {
    ($headers:expr) => {
        match $crate::shared::http::if_match(&$headers)? {
            Ok(v) => v,
            Err(response) => return Ok(response),
        }
    };
}
pub(crate) use expect_version;

/// 412 when the `If-Match` version is stale.
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

/// One-field 422.
pub fn invalid(field: &str, code: &str, message: impl Into<String>) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, code, message.into())])
}

/// 409 with a message.
pub fn conflict(message: impl Into<String>) -> KernelError {
    KernelError::Conflict(message.into())
}

/// Validates a required text of at most `max` characters.
pub fn validate_text(field: &str, value: &str, max: usize, v: &mut Vec<FieldViolation>) {
    let n = value.trim().chars().count();
    if n == 0 {
        v.push(FieldViolation::new(
            field,
            "required",
            format!("{field} is required"),
        ));
    } else if n > max {
        v.push(FieldViolation::new(
            field,
            "too-long",
            format!("{field} must be at most {max} characters"),
        ));
    }
}

/// Validates an optional text of at most `max` characters.
pub fn validate_opt_text(
    field: &str,
    value: Option<&str>,
    max: usize,
    v: &mut Vec<FieldViolation>,
) {
    if let Some(s) = value {
        if s.trim().chars().count() > max {
            v.push(FieldViolation::new(
                field,
                "too-long",
                format!("{field} must be at most {max} characters"),
            ));
        }
    }
}

/// Minimal e-mail shape check (`x@y.z`).
pub fn validate_email(field: &str, value: Option<&str>, v: &mut Vec<FieldViolation>) {
    if let Some(e) = value.map(str::trim).filter(|e| !e.is_empty()) {
        let ok = e.len() <= 254
            && e.split_once('@').is_some_and(|(l, d)| {
                !l.is_empty() && d.contains('.') && !d.starts_with('.') && !d.ends_with('.')
            })
            && !e.contains(char::is_whitespace);
        if !ok {
            v.push(FieldViolation::new(
                field,
                "format",
                format!("{field} must be an e-mail address"),
            ));
        }
    }
}

/// Phone: digits, spaces, `+`, `-`, `(`, `)`; 6–20 characters.
pub fn validate_phone(field: &str, value: Option<&str>, v: &mut Vec<FieldViolation>) {
    if let Some(p) = value.map(str::trim).filter(|p| !p.is_empty()) {
        let ok = (6..=20).contains(&p.len())
            && p.bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'+' | b'-' | b' ' | b'(' | b')'));
        if !ok {
            v.push(FieldViolation::new(
                field,
                "format",
                format!("{field} must be a phone number"),
            ));
        }
    }
}

/// Document type keys: `snake_case`, 2–48 characters.
pub fn validate_key(field: &str, value: &str, v: &mut Vec<FieldViolation>) {
    let ok = (2..=48).contains(&value.len())
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if !ok {
        v.push(FieldViolation::new(
            field,
            "format",
            format!("{field} must be snake_case (a-z, 0-9, _), 2-48 characters"),
        ));
    }
}

/// `Ok(())` when `v` is empty, else a 422 carrying it.
pub fn finish(v: Vec<FieldViolation>) -> Result<(), KernelError> {
    if v.is_empty() {
        Ok(())
    } else {
        Err(KernelError::Validation(v))
    }
}

/// `Option<&str>` trimmed, empty → `None`.
pub fn norm(o: Option<&str>) -> Option<String> {
    o.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Optional `{ "reason": "…" }` body of state commands.
#[derive(Debug, Default, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = StudentCommandBody)]
pub struct CommandBody {
    /// Business reason / decision note (max 1000 characters).
    pub reason: Option<String>,
}

impl CommandBody {
    pub fn reason(&self) -> Result<Option<String>, KernelError> {
        let r = norm(self.reason.as_deref());
        if r.as_ref().is_some_and(|r| r.chars().count() > 1000) {
            return Err(invalid(
                "reason",
                "too-long",
                "reason must be at most 1000 characters",
            ));
        }
        Ok(r)
    }

    /// Reason as mandatory evidence (422 when missing).
    pub fn required_reason(&self) -> Result<String, KernelError> {
        self.reason()?
            .ok_or_else(|| invalid("reason", "required", "a reason is required"))
    }
}

/// Body of `…/decision` endpoints.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = StudentDecisionBody)]
pub struct DecisionBody {
    pub decision: Decision,
    /// Required when rejecting.
    pub reason: Option<String>,
}

/// Approve or reject a pending governed step.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[schema(as = StudentDecision)]
pub enum Decision {
    Approve,
    Reject,
}

impl DecisionBody {
    /// The reason, required for a rejection.
    pub fn checked_reason(&self) -> Result<Option<String>, KernelError> {
        let body = CommandBody {
            reason: self.reason.clone(),
        };
        match self.decision {
            Decision::Approve => body.reason(),
            Decision::Reject => body.required_reason().map(Some),
        }
    }
}

/// `StatusChangedV1` payload.
#[allow(clippy::too_many_arguments)]
pub fn status_changed(
    aggregate_id: Uuid,
    institution_id: InstitutionId,
    student_id: Option<Uuid>,
    from: Option<&str>,
    to: &str,
    version: i64,
    reason: Option<String>,
    process_instance_id: Option<String>,
    student_subject_ref: Option<String>,
) -> StatusChangedV1 {
    StatusChangedV1 {
        aggregate_id,
        institution_id: institution_id.as_uuid(),
        student_id,
        from_status: from.map(str::to_owned),
        to_status: to.to_owned(),
        version,
        reason,
        process_instance_id,
        student_subject_ref,
    }
}

/// `ModifiedV1` payload.
pub fn modified(
    aggregate_id: Uuid,
    institution_id: InstitutionId,
    student_id: Option<Uuid>,
    version: i64,
    changed_fields: Vec<String>,
    student_subject_ref: Option<String>,
) -> ModifiedV1 {
    ModifiedV1 {
        aggregate_id,
        institution_id: institution_id.as_uuid(),
        student_id,
        version,
        changed_fields,
        student_subject_ref,
    }
}

/// Allocates the next value of a tenant-scoped counter (`scope_key`, e.g. `student:2026`).
pub async fn next_number(
    conn: &mut PgConnection,
    tenant: TenantId,
    scope_key: &str,
) -> Result<i64, KernelError> {
    sqlx::query_scalar(
        "INSERT INTO student.student_number_sequence (tenant_id, scope_key, next_value) \
         VALUES ($1, $2, 2) ON CONFLICT (tenant_id, scope_key) \
         DO UPDATE SET next_value = student.student_number_sequence.next_value + 1 \
         RETURNING next_value - 1",
    )
    .bind(tenant.as_uuid())
    .bind(scope_key)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)
}

/// Appends one `student_record_history` row (before/after values of a governed change).
#[allow(clippy::too_many_arguments)]
pub async fn record_history(
    conn: &mut PgConnection,
    tenant: TenantId,
    student_id: Uuid,
    context_id: Option<Uuid>,
    kind: &str,
    before: serde_json::Value,
    after: serde_json::Value,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    sqlx::query(
        "INSERT INTO student.student_record_history (tenant_id, student_id, student_context_id, \
         kind, before_values, after_values, reason, actor, correlation_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(tenant.as_uuid())
    .bind(student_id)
    .bind(context_id)
    .bind(kind)
    .bind(before)
    .bind(after)
    .bind(reason)
    .bind(actor.id.as_deref())
    .bind(actor.correlation_id.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

/// Error responses of read endpoints.
#[derive(utoipa::IntoResponses)]
#[allow(dead_code)]
pub enum ReadErrors {
    /// No live session.
    #[response(status = 401, content_type = "application/problem+json")]
    Unauthenticated(Problem),
    /// Missing permission.
    #[response(status = 403, content_type = "application/problem+json")]
    Forbidden(Problem),
    /// Missing, or owned by another tenant.
    #[response(status = 404, content_type = "application/problem+json")]
    NotFound(Problem),
}

/// Error responses of create endpoints (`Idempotency-Key` required).
#[derive(utoipa::IntoResponses)]
#[allow(dead_code)]
pub enum CreateErrors {
    /// Missing `Idempotency-Key` or malformed request.
    #[response(status = 400, content_type = "application/problem+json")]
    BadRequest(Problem),
    /// No live session.
    #[response(status = 401, content_type = "application/problem+json")]
    Unauthenticated(Problem),
    /// CSRF failure, missing permission or missing MFA step-up.
    #[response(status = 403, content_type = "application/problem+json")]
    Forbidden(Problem),
    /// Parent resource missing, or owned by another tenant.
    #[response(status = 404, content_type = "application/problem+json")]
    NotFound(Problem),
    /// Duplicate, illegal state or read-only institution.
    #[response(status = 409, content_type = "application/problem+json")]
    Conflict(Problem),
    /// Validation failed.
    #[response(status = 422, content_type = "application/problem+json")]
    Unprocessable(Problem),
}

/// Error responses of update / state commands (`If-Match` required).
#[derive(utoipa::IntoResponses)]
#[allow(dead_code)]
pub enum CommandErrors {
    /// Malformed `If-Match` or request.
    #[response(status = 400, content_type = "application/problem+json")]
    BadRequest(Problem),
    /// No live session.
    #[response(status = 401, content_type = "application/problem+json")]
    Unauthenticated(Problem),
    /// CSRF failure, missing permission or missing MFA step-up.
    #[response(status = 403, content_type = "application/problem+json")]
    Forbidden(Problem),
    /// Missing, or owned by another tenant.
    #[response(status = 404, content_type = "application/problem+json")]
    NotFound(Problem),
    /// Illegal lifecycle transition, unmet precondition or read-only institution.
    #[response(status = 409, content_type = "application/problem+json")]
    Conflict(Problem),
    /// `If-Match` is stale.
    #[response(status = 412, content_type = "application/problem+json")]
    Stale(Problem),
    /// Validation failed.
    #[response(status = 422, content_type = "application/problem+json")]
    Unprocessable(Problem),
    /// `If-Match` missing.
    #[response(status = 428, content_type = "application/problem+json")]
    PreconditionRequired(Problem),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert!(check_version(Version(2), 2).is_ok());
        assert!(matches!(
            check_version(Version(1), 2),
            Err(KernelError::VersionConflict {
                expected: 1,
                actual: 2
            })
        ));
        let mut v = vec![];
        validate_text("name", "Asha", 100, &mut v);
        validate_email("email", Some("a@b.in"), &mut v);
        validate_phone("phone", Some("+91 98765 43210"), &mut v);
        validate_key("documentType", "identity_proof", &mut v);
        assert!(finish(v).is_ok());
        let mut v = vec![];
        validate_text("name", " ", 100, &mut v);
        validate_email("email", Some("nope"), &mut v);
        validate_phone("phone", Some("abc"), &mut v);
        validate_key("documentType", "Bad-Key", &mut v);
        validate_opt_text("note", Some(&"x".repeat(11)), 10, &mut v);
        assert_eq!(v.len(), 5);
        assert_eq!(norm(Some("  ")), None);
        assert!(CommandBody::default().required_reason().is_err());
        let reject = DecisionBody {
            decision: Decision::Reject,
            reason: None,
        };
        assert!(reject.checked_reason().is_err());
        let approve = DecisionBody {
            decision: Decision::Approve,
            reason: None,
        };
        assert_eq!(approve.checked_reason().ok(), Some(None));
    }
}
