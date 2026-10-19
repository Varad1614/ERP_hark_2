//! Hostel self-service (`campus.self`, contract §4.4): the caller is resolved from the session
//! (`shared::me::my_person` — student record, else staff record) and only ever acts on their own
//! application / residency (someone else's → 404).

use super::admission::{
    submit_application, withdraw_application, HostelAllocationView, HostelApplicationView,
    ALLOCATION_SELECT, APPLICATION_SELECT,
};
use super::common::{
    complaints_of, ensure_member, expected, fetch, reply, ComplaintCreate, ComplaintView,
    ReasonBody,
};
use super::residency::{
    open_complaint, request_leave, ResidentLeaveCreate, ResidentLeaveView, LEAVE_SELECT,
};
use crate::directory::http::CampusPersonView;
use crate::shared::me::{my_person, PersonRef};
use crate::shared::scope::tenant_scope;
use crate::shared::CampusState;
use axum::extract::State;
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
use utoipa::ToSchema;
use uuid::Uuid;

/// The caller's hostel situation.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyHostelView {
    pub person: CampusPersonView,
    /// Latest application.
    pub application: Option<HostelApplicationView>,
    /// Live residency.
    pub allocation: Option<HostelAllocationView>,
    /// Leaves of the live residency.
    pub leaves: Vec<ResidentLeaveView>,
    /// Complaints raised for the live residency.
    pub complaints: Vec<ComplaintView>,
}

/// The caller's own hostel application.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyHostelApplicationCreate {
    /// Institution the caller studies / works at.
    pub institution_id: Uuid,
    pub preference: Option<String>,
}

async fn my_application(
    conn: &mut PgConnection,
    tenant: TenantId,
    p: &PersonRef,
) -> Result<Option<HostelApplicationView>, KernelError> {
    let sql = format!(
        "{APPLICATION_SELECT} WHERE x.tenant_id = $1 AND x.person_kind = $2 AND x.person_id = $3 \
         ORDER BY x.created_at DESC LIMIT 1"
    );
    sqlx::query_as::<_, HostelApplicationView>(&sql)
        .bind(tenant.as_uuid())
        .bind(p.kind.as_str())
        .bind(p.id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)
}

async fn my_allocation(
    conn: &mut PgConnection,
    tenant: TenantId,
    p: &PersonRef,
) -> Result<Option<HostelAllocationView>, KernelError> {
    let sql = format!(
        "{ALLOCATION_SELECT} WHERE x.tenant_id = $1 AND x.person_kind = $2 AND x.person_id = $3 \
         AND x.status <> 'closed'"
    );
    sqlx::query_as::<_, HostelAllocationView>(&sql)
        .bind(tenant.as_uuid())
        .bind(p.kind.as_str())
        .bind(p.id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)
}

async fn require_my_allocation(
    st: &CampusState,
    tenant: TenantId,
    p: &PersonRef,
) -> Result<HostelAllocationView, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    my_allocation(&mut conn, tenant, p)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "hostel residency",
        })
}

