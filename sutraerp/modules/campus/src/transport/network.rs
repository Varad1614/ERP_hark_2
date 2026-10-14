//! Routes, effective-dated versions with ordered stops and stops (L3-G-093 route creation →
//! activation (`ROUTE_ACTIVATION`), G-094 modification → new effective version (the old one is
//! superseded, never rewritten), G-095 suspension → passengers flagged, G-096 stop creation,
//! G-097 stop suspension → affected allocations flagged for adjustment).

use super::domain::{RouteStatus, RouteVersionStatus, StopStatus};
use crate::hostel::common::{
    apply, clock, code, created, ensure_version, expected, fetch, head, invalid, list_of,
    mark_pending, money, optional_text, reply, required_text, status_filter, step, Agg,
    DecisionBody, Head, Move, ReasonBody, StatusFilter,
};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::actor;
use crate::shared::rows::load_node;
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, CampusState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::campus::events::residence::{
    TRANSPORT_ROUTE_ACTIVATED, TRANSPORT_ROUTE_ACTIVATED_SCHEMA, TRANSPORT_ROUTE_SUSPENDED,
    TRANSPORT_ROUTE_SUSPENDED_SCHEMA,
};
use sutra_contracts::campus::ids::InfrastructureNodeId;
use sutra_kernel::auth::Permission;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use uuid::Uuid;

pub(crate) const ROUTE: Agg = Agg {
    table: "transport_route",
    entity: "transport route",
    number: "code",
    subject: "NULL",
};

pub(crate) const ROUTE_VERSION: Agg = Agg {
    table: "route_version",
    entity: "route version",
    number: "'v' || version_no",
    subject: "NULL",
};

pub(crate) const STOP: Agg = Agg {
    table: "transport_stop",
    entity: "transport stop",
    number: "code",
    subject: "NULL",
};

// ------------------------------------------------------------------ views

/// A transport route with its current version.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct TransportRouteView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "R-01")]
    pub code: String,
    pub name: String,
    /// `draft | active | suspended | retired`.
    pub status: String,
    pub status_reason: Option<String>,
    pub current_version_id: Option<Uuid>,
    pub current_version_no: Option<i32>,
    /// Pass fee of the current version.
    #[schema(example = "6000.00")]
    pub fare: Option<String>,
    /// Seats of the current version.
    pub capacity: Option<i32>,
    pub effective_from: Option<NaiveDate>,
    /// Active passenger allocations.
    pub passengers: i64,
    /// A draft version is waiting to be activated.
    pub has_draft: bool,
    /// Set while an activation awaits a decision.
    pub process_instance_id: Option<String>,
    pub version: i64,
}

pub(crate) const ROUTE_SELECT: &str = "SELECT x.id, x.institution_id, x.code, x.name, x.status, \
    x.status_reason, x.current_version_id, cv.version_no AS current_version_no, cv.fare::text AS fare, \
    cv.capacity, cv.effective_from, \
    (SELECT count(*) FROM campus.transport_allocation ta WHERE ta.route_id = x.id AND ta.status = 'active') AS passengers, \
    EXISTS (SELECT 1 FROM campus.route_version d WHERE d.route_id = x.id AND d.status = 'draft') AS has_draft, \
    x.process_instance_id, x.version FROM campus.transport_route x \
    LEFT JOIN campus.route_version cv ON cv.id = x.current_version_id";

/// Routes of an institution.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TransportRouteList {
    pub items: Vec<TransportRouteView>,
}

/// A new route (draft, with version 1 in draft).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransportRouteCreate {
    pub code: String,
    pub name: String,
    /// Seats offered.
    pub capacity: i32,
    /// Pass fee (`"0"` = free).
    #[schema(example = "6000.00")]
    pub fare: String,
    pub effective_from: NaiveDate,
    pub note: Option<String>,
}

/// One stop of a route version, in order.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct RouteStopView {
    pub stop_id: Uuid,
    pub code: String,
    pub name: String,
    pub sequence: i32,
    /// `HH:MM`.
    pub pickup_time: Option<String>,
    /// `active | suspended | retired`.
    pub stop_status: String,
}

