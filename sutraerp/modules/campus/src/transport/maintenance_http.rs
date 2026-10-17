//! **ERP_EXTENSIONS 2D** — fleet maintenance over the transport fleet (rules in
//! [`super::maintenance`], ported from HeyPuter/everything fleet, MIT). Operation ids
//! `campus_transport_*`, tag `campus-transport`.
//!
//! Service reminders repeat a task by time, by meter, or whichever comes first; renewals are
//! dated obligations (RC, insurance, fitness, PUC, permit, road tax) that roll forward on an
//! anchored repeat. A service entry completes the reminders it names; fuel entries give economy
//! between full fills (L/100 km); meter readings keep the vehicle's meter; expenses, fuel and
//! service feed the cost report. Money on the wire is rupees; the rules work in paise.
#![allow(clippy::type_complexity)]

use super::maintenance::{
    self as m, Anchor, Completion, CostLine, DueSoon, FuelEntry, Interval, Reading, Reminder,
    Renewal, RenewalDone,
};
use crate::infrastructure::common::{audit, ensure_version, expected, invalid};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::scope::institution_scope;
use crate::shared::time::business_today;
use crate::shared::CampusState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{Datelike, NaiveDate};
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

const UNITS: &[&str] = &["days", "weeks", "months", "years"];
const RENEWALS: &[&str] = &[
    "registration",
    "insurance",
    "fitness",
    "pollution",
    "permit",
    "road_tax",
    "other",
];

fn f(d: Decimal) -> f64 {
    d.to_f64().unwrap_or(0.0)
}

fn fo(d: Option<Decimal>) -> Option<f64> {
    d.map(f)
}

fn dec1(x: f64) -> Decimal {
    Decimal::from_f64(x).unwrap_or_default().round_dp(1)
}

fn rupees(paise: f64) -> Decimal {
    (Decimal::from_f64(paise).unwrap_or_default() / Decimal::from(100)).round_dp(2)
}

fn ds(d: Option<NaiveDate>) -> Option<String> {
    d.map(|d| d.format("%Y-%m-%d").to_string())
}

fn pd(s: &Option<String>) -> Option<NaiveDate> {
    s.as_deref()
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
}

// ------------------------------------------------------------------ settings

/// Fleet settings of an institution.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FleetSettings {
    /// A reminder or renewal is "due soon" this many days ahead (default 14).
    pub due_soon_days: i32,
    /// …or this close on the meter (default 500 km).
    #[schema(value_type = String, example = "500")]
    pub due_soon_meter: Decimal,
    /// `L | gal` (default litres).
    pub volume_unit: String,
}

async fn settings_of(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
) -> Result<FleetSettings, KernelError> {
    let r: Option<(i32, Decimal, String)> = sqlx::query_as(
        "SELECT due_soon_days, due_soon_meter, volume_unit FROM campus.fleet_settings \
         WHERE tenant_id = $1 AND institution_id = $2",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(r.map_or_else(
        || FleetSettings {
            due_soon_days: 14,
            due_soon_meter: Decimal::from(500),
            volume_unit: "L".into(),
        },
        |(a, b, c)| FleetSettings {
            due_soon_days: a,
            due_soon_meter: b,
            volume_unit: c,
        },
    ))
}

fn due_of(s: &FleetSettings) -> DueSoon {
    DueSoon {
        days: f64::from(s.due_soon_days),
        meter: f(s.due_soon_meter),
    }
}

/// Fleet settings (`campus.read`; defaults until saved).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/fleet-settings", operation_id = "campus_transport_fleet_settings_get", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = FleetSettings), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn settings_get(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<FleetSettings>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(Json(settings_of(&mut conn, ctx.scope.tenant_id, id).await?))
}

/// Save fleet settings (`campus.transport.manage`; audited).
#[utoipa::path(put, path = "/api/v1/campus/institutions/{id}/fleet-settings", operation_id = "campus_transport_fleet_settings_put", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid)), request_body = FleetSettings, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FleetSettings), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn settings_put(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<FleetSettings>,
) -> ApiResult<Json<FleetSettings>> {
    let ctx = auth.require(Permission::TransportManage)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    if !(0..=365).contains(&b.due_soon_days) {
        return Err(invalid("dueSoonDays", "out_of_range", "between 0 and 365 days").into());
    }
    if b.due_soon_meter < Decimal::ZERO {
        return Err(invalid("dueSoonMeter", "negative", "≥ 0").into());
    }
    if !matches!(b.volume_unit.as_str(), "L" | "gal") {
        return Err(invalid("volumeUnit", "invalid", "L or gal").into());
    }
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query(
        "INSERT INTO campus.fleet_settings (tenant_id, institution_id, due_soon_days, due_soon_meter, \
         volume_unit) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (tenant_id, institution_id) DO UPDATE \
         SET due_soon_days = EXCLUDED.due_soon_days, due_soon_meter = EXCLUDED.due_soon_meter, \
         volume_unit = EXCLUDED.volume_unit, version = campus.fleet_settings.version + 1, \
         updated_at = now()",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(b.due_soon_days)
    .bind(b.due_soon_meter)
    .bind(&b.volume_unit)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "campus.fleet_settings.update",
        "fleet_settings",
        id,
        serde_json::to_value(&b).unwrap_or_default(),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(b))
}

// ------------------------------------------------------------------ vehicles

struct Vehicle {
    institution: Uuid,
    registration_no: String,
    meter: Option<f64>,
    meter_unit: String,
    fuel_capacity: Option<f64>,
}

