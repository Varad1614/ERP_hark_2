use crate::domain::{Institution, RegisterInstitution, RenameInstitution};
use crate::repo::{self, Actor};
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use sqlx::PgPool;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::InstitutionId;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::request_id::current_correlation_id;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};

fn with_etag(status: StatusCode, inst: Institution) -> Response {
    let etag = HeaderValue::from_str(&inst.version.to_etag()).ok();
    let mut res = (status, Json(inst)).into_response();
    if let Some(e) = etag {
        res.headers_mut().insert(header::ETAG, e);
    }
    res
}

fn actor(ctx: &AuthContext) -> Actor {
    Actor {
        id: Some(ctx.subject.clone()),
        correlation_id: current_correlation_id(),
    }
}

/// Register an institution (reference aggregate) in the caller's session tenant.
///
/// Requires the `institution.register` permission (administrators).
#[utoipa::path(
    post,
    path = "/api/v1/institution/institutions",
    tag = "institution",
    params(
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key (1-255 visible ASCII)."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token from login or GET /api/v1/auth/session."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = RegisterInstitution,
    responses(
        (status = 201, body = Institution, headers(("ETag" = String, description = "Current version"))),
        (status = 400, body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "No live session", body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "CSRF failure or missing `institution.register` permission", body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn register(
    State(pool): State<PgPool>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(cmd): Json<RegisterInstitution>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionRegister)?;
    require_idempotency_key(&headers)?;
    let inst = repo::register(&pool, &ctx.scope, cmd, &actor(ctx)).await?;
    Ok(with_etag(StatusCode::CREATED, inst))
}

/// Get an institution of the caller's session tenant (`institution.read`).
///
/// Institutions of other tenants are indistinguishable from missing ones (404).
#[utoipa::path(
    get,
    path = "/api/v1/institution/institutions/{id}",
    tag = "institution",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = Institution, headers(("ETag" = String))),
        (status = 401, description = "No live session", body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "Missing `institution.read` permission", body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get(
    State(pool): State<PgPool>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    Ok(with_etag(
        StatusCode::OK,
        repo::get(&pool, &ctx.scope, id).await?,
    ))
}

/// Rename an institution of the caller's session tenant (optimistic concurrency via `If-Match`).
///
/// Requires the `institution.rename` permission (administrators).
#[utoipa::path(
    patch,
    path = "/api/v1/institution/institutions/{id}",
    tag = "institution",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the version being modified"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token from login or GET /api/v1/auth/session."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = RenameInstitution,
    responses(
        (status = 200, body = Institution, headers(("ETag" = String))),
        (status = 401, description = "No live session", body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "CSRF failure or missing `institution.rename` permission", body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn rename(
    State(pool): State<PgPool>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    Json(cmd): Json<RenameInstitution>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionRename)?;
    let expected = headers
        .get(header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            Version::from_if_match(v).ok_or_else(|| {
                KernelError::BadRequest("If-Match must be a strong ETag such as \"3\"".into())
            })
        })
        .transpose()?;
    let Some(expected) = expected else {
        return Ok(Problem::new(
            StatusCode::PRECONDITION_REQUIRED,
            "precondition-required",
            "If-Match header is required",
        )
        .into_response());
    };
    cmd.validate()?;
    Ok(with_etag(
        StatusCode::OK,
        repo::rename(&pool, &ctx.scope, id, expected, &cmd.name, &actor(ctx)).await?,
    ))
}

pub fn router(pool: PgPool) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(register))
        .routes(routes!(get, rename))
        .with_state(pool)
}
