//! HTTP handlers of the institutional campus (`tag = "campuses"`, operation ids `campus_*`).

use super::domain::{
    Campus, CampusList, CampusRevisionList, DecommissionCampus, ModifyCampus, RegisterCampus,
};
use super::repo;
use crate::curriculum::support::{body, DecisionBody, ReasonBody};
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::scope::institution_scope;
use crate::shared::InstitutionState;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use sutra_contracts::institution::ids::CampusId;
use sutra_contracts::institution::status::CampusStatus;
use sutra_kernel::auth::Permission;
use sutra_kernel::scope::InstitutionId;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct CampusQuery {
    /// Only campuses in this state.
    #[param(inline)]
    pub status: Option<CampusStatus>,
}

fn ok(c: &Campus) -> Response {
    with_etag(StatusCode::OK, c, c.version)
}

/// Register an institutional campus (L3-B-017). It starts in `registered` (Proposed).
///
/// Requires `campus.manage`. Code is unique within the institution and never reused.
#[utoipa::path(
    post,
    path = "/api/v1/institution/institutions/{id}/campuses",
    operation_id = "campus_register",
    tag = "campuses",
    params(
        ("id" = String, Path, format = Uuid, description = "Institution id"),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = RegisterCampus,
    responses(
        (status = 201, body = Campus, headers(("ETag" = String))),
        (status = 400, body = Problem, content_type = "application/problem+json"),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Duplicate code, second main campus, or institution suspended/archived", body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn register(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    Json(cmd): Json<RegisterCampus>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusManage)?;
    require_idempotency_key(&headers)?;
    let scope = institution_scope(ctx, id);
    let c = repo::register(&st.pool, &scope, id, cmd, &actor(ctx)).await?;
    Ok(with_etag(StatusCode::CREATED, &c, c.version))
}

/// List the campuses of an institution (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/institutions/{id}/campuses",
    operation_id = "campus_list",
    tag = "campuses",
    params(("id" = String, Path, format = Uuid, description = "Institution id"), CampusQuery),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = CampusList),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    Query(q): Query<CampusQuery>,
) -> ApiResult<Json<CampusList>> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let scope = institution_scope(ctx, id);
    let items = repo::list(&st.pool, &scope, id, q.status).await?;
    Ok(Json(CampusList { items }))
}