async fn vehicle_of(
    conn: &mut PgConnection,
    t: TenantId,
    id: Uuid,
) -> Result<Vehicle, KernelError> {
    let r: Option<(Uuid, String, Option<Decimal>, String, Option<Decimal>)> = sqlx::query_as(
        "SELECT institution_id, registration_no, meter, meter_unit, fuel_capacity FROM campus.vehicle \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let (institution, registration_no, meter, meter_unit, cap) =
        r.ok_or(KernelError::NotFound { entity: "vehicle" })?;
    Ok(Vehicle {
        institution,
        registration_no,
        meter: fo(meter),
        meter_unit,
        fuel_capacity: fo(cap),
    })
}

#[allow(clippy::too_many_arguments)]
/// Records a meter reading and raises the vehicle's meter to it (never lowers it).
async fn record_meter(
    conn: &mut PgConnection,
    t: TenantId,
    v: &Vehicle,
    vehicle: Uuid,
    on: NaiveDate,
    value: Decimal,
    source: &str,
    actor: Option<&str>,
) -> Result<(), KernelError> {
    sqlx::query(
        "INSERT INTO campus.vehicle_meter_reading (tenant_id, institution_id, vehicle_id, read_on, \
         value, source, actor) VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(t.as_uuid())
    .bind(v.institution)
    .bind(vehicle)
    .bind(on)
    .bind(value)
    .bind(source)
    .bind(actor)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    sqlx::query(
        "UPDATE campus.vehicle SET meter = GREATEST(coalesce(meter, 0), $3), updated_at = now() \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(vehicle)
    .bind(t.as_uuid())
    .bind(value)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

/// Set a vehicle's meter details.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VehicleMeterSetup {
    /// `km | mi | hr`.
    pub meter_unit: String,
    /// Tank size in the fleet's volume unit (warns on over-full fills).
    #[schema(value_type = Option<String>, example = "60")]
    pub fuel_capacity: Option<Decimal>,
}

/// A meter reading.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MeterReadingCreate {
    pub read_on: Option<NaiveDate>,
    #[schema(value_type = String, example = "48250")]
    pub value: Decimal,
    /// Optional meter set-up applied with the reading.
    pub setup: Option<VehicleMeterSetup>,
}

/// Record a meter reading (`campus.transport.manage`; Idempotency-Key): the vehicle's meter
/// rises to it; a reading lower than the meter is kept but changes nothing.
#[utoipa::path(post, path = "/api/v1/campus/vehicles/{id}/meter-readings", operation_id = "campus_transport_meter_reading_create", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = MeterReadingCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = VehicleMaintenance), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn meter_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<MeterReadingCreate>,
) -> ApiResult<(StatusCode, Json<VehicleMaintenance>)> {
    let ctx = auth.require(Permission::TransportManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let v = vehicle_of(&mut tx, t, id).await?;
    let inst = InstitutionId(v.institution);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    if b.value < Decimal::ZERO {
        return Err(invalid("value", "negative", "a meter reading is ≥ 0").into());
    }
    if let Some(s) = &b.setup {
        if !matches!(s.meter_unit.as_str(), "km" | "mi" | "hr") {
            return Err(invalid("setup.meterUnit", "invalid", "km, mi or hr").into());
        }
        if s.fuel_capacity.is_some_and(|c| c <= Decimal::ZERO) {
            return Err(invalid("setup.fuelCapacity", "invalid", "> 0").into());
        }
        sqlx::query("UPDATE campus.vehicle SET meter_unit = $3, fuel_capacity = $4 WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(t.as_uuid())
            .bind(&s.meter_unit)
            .bind(s.fuel_capacity)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
    }
    let on = match b.read_on {
        Some(d) => d,
        None => business_today(&st, &scope, inst).await?,
    };
    let a = actor(ctx);
    record_meter(
        &mut tx,
        t,
        &v,
        id,
        on,
        b.value.round_dp(1),
        "manual",
        a.id.as_deref(),
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "campus.vehicle.meter_reading",
        "vehicle",
        id,
        serde_json::json!({"value": b.value.to_string(), "readOn": on}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let today = business_today(&st, &scope, inst).await?;
    Ok((
        StatusCode::CREATED,
        Json(vehicle_maintenance(&mut conn, t, id, today).await?),
    ))
}

// ------------------------------------------------------------------ reminders

const REMINDER_COLUMNS: &str = "r.id, r.vehicle_id, v.registration_no, v.meter, v.meter_unit, r.task, \
    r.interval_every, r.interval_unit, r.meter_interval, r.due_soon_days, r.due_soon_meter, r.start_date, \
    r.start_meter, r.last_date, r.last_meter, r.next_date, r.next_meter, r.snoozed_until, r.history, \
    r.active, r.version, r.created_at";

struct ReminderRow {
    id: Uuid,
    vehicle_id: Uuid,
    registration_no: String,
    vehicle_meter: Option<f64>,
    meter_unit: String,
    task: String,
    active: bool,
    version: i64,
    rule: Reminder,
}

fn reminder_from(r: &PgRow) -> Result<ReminderRow, sqlx::Error> {
    let every: Option<i32> = r.try_get("interval_every")?;
    let unit: String = r.try_get("interval_unit")?;
    let history: serde_json::Value = r.try_get("history")?;
    let start: NaiveDate = r.try_get("start_date")?;
    Ok(ReminderRow {
        id: r.try_get("id")?,
        vehicle_id: r.try_get("vehicle_id")?,
        registration_no: r.try_get("registration_no")?,
        vehicle_meter: fo(r.try_get("meter")?),
        meter_unit: r.try_get("meter_unit")?,
        task: r.try_get("task")?,
        active: r.try_get("active")?,
        version: r.try_get("version")?,
        rule: Reminder {
            interval: every.map(|e| Interval {
                every: f64::from(e),
                unit,
            }),
            meter_interval: fo(r.try_get("meter_interval")?),
            due_soon_days: r.try_get::<Option<i32>, _>("due_soon_days")?.map(f64::from),
            due_soon_meter: fo(r.try_get("due_soon_meter")?),
            last_date: ds(r.try_get("last_date")?),
            last_meter: fo(r.try_get("last_meter")?),
            next_date: ds(r.try_get("next_date")?),
            next_meter: fo(r.try_get("next_meter")?),
            snoozed_until: ds(r.try_get("snoozed_until")?),
            // The schedule counts from the start date until a first completion.
            created_day: ds(Some(start)),
            start_date: ds(Some(start)),
            start_meter: fo(r.try_get("start_meter")?),
            history: serde_json::from_value(history).unwrap_or_default(),
        },
    })
}

/// A service reminder and where it stands.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServiceReminderView {
    pub id: Uuid,
    pub vehicle_id: Uuid,
    pub registration_no: String,
    pub task: String,
    /// "Every 6 months or 10000 km", "One time".
    pub schedule: String,
    pub interval: Option<Interval>,
    pub meter_interval: Option<f64>,
    pub due_soon_days: Option<f64>,
    pub due_soon_meter: Option<f64>,
    pub last_date: Option<String>,
    pub last_meter: Option<f64>,
    pub snoozed_until: Option<String>,
    pub state: m::ReminderState,
    /// Share of completions done on time (null without history).
    pub compliance: Option<f64>,
    pub history: Vec<Completion>,
    pub active: bool,
    pub version: i64,
}

fn reminder_view(r: &ReminderRow, today: &str, due: DueSoon) -> ServiceReminderView {
    ServiceReminderView {
        id: r.id,
        vehicle_id: r.vehicle_id,
        registration_no: r.registration_no.clone(),
        task: r.task.clone(),
        schedule: m::describe_schedule(&r.rule, &r.meter_unit),
        interval: r.rule.interval.clone(),
        meter_interval: r.rule.meter_interval,
        due_soon_days: r.rule.due_soon_days,
        due_soon_meter: r.rule.due_soon_meter,
        last_date: r.rule.last_date.clone(),
        last_meter: r.rule.last_meter,
        snoozed_until: r.rule.snoozed_until.clone(),
        state: m::reminder_state(&r.rule, r.vehicle_meter, today, due),
        compliance: m::compliance(&r.rule),
        history: r.rule.history.clone(),
        active: r.active,
        version: r.version,
    }
}

async fn reminders_where(
    conn: &mut PgConnection,
    t: TenantId,
    filter: &str,
    id: Uuid,
) -> Result<Vec<ReminderRow>, KernelError> {
    let sql = format!(
        "SELECT {REMINDER_COLUMNS} FROM campus.vehicle_service_reminder r JOIN campus.vehicle v \
         ON v.id = r.vehicle_id WHERE r.tenant_id = $1 AND {filter} = $2 ORDER BY r.created_at"
    );
    let rows = sqlx::query(&sql)
        .bind(t.as_uuid())
        .bind(id)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    rows.iter()
        .map(reminder_from)
        .collect::<Result<_, _>>()
        .map_err(map_db_error)
}

/// Create or change a service reminder.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServiceReminderInput {
    #[schema(example = "Engine oil and filter")]
    pub task: String,
    /// Repeat by time (`every` 1…1000, `unit` days | weeks | months | years).
    pub interval: Option<Interval>,
    /// Repeat by meter (km / mi / hours).
    pub meter_interval: Option<f64>,
    pub due_soon_days: Option<i32>,
    pub due_soon_meter: Option<f64>,
    /// Where the schedule starts (default today, and the vehicle's meter).
    pub start_date: Option<NaiveDate>,
    pub start_meter: Option<f64>,
    /// Manual override of the next due date / meter (cleared by a completion).
    pub next_date: Option<NaiveDate>,
    pub next_meter: Option<f64>,
    /// Update only (default true).
    pub active: Option<bool>,
}

fn check_reminder(b: &ServiceReminderInput) -> Result<String, KernelError> {
    let task = b.task.trim().to_owned();
    if task.is_empty() || task.chars().count() > 200 {
        return Err(invalid("task", "required", "1–200 characters"));
    }
    if let Some(i) = &b.interval {
        if !(1.0..=1000.0).contains(&i.every) || i.every.fract() != 0.0 {
            return Err(invalid(
                "interval.every",
                "out_of_range",
                "a whole number from 1 to 1000",
            ));
        }
        if !UNITS.contains(&i.unit.as_str()) {
            return Err(invalid(
                "interval.unit",
                "invalid",
                "days, weeks, months or years",
            ));
        }
    }
    if b.meter_interval.is_some_and(|x| x.is_nan() || x <= 0.0) {
        return Err(invalid("meterInterval", "invalid", "> 0"));
    }
    if b.due_soon_days.is_some_and(|d| !(0..=365).contains(&d)) {
        return Err(invalid(
            "dueSoonDays",
            "out_of_range",
            "between 0 and 365 days",
        ));
    }
    if b.interval.is_none()
        && b.meter_interval.is_none()
        && b.next_date.is_none()
        && b.next_meter.is_none()
    {
        return Err(invalid(
            "interval",
            "required",
            "repeat by time, by meter, or give a due date",
        ));
    }
    Ok(task)
}

/// Add a service reminder to a vehicle (`campus.transport.manage`; Idempotency-Key).
#[utoipa::path(post, path = "/api/v1/campus/vehicles/{id}/service-reminders", operation_id = "campus_transport_service_reminder_create", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ServiceReminderInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ServiceReminderView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn reminder_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ServiceReminderInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    require_idempotency_key(&headers)?;
    let task = check_reminder(&b)?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let v = vehicle_of(&mut tx, t, id).await?;
    let inst = InstitutionId(v.institution);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let start = b.start_date.unwrap_or(today);
    let start_meter = b.start_meter.or(v.meter);
    let rid: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.vehicle_service_reminder (tenant_id, institution_id, vehicle_id, task, \
         interval_every, interval_unit, meter_interval, due_soon_days, due_soon_meter, start_date, \
         start_meter, last_date, last_meter, next_date, next_meter) VALUES ($1, $2, $3, $4, $5, $6, \
         $7, $8, $9, $10, $11, $10, $11, $12, $13) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(v.institution)
    .bind(id)
    .bind(&task)
    .bind(b.interval.as_ref().map(|i| i.every as i32))
    .bind(b.interval.as_ref().map_or("months", |i| i.unit.as_str()))
    .bind(b.meter_interval.map(dec1))
    .bind(b.due_soon_days)
    .bind(b.due_soon_meter.map(dec1))
    .bind(start)
    .bind(start_meter.map(dec1))
    .bind(b.next_date)
    .bind(b.next_meter.map(dec1))
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let a = actor(ctx);
    audit(
        &mut tx,
        &scope,
        "campus.vehicle_service_reminder.create",
        "vehicle_service_reminder",
        rid,
        serde_json::json!({"vehicle": v.registration_no, "task": task}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    reminder_reply(&st, t, rid, today, StatusCode::CREATED).await
}

async fn reminder_reply(
    st: &CampusState,
    t: TenantId,
    rid: Uuid,
    today: NaiveDate,
    status: StatusCode,
) -> ApiResult<Response> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let row = reminders_where(&mut conn, t, "r.id", rid)
        .await?
        .into_iter()
        .next()
        .ok_or(KernelError::NotFound {
            entity: "service reminder",
        })?;
    let inst = row_institution(&mut conn, t, rid).await?;
    let s = settings_of(&mut conn, t, inst).await?;
    let view = reminder_view(&row, &today.format("%Y-%m-%d").to_string(), due_of(&s));
    let v = view.version;
    Ok(with_etag(status, &view, Version(v)))
}

async fn row_institution(
    conn: &mut PgConnection,
    t: TenantId,
    rid: Uuid,
) -> Result<Uuid, KernelError> {
    sqlx::query_scalar("SELECT institution_id FROM campus.vehicle_service_reminder WHERE id = $1 AND tenant_id = $2")
        .bind(rid)
        .bind(t.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound { entity: "service reminder" })
}

/// Change a service reminder (`campus.transport.manage`; If-Match).
#[utoipa::path(put, path = "/api/v1/campus/service-reminders/{id}", operation_id = "campus_transport_service_reminder_update", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = ServiceReminderInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ServiceReminderView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn reminder_update(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ServiceReminderInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    let want = expected!(headers);
    let task = check_reminder(&b)?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let (inst_id, version): (Uuid, i64) = sqlx::query_as(
        "SELECT institution_id, version FROM campus.vehicle_service_reminder WHERE id = $1 \
         AND tenant_id = $2 FOR UPDATE",
    )
    .bind(id)
    .bind(t.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "service reminder",
    })?;
    ensure_version(want, version)?;
    let inst = InstitutionId(inst_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    sqlx::query(
        "UPDATE campus.vehicle_service_reminder SET task = $3, interval_every = $4, interval_unit = $5, \
         meter_interval = $6, due_soon_days = $7, due_soon_meter = $8, next_date = $9, next_meter = $10, \
         active = $11, start_date = coalesce($12, start_date), start_meter = coalesce($13, start_meter), \
         version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(&task)
    .bind(b.interval.as_ref().map(|i| i.every as i32))
    .bind(b.interval.as_ref().map_or("months", |i| i.unit.as_str()))
    .bind(b.meter_interval.map(dec1))
    .bind(b.due_soon_days)
    .bind(b.due_soon_meter.map(dec1))
    .bind(b.next_date)
    .bind(b.next_meter.map(dec1))
    .bind(b.active.unwrap_or(true))
    .bind(b.start_date)
    .bind(b.start_meter.map(dec1))
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let a = actor(ctx);
    audit(
        &mut tx,
        &scope,
        "campus.vehicle_service_reminder.update",
        "vehicle_service_reminder",
        id,
        serde_json::json!({"task": task, "active": b.active.unwrap_or(true)}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let today = business_today(&st, &scope, inst).await?;
    reminder_reply(&st, t, id, today, StatusCode::OK).await
}

/// Snooze until a date (null clears).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SnoozeBody {
    pub until: Option<NaiveDate>,
}

/// Snooze a service reminder (`campus.transport.manage`): hides a due or overdue reminder
/// until the date; snoozing never makes the work on time.
#[utoipa::path(post, path = "/api/v1/campus/service-reminders/{id}/snooze", operation_id = "campus_transport_service_reminder_snooze", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid)), request_body = SnoozeBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ServiceReminderView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn reminder_snooze(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<SnoozeBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let inst = InstitutionId(row_institution(&mut tx, t, id).await?);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    sqlx::query(
        "UPDATE campus.vehicle_service_reminder SET snoozed_until = $3, version = version + 1, \
         updated_at = now() WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(b.until)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let a = actor(ctx);
    audit(
        &mut tx,
        &scope,
        "campus.vehicle_service_reminder.snooze",
        "vehicle_service_reminder",
        id,
        serde_json::json!({"until": b.until}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let today = business_today(&st, &scope, inst).await?;
    reminder_reply(&st, t, id, today, StatusCode::OK).await
}

// ------------------------------------------------------------------ renewals

const RENEWAL_COLUMNS: &str = "r.id, r.vehicle_id, v.registration_no, r.renewal_type, r.label, r.due_date, \
    r.due_soon_days, r.repeat_every, r.repeat_unit, r.repeat_anchor, r.snoozed_until, r.history, r.done, \
    r.version";

struct RenewalRow {
    id: Uuid,
    vehicle_id: Uuid,
    registration_no: String,
    renewal_type: String,
    label: Option<String>,
    version: i64,
    rule: Renewal,
}

fn renewal_from(r: &PgRow) -> Result<RenewalRow, sqlx::Error> {
    let every: Option<i32> = r.try_get("repeat_every")?;
    let unit: String = r.try_get("repeat_unit")?;
    let anchor: Option<serde_json::Value> = r.try_get("repeat_anchor")?;
    let history: serde_json::Value = r.try_get("history")?;
    Ok(RenewalRow {
        id: r.try_get("id")?,
        vehicle_id: r.try_get("vehicle_id")?,
        registration_no: r.try_get("registration_no")?,
        renewal_type: r.try_get("renewal_type")?,
        label: r.try_get("label")?,
        version: r.try_get("version")?,
        rule: Renewal {
            due_date: ds(r.try_get("due_date")?),
            due_soon_days: r.try_get::<Option<i32>, _>("due_soon_days")?.map(f64::from),
            repeat: every.map(|e| Interval {
                every: f64::from(e),
                unit,
            }),
            repeat_anchor: anchor.and_then(|a| serde_json::from_value::<Anchor>(a).ok()),
            snoozed_until: ds(r.try_get("snoozed_until")?),
            history: serde_json::from_value(history).unwrap_or_default(),
            done: r.try_get("done")?,
        },
    })
}

/// A renewal and where it stands.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VehicleRenewalView {
    pub id: Uuid,
    pub vehicle_id: Uuid,
    pub registration_no: String,
    /// `registration | insurance | fitness | pollution | permit | road_tax | other`.
    pub renewal_type: String,
    pub label: Option<String>,
    pub due_date: Option<String>,
    pub repeat: Option<Interval>,
    pub snoozed_until: Option<String>,
    /// `overdue | due_soon | upcoming | snoozed` (a renewal with no next date is `upcoming`).
    pub status: String,
    pub days_left: Option<i64>,
    pub done: bool,
    pub history: Vec<RenewalDone>,
    pub version: i64,
}

fn renewal_view(r: &RenewalRow, today: &str, due: DueSoon) -> VehicleRenewalView {
    let (status, days_left) = m::renewal_state(&r.rule, today, due);
    VehicleRenewalView {
        id: r.id,
        vehicle_id: r.vehicle_id,
        registration_no: r.registration_no.clone(),
        renewal_type: r.renewal_type.clone(),
        label: r.label.clone(),
        due_date: r.rule.due_date.clone(),
        repeat: r.rule.repeat.clone(),
        snoozed_until: r.rule.snoozed_until.clone(),
        status,
        days_left,
        done: r.rule.done,
        history: r.rule.history.clone(),
        version: r.version,
    }
}

async fn renewals_where(
    conn: &mut PgConnection,
    t: TenantId,
    filter: &str,
    id: Uuid,
) -> Result<Vec<RenewalRow>, KernelError> {
    let sql = format!(
        "SELECT {RENEWAL_COLUMNS} FROM campus.vehicle_renewal r JOIN campus.vehicle v ON v.id = r.vehicle_id \
         WHERE r.tenant_id = $1 AND {filter} = $2 ORDER BY r.due_date NULLS LAST, r.created_at"
    );
    let rows = sqlx::query(&sql)
        .bind(t.as_uuid())
        .bind(id)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    rows.iter()
        .map(renewal_from)
        .collect::<Result<_, _>>()
        .map_err(map_db_error)
}

/// A new renewal.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VehicleRenewalCreate {
    /// `registration | insurance | fitness | pollution | permit | road_tax | other`.
    pub renewal_type: String,
    pub label: Option<String>,
    pub due_date: NaiveDate,
    pub due_soon_days: Option<i32>,
    /// Rolls forward by this when completed (e.g. `{every: 1, unit: "years"}`).
    pub repeat: Option<Interval>,
}

/// Add a renewal to a vehicle (`campus.transport.manage`; Idempotency-Key).
#[utoipa::path(post, path = "/api/v1/campus/vehicles/{id}/renewals", operation_id = "campus_transport_vehicle_renewal_create", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = VehicleRenewalCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = VehicleRenewalView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn renewal_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<VehicleRenewalCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    require_idempotency_key(&headers)?;
    if !RENEWALS.contains(&b.renewal_type.as_str()) {
        return Err(invalid(
            "renewalType",
            "invalid",
            "registration, insurance, fitness, pollution, permit, road_tax or other",
        )
        .into());
    }
    if let Some(i) = &b.repeat {
        if !(1.0..=1000.0).contains(&i.every)
            || i.every.fract() != 0.0
            || !UNITS.contains(&i.unit.as_str())
        {
            return Err(invalid(
                "repeat",
                "invalid",
                "a whole 1…1000 days, weeks, months or years",
            )
            .into());
        }
    }
    if b.due_soon_days.is_some_and(|d| !(0..=365).contains(&d)) {
        return Err(invalid("dueSoonDays", "out_of_range", "between 0 and 365 days").into());
    }
    let label = b
        .label
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned);
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let v = vehicle_of(&mut tx, t, id).await?;
    let inst = InstitutionId(v.institution);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let rid: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.vehicle_renewal (tenant_id, institution_id, vehicle_id, renewal_type, label, \
         due_date, due_soon_days, repeat_every, repeat_unit) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(v.institution)
    .bind(id)
    .bind(&b.renewal_type)
    .bind(&label)
    .bind(b.due_date)
    .bind(b.due_soon_days)
    .bind(b.repeat.as_ref().map(|i| i.every as i32))
    .bind(b.repeat.as_ref().map_or("years", |i| i.unit.as_str()))
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let a = actor(ctx);
    audit(&mut tx, &scope, "campus.vehicle_renewal.create", "vehicle_renewal", rid,
        serde_json::json!({"vehicle": v.registration_no, "type": b.renewal_type, "dueDate": b.due_date}), &a)
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let today = business_today(&st, &scope, inst).await?;
    renewal_reply(&st, t, rid, inst_uuid(inst), today, StatusCode::CREATED).await
}

fn inst_uuid(i: InstitutionId) -> Uuid {
    i.as_uuid()
}

async fn renewal_reply(
    st: &CampusState,
    t: TenantId,
    rid: Uuid,
    inst: Uuid,
    today: NaiveDate,
    status: StatusCode,
) -> ApiResult<Response> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let row = renewals_where(&mut conn, t, "r.id", rid)
        .await?
        .into_iter()
        .next()
        .ok_or(KernelError::NotFound {
            entity: "vehicle renewal",
        })?;
    let s = settings_of(&mut conn, t, inst).await?;
    let view = renewal_view(&row, &today.format("%Y-%m-%d").to_string(), due_of(&s));
    let v = view.version;
    Ok(with_etag(status, &view, Version(v)))
}

