//! Vehicles and drivers (L3-G-108 vehicle availability → assignment, G-109 breakdown → open
//! trips need a replacement vehicle, G-102 driver assignment to a route → trip readiness).

use super::domain::{DriverAssignmentStatus, VehicleStatus};
use super::network::ROUTE;
use crate::hostel::common::{
    apply, created, ensure_version, expected, fetch, head, invalid, list_of, optional_text,
    person_in, reply, required_text, status_filter, step, Agg, Move, ReasonBody, StatusFilter,
};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::actor;
use crate::shared::scope::{institution_scope, tenant_scope};
use crate::shared::time::business_today;
use crate::shared::CampusState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sutra_contracts::campus::status::PersonKind;
use sutra_kernel::auth::Permission;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use uuid::Uuid;

pub(crate) const VEHICLE: Agg = Agg {
    table: "vehicle",
    entity: "vehicle",
    number: "registration_no",
    subject: "NULL",
};

pub(crate) const DRIVER: Agg = Agg {
    table: "driver_assignment",
    entity: "driver assignment",
    number: "staff_number",
    subject: "NULL",
};

/// A vehicle.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct VehicleView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "KA01AB1234")]
    pub registration_no: String,
    pub label: String,
    pub capacity: i32,
    /// `available | assigned | under_maintenance | broken_down | retired`.
    pub status: String,
    pub status_reason: Option<String>,
    /// Planned / ready / running trips using the vehicle.
    pub open_trips: i64,
    pub version: i64,
}

pub(crate) const VEHICLE_SELECT: &str = "SELECT x.id, x.institution_id, x.registration_no, x.label, \
    x.capacity, x.status, x.status_reason, \
    (SELECT count(*) FROM campus.trip tr WHERE tr.vehicle_id = x.id AND tr.status IN ('planned', 'ready', 'in_progress')) AS open_trips, \
    x.version FROM campus.vehicle x";

/// Vehicles of an institution.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct VehicleList {
    pub items: Vec<VehicleView>,
}

/// A new vehicle (available).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VehicleCreate {
    pub registration_no: String,
    pub label: String,
    pub capacity: i32,
}

/// A manual vehicle status change (`available | under_maintenance | retired`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VehicleStatusBody {
    pub status: String,
    pub reason: Option<String>,
}

/// A driver assigned to a route.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct DriverAssignmentView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub route_id: Uuid,
    pub route_code: String,
    pub staff_id: Uuid,
    pub staff_number: String,
    pub driver_name: String,
    pub licence_no: String,
    pub valid_from: NaiveDate,
    pub valid_to: Option<NaiveDate>,
    /// `active | ended`.
    pub status: String,
    pub status_reason: Option<String>,
    pub version: i64,
}

const DRIVER_SELECT: &str = "SELECT x.id, x.institution_id, x.route_id, r.code AS route_code, \
    x.staff_id, x.staff_number, x.driver_name, x.licence_no, x.valid_from, x.valid_to, x.status, \
    x.status_reason, x.version FROM campus.driver_assignment x \
    JOIN campus.transport_route r ON r.id = x.route_id";

/// Driver assignments.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DriverAssignmentList {
    pub items: Vec<DriverAssignmentView>,
}

/// Assign a staff member as a driver of the route.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DriverAssignmentCreate {
    /// Domain E staff member of the institution.
    pub staff_id: Uuid,
    pub licence_no: String,
    pub valid_from: NaiveDate,
    pub valid_to: Option<NaiveDate>,
}

async fn vehicle_reply(
    st: &CampusState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v: VehicleView = fetch(&mut conn, VEHICLE_SELECT, tenant, id, VEHICLE.entity).await?;
    Ok(reply(status, &v, v.version))
}

async fn driver_reply(
    st: &CampusState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v: DriverAssignmentView =
        fetch(&mut conn, DRIVER_SELECT, tenant, id, DRIVER.entity).await?;
    Ok(reply(status, &v, v.version))
}

/// Vehicles of an institution (`campus.read`; `?status=`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/vehicles", operation_id = "campus_transport_vehicle_list", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), StatusFilter), security(("session_cookie" = [])),
    responses((status = 200, body = VehicleList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn vehicle_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StatusFilter>,
) -> ApiResult<Json<VehicleList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let status = status_filter::<VehicleStatus>(q.status.as_deref())?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let items = list_of(
        &mut conn,
        VEHICLE_SELECT,
        ctx.scope.tenant_id,
        "institution_id",
        id,
        status,
        "x.registration_no",
    )
    .await?;
    Ok(Json(VehicleList { items }))
}