/// An effective-dated version of a route with its ordered stops.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct RouteVersionView {
    pub id: Uuid,
    pub route_id: Uuid,
    pub version_no: i32,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub capacity: i32,
    #[schema(example = "6000.00")]
    pub fare: String,
    pub note: Option<String>,
    /// `draft | active | superseded`.
    pub status: String,
    pub process_instance_id: Option<String>,
    pub activated_at: Option<DateTime<Utc>>,
    pub version: i64,
    #[sqlx(skip)]
    pub stops: Vec<RouteStopView>,
}

const VERSION_SELECT: &str = "SELECT x.id, x.route_id, x.version_no, x.effective_from, x.effective_to, \
    x.capacity, x.fare::text AS fare, x.note, x.status, x.process_instance_id, x.activated_at, x.version \
    FROM campus.route_version x";

/// Versions of a route, newest first.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RouteVersionList {
    pub items: Vec<RouteVersionView>,
}

/// A new draft version (stops copied from the current version; edit them before activation).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RouteVersionCreate {
    /// After the current version's start.
    pub effective_from: NaiveDate,
    pub capacity: Option<i32>,
    #[schema(example = "6500.00")]
    pub fare: Option<String>,
    pub note: Option<String>,
}

/// One stop in a draft version.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RouteStopInput {
    pub stop_id: Uuid,
    pub sequence: i32,
    #[schema(example = "07:45")]
    pub pickup_time: Option<String>,
}

/// The ordered stops of a draft version (replaces the list).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RouteStopsBody {
    pub stops: Vec<RouteStopInput>,
}

/// A pickup / drop stop.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct TransportStopView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "ST-GATE")]
    pub code: String,
    pub name: String,
    pub landmark: Option<String>,
    pub node_id: Option<Uuid>,
    /// `active | suspended | retired`.
    pub status: String,
    pub status_reason: Option<String>,
    /// Active allocations boarding here.
    pub passengers: i64,
    pub version: i64,
}

const STOP_SELECT: &str = "SELECT x.id, x.institution_id, x.code, x.name, x.landmark, x.node_id, \
    x.status, x.status_reason, \
    (SELECT count(*) FROM campus.transport_allocation ta WHERE ta.stop_id = x.id AND ta.status = 'active') AS passengers, \
    x.version FROM campus.transport_stop x";

/// Stops of an institution.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TransportStopList {
    pub items: Vec<TransportStopView>,
}

/// A new stop.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransportStopCreate {
    pub code: String,
    pub name: String,
    pub landmark: Option<String>,
    /// Optional campus infrastructure node (gate, building) of the stop.
    pub node_id: Option<Uuid>,
}

// ------------------------------------------------------------------ helpers

pub(crate) async fn route_reply(
    st: &CampusState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v: TransportRouteView = fetch(&mut conn, ROUTE_SELECT, tenant, id, ROUTE.entity).await?;
    Ok(reply(status, &v, v.version))
}

async fn stops_of(
    conn: &mut PgConnection,
    version: Uuid,
) -> Result<Vec<RouteStopView>, KernelError> {
    sqlx::query_as::<_, RouteStopView>(
        "SELECT rs.stop_id, s.code, s.name, rs.sequence, rs.pickup_time, s.status AS stop_status \
         FROM campus.route_stop rs JOIN campus.transport_stop s ON s.id = rs.stop_id \
         WHERE rs.route_version_id = $1 ORDER BY rs.sequence",
    )
    .bind(version)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)
}

async fn version_view(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> Result<RouteVersionView, KernelError> {
    let mut v: RouteVersionView =
        fetch(conn, VERSION_SELECT, tenant, id, ROUTE_VERSION.entity).await?;
    v.stops = stops_of(conn, id).await?;
    Ok(v)
}

async fn stop_reply(
    st: &CampusState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v: TransportStopView = fetch(&mut conn, STOP_SELECT, tenant, id, STOP.entity).await?;
    Ok(reply(status, &v, v.version))
}

/// The draft version of a route, if any.
async fn draft_of(conn: &mut PgConnection, route: Uuid) -> Result<Option<Uuid>, KernelError> {
    sqlx::query_scalar(
        "SELECT id FROM campus.route_version WHERE route_id = $1 AND status = 'draft'",
    )
    .bind(route)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)
}