/// Get one campus (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/campuses/{campusId}",
    operation_id = "campus_get",
    tag = "campuses",
    params(("campusId" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = Campus, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(campus_id): Path<CampusId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    Ok(ok(&repo::get(&st.pool, &ctx.scope, campus_id).await?))
}

/// Governed modification of campus metadata (L3-B-019). Code, institution, type and declared
/// capacity are immutable; the previous metadata is kept in the revision history.
#[utoipa::path(
    patch,
    path = "/api/v1/institution/campuses/{campusId}",
    operation_id = "campus_modify",
    tag = "campuses",
    params(
        ("campusId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the version being modified"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = ModifyCampus,
    responses(
        (status = 200, body = Campus, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn modify(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(campus_id): Path<CampusId>,
    headers: HeaderMap,
    Json(cmd): Json<ModifyCampus>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(res) => return Ok(res),
    };
    let c = repo::modify(&st.pool, &ctx.scope, campus_id, expected, cmd, &actor(ctx)).await?;
    Ok(ok(&c))
}

/// Change history of a campus (registration, modifications, lifecycle transitions).
#[utoipa::path(
    get,
    path = "/api/v1/institution/campuses/{campusId}/revisions",
    operation_id = "campus_revisions",
    tag = "campuses",
    params(("campusId" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = CampusRevisionList),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn revisions(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(campus_id): Path<CampusId>,
) -> ApiResult<Json<CampusRevisionList>> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let items = repo::revisions(&st.pool, &ctx.scope, campus_id).await?;
    Ok(Json(CampusRevisionList { items }))
}

macro_rules! state_command {
    ($fn:ident, $path:literal, $op:literal, $doc:literal, $perm:expr, $reason_required:expr, $to:expr) => {
        #[doc = $doc]
        #[utoipa::path(
            post,
            path = $path,
            operation_id = $op,
            tag = "campuses",
            params(
                ("campusId" = String, Path, format = Uuid),
                ("If-Match" = String, Header, description = "ETag of the current version"),
                ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
            ),
            security(("session_cookie" = [], "csrf_token" = [])),
            request_body(content = Option<ReasonBody>),
            responses(
                (status = 200, body = Campus, headers(("ETag" = String))),
                (status = 403, body = Problem, content_type = "application/problem+json"),
                (status = 404, body = Problem, content_type = "application/problem+json"),
                (status = 409, description = "Illegal transition or institution read-only", body = Problem, content_type = "application/problem+json"),
                (status = 412, body = Problem, content_type = "application/problem+json"),
                (status = 422, body = Problem, content_type = "application/problem+json"),
                (status = 428, body = Problem, content_type = "application/problem+json"),
            )
        )]
        pub async fn $fn(
            State(st): State<InstitutionState>,
            auth: Authenticated,
            Path(campus_id): Path<CampusId>,
            headers: HeaderMap,
            raw: Bytes,
        ) -> ApiResult<Response> {
            let ctx = auth.require($perm)?;
            let expected = match if_match(&headers)? {
                Ok(v) => v,
                Err(res) => return Ok(res),
            };
            let reason = body::<ReasonBody>(&raw)?.validated($reason_required)?;
            let c = repo::transition(
                &st.pool,
                &ctx.scope,
                campus_id,
                expected,
                $to,
                reason,
                &actor(ctx),
            )
            .await?;
            Ok(ok(&c))
        }
    };
}

state_command!(
    suspend,
    "/api/v1/institution/campuses/{campusId}/suspend",
    "campus_suspend",
    "Temporarily close an active campus (L3-B-020, `campus.manage`; reason required).",
    Permission::CampusManage,
    true,
    CampusStatus::Suspended
);
state_command!(
    restore,
    "/api/v1/institution/campuses/{campusId}/restore",
    "campus_restore",
    "Restore a suspended campus to active (L3-B-020, `campus.manage`).",
    Permission::CampusManage,
    false,
    CampusStatus::Active
);
state_command!(
    archive,
    "/api/v1/institution/campuses/{campusId}/archive",
    "campus_archive",
    "Archive a decommissioned campus as terminal history (L3-B-022, `campus.decommission`, step-up).",
    Permission::CampusDecommission,
    false,
    CampusStatus::Archived
);

/// Request the activation decision (L3-B-018, `campus.manage`). Address line 1, city, state and
/// postal code must be set. With Batch 2 auto-approval the campus is returned `active`.
#[utoipa::path(
    post,
    path = "/api/v1/institution/campuses/{campusId}/activate",
    operation_id = "campus_activate",
    tag = "campuses",
    params(
        ("campusId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the current version"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<ReasonBody>),
    responses(
        (status = 200, body = Campus, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "Mandatory metadata missing", body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn activate(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(campus_id): Path<CampusId>,
    headers: HeaderMap,
    raw: Bytes,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(res) => return Ok(res),
    };
    let reason = body::<ReasonBody>(&raw)?.validated(false)?;
    let c = repo::activate(&st, &ctx.scope, campus_id, expected, reason, &actor(ctx)).await?;
    Ok(ok(&c))
}

/// Decide a pending activation (`campus.manage`) or decommissioning (`campus.decommission`,
/// step-up). Used when approvals await a human decision (WorkflowPort `AwaitDecision`).
#[utoipa::path(
    post,
    path = "/api/v1/institution/campuses/{campusId}/decision",
    operation_id = "campus_decision",
    tag = "campuses",
    params(
        ("campusId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the current version"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = DecisionBody,
    responses(
        (status = 200, body = Campus, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "No pending decision", body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn decision(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(campus_id): Path<CampusId>,
    headers: HeaderMap,
    raw: Bytes,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(res) => return Ok(res),
    };
    let (decision, reason) = DecisionBody::parse(&raw)?;
    if repo::pending_action(&st.pool, &ctx.scope, campus_id)
        .await?
        .as_deref()
        == Some("decommission")
    {
        auth.require(Permission::CampusDecommission)?;
    }
    let c = repo::decide(
        &st.pool,
        &ctx.scope,
        campus_id,
        expected,
        decision,
        reason,
        &actor(ctx),
    )
    .await?;
    Ok(ok(&c))
}

/// Decommission an active campus (L3-B-021, `campus.decommission`, step-up). Requires an
/// explicit effective date and reason; blocked while non-retired programmes are offered there.
#[utoipa::path(
    post,
    path = "/api/v1/institution/campuses/{campusId}/decommission",
    operation_id = "campus_decommission",
    tag = "campuses",
    params(
        ("campusId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the current version"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = DecommissionCampus,
    responses(
        (status = 200, body = Campus, headers(("ETag" = String))),
        (status = 403, description = "Missing permission or step-up", body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Illegal transition or unresolved usages", body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn decommission(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(campus_id): Path<CampusId>,
    headers: HeaderMap,
    Json(cmd): Json<DecommissionCampus>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusDecommission)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(res) => return Ok(res),
    };
    let c = repo::decommission(&st, &ctx.scope, campus_id, expected, cmd, &actor(ctx)).await?;
    Ok(ok(&c))
}
