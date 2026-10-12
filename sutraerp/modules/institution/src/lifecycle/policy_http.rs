//! HTTP handlers of institutional policies (tag `institution`, operation ids `institution_policy_*`).

use super::common::{expect_version, CommandErrors, CreateErrors, ReadErrors};
use super::policy::{self, CreatePolicy, DraftPolicyRevision, Policy, PolicyRevision};
use crate::shared::http::{actor, with_etag};
use crate::shared::scope::institution_scope;
use crate::shared::InstitutionState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::NaiveDate;
use sutra_contracts::institution::ids::InstitutionPolicyId;
use sutra_contracts::institution::status::PolicyStatus;
use sutra_kernel::auth::Permission;
use sutra_kernel::scope::InstitutionId;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated};

/// Policy collection.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct PolicyList {
    pub items: Vec<Policy>,
}

/// Revision collection.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct PolicyRevisionList {
    pub items: Vec<PolicyRevision>,
}

/// `?status=` filter.
#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PolicyListQuery {
    pub status: Option<PolicyStatus>,
}

/// `?asOf=` business-local date.
#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct AsOfQuery {
    /// Business-local date (`YYYY-MM-DD`, institution timezone); default today.
    pub as_of: Option<NaiveDate>,
}

fn respond(status: StatusCode, p: Policy) -> Response {
    with_etag(status, &p, p.version)
}

/// Create a policy with its first draft revision (`institution.policy.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/institution/institutions/{id}/policies",
    operation_id = "institution_policy_create",
    tag = "institution",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = CreatePolicy,
    responses((status = 201, body = Policy, headers(("ETag" = String))), CreateErrors)
)]
pub async fn create(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    Json(cmd): Json<CreatePolicy>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionPolicyManage)?;
    require_idempotency_key(&headers)?;
    let scope = institution_scope(ctx, id);
    let p = policy::create(&st.pool, &scope, id, &cmd, &actor(ctx)).await?;
    Ok(respond(StatusCode::CREATED, p))
}

/// Policies of an institution (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/institutions/{id}/policies",
    operation_id = "institution_policy_list",
    tag = "institution",
    params(("id" = String, Path, format = Uuid), PolicyListQuery),
    security(("session_cookie" = [])),
    responses((status = 200, body = PolicyList), ReadErrors)
)]
pub async fn list(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    Query(q): Query<PolicyListQuery>,
) -> ApiResult<Json<PolicyList>> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let scope = institution_scope(ctx, id);
    let items = policy::list(&st.pool, &scope, id, q.status).await?;
    Ok(Json(PolicyList { items }))
}

/// One policy with the revision in force on `asOf` (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/policies/{id}",
    operation_id = "institution_policy_get",
    tag = "institution",
    params(("id" = String, Path, format = Uuid), AsOfQuery),
    security(("session_cookie" = [])),
    responses((status = 200, body = Policy, headers(("ETag" = String))), ReadErrors)
)]
pub async fn get(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionPolicyId>,
    Query(q): Query<AsOfQuery>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let p = policy::get(&st.pool, &ctx.scope, id, q.as_of).await?;
    Ok(respond(StatusCode::OK, p))
}

/// Every revision of a policy, newest first (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/policies/{id}/revisions",
    operation_id = "institution_policy_list_revisions",
    tag = "institution",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = PolicyRevisionList), ReadErrors)
)]
pub async fn list_revisions(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionPolicyId>,
) -> ApiResult<Json<PolicyRevisionList>> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let items = policy::revisions(&st.pool, &ctx.scope, id).await?;
    Ok(Json(PolicyRevisionList { items }))
}

/// Create the successor draft revision, or replace the open draft (`institution.policy.manage`).
///
/// The effective revision is never edited in place.
#[utoipa::path(
    post,
    path = "/api/v1/institution/policies/{id}/revisions",
    operation_id = "institution_policy_draft_revision",
    tag = "institution",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the policy")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = DraftPolicyRevision,
    responses((status = 200, body = Policy, headers(("ETag" = String))), CommandErrors)
)]
pub async fn draft_revision(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionPolicyId>,
    headers: HeaderMap,
    Json(cmd): Json<DraftPolicyRevision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionPolicyManage)?;
    let expected = expect_version!(headers);
    let p = policy::draft_revision(&st.pool, &ctx.scope, id, expected, &cmd, &actor(ctx)).await?;
    Ok(respond(StatusCode::OK, p))
}

/// Publish the draft revision: it governs from its `effectiveFrom` (not before today in the
/// institution timezone, strictly after the current revision's date); the predecessor becomes
/// `superseded` (`institution.policy.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/institution/policies/{id}/publish",
    operation_id = "institution_policy_publish",
    tag = "institution",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the policy")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = Policy, headers(("ETag" = String))), CommandErrors)
)]
pub async fn publish(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionPolicyId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionPolicyManage)?;
    let expected = expect_version!(headers);
    let p = policy::publish(&st.pool, &ctx.scope, id, expected, &actor(ctx)).await?;
    Ok(respond(StatusCode::OK, p))
}