/// Checks a draft version can be activated: ≥ 2 stops, every stop active.
async fn check_activatable(conn: &mut PgConnection, version: Uuid) -> Result<(), KernelError> {
    let r = sqlx::query(
        "SELECT count(*) AS stops, count(*) FILTER (WHERE s.status <> 'active') AS inactive \
         FROM campus.route_stop rs JOIN campus.transport_stop s ON s.id = rs.stop_id \
         WHERE rs.route_version_id = $1",
    )
    .bind(version)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let stops: i64 = r.try_get("stops").map_err(map_db_error)?;
    let inactive: i64 = r.try_get("inactive").map_err(map_db_error)?;
    if stops < 2 {
        return Err(KernelError::Conflict(
            "a route version needs at least two stops before activation".into(),
        ));
    }
    if inactive > 0 {
        return Err(KernelError::Conflict(format!(
            "{inactive} stop(s) of the version are not active"
        )));
    }
    Ok(())
}

/// Applies an approved activation on the locked route `rh`: the draft version (if any) becomes
/// active and the previous active version superseded (effective_to = day before); allocations on
/// stops the new version dropped are flagged; the route is active (`transport_route.activated`);
/// passengers flagged by a route suspension are cleared.
async fn apply_activation(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    rh: &mut Head,
    pid: Option<&str>,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let t = scope.tenant_id;
    if let Some(draft) = draft_of(conn, rh.id).await? {
        let mut vh = head(conn, &ROUTE_VERSION, t, draft, true).await?;
        let from: NaiveDate =
            sqlx::query_scalar("SELECT effective_from FROM campus.route_version WHERE id = $1")
                .bind(draft)
                .fetch_one(&mut *conn)
                .await
                .map_err(map_db_error)?;
        let previous: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM campus.route_version WHERE route_id = $1 AND status = 'active' FOR UPDATE",
        )
        .bind(rh.id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
        if let Some(prev) = previous {
            let mut ph = head(conn, &ROUTE_VERSION, t, prev, true).await?;
            sqlx::query(
                "UPDATE campus.route_version SET effective_to = $1::date - 1 WHERE id = $2",
            )
            .bind(from)
            .bind(prev)
            .execute(&mut *conn)
            .await
            .map_err(map_db_error)?;
            apply(
                conn,
                scope,
                &ROUTE_VERSION,
                &mut ph,
                Move {
                    to: RouteVersionStatus::Superseded.as_str(),
                    event: None,
                    action: "campus.route_version.supersede",
                    reason: Some("superseded by a newer route version"),
                    process_instance_id: pid,
                },
                actor,
            )
            .await?;
        }
        sqlx::query(
            "UPDATE campus.route_version SET activated_at = now(), process_instance_id = $1 WHERE id = $2",
        )
        .bind(pid)
        .bind(draft)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
        apply(
            conn,
            scope,
            &ROUTE_VERSION,
            &mut vh,
            Move {
                to: RouteVersionStatus::Active.as_str(),
                event: None,
                action: "campus.route_version.activate",
                reason,
                process_instance_id: pid,
            },
            actor,
        )
        .await?;
        sqlx::query("UPDATE campus.transport_route SET current_version_id = $1 WHERE id = $2")
            .bind(draft)
            .bind(rh.id)
            .execute(&mut *conn)
            .await
            .map_err(map_db_error)?;
        sqlx::query(
            "UPDATE campus.transport_allocation SET flagged = true, \
             flag_reason = 'stop not on the active route version' \
             WHERE route_id = $1 AND status = 'active' AND stop_id NOT IN \
             (SELECT stop_id FROM campus.route_stop WHERE route_version_id = $2)",
        )
        .bind(rh.id)
        .bind(draft)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    sqlx::query(
        "UPDATE campus.transport_allocation SET flagged = false, flag_reason = NULL \
         WHERE route_id = $1 AND flag_reason = 'route suspended'",
    )
    .bind(rh.id)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    sqlx::query("UPDATE campus.transport_route SET process_instance_id = NULL WHERE id = $1")
        .bind(rh.id)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    apply(
        conn,
        scope,
        &ROUTE,
        rh,
        Move {
            to: RouteStatus::Active.as_str(),
            event: Some((TRANSPORT_ROUTE_ACTIVATED, TRANSPORT_ROUTE_ACTIVATED_SCHEMA)),
            action: "campus.transport_route.activate",
            reason,
            process_instance_id: pid,
        },
        actor,
    )
    .await?;
    Ok(())
}

/// Inserts a version and copies the stops of `copy_from` (if any).
#[allow(clippy::too_many_arguments)]
async fn insert_version(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    route: Uuid,
    effective_from: NaiveDate,
    capacity: i32,
    fare: rust_decimal::Decimal,
    note: Option<String>,
    copy_from: Option<Uuid>,
    actor: &Actor,
) -> Result<Uuid, KernelError> {
    let t = scope.tenant_id;
    let inst = crate::hostel::common::scope_institution(scope)?;
    let no: i32 = sqlx::query_scalar(
        "SELECT COALESCE(max(version_no), 0) + 1 FROM campus.route_version WHERE route_id = $1",
    )
    .bind(route)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.route_version (tenant_id, institution_id, route_id, version_no, \
         effective_from, capacity, fare, note) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(route)
    .bind(no)
    .bind(effective_from)
    .bind(capacity)
    .bind(fare)
    .bind(note)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if let Some(src) = copy_from {
        sqlx::query(
            "INSERT INTO campus.route_stop (tenant_id, route_version_id, stop_id, sequence, pickup_time) \
             SELECT tenant_id, $1, stop_id, sequence, pickup_time FROM campus.route_stop \
             WHERE route_version_id = $2",
        )
        .bind(id)
        .bind(src)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    created(
        conn,
        scope,
        &ROUTE_VERSION,
        id,
        RouteVersionStatus::Draft.as_str(),
        "campus.route_version.create",
        serde_json::json!({ "route": route, "versionNo": no, "effectiveFrom": effective_from }),
        actor,
    )
    .await?;
    Ok(id)
}

fn seats(field: &str, capacity: i32) -> Result<i32, KernelError> {
    if (1..=500).contains(&capacity) {
        Ok(capacity)
    } else {
        Err(invalid(field, "out_of_range", "between 1 and 500 seats"))
    }
}

// ------------------------------------------------------------------ handlers: routes

/// Routes of an institution (`campus.read`; `?status=`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/transport-routes", operation_id = "campus_transport_route_list", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), StatusFilter), security(("session_cookie" = [])),
    responses((status = 200, body = TransportRouteList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn route_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StatusFilter>,
) -> ApiResult<Json<TransportRouteList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let status = status_filter::<RouteStatus>(q.status.as_deref())?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let items = list_of(
        &mut conn,
        ROUTE_SELECT,
        ctx.scope.tenant_id,
        "institution_id",
        id,
        status,
        "x.code",
    )
    .await?;
    Ok(Json(TransportRouteList { items }))
}

/// Create a route (`campus.transport.manage`): draft route + draft version 1 (add stops, then
/// activate).
#[utoipa::path(post, path = "/api/v1/campus/institutions/{id}/transport-routes", operation_id = "campus_transport_route_create", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = TransportRouteCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = TransportRouteView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn route_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<TransportRouteCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    require_idempotency_key(&headers)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let code = code("code", &b.code, 32)?;
    let name = required_text("name", &b.name, 200)?;
    let capacity = seats("capacity", b.capacity)?;
    let fare = money("fare", &b.fare)?;
    let note = optional_text("note", b.note.clone(), 1000)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let rid: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.transport_route (tenant_id, institution_id, code, name) \
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&code)
    .bind(&name)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    created(
        &mut tx,
        &scope,
        &ROUTE,
        rid,
        RouteStatus::Draft.as_str(),
        "campus.transport_route.create",
        serde_json::json!({ "code": code }),
        &a,
    )
    .await?;
    insert_version(
        &mut tx,
        &scope,
        rid,
        b.effective_from,
        capacity,
        fare,
        note,
        None,
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(route_reply(&st, t, rid, StatusCode::CREATED).await?)
}

/// One route (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/transport-routes/{id}", operation_id = "campus_transport_route_get", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = TransportRouteView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn route_get(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusRead)?;
    Ok(route_reply(&st, ctx.scope.tenant_id, id, StatusCode::OK).await?)
}