async fn lock_renewal(
    conn: &mut PgConnection,
    t: TenantId,
    id: Uuid,
) -> Result<(RenewalRow, Uuid), KernelError> {
    let inst: Uuid = sqlx::query_scalar(
        "SELECT institution_id FROM campus.vehicle_renewal WHERE id = $1 AND tenant_id = $2 FOR UPDATE",
    )
    .bind(id)
    .bind(t.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: "vehicle renewal" })?;
    let row = renewals_where(conn, t, "r.id", id)
        .await?
        .into_iter()
        .next()
        .ok_or(KernelError::NotFound {
            entity: "vehicle renewal",
        })?;
    Ok((row, inst))
}

/// Mark a renewal done.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RenewalComplete {
    /// Default today.
    pub date: Option<NaiveDate>,
    /// The new due date when it is not the repeat (e.g. a 3-year policy this time).
    pub next_due: Option<NaiveDate>,
    /// What the renewal cost, ₹ (goes to the vehicle's other costs).
    #[schema(value_type = Option<String>, example = "12450.00")]
    pub cost: Option<Decimal>,
}

/// Complete a renewal (`campus.transport.manage`; If-Match): it rolls forward by its repeat,
/// counted from where the series started; with no repeat and no next date it is done. A cost
/// is booked as a vehicle expense.
#[utoipa::path(post, path = "/api/v1/campus/vehicle-renewals/{id}/complete", operation_id = "campus_transport_vehicle_renewal_complete", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = RenewalComplete, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = VehicleRenewalView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn renewal_complete(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<RenewalComplete>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    let want = expected!(headers);
    let t = ctx.scope.tenant_id;
    if b.cost.is_some_and(|c| c < Decimal::ZERO) {
        return Err(invalid("cost", "negative", "≥ 0").into());
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let (row, inst_id) = lock_renewal(&mut tx, t, id).await?;
    ensure_version(want, row.version)?;
    let inst = InstitutionId(inst_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    if row.rule.done {
        return Err(KernelError::Conflict("this renewal is already done".into()).into());
    }
    let today = business_today(&st, &scope, inst).await?;
    let date = b.date.unwrap_or(today);
    let date_s = date.format("%Y-%m-%d").to_string();
    let next = b.next_due.map(|d| d.format("%Y-%m-%d").to_string());
    let paise = b.cost.map(|c| f(c) * 100.0);
    let out = m::complete_renewal(&row.rule, &date_s, next.as_deref(), paise);
    sqlx::query(
        "UPDATE campus.vehicle_renewal SET due_date = $3, repeat_anchor = $4, snoozed_until = NULL, \
         history = $5, done = $6, version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(pd(&out.due_date))
    .bind(out.repeat_anchor.as_ref().map(|a| serde_json::to_value(a).unwrap_or_default()))
    .bind(serde_json::to_value(&out.history).unwrap_or_default())
    .bind(out.done)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let a = actor(ctx);
    if let Some(c) = b.cost.filter(|c| *c > Decimal::ZERO) {
        sqlx::query(
            "INSERT INTO campus.vehicle_expense (tenant_id, institution_id, vehicle_id, spent_on, \
             expense_type, amount, note, actor) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(t.as_uuid())
        .bind(inst_id)
        .bind(row.vehicle_id)
        .bind(date)
        .bind(words(&row.renewal_type))
        .bind(c.round_dp(2))
        .bind("renewal")
        .bind(a.id.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    audit(
        &mut tx,
        &scope,
        "campus.vehicle_renewal.complete",
        "vehicle_renewal",
        id,
        serde_json::json!({"date": date_s, "nextDue": out.due_date, "done": out.done}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    renewal_reply(&st, t, id, inst_id, today, StatusCode::OK).await
}

fn words(s: &str) -> String {
    let w = s.replace('_', " ");
    let mut c = w.chars();
    c.next().map_or_else(String::new, |h| {
        h.to_uppercase().collect::<String>() + c.as_str()
    })
}

/// Snooze a renewal (`campus.transport.manage`).
#[utoipa::path(post, path = "/api/v1/campus/vehicle-renewals/{id}/snooze", operation_id = "campus_transport_vehicle_renewal_snooze", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid)), request_body = SnoozeBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = VehicleRenewalView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn renewal_snooze(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<SnoozeBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let (_, inst_id) = lock_renewal(&mut tx, t, id).await?;
    let inst = InstitutionId(inst_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    sqlx::query(
        "UPDATE campus.vehicle_renewal SET snoozed_until = $3, version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(b.until)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let a = actor(ctx);
    audit(
        &mut tx,
        &scope,
        "campus.vehicle_renewal.snooze",
        "vehicle_renewal",
        id,
        serde_json::json!({"until": b.until}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let today = business_today(&st, &scope, inst).await?;
    renewal_reply(&st, t, id, inst_id, today, StatusCode::OK).await
}

// ------------------------------------------------------------------ board + vehicle

/// One line of the maintenance board.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MaintenanceBoardRow {
    /// `service | renewal`.
    pub kind: String,
    pub id: Uuid,
    pub vehicle_id: Uuid,
    pub registration_no: String,
    /// The task, or the renewal's label / type.
    pub title: String,
    pub status: String,
    pub next_date: Option<String>,
    pub next_meter: Option<f64>,
    pub days_left: Option<i64>,
    pub meter_left: Option<f64>,
    pub version: i64,
}

/// The maintenance board.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MaintenanceBoard {
    pub as_of: NaiveDate,
    pub overdue: usize,
    pub due_soon: usize,
    pub items: Vec<MaintenanceBoardRow>,
}

/// Board filter.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct BoardQuery {
    /// `overdue | due_soon | upcoming | snoozed`; default every status but upcoming.
    pub status: Option<String>,
}

fn rank(s: &str) -> u8 {
    match s {
        "overdue" => 0,
        "due_soon" => 1,
        "snoozed" => 2,
        _ => 3,
    }
}

/// Service reminders and renewals of every vehicle (`campus.read`): overdue first, then due
/// soon, then snoozed, nearest first.
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/fleet-maintenance", operation_id = "campus_transport_maintenance_board", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), BoardQuery), security(("session_cookie" = [])),
    responses((status = 200, body = MaintenanceBoard), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn board(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<BoardQuery>,
) -> ApiResult<Json<MaintenanceBoard>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let t = ctx.scope.tenant_id;
    let today = business_today(&st, &scope, inst).await?;
    let ts = today.format("%Y-%m-%d").to_string();
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let due = due_of(&settings_of(&mut conn, t, id).await?);
    let mut items = Vec::new();
    for r in reminders_where(&mut conn, t, "r.institution_id", id).await? {
        if !r.active {
            continue;
        }
        let s = m::reminder_state(&r.rule, r.vehicle_meter, &ts, due);
        items.push(MaintenanceBoardRow {
            kind: "service".into(),
            id: r.id,
            vehicle_id: r.vehicle_id,
            registration_no: r.registration_no,
            title: r.task,
            status: s.status,
            next_date: s.next_date,
            next_meter: s.next_meter,
            days_left: s.days_left,
            meter_left: s.meter_left,
            version: r.version,
        });
    }
    for r in renewals_where(&mut conn, t, "r.institution_id", id).await? {
        if r.rule.done {
            continue;
        }
        let (status, days_left) = m::renewal_state(&r.rule, &ts, due);
        items.push(MaintenanceBoardRow {
            kind: "renewal".into(),
            id: r.id,
            vehicle_id: r.vehicle_id,
            registration_no: r.registration_no,
            title: r.label.unwrap_or_else(|| words(&r.renewal_type)),
            status,
            next_date: r.rule.due_date,
            next_meter: None,
            days_left,
            meter_left: None,
            version: r.version,
        });
    }
    match q.status.as_deref() {
        Some(s) => items.retain(|r| r.status == s),
        None => items.retain(|r| r.status != "upcoming"),
    }
    items.sort_by_key(|r| (rank(&r.status), r.days_left.unwrap_or(i64::MAX)));
    let overdue = items.iter().filter(|r| r.status == "overdue").count();
    let due_soon = items.iter().filter(|r| r.status == "due_soon").count();
    Ok(Json(MaintenanceBoard {
        as_of: today,
        overdue,
        due_soon,
        items,
    }))
}

/// One service done.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ServiceEntryView {
    pub id: Uuid,
    pub serviced_on: NaiveDate,
    #[schema(value_type = Option<String>)]
    pub meter: Option<Decimal>,
    pub description: String,
    pub vendor: Option<String>,
    #[schema(value_type = String, example = "3450.00")]
    pub amount: Decimal,
    pub reminder_ids: Vec<Uuid>,
}

/// A vehicle's maintenance file.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VehicleMaintenance {
    pub vehicle_id: Uuid,
    pub registration_no: String,
    pub meter: Option<f64>,
    pub meter_unit: String,
    pub fuel_capacity: Option<f64>,
    pub reminders: Vec<ServiceReminderView>,
    pub renewals: Vec<VehicleRenewalView>,
    /// Latest 50 services, newest first.
    pub services: Vec<ServiceEntryView>,
}

async fn vehicle_maintenance(
    conn: &mut PgConnection,
    t: TenantId,
    id: Uuid,
    today: NaiveDate,
) -> Result<VehicleMaintenance, KernelError> {
    let v = vehicle_of(conn, t, id).await?;
    let due = due_of(&settings_of(conn, t, v.institution).await?);
    let ts = today.format("%Y-%m-%d").to_string();
    let reminders = reminders_where(conn, t, "r.vehicle_id", id)
        .await?
        .iter()
        .map(|r| reminder_view(r, &ts, due))
        .collect();
    let renewals = renewals_where(conn, t, "r.vehicle_id", id)
        .await?
        .iter()
        .map(|r| renewal_view(r, &ts, due))
        .collect();
    let services = sqlx::query_as::<_, ServiceEntryView>(
        "SELECT id, serviced_on, meter, description, vendor, amount, reminder_ids FROM \
         campus.vehicle_service_entry WHERE tenant_id = $1 AND vehicle_id = $2 \
         ORDER BY serviced_on DESC, created_at DESC LIMIT 50",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(VehicleMaintenance {
        vehicle_id: id,
        registration_no: v.registration_no,
        meter: v.meter,
        meter_unit: v.meter_unit,
        fuel_capacity: v.fuel_capacity,
        reminders,
        renewals,
        services,
    })
}

/// A vehicle's reminders, renewals and service history (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/vehicles/{id}/maintenance", operation_id = "campus_transport_vehicle_maintenance_get", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = VehicleMaintenance), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn vehicle_get(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<VehicleMaintenance>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v = vehicle_of(&mut conn, t, id).await?;
    let inst = InstitutionId(v.institution);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    Ok(Json(vehicle_maintenance(&mut conn, t, id, today).await?))
}

// ------------------------------------------------------------------ service entries

/// Record a service.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServiceEntryCreate {
    /// Default today.
    pub serviced_on: Option<NaiveDate>,
    #[schema(value_type = Option<String>, example = "48250")]
    pub meter: Option<Decimal>,
    #[schema(example = "Oil change, air filter")]
    pub description: String,
    pub vendor: Option<String>,
    /// ₹.
    #[schema(value_type = String, example = "3450.00")]
    pub amount: Decimal,
    /// Service reminders of this vehicle the work completes.
    #[serde(default)]
    pub reminder_ids: Vec<Uuid>,
}

/// Record a service (`campus.transport.manage`; Idempotency-Key): completes the named
/// reminders (graded on time against the meter when the work was done), and records the meter.
#[utoipa::path(post, path = "/api/v1/campus/vehicles/{id}/service-entries", operation_id = "campus_transport_service_entry_create", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ServiceEntryCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = VehicleMaintenance), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn service_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ServiceEntryCreate>,
) -> ApiResult<(StatusCode, Json<VehicleMaintenance>)> {
    let ctx = auth.require(Permission::TransportManage)?;
    require_idempotency_key(&headers)?;
    let description = b.description.trim().to_owned();
    if description.is_empty() || description.chars().count() > 500 {
        return Err(invalid("description", "required", "1–500 characters").into());
    }
    if b.amount < Decimal::ZERO || b.meter.is_some_and(|x| x < Decimal::ZERO) {
        return Err(invalid("amount", "negative", "amount and meter are ≥ 0").into());
    }
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let v = vehicle_of(&mut tx, t, id).await?;
    let inst = InstitutionId(v.institution);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let on = b.serviced_on.unwrap_or(today);
    let a = actor(ctx);
    let eid: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.vehicle_service_entry (tenant_id, institution_id, vehicle_id, serviced_on, \
         meter, description, vendor, amount, reminder_ids, actor) VALUES ($1, $2, $3, $4, $5, $6, $7, \
         $8, $9, $10) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(v.institution)
    .bind(id)
    .bind(on)
    .bind(b.meter.map(|x| x.round_dp(1)))
    .bind(&description)
    .bind(b.vendor.as_deref().map(str::trim).filter(|x| !x.is_empty()))
    .bind(b.amount.round_dp(2))
    .bind(&b.reminder_ids)
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let due = due_of(&settings_of(&mut tx, t, v.institution).await?);
    let on_s = on.format("%Y-%m-%d").to_string();
    let entry = eid.to_string();
    for (i, rid) in b.reminder_ids.iter().enumerate() {
        let row = reminders_where(&mut tx, t, "r.id", *rid)
            .await?
            .into_iter()
            .next()
            .filter(|r| r.vehicle_id == id)
            .ok_or_else(|| {
                invalid(
                    &format!("reminderIds[{i}]"),
                    "unknown_reminder",
                    "not a reminder of this vehicle",
                )
            })?;
        let done =
            m::complete_reminder(&row.rule, &on_s, b.meter.map(f), Some(&entry), v.meter, due);
        sqlx::query(
            "UPDATE campus.vehicle_service_reminder SET last_date = $3, last_meter = $4, next_date = $5, \
             next_meter = $6, snoozed_until = $7, history = $8, version = version + 1, updated_at = now() \
             WHERE id = $1 AND tenant_id = $2",
        )
        .bind(rid)
        .bind(t.as_uuid())
        .bind(pd(&done.last_date))
        .bind(done.last_meter.map(dec1))
        .bind(pd(&done.next_date))
        .bind(done.next_meter.map(dec1))
        .bind(pd(&done.snoozed_until))
        .bind(serde_json::to_value(&done.history).unwrap_or_default())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    if let Some(meter) = b.meter {
        record_meter(
            &mut tx,
            t,
            &v,
            id,
            on,
            meter.round_dp(1),
            "service",
            a.id.as_deref(),
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "campus.vehicle_service_entry.create",
        "vehicle_service_entry",
        eid,
        serde_json::json!({"vehicle": v.registration_no, "amount": b.amount.to_string(),
            "reminders": b.reminder_ids.len()}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok((
        StatusCode::CREATED,
        Json(vehicle_maintenance(&mut conn, t, id, today).await?),
    ))
}

// ------------------------------------------------------------------ fuel

/// A fill-up or charging session.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FuelEntryCreate {
    pub filled_on: Option<NaiveDate>,
    /// `HH:MM`.
    pub filled_time: Option<String>,
    #[schema(value_type = Option<String>, example = "48250")]
    pub meter: Option<Decimal>,
    #[schema(value_type = String, example = "42.5")]
    pub volume: Decimal,
    /// ₹ per unit; the total is volume × price unless `total` is given.
    #[schema(value_type = Option<String>, example = "94.72")]
    pub price: Option<Decimal>,
    #[schema(value_type = Option<String>, example = "4025.60")]
    pub total: Option<Decimal>,
    /// The tank was not filled.
    #[serde(default)]
    pub partial: bool,
    /// A fill-up was missed before this one: start a new measurement.
    #[serde(default)]
    pub reset: bool,
    /// `fuel | charge`.
    pub kind: Option<String>,
    pub vendor: Option<String>,
}

/// A fuel entry with its derived values.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FuelEntryView {
    pub id: Uuid,
    pub filled_on: NaiveDate,
    pub filled_time: Option<String>,
    pub meter: Option<f64>,
    pub volume: f64,
    /// ₹.
    #[schema(value_type = String)]
    pub total: Decimal,
    pub unit_price: Option<f64>,
    pub partial: bool,
    pub reset: bool,
    pub kind: String,
    pub vendor: Option<String>,
    pub derived: m::FuelDerived,
}

/// A vehicle's fuel log.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FuelLog {
    pub meter_unit: String,
    pub volume_unit: String,
    /// `L/100 km` for km with litres; distance per volume otherwise.
    pub economy_unit: String,
    pub fuel: m::FuelSummary,
    pub charge: m::FuelSummary,
    /// Newest first.
    pub items: Vec<FuelEntryView>,
    /// Checks on the entry just recorded (create only).
    pub warnings: Vec<String>,
}

fn economy_unit(meter: &str, volume: &str) -> String {
    match (meter, volume) {
        ("hr", v) => format!("{v}/hr"),
        ("km", "L") => "L/100 km".into(),
        ("km", "kWh") => "kWh/100 km".into(),
        (m, v) => format!("{m}/{v}"),
    }
}

async fn fuel_log(
    conn: &mut PgConnection,
    t: TenantId,
    id: Uuid,
    warnings: Vec<String>,
) -> Result<FuelLog, KernelError> {
    let v = vehicle_of(conn, t, id).await?;
    let s = settings_of(conn, t, v.institution).await?;
    let rows: Vec<(
        Uuid,
        NaiveDate,
        Option<String>,
        Option<Decimal>,
        Decimal,
        Decimal,
        bool,
        bool,
        String,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT id, filled_on, filled_time, meter, volume, total, partial, reset, kind, vendor \
             FROM campus.vehicle_fuel_entry WHERE tenant_id = $1 AND vehicle_id = $2 \
             ORDER BY filled_on DESC, filled_time DESC NULLS LAST, created_at DESC",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let entry = |r: &(
        Uuid,
        NaiveDate,
        Option<String>,
        Option<Decimal>,
        Decimal,
        Decimal,
        bool,
        bool,
        String,
        Option<String>,
    )| FuelEntry {
        id: r.0.to_string(),
        date: r.1.format("%Y-%m-%d").to_string(),
        time: r.2.clone(),
        meter: fo(r.3),
        volume: f(r.4),
        total: f(r.5) * 100.0,
        partial: r.6,
        reset: r.7,
    };
    let fuel: Vec<FuelEntry> = rows.iter().filter(|r| r.8 != "charge").map(entry).collect();
    let charge: Vec<FuelEntry> = rows.iter().filter(|r| r.8 == "charge").map(entry).collect();
    let df = m::fuel_derived(&fuel, &v.meter_unit, &s.volume_unit);
    let dc = m::fuel_derived(&charge, &v.meter_unit, "kWh");
    let items = rows
        .iter()
        .map(|r| {
            let key = r.0.to_string();
            let derived = df
                .get(&key)
                .or_else(|| dc.get(&key))
                .copied()
                .unwrap_or_default();
            FuelEntryView {
                id: r.0,
                filled_on: r.1,
                filled_time: r.2.clone(),
                meter: fo(r.3),
                volume: f(r.4),
                total: r.5,
                unit_price: m::unit_price(f(r.5) * 100.0, f(r.4)),
                partial: r.6,
                reset: r.7,
                kind: r.8.clone(),
                vendor: r.9.clone(),
                derived,
            }
        })
        .collect();
    Ok(FuelLog {
        economy_unit: economy_unit(&v.meter_unit, &s.volume_unit),
        fuel: m::fuel_summary(&fuel, &df, &v.meter_unit, &s.volume_unit),
        charge: m::fuel_summary(&charge, &dc, &v.meter_unit, "kWh"),
        meter_unit: v.meter_unit,
        volume_unit: s.volume_unit,
        items,
        warnings,
    })
}

/// A vehicle's fuel log with economy between full fills (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/vehicles/{id}/fuel-entries", operation_id = "campus_transport_fuel_entry_list", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = FuelLog), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn fuel_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<FuelLog>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v = vehicle_of(&mut conn, t, id).await?;
    let inst = InstitutionId(v.institution);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    Ok(Json(fuel_log(&mut conn, t, id, Vec::new()).await?))
}

/// Record a fill-up (`campus.transport.manage`; Idempotency-Key): warns when the meter is
/// lower than an earlier entry or the volume is more than the tank holds; records the meter.
#[utoipa::path(post, path = "/api/v1/campus/vehicles/{id}/fuel-entries", operation_id = "campus_transport_fuel_entry_create", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = FuelEntryCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = FuelLog), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn fuel_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FuelEntryCreate>,
) -> ApiResult<(StatusCode, Json<FuelLog>)> {
    let ctx = auth.require(Permission::TransportManage)?;
    require_idempotency_key(&headers)?;
    let kind = b.kind.clone().unwrap_or_else(|| "fuel".into());
    if !matches!(kind.as_str(), "fuel" | "charge") {
        return Err(invalid("kind", "invalid", "fuel or charge").into());
    }
    if b.volume <= Decimal::ZERO {
        return Err(invalid("volume", "invalid", "> 0").into());
    }
    if b.meter.is_some_and(|x| x < Decimal::ZERO) {
        return Err(invalid("meter", "negative", "≥ 0").into());
    }
    let total = match (b.total, b.price) {
        (Some(t), _) if t >= Decimal::ZERO => t.round_dp(2),
        (None, Some(p)) if p >= Decimal::ZERO => rupees(m::fuel_total(f(b.volume), f(p))),
        _ => {
            return Err(invalid(
                "total",
                "required",
                "give the total or the unit price (≥ 0)",
            )
            .into())
        }
    };
    if let Some(time) = &b.filled_time {
        let ok = time.len() == 5
            && time.as_bytes()[2] == b':'
            && time[..2].parse::<u8>().is_ok_and(|h| h < 24)
            && time[3..].parse::<u8>().is_ok_and(|m| m < 60);
        if !ok {
            return Err(invalid("filledTime", "invalid", "HH:MM").into());
        }
    }
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let v = vehicle_of(&mut tx, t, id).await?;
    let inst = InstitutionId(v.institution);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let on = match b.filled_on {
        Some(d) => d,
        None => business_today(&st, &scope, inst).await?,
    };
    let mut warnings = Vec::new();
    if let Some(meter) = b.meter {
        let earlier: Option<Decimal> = sqlx::query_scalar(
            "SELECT max(meter) FROM campus.vehicle_fuel_entry WHERE tenant_id = $1 AND vehicle_id = $2 \
             AND (filled_on < $3 OR (filled_on = $3 AND ($4::text IS NULL OR filled_time IS NULL \
             OR filled_time <= $4)))",
        )
        .bind(t.as_uuid())
        .bind(id)
        .bind(on)
        .bind(b.filled_time.as_deref())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if let Some(e) = earlier.filter(|e| *e > Decimal::ZERO && meter < *e) {
            warnings.push(format!(
                "The meter is lower than an earlier fuel entry ({}).",
                e.normalize()
            ));
        }
    }
    if let Some(cap) = v.fuel_capacity.filter(|c| kind != "charge" && *c > 0.0) {
        if f(b.volume) > cap * 1.05 {
            warnings.push(format!(
                "The volume is more than this vehicle's tank holds ({cap:.1})."
            ));
        }
    }
    let a = actor(ctx);
    let fid: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.vehicle_fuel_entry (tenant_id, institution_id, vehicle_id, filled_on, \
         filled_time, meter, volume, total, partial, reset, kind, vendor, actor) VALUES ($1, $2, $3, \
         $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(v.institution)
    .bind(id)
    .bind(on)
    .bind(b.filled_time.as_deref())
    .bind(b.meter.map(|x| x.round_dp(1)))
    .bind(b.volume.round_dp(3))
    .bind(total)
    .bind(b.partial)
    .bind(b.reset)
    .bind(&kind)
    .bind(b.vendor.as_deref().map(str::trim).filter(|x| !x.is_empty()))
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if let Some(meter) = b.meter {
        record_meter(
            &mut tx,
            t,
            &v,
            id,
            on,
            meter.round_dp(1),
            "fuel",
            a.id.as_deref(),
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "campus.vehicle_fuel_entry.create",
        "vehicle_fuel_entry",
        fid,
        serde_json::json!({"vehicle": v.registration_no, "volume": b.volume.to_string(),
            "total": total.to_string(), "warnings": warnings}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok((
        StatusCode::CREATED,
        Json(fuel_log(&mut conn, t, id, warnings).await?),
    ))
}

// ------------------------------------------------------------------ expenses + costs

/// Another operating cost (tolls, parking, cleaning, fines…).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VehicleExpenseCreate {
    pub spent_on: Option<NaiveDate>,
    #[schema(example = "Tolls")]
    pub expense_type: String,
    /// ₹.
    #[schema(value_type = String, example = "450.00")]
    pub amount: Decimal,
    pub note: Option<String>,
}

/// The id of a created record.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FleetCreated {
    pub id: Uuid,
}

/// Record a vehicle expense (`campus.transport.manage`; Idempotency-Key).
#[utoipa::path(post, path = "/api/v1/campus/vehicles/{id}/expenses", operation_id = "campus_transport_vehicle_expense_create", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = VehicleExpenseCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = FleetCreated), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn expense_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<VehicleExpenseCreate>,
) -> ApiResult<(StatusCode, Json<FleetCreated>)> {
    let ctx = auth.require(Permission::TransportManage)?;
    require_idempotency_key(&headers)?;
    let kind = b.expense_type.trim().to_owned();
    if kind.is_empty() || kind.chars().count() > 60 {
        return Err(invalid("expenseType", "required", "1–60 characters").into());
    }
    if b.amount < Decimal::ZERO {
        return Err(invalid("amount", "negative", "≥ 0").into());
    }
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let v = vehicle_of(&mut tx, t, id).await?;
    let inst = InstitutionId(v.institution);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let on = match b.spent_on {
        Some(d) => d,
        None => business_today(&st, &scope, inst).await?,
    };
    let a = actor(ctx);
    let eid: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.vehicle_expense (tenant_id, institution_id, vehicle_id, spent_on, \
         expense_type, amount, note, actor) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(v.institution)
    .bind(id)
    .bind(on)
    .bind(&kind)
    .bind(b.amount.round_dp(2))
    .bind(b.note.as_deref())
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(&mut tx, &scope, "campus.vehicle_expense.create", "vehicle_expense", eid,
        serde_json::json!({"vehicle": v.registration_no, "type": kind, "amount": b.amount.to_string()}), &a)
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(FleetCreated { id: eid })))
}

/// Cost report range.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct CostQuery {
    /// Inclusive; default the first of this month a year ago.
    pub from: Option<NaiveDate>,
    /// Inclusive; default today.
    pub to: Option<NaiveDate>,
}

