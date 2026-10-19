//! Trips (L3-G-104 planning → ready (vehicle + driver + complete checklist), G-106 ready →
//! running → completed, G-105 scheduled trip cancellation → closure, G-108 vehicle assignment,
//! G-109 replacement vehicle after a breakdown, G-102 driver readiness).

use super::domain::{checklist_complete, seats_available, TripStatus, VehicleStatus};
use super::fleet::VEHICLE;
use super::network::ROUTE;
use crate::hostel::common::{
    apply, created, ensure_version, expected, fetch, head, invalid, list_of, required_text,
    status_filter, step, Agg, Move, StatusFilter,
};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::actor;
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, CampusState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Row};
use sutra_contracts::campus::events::residence::{
    TRIP_CANCELLED, TRIP_CANCELLED_SCHEMA, TRIP_COMPLETED, TRIP_COMPLETED_SCHEMA,
};
use sutra_kernel::auth::Permission;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use uuid::Uuid;

pub(crate) const TRIP: Agg = Agg {
    table: "trip",
    entity: "trip",
    number: "(SELECT r.code FROM campus.transport_route r WHERE r.id = route_id) || ' ' || trip_date::text || ' ' || direction",
    subject: "NULL",
};

/// A trip of a route on a date.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct TripView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub route_id: Uuid,
    pub route_code: String,
    pub route_version_id: Uuid,
    pub trip_date: NaiveDate,
    /// `pickup | drop`.
    pub direction: String,
    /// `planned | ready | in_progress | completed | cancelled`.
    pub status: String,
    pub status_reason: Option<String>,
    pub vehicle_id: Option<Uuid>,
    pub vehicle_registration: Option<String>,
    pub driver_assignment_id: Option<Uuid>,
    pub driver_name: Option<String>,
    /// Readiness checklist (`{"brakes": true, …}`).
    #[schema(value_type = Object)]
    pub checklist: serde_json::Value,
    /// The vehicle broke down: replace it before the trip can run.
    pub needs_vehicle: bool,
    pub passenger_count: Option<i32>,
    /// Active passengers allocated to the route.
    pub allocated_passengers: i64,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub version: i64,
}

const TRIP_SELECT: &str = "SELECT x.id, x.institution_id, x.route_id, r.code AS route_code, \
    x.route_version_id, x.trip_date, x.direction, x.status, x.status_reason, x.vehicle_id, \
    v.registration_no AS vehicle_registration, x.driver_assignment_id, d.driver_name, x.checklist, \
    x.needs_vehicle, x.passenger_count, \
    (SELECT count(*) FROM campus.transport_allocation ta WHERE ta.route_id = x.route_id AND ta.status = 'active') AS allocated_passengers, \
    x.started_at, x.completed_at, x.version FROM campus.trip x \
    JOIN campus.transport_route r ON r.id = x.route_id \
    LEFT JOIN campus.vehicle v ON v.id = x.vehicle_id \
    LEFT JOIN campus.driver_assignment d ON d.id = x.driver_assignment_id";

/// Trips.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TripList {
    pub items: Vec<TripView>,
}

/// Plan a trip.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TripCreate {
    pub trip_date: NaiveDate,
    /// `pickup | drop`.
    pub direction: String,
    pub vehicle_id: Option<Uuid>,
    pub driver_assignment_id: Option<Uuid>,
}

/// Make a trip ready: vehicle, driver and a complete checklist.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TripReadyBody {
    /// Default: the planned vehicle.
    pub vehicle_id: Option<Uuid>,
    /// Default: the planned driver.
    pub driver_assignment_id: Option<Uuid>,
    /// Every item must be `true`.
    #[schema(value_type = Object)]
    pub checklist: serde_json::Value,
}

/// Complete a running trip.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TripCompleteBody {
    pub passenger_count: Option<i32>,
}

/// Cancel a trip (reason required).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TripCancelBody {
    pub reason: String,
}

/// Put another vehicle on a trip.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TripReplaceVehicleBody {
    pub vehicle_id: Uuid,
    pub reason: Option<String>,
}

async fn trip_reply(
    st: &CampusState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v: TripView = fetch(&mut conn, TRIP_SELECT, tenant, id, TRIP.entity).await?;
    Ok(reply_trip(status, &v))
}

