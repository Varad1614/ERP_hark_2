//! HTTP handlers of position definitions (tag `organisation`, operation ids `position_*`).

use super::domain::{
    ChangePosition, EstablishPosition, Position, PositionRevision, RetirePosition,
    UpdateDraftPosition,
};
use super::repo;
use crate::lifecycle::common::{expect_version, CommandErrors, CreateErrors, ReadErrors};
use crate::lifecycle::domain::CommandBody;
use crate::shared::http::{actor, with_etag};
use crate::shared::scope::institution_scope;
use crate::shared::InstitutionState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::NaiveDate;
use sutra_contracts::institution::ids::{OrgUnitId, PositionId};
use sutra_contracts::institution::status::PositionStatus;
use sutra_kernel::auth::Permission;
use sutra_kernel::scope::InstitutionId;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated};

/// Position collection.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct PositionList {
    pub items: Vec<Position>,
}

/// Revision collection.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct PositionRevisionList {
    pub items: Vec<PositionRevision>,
}

/// Filters of `GET …/positions`.
#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct PositionListQuery {
    pub status: Option<PositionStatus>,
    /// Only definitions placed in this unit on `asOf`.
    pub org_unit_id: Option<OrgUnitId>,
    /// Business-local date (`YYYY-MM-DD`); default today in the institution timezone.
    pub as_of: Option<NaiveDate>,
}

/// `?asOf=`.
#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct PositionAsOf {
    /// Business-local date (`YYYY-MM-DD`); default today in the institution timezone.
    pub as_of: Option<NaiveDate>,
}

fn respond(status: StatusCode, p: Position) -> Response {
    with_etag(status, &p, p.version)
}

/// Establish a position definition in `draft` (`position.manage`). Creates no occupant and no
/// assignment.
#[utoipa::path(
    post,
    path = "/api/v1/institution/institutions/{id}/positions",
    operation_id = "position_establish",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = EstablishPosition,
    responses((status = 201, body = Position, headers(("ETag" = String))), CreateErrors)
)]
pub async fn establish(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    Json(cmd): Json<EstablishPosition>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PositionManage)?;
    require_idempotency_key(&headers)?;
    let scope = institution_scope(ctx, id);
    Ok(respond(
        StatusCode::CREATED,
        repo::establish(&st.pool, &scope, id, &cmd, &actor(ctx)).await?,
    ))
}

/// Position definitions of an institution as of a date (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/institutions/{id}/positions",
    operation_id = "position_list",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid), PositionListQuery),
    security(("session_cookie" = [])),
    responses((status = 200, body = PositionList), ReadErrors)
)]
pub async fn list(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    Query(q): Query<PositionListQuery>,
) -> ApiResult<Json<PositionList>> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let scope = institution_scope(ctx, id);
    Ok(Json(PositionList {
        items: repo::list(&st.pool, &scope, id, q.status, q.org_unit_id, q.as_of).await?,
    }))
}

/// One position definition as of a date (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/positions/{id}",
    operation_id = "position_get",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid), PositionAsOf),
    security(("session_cookie" = [])),
    responses((status = 200, body = Position, headers(("ETag" = String))), ReadErrors)
)]
pub async fn get(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<PositionId>,
    Query(q): Query<PositionAsOf>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    Ok(respond(
        StatusCode::OK,
        repo::get(&st.pool, &ctx.scope, id, q.as_of).await?,
    ))
}

/// Edit a draft definition in place (`position.manage`).
#[utoipa::path(
    patch,
    path = "/api/v1/institution/positions/{id}",
    operation_id = "position_update_draft",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = UpdateDraftPosition,
    responses((status = 200, body = Position, headers(("ETag" = String))), CommandErrors)
)]
pub async fn update_draft(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<PositionId>,
    headers: HeaderMap,
    Json(cmd): Json<UpdateDraftPosition>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PositionManage)?;
    let expected = expect_version!(headers);
    Ok(respond(
        StatusCode::OK,
        repo::update_draft(&st.pool, &ctx.scope, id, expected, &cmd, &actor(ctx)).await?,
    ))
}

/// Every revision of a definition, oldest first (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/positions/{id}/revisions",
    operation_id = "position_list_revisions",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = PositionRevisionList), ReadErrors)
)]
pub async fn list_revisions(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<PositionId>,
) -> ApiResult<Json<PositionRevisionList>> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    Ok(Json(PositionRevisionList {
        items: repo::revisions(&st.pool, &ctx.scope, id).await?,
    }))
}

/// Activate a draft definition (`position.manage`); its organisation unit must be active.
#[utoipa::path(
    post,
    path = "/api/v1/institution/positions/{id}/activate",
    operation_id = "position_activate",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<CommandBody>, description = "Optional `{ reason }`"),
    responses((status = 200, body = Position, headers(("ETag" = String))), CommandErrors)
)]
pub async fn activate(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<PositionId>,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PositionManage)?;
    let expected = expect_version!(headers);
    let reason = body.map(|b| b.0).unwrap_or_default().optional_reason()?;
    Ok(respond(
        StatusCode::OK,
        repo::activate(&st.pool, &ctx.scope, id, expected, reason, &actor(ctx)).await?,
    ))
}

/// Record an effective-dated change of an active definition (`position.manage`).
///
/// Creates the successor revision from `effectiveFrom` (not before today, after the latest
/// revision); the predecessor stays in force until the day before. Existing assignments are
/// not touched.
#[utoipa::path(
    post,
    path = "/api/v1/institution/positions/{id}/changes",
    operation_id = "position_change",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = ChangePosition,
    responses((status = 200, body = Position, headers(("ETag" = String))), CommandErrors)
)]
pub async fn change(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<PositionId>,
    headers: HeaderMap,
    Json(cmd): Json<ChangePosition>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PositionManage)?;
    let expected = expect_version!(headers);
    Ok(respond(
        StatusCode::OK,
        repo::change(&st.pool, &ctx.scope, id, expected, &cmd, &actor(ctx)).await?,
    ))
}

/// Retire an active definition (`position.manage`); reason required.
#[utoipa::path(
    post,
    path = "/api/v1/institution/positions/{id}/retire",
    operation_id = "position_retire",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = RetirePosition,
    responses((status = 200, body = Position, headers(("ETag" = String))), CommandErrors)
)]
pub async fn retire(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<PositionId>,
    headers: HeaderMap,
    body: Option<Json<RetirePosition>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PositionManage)?;
    let expected = expect_version!(headers);
    let cmd = body.map(|b| b.0).unwrap_or_default();
    Ok(respond(
        StatusCode::OK,
        repo::retire(&st.pool, &ctx.scope, id, expected, &cmd, &actor(ctx)).await?,
    ))
}

/// Archive a retired definition (`position.manage`; terminal).
#[utoipa::path(
    post,
    path = "/api/v1/institution/positions/{id}/archive",
    operation_id = "position_archive",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<CommandBody>, description = "Optional `{ reason }`"),
    responses((status = 200, body = Position, headers(("ETag" = String))), CommandErrors)
)]
pub async fn archive(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<PositionId>,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PositionManage)?;
    let expected = expect_version!(headers);
    let reason = body.map(|b| b.0).unwrap_or_default().optional_reason()?;
    Ok(respond(
        StatusCode::OK,
        repo::archive(&st.pool, &ctx.scope, id, expected, reason, &actor(ctx)).await?,
    ))
}