/// Operating cost of one vehicle.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FleetCostRow {
    pub vehicle_id: Uuid,
    pub registration_no: String,
    #[schema(value_type = String)]
    pub fuel: Decimal,
    #[schema(value_type = String)]
    pub service: Decimal,
    #[schema(value_type = String)]
    pub other: Decimal,
    #[schema(value_type = String)]
    pub total: Decimal,
    /// Meter distance (or hours) in the range.
    pub usage: Option<f64>,
    pub meter_unit: String,
    /// ₹ per km (or hour), when usage is known.
    #[schema(value_type = Option<String>)]
    pub cost_per_meter: Option<Decimal>,
}

/// The fleet cost report.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FleetCosts {
    pub from: NaiveDate,
    pub to: NaiveDate,
    #[schema(value_type = String)]
    pub total: Decimal,
    pub items: Vec<FleetCostRow>,
}

/// Fuel, service and other costs per vehicle with usage and ₹ per km (`campus.read`),
/// costliest first.
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/fleet-costs", operation_id = "campus_transport_fleet_costs", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), CostQuery), security(("session_cookie" = [])),
    responses((status = 200, body = FleetCosts), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn costs(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<CostQuery>,
) -> ApiResult<Json<FleetCosts>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let t = ctx.scope.tenant_id;
    let today = business_today(&st, &scope, inst).await?;
    let to = q.to.unwrap_or(today);
    let from = q.from.unwrap_or_else(|| {
        let first = to.with_day(1).unwrap_or(to);
        first
            .checked_sub_months(chrono::Months::new(12))
            .unwrap_or(first)
    });
    if from > to {
        return Err(invalid("from", "after_to", "from must not be after to").into());
    }
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let lines: Vec<(Uuid, NaiveDate, String, Decimal)> = sqlx::query_as(
        "SELECT vehicle_id, filled_on, 'fuel', total FROM campus.vehicle_fuel_entry WHERE tenant_id = $1 \
         AND institution_id = $2 UNION ALL SELECT vehicle_id, serviced_on, 'service', amount FROM \
         campus.vehicle_service_entry WHERE tenant_id = $1 AND institution_id = $2 UNION ALL \
         SELECT vehicle_id, spent_on, 'other', amount FROM campus.vehicle_expense WHERE tenant_id = $1 \
         AND institution_id = $2",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let lines: Vec<CostLine> = lines
        .into_iter()
        .map(|(v, d, k, a)| CostLine {
            vehicle_id: v.to_string(),
            date: d.format("%Y-%m-%d").to_string(),
            kind: k,
            amount: f(a) * 100.0,
        })
        .collect();
    let readings: Vec<(Uuid, NaiveDate, Decimal, bool)> = sqlx::query_as(
        "SELECT vehicle_id, read_on, value, void FROM campus.vehicle_meter_reading WHERE tenant_id = $1 \
         AND institution_id = $2 ORDER BY read_on, created_at",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let readings: Vec<Reading> = readings
        .into_iter()
        .map(|(v, d, x, void)| Reading {
            vehicle_id: v.to_string(),
            date: d.format("%Y-%m-%d").to_string(),
            value: Some(f(x)),
            void,
        })
        .collect();
    let (fs, ts) = (
        from.format("%Y-%m-%d").to_string(),
        to.format("%Y-%m-%d").to_string(),
    );
    let by = m::costs_by_vehicle(&lines, Some(&fs), Some(&ts));
    let usage = m::usage_by_vehicle(&readings, Some(&fs), Some(&ts));
    let vehicles: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, registration_no, meter_unit FROM campus.vehicle WHERE tenant_id = $1 AND institution_id = $2",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut items: Vec<FleetCostRow> = vehicles
        .into_iter()
        .filter_map(|(vid, reg, unit)| {
            let key = vid.to_string();
            let c = by.get(&key).copied();
            let u = usage.get(&key).copied();
            if c.is_none() && u.is_none() {
                return None;
            }
            let c = c.unwrap_or_default();
            Some(FleetCostRow {
                vehicle_id: vid,
                registration_no: reg,
                fuel: rupees(c.fuel),
                service: rupees(c.service),
                other: rupees(c.other),
                total: rupees(c.total),
                usage: u,
                meter_unit: unit,
                cost_per_meter: u.filter(|u| *u > 0.0).map(|u| rupees(c.total / u)),
            })
        })
        .collect();
    items.sort_by(|a, b| {
        b.total
            .cmp(&a.total)
            .then_with(|| a.registration_no.cmp(&b.registration_no))
    });
    let total = items.iter().map(|r| r.total).sum();
    Ok(Json(FleetCosts {
        from,
        to,
        total,
        items,
    }))
}

/// Routes of fleet maintenance.
pub fn router(state: CampusState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(settings_get, settings_put))
        .routes(routes!(board))
        .routes(routes!(vehicle_get))
        .routes(routes!(meter_create))
        .routes(routes!(reminder_create))
        .routes(routes!(reminder_update))
        .routes(routes!(reminder_snooze))
        .routes(routes!(renewal_create))
        .routes(routes!(renewal_complete))
        .routes(routes!(renewal_snooze))
        .routes(routes!(service_create))
        .routes(routes!(fuel_list, fuel_create))
        .routes(routes!(expense_create))
        .routes(routes!(costs))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(words("road_tax"), "Road tax");
        assert_eq!(economy_unit("km", "L"), "L/100 km");
        assert_eq!(rupees(12_345.0).to_string(), "123.45");
        assert_eq!(rank("overdue"), 0);
    }
}
