//! Self-service platform features (Batch 1D): profile, notifications, notices and the to-do
//! inbox, plus the in-process event consumer that turns domain events into notifications and
//! to-dos, the e-mail delivery job and their sweepers.
//!
//! Every query is filtered by the **session** tenant (ADR-L5-0004) and, where a row carries an
//! institution, by the session institution; rows of other tenants are indistinguishable from
//! missing ones (404). Permissions are the deny-by-default D02 map in `sutra_kernel::auth`.

pub mod consumer;
pub mod delivery;
pub mod jobs;
pub mod notices;
pub mod notifications;
pub mod profile;
pub mod todos;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use sutra_kernel::auth::{AuthContext, Role};
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::ExecutionScope;
use utoipa_axum::router::OpenApiRouter;
use uuid::Uuid;

/// Handler state shared by the inbox routers.
#[derive(Clone)]
pub struct InboxState {
    pub pool: PgPool,
}

pub fn router(pool: PgPool) -> OpenApiRouter {
    let state = InboxState { pool };
    OpenApiRouter::new()
        .merge(profile::routes())
        .merge(notifications::routes())
        .merge(notices::routes())
        .merge(todos::routes())
        .with_state(state)
}

/// Role names of the caller (for audience / role-addressed matching).
pub(crate) fn role_names(roles: &[Role]) -> Vec<String> {
    roles.iter().map(|r| r.as_str().to_owned()).collect()
}

pub(crate) fn institution_of(scope: &ExecutionScope) -> Option<Uuid> {
    scope.institution_id.map(|i| i.as_uuid())
}

/// Actor id + correlation id of a mutation.
pub(crate) fn actor(ctx: &AuthContext) -> (String, Option<String>) {
    (
        ctx.subject.clone(),
        sutra_web::request_id::current_correlation_id(),
    )
}

/// Checks a trimmed text field length (in characters) and the absence of control characters
/// (newlines allowed when `multiline`).
pub(crate) fn check_text(
    field: &str,
    value: &str,
    min: usize,
    max: usize,
    multiline: bool,
    v: &mut Vec<FieldViolation>,
) {
    let len = value.trim().chars().count();
    if len < min {
        v.push(FieldViolation::new(
            field,
            "required",
            format!("{field} is required"),
        ));
    } else if len > max {
        v.push(FieldViolation::new(
            field,
            "too-long",
            format!("{field} must be at most {max} characters"),
        ));
    } else if value
        .chars()
        .any(|c| c.is_control() && !(multiline && (c == '\n' || c == '\r' || c == '\t')))
    {
        v.push(FieldViolation::new(
            field,
            "format",
            format!("{field} contains control characters"),
        ));
    }
}

/// In-app links are SPA-relative paths only (no absolute URLs → no open redirect).
pub(crate) fn check_link(field: &str, link: Option<&str>, v: &mut Vec<FieldViolation>) {
    if let Some(l) = link {
        let ok = l.starts_with('/')
            && !l.starts_with("//")
            && !l.contains('\\')
            && l.len() <= 512
            && !l.chars().any(char::is_control);
        if !ok {
            v.push(FieldViolation::new(
                field,
                "format",
                "link must be an application path such as /todos",
            ));
        }
    }
}

pub(crate) fn violations(v: Vec<FieldViolation>) -> Result<(), KernelError> {
    if v.is_empty() {
        Ok(())
    } else {
        Err(KernelError::Validation(v))
    }
}

/// Page size bounds for list endpoints.
pub(crate) fn page_limit(requested: Option<u32>, default: u32) -> i64 {
    i64::from(requested.unwrap_or(default).clamp(1, 100))
}

/// `If-Match` → expected version (`428` handled by the caller when absent).
pub(crate) fn if_match(headers: &axum::http::HeaderMap) -> Result<Option<i64>, KernelError> {
    headers
        .get(axum::http::header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            sutra_kernel::concurrency::Version::from_if_match(v)
                .map(|v| v.0)
                .ok_or_else(|| {
                    KernelError::BadRequest("If-Match must be a strong ETag such as \"3\"".into())
                })
        })
        .transpose()
}

pub(crate) fn precondition_required() -> axum::response::Response {
    use axum::response::IntoResponse;
    sutra_web::Problem::new(
        axum::http::StatusCode::PRECONDITION_REQUIRED,
        "precondition-required",
        "If-Match header is required",
    )
    .into_response()
}

pub(crate) fn with_etag<T: serde::Serialize>(
    status: axum::http::StatusCode,
    version: i64,
    body: T,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let mut res = (status, axum::Json(body)).into_response();
    if let Ok(v) = axum::http::HeaderValue::from_str(&format!("\"{version}\"")) {
        res.headers_mut().insert(axum::http::header::ETAG, v);
    }
    res
}

/// `true` when `t` lies in the visibility window `[from, until)`.
pub(crate) fn in_window(
    t: DateTime<Utc>,
    from: DateTime<Utc>,
    until: Option<DateTime<Utc>>,
) -> bool {
    from <= t && until.is_none_or(|u| t < u)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_and_text_are_validated() {
        let mut v = vec![];
        check_link("link", Some("/todos"), &mut v);
        check_link("link", None, &mut v);
        assert!(v.is_empty());
        for bad in ["https://evil.example", "//evil.example", "todos", "/a\\b"] {
            let mut v = vec![];
            check_link("link", Some(bad), &mut v);
            assert_eq!(v.len(), 1, "{bad}");
        }
        let mut v = vec![];
        check_text("title", "  ", 1, 10, false, &mut v);
        check_text("title", "x\ny", 1, 10, false, &mut v);
        check_text("body", "x\ny", 1, 10, true, &mut v);
        check_text("title", &"x".repeat(11), 1, 10, false, &mut v);
        let codes: Vec<_> = v.iter().map(|x| x.code.as_str()).collect();
        assert_eq!(codes, ["required", "format", "too-long"]);
        assert_eq!(page_limit(Some(500), 20), 100);
        assert_eq!(page_limit(None, 20), 20);
    }
}
