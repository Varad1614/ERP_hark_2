//! Mess definitions and dining assignments of residents (L3-G-090 mess allocation → active
//! dining assignment with a capacity check, G-091 change → effective reallocation: the old
//! assignment becomes `changed`, a new one starts).

use super::admission::ALLOCATION;
use super::common::{
    apply, code, created, ensure_version, expected, fetch, head, invalid, list_of, reply,
    required_text, status_filter, Agg, Move, StatusFilter,
};
use super::domain::{HostelAllocationStatus, MessAllocationStatus};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::actor;
use crate::shared::scope::institution_scope;
use crate::shared::time::business_today;
use crate::shared::{Actor, CampusState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Row};
use sutra_kernel::auth::Permission;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use uuid::Uuid;

pub(crate) const MESS: Agg = Agg {
    table: "mess",
    entity: "mess",
    number: "code",
    subject: "NULL",
};

pub(crate) const MESS_ALLOCATION: Agg = Agg {
    table: "mess_allocation",
    entity: "mess allocation",
    number: "person_number",
    subject: "NULL",
};

/// A mess (dining hall).
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct MessView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "MESS-A")]
    pub code: String,
    pub name: String,
    /// `veg | non_veg | mixed`.
    pub menu_type: String,
    pub capacity: i32,
    /// Active dining assignments.
    pub assigned: i64,
    /// `active | inactive`.
    pub status: String,
    pub version: i64,
}

const MESS_SELECT: &str = "SELECT x.id, x.institution_id, x.code, x.name, x.menu_type, x.capacity, \
    (SELECT count(*) FROM campus.mess_allocation m WHERE m.mess_id = x.id AND m.status = 'active') AS assigned, \
    x.status, x.version FROM campus.mess x";

/// Messes of an institution.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MessList {
    pub items: Vec<MessView>,
}

/// A new mess.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MessCreate {
    pub code: String,
    pub name: String,
    /// `veg | non_veg | mixed`.
    pub menu_type: String,
    pub capacity: i32,
}

/// A resident's dining assignment.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct MessAllocationView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub mess_id: Uuid,
    pub mess_code: String,
    pub hostel_allocation_id: Uuid,
    pub person_number: String,
    pub person_name: String,
    pub start_date: NaiveDate,
    pub end_date: Option<NaiveDate>,
    /// `active | changed | ended`.
    pub status: String,
    pub status_reason: Option<String>,
    pub previous_id: Option<Uuid>,
    pub version: i64,
}

const MESS_ALLOCATION_SELECT: &str =
    "SELECT x.id, x.institution_id, x.mess_id, m.code AS mess_code, \
    x.hostel_allocation_id, x.person_number, x.person_name, x.start_date, x.end_date, x.status, \
    x.status_reason, x.previous_id, x.version FROM campus.mess_allocation x \
    JOIN campus.mess m ON m.id = x.mess_id";

/// Dining assignments.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MessAllocationList {
    pub items: Vec<MessAllocationView>,
}

/// Assign a resident to this mess.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MessAllocationCreate {
    pub hostel_allocation_id: Uuid,
    /// Default: today.
    pub start_date: Option<NaiveDate>,
}

/// Move a resident to another mess.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MessAllocationChange {
    pub mess_id: Uuid,
    /// First day in the new mess (default: today).
    pub effective_from: Option<NaiveDate>,
    pub reason: Option<String>,
}

async fn mess_allocation_reply(
    st: &CampusState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v: MessAllocationView = fetch(
        &mut conn,
        MESS_ALLOCATION_SELECT,
        tenant,
        id,
        MESS_ALLOCATION.entity,
    )
    .await?;
    Ok(reply(status, &v, v.version))
}

