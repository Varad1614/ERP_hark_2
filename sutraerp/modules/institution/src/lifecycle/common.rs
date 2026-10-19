//! Helpers shared by the 2A areas (`lifecycle`, `org`, `position`). Slice-private.

use chrono::NaiveDate;
use sqlx::PgExecutor;
use sutra_contracts::institution::events::StatusChangedV1;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
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

/// Today's business-local date of the institution (its IANA `timezone`, ADR-L5-0036 §3.4).
/// Every effective-dating rule of 2A compares against this date, never the server's clock.
pub async fn business_today<'e, E: PgExecutor<'e>>(
    executor: E,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
) -> Result<NaiveDate, KernelError> {
    sqlx::query_scalar(
        "SELECT (now() AT TIME ZONE timezone)::date FROM institution.institution \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(institution_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .fetch_optional(executor)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "institution",
    })
}

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

/// `StatusChangedV1` payload.
pub fn status_changed(
    aggregate_id: Uuid,
    institution_id: InstitutionId,
    from: &str,
    to: &str,
    version: i64,
    reason: Option<String>,
    process_instance_id: Option<String>,
) -> StatusChangedV1 {
    StatusChangedV1 {
        aggregate_id,
        institution_id: institution_id.as_uuid(),
        from_status: from.to_owned(),
        to_status: to.to_owned(),
        version,
        reason,
        process_instance_id,
    }
}

/// Validates a business code (`A-Z0-9-_`, 2–32 characters).
pub fn validate_code(field: &str, code: &str, v: &mut Vec<FieldViolation>) {
    let c = code.trim();
    if !(2..=32).contains(&c.len())
        || !c
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    {
        v.push(FieldViolation::new(
            field,
            "format",
            format!("{field} must be 2-32 characters of A-Z, 0-9, '-' or '_'"),
        ));
    }
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

/// `Ok(())` when `v` is empty, else a 422 carrying it.
pub fn finish(v: Vec<FieldViolation>) -> Result<(), KernelError> {
    if v.is_empty() {
        Ok(())
    } else {
        Err(KernelError::Validation(v))
    }
}

/// `Option<String>` trimmed, empty → `None`.
pub fn norm(o: Option<&str>) -> Option<String> {
    o.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Error responses of read endpoints.
#[derive(utoipa::IntoResponses)]
#[allow(dead_code)]
pub enum ReadErrors {
    /// No live session.
    #[response(status = 401, content_type = "application/problem+json")]
    Unauthenticated(Problem),
    /// Missing `institution.read` permission.
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
    /// Duplicate code, illegal state or read-only institution.
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
        validate_code("code", "CSE-1", &mut v);
        validate_text("name", "Computer Science", 200, &mut v);
        assert!(finish(v).is_ok());
        let mut v = vec![];
        validate_code("code", "c", &mut v);
        validate_text("name", " ", 200, &mut v);
        validate_text("title", &"x".repeat(5), 4, &mut v);
        assert_eq!(v.len(), 3);
        assert_eq!(norm(Some("  ")), None);
        assert_eq!(norm(Some(" a ")).as_deref(), Some("a"));
    }
}