fn reply_trip(status: StatusCode, v: &TripView) -> Response {
    crate::hostel::common::reply(status, v, v.version)
}

/// A vehicle that can serve a trip of `institution` with `passengers` (locked).
async fn usable_vehicle(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    vehicle: Uuid,
    passengers: i64,
) -> Result<(), KernelError> {
    let r = sqlx::query(
        "SELECT institution_id, status, capacity, registration_no FROM campus.vehicle \
         WHERE id = $1 AND tenant_id = $2 FOR UPDATE",
    )
    .bind(vehicle)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| invalid("vehicleId", "unknown_vehicle", "no vehicle with this id"))?;
    let inst: Uuid = r.try_get("institution_id").map_err(map_db_error)?;
    let status: String = r.try_get("status").map_err(map_db_error)?;
    let capacity: i32 = r.try_get("capacity").map_err(map_db_error)?;
    let reg: String = r.try_get("registration_no").map_err(map_db_error)?;
    if inst != institution.as_uuid() {
        return Err(invalid(
            "vehicleId",
            "unknown_vehicle",
            "no vehicle with this id in the institution",
        ));
    }
    if !matches!(status.as_str(), "available" | "assigned") {
        return Err(KernelError::Conflict(format!("vehicle {reg} is {status}")));
    }
    if !seats_available(capacity, 0, passengers) {
        return Err(KernelError::Conflict(format!(
            "vehicle {reg} seats {capacity}; the route has {passengers} passengers"
        )));
    }
    Ok(())
}

/// available → assigned (a vehicle already assigned stays so).
async fn assign_vehicle(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    vehicle: Uuid,
    actor: &Actor,
) -> Result<(), KernelError> {
    let mut h = head(conn, &VEHICLE, scope.tenant_id, vehicle, true).await?;
    if h.status == VehicleStatus::Available.as_str() {
        apply(
            conn,
            scope,
            &VEHICLE,
            &mut h,
            Move {
                to: VehicleStatus::Assigned.as_str(),
                event: None,
                action: "campus.vehicle.assign",
                reason: Some("trip ready"),
                process_instance_id: None,
            },
            actor,
        )
        .await?;
    }
    Ok(())
}

/// assigned → available when no other ready / running trip uses the vehicle.
async fn release_vehicle(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    vehicle: Uuid,
    except_trip: Uuid,
    actor: &Actor,
) -> Result<(), KernelError> {
    let mut h = head(conn, &VEHICLE, scope.tenant_id, vehicle, true).await?;
    if h.status != VehicleStatus::Assigned.as_str() {
        return Ok(());
    }
    let busy: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM campus.trip WHERE vehicle_id = $1 AND id <> $2 AND status IN ('ready', 'in_progress')",
    )
    .bind(vehicle)
    .bind(except_trip)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if busy == 0 {
        apply(
            conn,
            scope,
            &VEHICLE,
            &mut h,
            Move {
                to: VehicleStatus::Available.as_str(),
                event: None,
                action: "campus.vehicle.release",
                reason: Some("trip finished"),
                process_instance_id: None,
            },
            actor,
        )
        .await?;
    }
    Ok(())
}

async fn route_passengers(conn: &mut PgConnection, route: Uuid) -> Result<i64, KernelError> {
    sqlx::query_scalar(
        "SELECT count(*) FROM campus.transport_allocation WHERE route_id = $1 AND status = 'active'",
    )
    .bind(route)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)
}

struct TripFacts {
    route_id: Uuid,
    trip_date: NaiveDate,
    vehicle_id: Option<Uuid>,
    driver_assignment_id: Option<Uuid>,
    needs_vehicle: bool,
}

async fn trip_facts(conn: &mut PgConnection, id: Uuid) -> Result<TripFacts, KernelError> {
    let r = sqlx::query(
        "SELECT route_id, trip_date, vehicle_id, driver_assignment_id, needs_vehicle FROM campus.trip WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(TripFacts {
        route_id: r.try_get("route_id").map_err(map_db_error)?,
        trip_date: r.try_get("trip_date").map_err(map_db_error)?,
        vehicle_id: r.try_get("vehicle_id").map_err(map_db_error)?,
        driver_assignment_id: r.try_get("driver_assignment_id").map_err(map_db_error)?,
        needs_vehicle: r.try_get("needs_vehicle").map_err(map_db_error)?,
    })
}