/// Inserts an active dining assignment in `mess` (locked, active, with room) for a live residency.
async fn assign(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    mess: Uuid,
    hostel_allocation: Uuid,
    start: NaiveDate,
    previous: Option<Uuid>,
    actor: &Actor,
) -> Result<Uuid, KernelError> {
    let t = scope.tenant_id;
    let m = sqlx::query(
        "SELECT institution_id, status, capacity, code FROM campus.mess WHERE id = $1 AND tenant_id = $2 FOR UPDATE",
    )
    .bind(mess)
    .bind(t.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| invalid("messId", "unknown_mess", "no mess with this id"))?;
    let inst: Uuid = m.try_get("institution_id").map_err(map_db_error)?;
    let status: String = m.try_get("status").map_err(map_db_error)?;
    let capacity: i32 = m.try_get("capacity").map_err(map_db_error)?;
    let mess_code: String = m.try_get("code").map_err(map_db_error)?;
    if status != "active" {
        return Err(KernelError::Conflict(format!(
            "mess {mess_code} is {status}"
        )));
    }
    let a = sqlx::query(
        "SELECT institution_id, status, person_number, person_name FROM campus.hostel_allocation \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(hostel_allocation)
    .bind(t.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| {
        invalid(
            "hostelAllocationId",
            "unknown_allocation",
            "no residency with this id",
        )
    })?;
    let a_inst: Uuid = a.try_get("institution_id").map_err(map_db_error)?;
    let a_status: String = a.try_get("status").map_err(map_db_error)?;
    if a_inst != inst {
        return Err(invalid(
            "hostelAllocationId",
            "other_institution",
            "the residency belongs to another institution",
        ));
    }
    if a_status == HostelAllocationStatus::Closed.as_str()
        || a_status == HostelAllocationStatus::Vacating.as_str()
    {
        return Err(KernelError::Conflict(format!(
            "the residency is {a_status}; it takes no new mess assignment"
        )));
    }
    let used: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM campus.mess_allocation WHERE mess_id = $1 AND status = 'active'",
    )
    .bind(mess)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if used >= i64::from(capacity) {
        return Err(KernelError::Conflict(format!(
            "mess {mess_code} is full ({used} of {capacity})"
        )));
    }
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.mess_allocation (tenant_id, institution_id, mess_id, hostel_allocation_id, \
         person_number, person_name, start_date, previous_id) \
         SELECT $1, $2, $3, $4, person_number, person_name, $5, $6 FROM campus.hostel_allocation \
         WHERE id = $4 RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .bind(mess)
    .bind(hostel_allocation)
    .bind(start)
    .bind(previous)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    created(
        conn,
        scope,
        &MESS_ALLOCATION,
        id,
        MessAllocationStatus::Active.as_str(),
        "campus.mess_allocation.assign",
        serde_json::json!({ "mess": mess_code, "previous": previous }),
        actor,
    )
    .await?;
    Ok(id)
}

/// Messes of an institution (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/messes", operation_id = "campus_hostel_mess_list", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = MessList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn mess_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<MessList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let items = list_of(
        &mut conn,
        MESS_SELECT,
        ctx.scope.tenant_id,
        "institution_id",
        id,
        None,
        "x.code",
    )
    .await?;
    Ok(Json(MessList { items }))
}

