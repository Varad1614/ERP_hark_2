use crate::domain::{Institution, RegisterInstitution, RenameInstitution};
use crate::repo::{self, Actor};
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use sqlx::PgPool;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::cache::{AdvisoryCache, CacheKey};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
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

/// Handler state: the owner's pool plus the advisory read-through cache (ADR-L5-0037).
#[derive(Clone)]
pub struct InstitutionState {
    pub pool: PgPool,
    pub cache: AdvisoryCache,
}

/// Cache owner segment: entries belong to the institution module.
const CACHE_OWNER: &str = "institution";

/// Read-through cache key of one institution, bound to the caller's *session tenant* — the
/// tenant is resolved and the permission checked before the cache is consulted, so a key can
/// never serve another tenant's row (the database query is tenant-filtered the same way).
fn cache_key(cache: &AdvisoryCache, scope: &ExecutionScope, id: InstitutionId) -> CacheKey {
    cache.key(
        CACHE_OWNER,
        &ExecutionScope::tenant(scope.tenant_id),
        &["institution", &id.to_string()],
    )
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
    State(st): State<InstitutionState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(cmd): Json<RegisterInstitution>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionRegister)?;
    require_idempotency_key(&headers)?;
    let inst = repo::register(&st.pool, &ctx.scope, cmd, &actor(ctx)).await?;
    // Owner-driven invalidation (defensive: guards against any negative/stale entry for the id).
    st.cache
        .invalidate(&cache_key(&st.cache, &ctx.scope, inst.id))
        .await;
    Ok(with_etag(StatusCode::CREATED, inst))
}

/// Get an institution of the caller's session tenant (`institution.read`).
///
/// Institutions of other tenants are indistinguishable from missing ones (404).
#[utoipa::path(
    get,
    path = "/api/v1/institution/institutions/{id}",
    operation_id = "get_institution",
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
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let key = cache_key(&st.cache, &ctx.scope, id);
    if let Some(hit) = st.cache.get_json::<Institution>(&key).await {
        // Defence in depth: never serve an entry whose tenant differs from the session's.
        if hit.tenant_id == ctx.scope.tenant_id && hit.id == id {
            return Ok(with_etag(StatusCode::OK, hit));
        }
        st.cache.invalidate(&key).await;
    }
    let inst = repo::get(&st.pool, &ctx.scope, id).await?;
    st.cache.put_json(&key, &inst).await;
    Ok(with_etag(StatusCode::OK, inst))
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
    State(st): State<InstitutionState>,
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
    let key = cache_key(&st.cache, &ctx.scope, id);
    // Freshness-critical write: the version check runs against the owner's row, never the cache.
    let result = repo::rename(&st.pool, &ctx.scope, id, expected, &cmd.name, &actor(ctx)).await;
    // Invalidate after commit, and also on a conflict/not-found (the cached ETag is stale), so a
    // client that refetches after 412 sees the current version.
    st.cache.invalidate(&key).await;
    Ok(with_etag(StatusCode::OK, result?))
}

/// Institution routes. `cache` is advisory: pass `AdvisoryCache::disabled()` to turn it off.
pub fn router(pool: PgPool, cache: AdvisoryCache) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(register))
        .routes(routes!(get, rename))
        .with_state(InstitutionState { pool, cache })
}