// ------------------------------------------------------------------ handlers

/// Trips of a route (`campus.read`; `?status=`).
#[utoipa::path(get, path = "/api/v1/campus/transport-routes/{id}/trips", operation_id = "campus_transport_trip_list", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), StatusFilter), security(("session_cookie" = [])),
    responses((status = 200, body = TripList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn trip_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StatusFilter>,
) -> ApiResult<Json<TripList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let status = status_filter::<TripStatus>(q.status.as_deref())?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    head(&mut conn, &ROUTE, ctx.scope.tenant_id, id, false).await?;
    let items = list_of(
        &mut conn,
        TRIP_SELECT,
        ctx.scope.tenant_id,
        "route_id",
        id,
        status,
        "x.trip_date DESC, x.direction",
    )
    .await?;
    Ok(Json(TripList { items }))
}

/// Trips of an institution (`campus.read`; `?status=`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/trips", operation_id = "campus_transport_trip_board", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), StatusFilter), security(("session_cookie" = [])),
    responses((status = 200, body = TripList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn trip_board(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StatusFilter>,
) -> ApiResult<Json<TripList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let status = status_filter::<TripStatus>(q.status.as_deref())?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let items = list_of(
        &mut conn,
        TRIP_SELECT,
        ctx.scope.tenant_id,
        "institution_id",
        id,
        status,
        "x.trip_date DESC, r.code, x.direction",
    )
    .await?;
    Ok(Json(TripList { items }))
}

/// Plan a trip of an active route (`campus.transport.manage`; once per route, date and direction).
#[utoipa::path(post, path = "/api/v1/campus/transport-routes/{id}/trips", operation_id = "campus_transport_trip_plan", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = TripCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = TripView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn trip_plan(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<TripCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    require_idempotency_key(&headers)?;
    if !matches!(b.direction.as_str(), "pickup" | "drop") {
        return Err(invalid("direction", "invalid", "pickup or drop").into());
    }
    let t = ctx.scope.tenant_id;
    let inst = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        head(&mut conn, &ROUTE, t, id, false).await?.institution
    };
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let rh = head(&mut tx, &ROUTE, t, id, true).await?;
    if rh.status != "active" {
        return Err(KernelError::Conflict(format!(
            "route {} is {}; trips run on active routes",
            rh.number.clone().unwrap_or_default(),
            rh.status
        ))
        .into());
    }
    let version: Uuid =
        sqlx::query_scalar("SELECT current_version_id FROM campus.transport_route WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
    if let Some(vehicle) = b.vehicle_id {
        let passengers = route_passengers(&mut tx, id).await?;
        usable_vehicle(&mut tx, t, inst, vehicle, passengers).await?;
    }
    if let Some(driver) = b.driver_assignment_id {
        let ok: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM campus.driver_assignment WHERE id = $1 AND route_id = $2 AND status = 'active')",
        )
        .bind(driver)
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if !ok {
            return Err(invalid(
                "driverAssignmentId",
                "not_on_route",
                "no active driver assignment of this route",
            )
            .into());
        }
    }
    let trip: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.trip (tenant_id, institution_id, route_id, route_version_id, trip_date, \
         direction, vehicle_id, driver_assignment_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(id)
    .bind(version)
    .bind(b.trip_date)
    .bind(&b.direction)
    .bind(b.vehicle_id)
    .bind(b.driver_assignment_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    created(
        &mut tx,
        &scope,
        &TRIP,
        trip,
        TripStatus::Planned.as_str(),
        "campus.trip.plan",
        serde_json::json!({ "route": rh.number, "date": b.trip_date, "direction": b.direction }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(trip_reply(&st, t, trip, StatusCode::CREATED).await?)
}

/// Make a trip ready (`campus.transport.manage`; G-104): a usable vehicle seating the route's
/// passengers, an active driver of the route valid on the trip date and a complete checklist;
/// the vehicle becomes `assigned`.
#[utoipa::path(post, path = "/api/v1/campus/trips/{id}/ready", operation_id = "campus_transport_trip_ready", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the trip")),
    request_body = TripReadyBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TripView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn trip_ready(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<TripReadyBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    let v = expected!(headers);
    if !checklist_complete(&b.checklist) {
        return Err(invalid(
            "checklist",
            "incomplete",
            "every checklist item must be true (at least one item)",
        )
        .into());
    }
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut h = head(&mut tx, &TRIP, t, id, true).await?;
    ensure_version(v, h.version)?;
    step(&h, TripStatus::Ready)?;
    let f = trip_facts(&mut tx, id).await?;
    let vehicle = b
        .vehicle_id
        .or(f.vehicle_id)
        .ok_or_else(|| invalid("vehicleId", "required", "a ready trip needs a vehicle"))?;
    let driver = b
        .driver_assignment_id
        .or(f.driver_assignment_id)
        .ok_or_else(|| {
            invalid(
                "driverAssignmentId",
                "required",
                "a ready trip needs a driver",
            )
        })?;
    if f.needs_vehicle && b.vehicle_id.is_none() {
        return Err(KernelError::Conflict(
            "the planned vehicle broke down; choose a replacement".into(),
        )
        .into());
    }
    let passengers = route_passengers(&mut tx, f.route_id).await?;
    usable_vehicle(&mut tx, t, h.institution, vehicle, passengers).await?;
    let driver_ok: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM campus.driver_assignment WHERE id = $1 AND route_id = $2 \
         AND status = 'active' AND valid_from <= $3 AND (valid_to IS NULL OR valid_to >= $3))",
    )
    .bind(driver)
    .bind(f.route_id)
    .bind(f.trip_date)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if !driver_ok {
        return Err(invalid(
            "driverAssignmentId",
            "not_valid",
            "no active driver assignment of this route valid on the trip date",
        )
        .into());
    }
    let scope = institution_scope(ctx, h.institution);
    sqlx::query(
        "UPDATE campus.trip SET vehicle_id = $1, driver_assignment_id = $2, checklist = $3, \
         needs_vehicle = false WHERE id = $4",
    )
    .bind(vehicle)
    .bind(driver)
    .bind(&b.checklist)
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    assign_vehicle(&mut tx, &scope, vehicle, &a).await?;
    apply(
        &mut tx,
        &scope,
        &TRIP,
        &mut h,
        Move {
            to: TripStatus::Ready.as_str(),
            event: None,
            action: "campus.trip.ready",
            reason: None,
            process_instance_id: None,
        },
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(trip_reply(&st, t, id, StatusCode::OK).await?)
}

/// Start a ready trip (`campus.transport.manage`; not while it needs a replacement vehicle).
#[utoipa::path(post, path = "/api/v1/campus/trips/{id}/start", operation_id = "campus_transport_trip_start", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the trip")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TripView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn trip_start(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut h = head(&mut tx, &TRIP, t, id, true).await?;
    ensure_version(v, h.version)?;
    step(&h, TripStatus::InProgress)?;
    let f = trip_facts(&mut tx, id).await?;
    if f.needs_vehicle {
        return Err(KernelError::Conflict(
            "the vehicle broke down; replace it before starting".into(),
        )
        .into());
    }
    let scope = institution_scope(ctx, h.institution);
    sqlx::query("UPDATE campus.trip SET started_at = now() WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    apply(
        &mut tx,
        &scope,
        &TRIP,
        &mut h,
        Move {
            to: TripStatus::InProgress.as_str(),
            event: None,
            action: "campus.trip.start",
            reason: None,
            process_instance_id: None,
        },
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(trip_reply(&st, t, id, StatusCode::OK).await?)
}

/// Complete a running trip (`campus.transport.manage`; G-106): the vehicle is released.
#[utoipa::path(post, path = "/api/v1/campus/trips/{id}/complete", operation_id = "campus_transport_trip_complete", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the trip")),
    request_body = TripCompleteBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TripView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn trip_complete(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<TripCompleteBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    let v = expected!(headers);
    if b.passenger_count.is_some_and(|c| !(0..=500).contains(&c)) {
        return Err(invalid("passengerCount", "out_of_range", "between 0 and 500").into());
    }
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut h = head(&mut tx, &TRIP, t, id, true).await?;
    ensure_version(v, h.version)?;
    step(&h, TripStatus::Completed)?;
    let f = trip_facts(&mut tx, id).await?;
    let scope = institution_scope(ctx, h.institution);
    sqlx::query("UPDATE campus.trip SET completed_at = now(), passenger_count = $1 WHERE id = $2")
        .bind(b.passenger_count)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    apply(
        &mut tx,
        &scope,
        &TRIP,
        &mut h,
        Move {
            to: TripStatus::Completed.as_str(),
            event: Some((TRIP_COMPLETED, TRIP_COMPLETED_SCHEMA)),
            action: "campus.trip.complete",
            reason: None,
            process_instance_id: None,
        },
        &a,
    )
    .await?;
    if let Some(vehicle) = f.vehicle_id {
        release_vehicle(&mut tx, &scope, vehicle, id, &a).await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(trip_reply(&st, t, id, StatusCode::OK).await?)
}

/// Cancel a planned / ready trip (`campus.transport.manage`; G-105; reason required).
#[utoipa::path(post, path = "/api/v1/campus/trips/{id}/cancel", operation_id = "campus_transport_trip_cancel", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the trip")),
    request_body = TripCancelBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TripView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn trip_cancel(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<TripCancelBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    let v = expected!(headers);
    let reason = required_text("reason", &b.reason, 1000)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut h = head(&mut tx, &TRIP, t, id, true).await?;
    ensure_version(v, h.version)?;
    step(&h, TripStatus::Cancelled)?;
    let was_ready = h.status == TripStatus::Ready.as_str();
    let f = trip_facts(&mut tx, id).await?;
    let scope = institution_scope(ctx, h.institution);
    apply(
        &mut tx,
        &scope,
        &TRIP,
        &mut h,
        Move {
            to: TripStatus::Cancelled.as_str(),
            event: Some((TRIP_CANCELLED, TRIP_CANCELLED_SCHEMA)),
            action: "campus.trip.cancel",
            reason: Some(reason.as_str()),
            process_instance_id: None,
        },
        &a,
    )
    .await?;
    if let (true, Some(vehicle)) = (was_ready, f.vehicle_id) {
        release_vehicle(&mut tx, &scope, vehicle, id, &a).await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(trip_reply(&st, t, id, StatusCode::OK).await?)
}

/// Put a replacement vehicle on a trip (`campus.transport.manage`; G-109): clears the breakdown
/// flag; a ready / running trip assigns the new vehicle and releases the old one when free.
#[utoipa::path(post, path = "/api/v1/campus/trips/{id}/replace-vehicle", operation_id = "campus_transport_trip_replace_vehicle", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the trip")),
    request_body = TripReplaceVehicleBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TripView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn trip_replace_vehicle(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<TripReplaceVehicleBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    let v = expected!(headers);
    let reason = crate::hostel::common::optional_text("reason", b.reason.clone(), 1000)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let h = head(&mut tx, &TRIP, t, id, true).await?;
    ensure_version(v, h.version)?;
    if !matches!(h.status.as_str(), "planned" | "ready" | "in_progress") {
        return Err(KernelError::Conflict(format!("the trip is {}", h.status)).into());
    }
    let f = trip_facts(&mut tx, id).await?;
    if f.vehicle_id == Some(b.vehicle_id) {
        return Err(invalid(
            "vehicleId",
            "same_vehicle",
            "the trip already uses this vehicle",
        )
        .into());
    }
    let passengers = route_passengers(&mut tx, f.route_id).await?;
    usable_vehicle(&mut tx, t, h.institution, b.vehicle_id, passengers).await?;
    let scope = institution_scope(ctx, h.institution);
    sqlx::query(
        "UPDATE campus.trip SET vehicle_id = $1, needs_vehicle = false, status_reason = $2, \
         version = version + 1, updated_at = now() WHERE id = $3",
    )
    .bind(b.vehicle_id)
    .bind(reason.as_deref())
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    crate::shared::publish::audit(
        &mut tx,
        &scope,
        "campus.trip.replace_vehicle",
        TRIP.table,
        id.to_string(),
        serde_json::json!({ "from": f.vehicle_id, "to": b.vehicle_id, "reason": reason }),
        &a,
    )
    .await?;
    if h.status != TripStatus::Planned.as_str() {
        assign_vehicle(&mut tx, &scope, b.vehicle_id, &a).await?;
        if let Some(old) = f.vehicle_id {
            release_vehicle(&mut tx, &scope, old, id, &a).await?;
        }
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(trip_reply(&st, t, id, StatusCode::OK).await?)
}