/// Define a mess (`campus.hostel.manage`).
#[utoipa::path(post, path = "/api/v1/campus/institutions/{id}/messes", operation_id = "campus_hostel_mess_create", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = MessCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = MessView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn mess_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<MessCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::HostelManage)?;
    require_idempotency_key(&headers)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let code = code("code", &b.code, 32)?;
    let name = required_text("name", &b.name, 200)?;
    if !matches!(b.menu_type.as_str(), "veg" | "non_veg" | "mixed") {
        return Err(invalid("menuType", "invalid", "veg, non_veg or mixed").into());
    }
    if !(1..=100_000).contains(&b.capacity) {
        return Err(invalid("capacity", "out_of_range", "at least 1").into());
    }
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mid: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.mess (tenant_id, institution_id, code, name, menu_type, capacity) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&code)
    .bind(&name)
    .bind(&b.menu_type)
    .bind(b.capacity)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    created(
        &mut tx,
        &scope,
        &MESS,
        mid,
        "active",
        "campus.mess.create",
        serde_json::json!({ "code": code }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v: MessView = fetch(&mut conn, MESS_SELECT, t, mid, MESS.entity).await?;
    Ok(reply(StatusCode::CREATED, &v, v.version))
}

/// Dining assignments of a mess (`campus.read`; `?status=`).
#[utoipa::path(get, path = "/api/v1/campus/messes/{id}/allocations", operation_id = "campus_hostel_mess_allocation_list", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), StatusFilter), security(("session_cookie" = [])),
    responses((status = 200, body = MessAllocationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn mess_allocation_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StatusFilter>,
) -> ApiResult<Json<MessAllocationList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let status = status_filter::<MessAllocationStatus>(q.status.as_deref())?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    head(&mut conn, &MESS, ctx.scope.tenant_id, id, false).await?;
    let items = list_of(
        &mut conn,
        MESS_ALLOCATION_SELECT,
        ctx.scope.tenant_id,
        "mess_id",
        id,
        status,
        "x.start_date DESC",
    )
    .await?;
    Ok(Json(MessAllocationList { items }))
}

/// Assign a resident to a mess (`campus.hostel.manage`; one active assignment per residency,
/// mess capacity checked).
#[utoipa::path(post, path = "/api/v1/campus/messes/{id}/allocations", operation_id = "campus_hostel_mess_allocation_create", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = MessAllocationCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = MessAllocationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn mess_allocation_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<MessAllocationCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::HostelManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let inst = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        head(&mut conn, &MESS, t, id, false).await?.institution
    };
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let start = match b.start_date {
        Some(d) => d,
        None => business_today(&st, &scope, inst).await?,
    };
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    // Residency lock first (one active assignment per residency).
    head(&mut tx, &ALLOCATION, t, b.hostel_allocation_id, true)
        .await
        .map_err(|_| {
            invalid(
                "hostelAllocationId",
                "unknown_allocation",
                "no residency with this id",
            )
        })?;
    let mid = assign(&mut tx, &scope, id, b.hostel_allocation_id, start, None, &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(mess_allocation_reply(&st, t, mid, StatusCode::CREATED).await?)
}

/// Move a resident to another mess (`campus.hostel.manage`): this assignment → changed (ends the
/// day before), a new active assignment starts in the target mess.
#[utoipa::path(post, path = "/api/v1/campus/mess-allocations/{id}/change", operation_id = "campus_hostel_mess_allocation_change", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the assignment")),
    request_body = MessAllocationChange, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MessAllocationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn mess_allocation_change(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<MessAllocationChange>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::HostelManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let reason = super::common::optional_text("reason", b.reason.clone(), 1000)?;
    let inst = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        head(&mut conn, &MESS_ALLOCATION, t, id, false)
            .await?
            .institution
    };
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let from = match b.effective_from {
        Some(d) => d,
        None => business_today(&st, &scope, inst).await?,
    };
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = sqlx::query(
        "SELECT hostel_allocation_id, mess_id, start_date FROM campus.mess_allocation WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let residency: Uuid = r.try_get("hostel_allocation_id").map_err(map_db_error)?;
    let current_mess: Uuid = r.try_get("mess_id").map_err(map_db_error)?;
    let start: NaiveDate = r.try_get("start_date").map_err(map_db_error)?;
    head(&mut tx, &ALLOCATION, t, residency, true).await?;
    let mut h = head(&mut tx, &MESS_ALLOCATION, t, id, true).await?;
    ensure_version(v, h.version)?;
    super::common::step(&h, MessAllocationStatus::Changed)?;
    if b.mess_id == current_mess {
        return Err(invalid(
            "messId",
            "same_mess",
            "the resident already eats in this mess",
        )
        .into());
    }
    if from <= start {
        return Err(invalid(
            "effectiveFrom",
            "too_early",
            "the change must start after the current assignment started",
        )
        .into());
    }
    sqlx::query("UPDATE campus.mess_allocation SET end_date = $1 WHERE id = $2")
        .bind(from.pred_opt().unwrap_or(from))
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    apply(
        &mut tx,
        &scope,
        &MESS_ALLOCATION,
        &mut h,
        Move {
            to: MessAllocationStatus::Changed.as_str(),
            event: None,
            action: "campus.mess_allocation.change",
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &a,
    )
    .await?;
    let new_id = assign(&mut tx, &scope, b.mess_id, residency, from, Some(id), &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(mess_allocation_reply(&st, t, new_id, StatusCode::OK).await?)
}
