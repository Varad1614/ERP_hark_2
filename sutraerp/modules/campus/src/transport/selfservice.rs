//! Transport self-service (`campus.self`, contract §4.4): the caller (student or staff, from the
//! session) requests a seat, sees their allocation and pass and raises complaints.

use super::network::{TransportRouteView, ROUTE_SELECT};
use super::passengers::{
    open_transport_complaint, request_allocation, TransportAllocationView, TransportPassView,
    PASS_SELECT, T_ALLOCATION_SELECT,
};
use crate::directory::http::CampusPersonView;
use crate::hostel::common::{
    complaints_of, ensure_member, fetch, reply, ComplaintCreate, ComplaintView,
};
use crate::shared::me::{my_person, PersonRef};
use crate::shared::scope::tenant_scope;
use crate::shared::CampusState;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use sutra_kernel::auth::Permission;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// The caller's transport situation.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyTransportView {
    pub person: CampusPersonView,
    /// Live (requested / active) allocation.
    pub allocation: Option<TransportAllocationView>,
    /// Live pass of that allocation.
    pub pass: Option<TransportPassView>,
    pub complaints: Vec<ComplaintView>,
}

/// Request a seat on a route.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyTransportRequest {
    pub institution_id: Uuid,
    pub route_id: Uuid,
    pub stop_id: Uuid,
}

/// Which institution's routes.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct MyRoutesQuery {
    pub institution_id: Uuid,
}

/// Routes a passenger can request.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MyTransportRouteList {
    pub items: Vec<TransportRouteView>,
}

async fn my_allocation(
    conn: &mut PgConnection,
    tenant: TenantId,
    p: &PersonRef,
) -> Result<Option<TransportAllocationView>, KernelError> {
    let sql = format!(
        "{T_ALLOCATION_SELECT} WHERE x.tenant_id = $1 AND x.person_kind = $2 AND x.person_id = $3 \
         AND x.status IN ('requested', 'active')"
    );
    sqlx::query_as::<_, TransportAllocationView>(&sql)
        .bind(tenant.as_uuid())
        .bind(p.kind.as_str())
        .bind(p.id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)
}

/// My allocation, pass and complaints (`campus.self`).
#[utoipa::path(get, path = "/api/v1/campus/me/transport", operation_id = "campus_transport_self_overview", tag = "campus-transport",
    security(("session_cookie" = [])),
    responses((status = 200, body = MyTransportView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn me_transport(
    State(st): State<CampusState>,
    auth: Authenticated,
) -> ApiResult<Json<MyTransportView>> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    let p = my_person(&st, ctx).await?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let allocation = my_allocation(&mut conn, t, &p).await?;
    let (pass, complaints) = match &allocation {
        Some(a) => {
            let pass = match a.pass_id {
                Some(id) => Some(
                    fetch::<TransportPassView>(&mut conn, PASS_SELECT, t, id, "transport pass")
                        .await?,
                ),
                None => None,
            };
            (
                pass,
                complaints_of(&mut conn, t, "transport", a.id).await?.items,
            )
        }
        None => (None, Vec::new()),
    };
    Ok(Json(MyTransportView {
        person: CampusPersonView::from(&p),
        allocation,
        pass,
        complaints,
    }))
}

/// Active routes of my institution (`campus.self`).
#[utoipa::path(get, path = "/api/v1/campus/me/transport/routes", operation_id = "campus_transport_self_routes", tag = "campus-transport",
    params(MyRoutesQuery), security(("session_cookie" = [])),
    responses((status = 200, body = MyTransportRouteList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn me_routes(
    State(st): State<CampusState>,
    auth: Authenticated,
    Query(q): Query<MyRoutesQuery>,
) -> ApiResult<Json<MyTransportRouteList>> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    let p = my_person(&st, ctx).await?;
    let inst = InstitutionId(q.institution_id);
    ensure_member(&st, &tenant_scope(ctx), &p, inst, "institutionId").await?;
    let sql = format!(
        "{ROUTE_SELECT} WHERE x.tenant_id = $1 AND x.institution_id = $2 AND x.status = 'active' ORDER BY x.code"
    );
    let items = sqlx::query_as::<_, TransportRouteView>(&sql)
        .bind(ctx.scope.tenant_id.as_uuid())
        .bind(q.institution_id)
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
    Ok(Json(MyTransportRouteList { items }))
}

/// Request a seat (`campus.self`): active when a seat is free, else `requested`.
#[utoipa::path(post, path = "/api/v1/campus/me/transport/request", operation_id = "campus_transport_self_request", tag = "campus-transport",
    params(("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = MyTransportRequest, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = TransportAllocationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn me_request(
    State(st): State<CampusState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(b): Json<MyTransportRequest>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    require_idempotency_key(&headers)?;
    let p = my_person(&st, ctx).await?;
    let inst = InstitutionId(b.institution_id);
    ensure_member(&st, &tenant_scope(ctx), &p, inst, "institutionId").await?;
    let id = request_allocation(
        &st,
        ctx,
        inst,
        &p,
        b.route_id,
        b.stop_id,
        None,
        "self_service",
    )
    .await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v: TransportAllocationView = fetch(
        &mut conn,
        T_ALLOCATION_SELECT,
        ctx.scope.tenant_id,
        id,
        "transport allocation",
    )
    .await?;
    Ok(reply(StatusCode::CREATED, &v, v.version))
}

/// My live pass (`campus.self`; 404 without one).
#[utoipa::path(get, path = "/api/v1/campus/me/transport/pass", operation_id = "campus_transport_self_pass", tag = "campus-transport",
    security(("session_cookie" = [])),
    responses((status = 200, body = TransportPassView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn me_pass(State(st): State<CampusState>, auth: Authenticated) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    let p = my_person(&st, ctx).await?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let sql = format!(
        "{PASS_SELECT} WHERE x.tenant_id = $1 AND x.person_kind = $2 AND x.person_id = $3 \
         AND x.status IN ('issued', 'active', 'suspended') ORDER BY x.valid_from DESC LIMIT 1"
    );
    let v = sqlx::query_as::<_, TransportPassView>(&sql)
        .bind(t.as_uuid())
        .bind(p.kind.as_str())
        .bind(p.id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "transport pass",
        })?;
    Ok(reply(StatusCode::OK, &v, v.version))
}

/// Raise a transport complaint (`campus.self`): a service desk ticket (source `transport`).
#[utoipa::path(post, path = "/api/v1/campus/me/transport/complaints", operation_id = "campus_transport_self_complaint", tag = "campus-transport",
    params(("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ComplaintCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ComplaintView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn me_complaint(
    State(st): State<CampusState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(b): Json<ComplaintCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    require_idempotency_key(&headers)?;
    let p = my_person(&st, ctx).await?;
    let alloc = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        my_allocation(&mut conn, ctx.scope.tenant_id, &p)
            .await?
            .ok_or(KernelError::NotFound {
                entity: "transport allocation",
            })?
    };
    let c = open_transport_complaint(&st, ctx, alloc.id, &b).await?;
    Ok(reply(StatusCode::CREATED, &c, c.version))
}