/// My hostel application, residency, leaves and complaints (`campus.self`).
#[utoipa::path(get, path = "/api/v1/campus/me/hostel", operation_id = "campus_hostel_self_overview", tag = "campus-hostel",
    security(("session_cookie" = [])),
    responses((status = 200, body = MyHostelView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn me_hostel(
    State(st): State<CampusState>,
    auth: Authenticated,
) -> ApiResult<Json<MyHostelView>> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    let p = my_person(&st, ctx).await?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let application = my_application(&mut conn, t, &p).await?;
    let allocation = my_allocation(&mut conn, t, &p).await?;
    let (leaves, complaints) = match &allocation {
        Some(a) => {
            let sql = format!(
                "{LEAVE_SELECT} WHERE x.tenant_id = $1 AND x.allocation_id = $2 ORDER BY x.from_date DESC LIMIT 50"
            );
            let leaves = sqlx::query_as::<_, ResidentLeaveView>(&sql)
                .bind(t.as_uuid())
                .bind(a.id)
                .fetch_all(&mut *conn)
                .await
                .map_err(map_db_error)?;
            let complaints = complaints_of(&mut conn, t, "hostel", a.id).await?.items;
            (leaves, complaints)
        }
        None => (Vec::new(), Vec::new()),
    };
    Ok(Json(MyHostelView {
        person: CampusPersonView::from(&p),
        application,
        allocation,
        leaves,
        complaints,
    }))
}

/// Apply for a hostel place myself (`campus.self`; students and staff of the institution).
#[utoipa::path(post, path = "/api/v1/campus/me/hostel/application", operation_id = "campus_hostel_self_apply", tag = "campus-hostel",
    params(("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = MyHostelApplicationCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = HostelApplicationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn me_apply(
    State(st): State<CampusState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(b): Json<MyHostelApplicationCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    require_idempotency_key(&headers)?;
    let p = my_person(&st, ctx).await?;
    let inst = InstitutionId(b.institution_id);
    ensure_member(&st, &tenant_scope(ctx), &p, inst, "institutionId").await?;
    let id = submit_application(&st, ctx, inst, &p, b.preference, "self_service").await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v: HostelApplicationView = fetch(
        &mut conn,
        APPLICATION_SELECT,
        ctx.scope.tenant_id,
        id,
        "hostel application",
    )
    .await?;
    Ok(reply(StatusCode::CREATED, &v, v.version))
}

/// Withdraw my open application (`campus.self`).
#[utoipa::path(post, path = "/api/v1/campus/me/hostel/application/withdraw", operation_id = "campus_hostel_self_withdraw", tag = "campus-hostel",
    params(("If-Match" = String, Header, description = "ETag of the application")),
    request_body = ReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = HostelApplicationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn me_withdraw(
    State(st): State<CampusState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(b): Json<ReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    let v = expected!(headers);
    let reason = b.text()?;
    let p = my_person(&st, ctx).await?;
    let t = ctx.scope.tenant_id;
    let app = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        my_application(&mut conn, t, &p)
            .await?
            .ok_or(KernelError::NotFound {
                entity: "hostel application",
            })?
    };
    withdraw_application(&st, ctx, app.id, v, reason.as_deref()).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let out: HostelApplicationView = fetch(
        &mut conn,
        APPLICATION_SELECT,
        t,
        app.id,
        "hostel application",
    )
    .await?;
    Ok(reply(StatusCode::OK, &out, out.version))
}

/// Request leave from my residency (`campus.self`).
#[utoipa::path(post, path = "/api/v1/campus/me/hostel/leave", operation_id = "campus_hostel_self_leave", tag = "campus-hostel",
    params(("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ResidentLeaveCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ResidentLeaveView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn me_leave(
    State(st): State<CampusState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(b): Json<ResidentLeaveCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    require_idempotency_key(&headers)?;
    let p = my_person(&st, ctx).await?;
    let t = ctx.scope.tenant_id;
    let alloc = require_my_allocation(&st, t, &p).await?;
    let id = request_leave(&st, ctx, alloc.id, b).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v: ResidentLeaveView = fetch(&mut conn, LEAVE_SELECT, t, id, "resident leave").await?;
    Ok(reply(StatusCode::CREATED, &v, v.version))
}

/// Raise a complaint about my hostel stay (`campus.self`): a service desk ticket (source `hostel`).
#[utoipa::path(post, path = "/api/v1/campus/me/hostel/complaints", operation_id = "campus_hostel_self_complaint", tag = "campus-hostel",
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
    let alloc = require_my_allocation(&st, ctx.scope.tenant_id, &p).await?;
    let c = open_complaint(&st, ctx, alloc.id, &b).await?;
    Ok(reply(StatusCode::CREATED, &c, c.version))
}