/// Versions of a route with their stops, newest first (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/transport-routes/{id}/versions", operation_id = "campus_transport_version_list", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = RouteVersionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn version_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<RouteVersionList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    head(&mut conn, &ROUTE, ctx.scope.tenant_id, id, false).await?;
    let mut items: Vec<RouteVersionView> = list_of(
        &mut conn,
        VERSION_SELECT,
        ctx.scope.tenant_id,
        "route_id",
        id,
        None,
        "x.version_no DESC",
    )
    .await?;
    for v in &mut items {
        v.stops = stops_of(&mut conn, v.id).await?;
    }
    Ok(Json(RouteVersionList { items }))
}

/// Draft a new route version (`campus.transport.manage`; G-094): stops copied from the current
/// version; one draft at a time; effective after the current version's start.
#[utoipa::path(post, path = "/api/v1/campus/transport-routes/{id}/versions", operation_id = "campus_transport_version_create", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = RouteVersionCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = RouteVersionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn version_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<RouteVersionCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let note = optional_text("note", b.note.clone(), 1000)?;
    let a = actor(ctx);
    let inst = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        head(&mut conn, &ROUTE, t, id, false).await?.institution
    };
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let rh = head(&mut tx, &ROUTE, t, id, true).await?;
    if rh.status == RouteStatus::Retired.as_str() {
        return Err(KernelError::Conflict("a retired route takes no new version".into()).into());
    }
    if draft_of(&mut tx, id).await?.is_some() {
        return Err(KernelError::Conflict(
            "the route already has a draft version; edit or activate it first".into(),
        )
        .into());
    }
    let current = sqlx::query(
        "SELECT v.id, v.effective_from, v.capacity, v.fare FROM campus.route_version v \
         JOIN campus.transport_route r ON r.current_version_id = v.id WHERE r.id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let (copy_from, capacity, fare) = match current {
        Some(c) => {
            let from: NaiveDate = c.try_get("effective_from").map_err(map_db_error)?;
            if b.effective_from <= from {
                return Err(invalid(
                    "effectiveFrom",
                    "not_after_current",
                    "a new version starts after the current version's start",
                )
                .into());
            }
            let cap: i32 = c.try_get("capacity").map_err(map_db_error)?;
            let fare: rust_decimal::Decimal = c.try_get("fare").map_err(map_db_error)?;
            (
                Some(c.try_get::<Uuid, _>("id").map_err(map_db_error)?),
                cap,
                fare,
            )
        }
        None => (None, 40, rust_decimal::Decimal::ZERO),
    };
    let capacity = match b.capacity {
        Some(c) => seats("capacity", c)?,
        None => capacity,
    };
    let fare = match b.fare.as_deref() {
        Some(f) => money("fare", f)?,
        None => fare,
    };
    let vid = insert_version(
        &mut tx,
        &scope,
        id,
        b.effective_from,
        capacity,
        fare,
        note,
        copy_from,
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v = version_view(&mut conn, t, vid).await?;
    Ok(reply(StatusCode::CREATED, &v, v.version))
}

