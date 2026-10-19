//! HTTP helpers shared by the Domain B handlers (same conventions as the reference aggregate:
//! strong `ETag` = version, required `If-Match` on updates → 428 when missing, 412 on mismatch).

use crate::shared::Actor;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use sutra_kernel::auth::AuthContext;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_web::request_id::current_correlation_id;
use sutra_web::Problem;

/// JSON response with `ETag: "<version>"`.
pub fn with_etag<T: serde::Serialize>(status: StatusCode, body: &T, version: Version) -> Response {
    let etag = HeaderValue::from_str(&version.to_etag()).ok();
    let mut res = (status, Json(body)).into_response();
    if let Some(e) = etag {
        res.headers_mut().insert(header::ETAG, e);
    }
    res
}

/// Parses the required `If-Match`. `Ok(Err(response))` is the ready 428 response when absent;
/// a malformed value is a 400.
pub fn if_match(headers: &HeaderMap) -> Result<Result<Version, Response>, KernelError> {
    let Some(raw) = headers.get(header::IF_MATCH) else {
        return Ok(Err(Problem::new(
            StatusCode::PRECONDITION_REQUIRED,
            "precondition-required",
            "If-Match header is required",
        )
        .into_response()));
    };
    let v = raw
        .to_str()
        .ok()
        .and_then(Version::from_if_match)
        .ok_or_else(|| {
            KernelError::BadRequest("If-Match must be a strong ETag such as \"3\"".into())
        })?;
    Ok(Ok(v))
}

/// Mutation actor from the authenticated context + current correlation id.
pub fn actor(ctx: &AuthContext) -> Actor {
    Actor {
        id: Some(ctx.subject.clone()),
        correlation_id: current_correlation_id(),
    }
}