/// Register a vehicle (`campus.transport.manage`).
#[utoipa::path(post, path = "/api/v1/campus/institutions/{id}/vehicles", operation_id = "campus_transport_vehicle_create", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = VehicleCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = VehicleView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn vehicle_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<VehicleCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    require_idempotency_key(&headers)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let reg = b.registration_no.trim().to_ascii_uppercase();
    let reg_ok = (2..=20).contains(&reg.len())
        && reg.as_bytes()[0].is_ascii_alphanumeric()
        && reg
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b' ' || c == b'-');
    if !reg_ok {
        return Err(invalid(
            "registrationNo",
            "invalid",
            "2–20 letters, digits, spaces or '-'",
        )
        .into());
    }
    let label = required_text("label", &b.label, 200)?;
    if !(1..=200).contains(&b.capacity) {
        return Err(invalid("capacity", "out_of_range", "between 1 and 200 seats").into());
    }
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let vid: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.vehicle (tenant_id, institution_id, registration_no, label, capacity) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&reg)
    .bind(&label)
    .bind(b.capacity)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    created(
        &mut tx,
        &scope,
        &VEHICLE,
        vid,
        VehicleStatus::Available.as_str(),
        "campus.vehicle.create",
        serde_json::json!({ "registrationNo": reg }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(vehicle_reply(&st, t, vid, StatusCode::CREATED).await?)
}

/// Change a vehicle's availability (`campus.transport.manage`; `available`,
/// `under_maintenance`, `retired` — retiring needs no open trip).
#[utoipa::path(post, path = "/api/v1/campus/vehicles/{id}/status", operation_id = "campus_transport_vehicle_status", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the vehicle")),
    request_body = VehicleStatusBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = VehicleView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn vehicle_status(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<VehicleStatusBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    let v = expected!(headers);
    let to = match VehicleStatus::parse(&b.status) {
        Some(s @ (VehicleStatus::Available | VehicleStatus::UnderMaintenance | VehicleStatus::Retired)) => s,
        _ => {
            return Err(invalid(
                "status",
                "invalid",
                "available, under_maintenance or retired (breakdowns use /breakdown, assignment happens on trips)",
            )
            .into())
        }
    };
    let reason = optional_text("reason", b.reason.clone(), 1000)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut h = head(&mut tx, &VEHICLE, t, id, true).await?;
    ensure_version(v, h.version)?;
    step(&h, to)?;
    if to == VehicleStatus::Retired {
        let open: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM campus.trip WHERE vehicle_id = $1 AND status IN ('planned', 'ready', 'in_progress')",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if open > 0 {
            return Err(KernelError::Conflict(format!(
                "{open} open trip(s) still use the vehicle; replace it on them first"
            ))
            .into());
        }
    }
    let scope = institution_scope(ctx, h.institution);
    apply(
        &mut tx,
        &scope,
        &VEHICLE,
        &mut h,
        Move {
            to: to.as_str(),
            event: None,
            action: match to {
                VehicleStatus::Retired => "campus.vehicle.retire",
                VehicleStatus::UnderMaintenance => "campus.vehicle.maintenance",
                _ => "campus.vehicle.available",
            },
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(vehicle_reply(&st, t, id, StatusCode::OK).await?)
}

/// Report a breakdown (`campus.transport.manage`; G-109): the vehicle is `broken_down` and its
/// open trips are flagged as needing a replacement vehicle (`/trips/{id}/replace-vehicle`).
#[utoipa::path(post, path = "/api/v1/campus/vehicles/{id}/breakdown", operation_id = "campus_transport_vehicle_breakdown", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the vehicle")),
    request_body = ReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = VehicleView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn vehicle_breakdown(
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
    let mut h = head(&mut tx, &VEHICLE, t, id, true).await?;
    ensure_version(v, h.version)?;
    step(&h, VehicleStatus::BrokenDown)?;
    let scope = institution_scope(ctx, h.institution);
    sqlx::query(
        "UPDATE campus.trip SET needs_vehicle = true, updated_at = now() WHERE vehicle_id = $1 \
         AND status IN ('planned', 'ready', 'in_progress')",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    apply(
        &mut tx,
        &scope,
        &VEHICLE,
        &mut h,
        Move {
            to: VehicleStatus::BrokenDown.as_str(),
            event: None,
            action: "campus.vehicle.breakdown",
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(vehicle_reply(&st, t, id, StatusCode::OK).await?)
}

/// Drivers of a route (`campus.read`; `?status=`).
#[utoipa::path(get, path = "/api/v1/campus/transport-routes/{id}/drivers", operation_id = "campus_transport_driver_list", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), StatusFilter), security(("session_cookie" = [])),
    responses((status = 200, body = DriverAssignmentList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn driver_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StatusFilter>,
) -> ApiResult<Json<DriverAssignmentList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let status = status_filter::<DriverAssignmentStatus>(q.status.as_deref())?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    head(&mut conn, &ROUTE, ctx.scope.tenant_id, id, false).await?;
    let items = list_of(
        &mut conn,
        DRIVER_SELECT,
        ctx.scope.tenant_id,
        "route_id",
        id,
        status,
        "x.valid_from DESC",
    )
    .await?;
    Ok(Json(DriverAssignmentList { items }))
}

/// Assign a driver to a route (`campus.transport.manage`; a live staff member of the
/// institution, once per route while active).
#[utoipa::path(post, path = "/api/v1/campus/transport-routes/{id}/drivers", operation_id = "campus_transport_driver_assign", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = DriverAssignmentCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = DriverAssignmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn driver_assign(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<DriverAssignmentCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let licence = required_text("licenceNo", &b.licence_no, 40)?;
    if licence.chars().count() < 3 {
        return Err(invalid("licenceNo", "invalid", "3–40 characters").into());
    }
    if b.valid_to.is_some_and(|to| to < b.valid_from) {
        return Err(invalid("validTo", "before_start", "validTo is before validFrom").into());
    }
    let rh = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        head(&mut conn, &ROUTE, t, id, false).await?
    };
    if rh.status == "retired" {
        return Err(KernelError::Conflict("the route is retired".into()).into());
    }
    let scope = institution_scope(ctx, rh.institution);
    require_institution_writable(&st, &scope, rh.institution).await?;
    let driver = person_in(
        &st,
        &tenant_scope(ctx),
        PersonKind::Staff,
        b.staff_id,
        rh.institution,
        "staffId",
    )
    .await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let did: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.driver_assignment (tenant_id, institution_id, route_id, staff_id, \
         staff_number, driver_name, licence_no, valid_from, valid_to) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(rh.institution.as_uuid())
    .bind(id)
    .bind(driver.id)
    .bind(&driver.number)
    .bind(&driver.display_name)
    .bind(&licence)
    .bind(b.valid_from)
    .bind(b.valid_to)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    created(
        &mut tx,
        &scope,
        &DRIVER,
        did,
        DriverAssignmentStatus::Active.as_str(),
        "campus.driver_assignment.assign",
        serde_json::json!({ "route": rh.number, "staff": driver.number }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(driver_reply(&st, t, did, StatusCode::CREATED).await?)
}

/// End a driver assignment (`campus.transport.manage`; not while a ready / running trip uses it).
#[utoipa::path(post, path = "/api/v1/campus/driver-assignments/{id}/end", operation_id = "campus_transport_driver_end", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the assignment")),
    request_body = ReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = DriverAssignmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn driver_end(
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
    let inst = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        head(&mut conn, &DRIVER, t, id, false).await?.institution
    };
    let scope = institution_scope(ctx, inst);
    let today = business_today(&st, &scope, inst).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut h = head(&mut tx, &DRIVER, t, id, true).await?;
    ensure_version(v, h.version)?;
    step(&h, DriverAssignmentStatus::Ended)?;
    let busy: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM campus.trip WHERE driver_assignment_id = $1 AND status IN ('ready', 'in_progress')",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if busy > 0 {
        return Err(
            KernelError::Conflict("the driver is on a ready or running trip".into()).into(),
        );
    }
    sqlx::query(
        "UPDATE campus.driver_assignment SET valid_to = GREATEST(valid_from, LEAST(COALESCE(valid_to, $1), $1)) WHERE id = $2",
    )
    .bind(today)
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    apply(
        &mut tx,
        &scope,
        &DRIVER,
        &mut h,
        Move {
            to: DriverAssignmentStatus::Ended.as_str(),
            event: None,
            action: "campus.driver_assignment.end",
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(driver_reply(&st, t, id, StatusCode::OK).await?)
}