/// Replace the ordered stops of a draft version (`campus.transport.manage`; a version that was
/// ever active is never rewritten → 409).
#[utoipa::path(post, path = "/api/v1/campus/route-versions/{id}/stops", operation_id = "campus_transport_version_stops", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the route version")),
    request_body = RouteStopsBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RouteVersionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn version_stops(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<RouteStopsBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    if b.stops.len() > 100 {
        return Err(invalid("stops", "too_many", "at most 100 stops").into());
    }
    let mut seqs = std::collections::HashSet::new();
    let mut ids = std::collections::HashSet::new();
    let mut times = Vec::with_capacity(b.stops.len());
    for (i, s) in b.stops.iter().enumerate() {
        if s.sequence < 1 || !seqs.insert(s.sequence) {
            return Err(invalid(
                &format!("stops[{i}].sequence"),
                "invalid",
                "unique positive sequence",
            )
            .into());
        }
        if !ids.insert(s.stop_id) {
            return Err(invalid(
                &format!("stops[{i}].stopId"),
                "duplicate",
                "a stop appears once per version",
            )
            .into());
        }
        times.push(clock(
            &format!("stops[{i}].pickupTime"),
            s.pickup_time.clone(),
        )?);
    }
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let vh = head(&mut tx, &ROUTE_VERSION, t, id, true).await?;
    ensure_version(v, vh.version)?;
    if vh.status != RouteVersionStatus::Draft.as_str() {
        return Err(KernelError::Conflict(format!(
            "route version {} is {}; only a draft version's stops can change",
            vh.number.clone().unwrap_or_default(),
            vh.status
        ))
        .into());
    }
    let unknown: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM unnest($1::uuid[]) AS s(id) WHERE NOT EXISTS \
         (SELECT 1 FROM campus.transport_stop t WHERE t.id = s.id AND t.tenant_id = $2 \
         AND t.institution_id = $3 AND t.status <> 'retired')",
    )
    .bind(ids.iter().copied().collect::<Vec<Uuid>>())
    .bind(t.as_uuid())
    .bind(vh.institution.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if unknown > 0 {
        return Err(invalid(
            "stops",
            "unknown_stop",
            "every stop must be a non-retired stop of the institution",
        )
        .into());
    }
    sqlx::query("DELETE FROM campus.route_stop WHERE route_version_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    for (s, time) in b.stops.iter().zip(times) {
        sqlx::query(
            "INSERT INTO campus.route_stop (tenant_id, route_version_id, stop_id, sequence, pickup_time) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(t.as_uuid())
        .bind(id)
        .bind(s.stop_id)
        .bind(s.sequence)
        .bind(time)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    let scope = institution_scope(ctx, vh.institution);
    sqlx::query(
        "UPDATE campus.route_version SET version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    crate::shared::publish::audit(
        &mut tx,
        &scope,
        "campus.route_version.stops",
        ROUTE_VERSION.table,
        id.to_string(),
        serde_json::json!({ "stops": b.stops.len() }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let out = version_view(&mut conn, t, id).await?;
    Ok(reply(StatusCode::OK, &out, out.version))
}

/// Activate a route (`campus.transport.manage`; `ROUTE_ACTIVATION`, auto-approved in Batch 8):
/// the draft version (≥ 2 active stops) becomes the effective one; a suspended route without a
/// draft is restored on its current version.
#[utoipa::path(post, path = "/api/v1/campus/transport-routes/{id}/activate", operation_id = "campus_transport_route_activate", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the route")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TransportRouteView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn route_activate(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let rh = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let rh = head(&mut conn, &ROUTE, t, id, false).await?;
        ensure_version(v, rh.version)?;
        let pending: Option<String> = sqlx::query_scalar(
            "SELECT process_instance_id FROM campus.transport_route WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
        if pending.is_some() {
            return Err(
                KernelError::Conflict("an activation already awaits a decision".into()).into(),
            );
        }
        match draft_of(&mut conn, id).await? {
            Some(draft) => {
                if rh.status == RouteStatus::Retired.as_str() {
                    return Err(KernelError::Conflict("the route is retired".into()).into());
                }
                check_activatable(&mut conn, draft).await?;
            }
            None => {
                step(&rh, RouteStatus::Active)?;
                let current: Option<Uuid> = sqlx::query_scalar(
                    "SELECT current_version_id FROM campus.transport_route WHERE id = $1",
                )
                .bind(id)
                .fetch_one(&mut *conn)
                .await
                .map_err(map_db_error)?;
                if current.is_none() {
                    return Err(KernelError::Conflict(
                        "the route has no version to activate".into(),
                    )
                    .into());
                }
            }
        }
        rh
    };
    let scope = institution_scope(ctx, rh.institution);
    require_institution_writable(&st, &scope, rh.institution).await?;
    let outcome = request_approval(
        &st,
        &scope,
        process::ROUTE_ACTIVATION,
        business_key("transport_route", id, rh.version),
        BTreeMap::new(),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut rh = head(&mut tx, &ROUTE, t, id, true).await?;
    ensure_version(v, rh.version)?;
    mark_pending(
        &mut tx,
        &scope,
        &ROUTE,
        &mut rh,
        Some(&pid),
        "campus.transport_route.submit_activation",
        &a,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let sys = Actor::system(a.correlation_id.clone());
        apply_activation(&mut tx, &scope, &mut rh, Some(&pid), None, &sys).await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(route_reply(&st, t, id, StatusCode::OK).await?)
}

/// Decide a pending route activation (`campus.approve`, step-up).
#[utoipa::path(post, path = "/api/v1/campus/transport-routes/{id}/decision", operation_id = "campus_transport_route_decide", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the route")),
    request_body = DecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TransportRouteView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn route_decide(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<DecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusApprove)?;
    let v = expected!(headers);
    let reason = b.checked_reason()?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut rh = head(&mut tx, &ROUTE, t, id, true).await?;
    ensure_version(v, rh.version)?;
    let pid: Option<String> =
        sqlx::query_scalar("SELECT process_instance_id FROM campus.transport_route WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
    let Some(pid) = pid else {
        return Err(KernelError::Conflict("no route activation awaits a decision".into()).into());
    };
    let scope = institution_scope(ctx, rh.institution);
    if b.approve() {
        if let Some(draft) = draft_of(&mut tx, id).await? {
            check_activatable(&mut tx, draft).await?;
        }
        apply_activation(&mut tx, &scope, &mut rh, Some(&pid), reason.as_deref(), &a).await?;
    } else {
        sqlx::query("UPDATE campus.transport_route SET status_reason = $1 WHERE id = $2")
            .bind(reason.as_deref())
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        mark_pending(
            &mut tx,
            &scope,
            &ROUTE,
            &mut rh,
            None,
            "campus.transport_route.reject_activation",
            &a,
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(route_reply(&st, t, id, StatusCode::OK).await?)
}

/// Suspend a route (`campus.transport.manage`; G-095): active passengers are flagged.
#[utoipa::path(post, path = "/api/v1/campus/transport-routes/{id}/suspend", operation_id = "campus_transport_route_suspend", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the route")),
    request_body = ReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TransportRouteView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn route_suspend(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    let v = expected!(headers);
    let reason = b.text()?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut rh = head(&mut tx, &ROUTE, t, id, true).await?;
    ensure_version(v, rh.version)?;
    step(&rh, RouteStatus::Suspended)?;
    let scope = institution_scope(ctx, rh.institution);
    sqlx::query(
        "UPDATE campus.transport_allocation SET flagged = true, flag_reason = 'route suspended' \
         WHERE route_id = $1 AND status = 'active'",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    apply(
        &mut tx,
        &scope,
        &ROUTE,
        &mut rh,
        Move {
            to: RouteStatus::Suspended.as_str(),
            event: Some((TRANSPORT_ROUTE_SUSPENDED, TRANSPORT_ROUTE_SUSPENDED_SCHEMA)),
            action: "campus.transport_route.suspend",
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(route_reply(&st, t, id, StatusCode::OK).await?)
}

/// Retire a route (`campus.transport.manage`): only without active passengers.
#[utoipa::path(post, path = "/api/v1/campus/transport-routes/{id}/retire", operation_id = "campus_transport_route_retire", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the route")),
    request_body = ReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TransportRouteView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn route_retire(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    let v = expected!(headers);
    let reason = b.text()?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut rh = head(&mut tx, &ROUTE, t, id, true).await?;
    ensure_version(v, rh.version)?;
    step(&rh, RouteStatus::Retired)?;
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM campus.transport_allocation WHERE route_id = $1 AND status IN ('requested', 'active')",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if live > 0 {
        return Err(KernelError::Conflict(format!(
            "{live} passenger allocation(s) still use the route; move or withdraw them first"
        ))
        .into());
    }
    let scope = institution_scope(ctx, rh.institution);
    apply(
        &mut tx,
        &scope,
        &ROUTE,
        &mut rh,
        Move {
            to: RouteStatus::Retired.as_str(),
            event: None,
            action: "campus.transport_route.retire",
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(route_reply(&st, t, id, StatusCode::OK).await?)
}

// ------------------------------------------------------------------ handlers: stops

/// Stops of an institution (`campus.read`; `?status=`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/transport-stops", operation_id = "campus_transport_stop_list", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), StatusFilter), security(("session_cookie" = [])),
    responses((status = 200, body = TransportStopList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn stop_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StatusFilter>,
) -> ApiResult<Json<TransportStopList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let status = status_filter::<StopStatus>(q.status.as_deref())?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let items = list_of(
        &mut conn,
        STOP_SELECT,
        ctx.scope.tenant_id,
        "institution_id",
        id,
        status,
        "x.code",
    )
    .await?;
    Ok(Json(TransportStopList { items }))
}

/// Create a stop (`campus.transport.manage`; G-096).
#[utoipa::path(post, path = "/api/v1/campus/institutions/{id}/transport-stops", operation_id = "campus_transport_stop_create", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = TransportStopCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = TransportStopView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn stop_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<TransportStopCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    require_idempotency_key(&headers)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let code = code("code", &b.code, 32)?;
    let name = required_text("name", &b.name, 200)?;
    let landmark = optional_text("landmark", b.landmark.clone(), 300)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    if let Some(node) = b.node_id {
        let n = load_node(&mut tx, t, InfrastructureNodeId(node)).await?;
        if !n.is_some_and(|n| n.institution_id == inst) {
            return Err(invalid(
                "nodeId",
                "unknown_node",
                "no infrastructure node with this id in the institution",
            )
            .into());
        }
    }
    let sid: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.transport_stop (tenant_id, institution_id, code, name, landmark, node_id) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&code)
    .bind(&name)
    .bind(landmark)
    .bind(b.node_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    created(
        &mut tx,
        &scope,
        &STOP,
        sid,
        StopStatus::Active.as_str(),
        "campus.transport_stop.create",
        serde_json::json!({ "code": code }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(stop_reply(&st, t, sid, StatusCode::CREATED).await?)
}

/// Suspend a stop (`campus.transport.manage`; G-097): allocations boarding here are flagged for
/// adjustment.
#[utoipa::path(post, path = "/api/v1/campus/transport-stops/{id}/suspend", operation_id = "campus_transport_stop_suspend", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the stop")),
    request_body = ReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TransportStopView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn stop_suspend(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReasonBody>,
) -> ApiResult<Response> {
    stop_move(st, auth, id, headers, b, StopStatus::Suspended).await
}

/// Restore a suspended stop (`campus.transport.manage`): its flags are cleared.
#[utoipa::path(post, path = "/api/v1/campus/transport-stops/{id}/restore", operation_id = "campus_transport_stop_restore", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the stop")),
    request_body = ReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TransportStopView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn stop_restore(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReasonBody>,
) -> ApiResult<Response> {
    stop_move(st, auth, id, headers, b, StopStatus::Active).await
}

async fn stop_move(
    st: CampusState,
    auth: Authenticated,
    id: Uuid,
    headers: HeaderMap,
    b: ReasonBody,
    to: StopStatus,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    let v = expected!(headers);
    let reason = b.text()?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut h = head(&mut tx, &STOP, t, id, true).await?;
    ensure_version(v, h.version)?;
    step(&h, to)?;
    let scope = institution_scope(ctx, h.institution);
    if to == StopStatus::Suspended {
        sqlx::query(
            "UPDATE campus.transport_allocation SET flagged = true, flag_reason = 'stop suspended' \
             WHERE stop_id = $1 AND status = 'active'",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    } else {
        sqlx::query(
            "UPDATE campus.transport_allocation SET flagged = false, flag_reason = NULL \
             WHERE stop_id = $1 AND flag_reason = 'stop suspended'",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    apply(
        &mut tx,
        &scope,
        &STOP,
        &mut h,
        Move {
            to: to.as_str(),
            event: None,
            action: if to == StopStatus::Suspended {
                "campus.transport_stop.suspend"
            } else {
                "campus.transport_stop.restore"
            },
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(stop_reply(&st, t, id, StatusCode::OK).await?)
}
