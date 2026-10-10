//! RFC 9457 Problem Details.

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use sutra_kernel::error::{FieldViolation, KernelError};

pub const PROBLEM_CONTENT_TYPE: &str = "application/problem+json";

/// RFC 9457 problem document with SutraERP extension members.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Problem {
    /// URI reference identifying the problem type.
    #[serde(rename = "type")]
    pub problem_type: String,
    pub title: String,
    pub status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// SutraERP extension: stable machine-readable error code.
    pub error_code: String,
    /// SutraERP extension: request correlation id (technical context only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    /// SutraERP extension: structured field violations.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub validation_errors: Vec<FieldViolation>,
}

impl Problem {
    pub fn new(status: StatusCode, error_code: &str, title: &str) -> Self {
        Self {
            problem_type: format!("urn:sutraerp:problem:{error_code}"),
            title: title.to_owned(),
            status: status.as_u16(),
            detail: None,
            instance: None,
            error_code: error_code.to_owned(),
            correlation_id: crate::request_id::current_correlation_id(),
            validation_errors: Vec::new(),
        }
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let body = serde_json::to_vec(&self).unwrap_or_default();
        let mut res = (status, body).into_response();
        res.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(PROBLEM_CONTENT_TYPE),
        );
        res
    }
}

/// HTTP-facing error. Wraps the framework-neutral [`KernelError`].
#[derive(Debug)]
pub struct ApiError(pub KernelError);

pub type ApiResult<T> = Result<T, ApiError>;

impl From<KernelError> for ApiError {
    fn from(e: KernelError) -> Self {
        Self(e)
    }
}

impl ApiError {
    pub fn to_problem(&self) -> Problem {
        let e = &self.0;
        let code = e.code();
        let (status, title, detail): (StatusCode, &str, Option<String>) = match e {
            KernelError::Validation(_) => {
                (StatusCode::UNPROCESSABLE_ENTITY, "Validation failed", None)
            }
            KernelError::BadRequest(d) => (StatusCode::BAD_REQUEST, "Bad request", Some(d.clone())),
            KernelError::Unauthenticated => {
                (StatusCode::UNAUTHORIZED, "Authentication required", None)
            }
            KernelError::Forbidden(d) => (StatusCode::FORBIDDEN, "Forbidden", Some(d.clone())),
            KernelError::StepUpRequired(p) => (
                StatusCode::FORBIDDEN,
                "Multi-factor step-up required",
                Some(format!(
                    "`{p}` requires a session verified with a second factor; \
                     complete POST /api/v1/auth/mfa/step-up (or OIDC step-up) and retry"
                )),
            ),
            KernelError::Scope(d) => (
                StatusCode::BAD_REQUEST,
                "Invalid execution scope",
                Some(d.clone()),
            ),
            KernelError::NotFound { entity } => (
                StatusCode::NOT_FOUND,
                "Resource not found",
                Some(format!("{entity} not found")),
            ),
            KernelError::VersionConflict { expected, actual } => (
                StatusCode::PRECONDITION_FAILED,
                "Version conflict",
                Some(format!(
                    "expected version {expected} but current version is {actual}"
                )),
            ),
            KernelError::Conflict(d) => (StatusCode::CONFLICT, "Conflict", Some(d.clone())),
            KernelError::IdempotencyMismatch => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "Idempotency key reused",
                Some("the Idempotency-Key was already used with a different request".into()),
            ),
            KernelError::IdempotencyInFlight => (
                StatusCode::CONFLICT,
                "Request in progress",
                Some("a request with this Idempotency-Key is still being processed".into()),
            ),
            KernelError::NotImplemented(d) => (
                StatusCode::NOT_IMPLEMENTED,
                "Not implemented",
                Some((*d).into()),
            ),
            KernelError::Unavailable(d) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "Dependency unavailable",
                Some(d.clone()),
            ),
            KernelError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Internal error", None),
        };
        let mut p = Problem::new(status, code, title);
        p.detail = detail;
        if let KernelError::Validation(v) = e {
            p.validation_errors = v.clone();
        }
        p
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if let KernelError::Internal(source) = &self.0 {
            // Never echo internal details to the caller; keep them in the trace.
            tracing::error!(error = %source, "internal error");
        }
        self.to_problem().into_response()
    }
}

/// Fallback for unknown routes.
pub async fn not_found() -> Problem {
    Problem::new(StatusCode::NOT_FOUND, "route-not-found", "Route not found")
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    async fn body_json(res: Response) -> serde_json::Value {
        let bytes = res
            .into_body()
            .collect()
            .await
            .map(|b| b.to_bytes())
            .unwrap_or_default();
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    #[tokio::test]
    async fn validation_problem_shape() {
        let err = ApiError(KernelError::Validation(vec![FieldViolation::new(
            "name",
            "required",
            "name is required",
        )]));
        let res = err.into_response();
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(res.headers()[header::CONTENT_TYPE], PROBLEM_CONTENT_TYPE);
        let v = body_json(res).await;
        assert_eq!(v["type"], "urn:sutraerp:problem:validation-failed");
        assert_eq!(v["status"], 422);
        assert_eq!(v["errorCode"], "validation-failed");
        assert_eq!(v["validationErrors"][0]["field"], "name");
    }

    #[tokio::test]
    async fn internal_errors_are_not_leaked() {
        let res = ApiError(KernelError::internal_msg("db password=hunter2")).into_response();
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let v = body_json(res).await;
        assert!(!v.to_string().contains("hunter2"));
    }

    #[test]
    fn status_mapping() {
        let cases = [
            (KernelError::Unauthenticated, 401),
            (KernelError::Scope("x".into()), 400),
            (KernelError::NotFound { entity: "x" }, 404),
            (
                KernelError::VersionConflict {
                    expected: 1,
                    actual: 2,
                },
                412,
            ),
            (KernelError::IdempotencyMismatch, 422),
            (KernelError::IdempotencyInFlight, 409),
            (KernelError::NotImplemented("x"), 501),
            (KernelError::Unavailable("db".into()), 503),
        ];
        for (e, s) in cases {
            assert_eq!(ApiError(e).to_problem().status, s);
        }
    }
}
