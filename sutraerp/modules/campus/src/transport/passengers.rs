//! Passengers and passes (L3-G-099 transport request → active allocation (route + stop, seat
//! capacity), G-100 change → effective route / stop reassignment, G-101 withdrawal → service
//! closure, G-113 pass issuance with the `TRANSPORT-PASS` fee through `CampusChargePort`, G-114
//! renewal, G-115 lost / damaged pass → replacement (old pass `replaced`), G-116 suspension /
//! restoration); transport complaints → service desk tickets (source `transport`).

use super::domain::{pass_status_on_issue, seats_available, PassStatus, TransportAllocationStatus};
use crate::hostel::common::{
    apply, complaints_of, created, ensure_version, expected, fetch, head, invalid, list_of, money,
    optional_text, person_in, person_kind, raise_charge, reply, status_filter, step, Agg,
    ChargeAsk, ComplaintCreate, ComplaintList, ComplaintView, Move, ReasonBody, StatusFilter,
};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::actor;
use crate::shared::me::PersonRef;
use crate::shared::numbering::{self, kind};
use crate::shared::scope::{institution_scope, tenant_scope};
use crate::shared::tickets::{open_ticket, NewTicket};
use crate::shared::time::business_today;
use crate::shared::{Actor, CampusState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Row};
use sutra_contracts::campus::events::residence::{
    TRANSPORT_ALLOCATION_ACTIVATED, TRANSPORT_ALLOCATION_ACTIVATED_SCHEMA, TRANSPORT_PASS_ISSUED,
    TRANSPORT_PASS_ISSUED_SCHEMA, TRANSPORT_PASS_SUSPENDED, TRANSPORT_PASS_SUSPENDED_SCHEMA,
};
use sutra_contracts::campus::events::StatusChangedV1;
use sutra_contracts::campus::status::{PersonKind, TicketCategory, TicketSource};
use sutra_contracts::finance::ports::CampusChargeCategory;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use uuid::Uuid;

/// Fee code of the transport pass charge (contract §7).
pub const TRANSPORT_PASS: &str = "TRANSPORT-PASS";

pub(crate) const T_ALLOCATION: Agg = Agg {
    table: "transport_allocation",
    entity: "transport allocation",
    number: "person_number",
    subject: "subject_ref",
};

pub(crate) const PASS: Agg = Agg {
    table: "transport_pass",
    entity: "transport pass",
    number: "number",
    subject: "subject_ref",
};

// ------------------------------------------------------------------ views

/// A passenger allocation (route + boarding stop).
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct TransportAllocationView {
    pub id: Uuid,
    pub institution_id: Uuid,
    /// `student | staff`.
    pub person_kind: String,
    pub person_id: Uuid,
    pub person_number: String,
    pub person_name: String,
    pub route_id: Uuid,
    pub route_code: String,
    pub stop_id: Uuid,
    pub stop_code: String,
    pub stop_name: String,
    /// `self_service | office`.
    pub channel: String,
    pub start_date: NaiveDate,
    pub end_date: Option<NaiveDate>,
    /// `requested | active | changed | withdrawn`.
    pub status: String,
    pub status_reason: Option<String>,
    /// The stop / route was suspended or dropped: the office must adjust (G-095 / 097).
    pub flagged: bool,
    pub flag_reason: Option<String>,
    pub previous_id: Option<Uuid>,
    pub pass_id: Option<Uuid>,
    pub pass_number: Option<String>,
    pub version: i64,
}

pub(crate) const T_ALLOCATION_SELECT: &str = "SELECT x.id, x.institution_id, x.person_kind, x.person_id, \
    x.person_number, x.person_name, x.route_id, r.code AS route_code, x.stop_id, s.code AS stop_code, \
    s.name AS stop_name, x.channel, x.start_date, x.end_date, x.status, x.status_reason, x.flagged, \
    x.flag_reason, x.previous_id, p.id AS pass_id, p.number AS pass_number, x.version \
    FROM campus.transport_allocation x JOIN campus.transport_route r ON r.id = x.route_id \
    JOIN campus.transport_stop s ON s.id = x.stop_id \
    LEFT JOIN campus.transport_pass p ON p.allocation_id = x.id AND p.status IN ('issued', 'active', 'suspended')";

/// Passenger allocations.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TransportAllocationList {
    pub items: Vec<TransportAllocationView>,
}

/// Allocate a passenger (office).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransportAllocationCreate {
    /// `student | staff`.
    pub person_kind: String,
    pub person_id: Uuid,
    pub route_id: Uuid,
    pub stop_id: Uuid,
    /// Default: today.
    pub start_date: Option<NaiveDate>,
}

/// Move a passenger to another stop / route.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransportAllocationChange {
    /// Default: the current route.
    pub route_id: Option<Uuid>,
    pub stop_id: Uuid,
    /// Default: today.
    pub effective_from: Option<NaiveDate>,
    pub reason: Option<String>,
}

/// A transport pass.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct TransportPassView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "TP-000001")]
    pub number: String,
    pub allocation_id: Uuid,
    pub person_kind: String,
    pub person_number: String,
    pub person_name: String,
    pub route_id: Uuid,
    pub route_code: String,
    pub valid_from: NaiveDate,
    pub valid_to: NaiveDate,
    /// `new | renewal | replacement`.
    pub kind: String,
    /// `issued | active | expired | suspended | replaced | cancelled`.
    pub status: String,
    pub status_reason: Option<String>,
    #[schema(example = "6000.00")]
    pub fee_amount: String,
    pub charge_id: Option<Uuid>,
    pub charge_number: Option<String>,
    pub replaces_id: Option<Uuid>,
    pub version: i64,
}

pub(crate) const PASS_SELECT: &str = "SELECT x.id, x.institution_id, x.number, x.allocation_id, \
    x.person_kind, x.person_number, x.person_name, x.route_id, r.code AS route_code, x.valid_from, \
    x.valid_to, x.kind, x.status, x.status_reason, x.fee_amount::text AS fee_amount, x.charge_id, \
    x.charge_number, x.replaces_id, x.version FROM campus.transport_pass x \
    JOIN campus.transport_route r ON r.id = x.route_id";

/// Transport passes.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TransportPassList {
    pub items: Vec<TransportPassView>,
}

/// Issue a pass for an active allocation.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransportPassIssue {
    pub valid_from: NaiveDate,
    pub valid_to: NaiveDate,
}

/// Renew a pass up to a new end date (a new pass from the day after the current one ends).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransportPassRenew {
    pub valid_to: NaiveDate,
}

/// Replace a lost / damaged pass.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransportPassReplace {
    pub reason: String,
    /// Optional replacement fee (students are charged).
    #[schema(example = "200.00")]
    pub fee: Option<String>,
}

// ------------------------------------------------------------------ shared logic

pub(crate) async fn t_allocation_reply(
    st: &CampusState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v: TransportAllocationView = fetch(
        &mut conn,
        T_ALLOCATION_SELECT,
        tenant,
        id,
        T_ALLOCATION.entity,
    )
    .await?;
    Ok(reply(status, &v, v.version))
}

pub(crate) async fn pass_reply(
    st: &CampusState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v: TransportPassView = fetch(&mut conn, PASS_SELECT, tenant, id, PASS.entity).await?;
    Ok(reply(status, &v, v.version))
}

/// The route's seats and current version, after locking the route row (serialises seat checks).
struct RouteSeats {
    institution: Uuid,
    status: String,
    capacity: i32,
    version_id: Uuid,
    fare: Decimal,
}

async fn lock_route_seats(
    conn: &mut PgConnection,
    tenant: TenantId,
    route: Uuid,
) -> Result<RouteSeats, KernelError> {
    let r = sqlx::query(
        "SELECT r.institution_id, r.status, v.capacity, v.id AS version_id, v.fare \
         FROM campus.transport_route r JOIN campus.route_version v ON v.id = r.current_version_id \
         WHERE r.id = $1 AND r.tenant_id = $2 FOR UPDATE OF r",
    )
    .bind(route)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| {
        invalid(
            "routeId",
            "unknown_route",
            "no activated route with this id",
        )
    })?;
    Ok(RouteSeats {
        institution: r.try_get("institution_id").map_err(map_db_error)?,
        status: r.try_get("status").map_err(map_db_error)?,
        capacity: r.try_get("capacity").map_err(map_db_error)?,
        version_id: r.try_get("version_id").map_err(map_db_error)?,
        fare: r.try_get("fare").map_err(map_db_error)?,
    })
}

/// 409 / 422 unless the route is active in `institution` and `stop` is an active stop of its
/// current version.
async fn check_route_stop(
    conn: &mut PgConnection,
    seats: &RouteSeats,
    institution: InstitutionId,
    stop: Uuid,
) -> Result<(), KernelError> {
    if seats.institution != institution.as_uuid() {
        return Err(invalid(
            "routeId",
            "unknown_route",
            "no route with this id in the institution",
        ));
    }
    if seats.status != "active" {
        return Err(KernelError::Conflict(format!(
            "the route is {}; passengers join active routes only",
            seats.status
        )));
    }
    let stop_status: Option<String> = sqlx::query_scalar(
        "SELECT s.status FROM campus.route_stop rs JOIN campus.transport_stop s ON s.id = rs.stop_id \
         WHERE rs.route_version_id = $1 AND rs.stop_id = $2",
    )
    .bind(seats.version_id)
    .bind(stop)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    match stop_status.as_deref() {
        None => Err(invalid(
            "stopId",
            "not_on_route",
            "the stop is not on the route's current version",
        )),
        Some("active") => Ok(()),
        Some(s) => Err(KernelError::Conflict(format!("the stop is {s}"))),
    }
}

async fn used_seats(conn: &mut PgConnection, route: Uuid) -> Result<i64, KernelError> {
    sqlx::query_scalar(
        "SELECT count(*) FROM campus.transport_allocation WHERE route_id = $1 AND status = 'active'",
    )
    .bind(route)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)
}

/// Activates a locked `requested` allocation when its route has a free seat; `false` = still
/// waiting for a seat.
async fn try_activate(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    alloc: Uuid,
    actor: &Actor,
) -> Result<bool, KernelError> {
    let t = scope.tenant_id;
    let route: Uuid =
        sqlx::query_scalar("SELECT route_id FROM campus.transport_allocation WHERE id = $1")
            .bind(alloc)
            .fetch_one(&mut *conn)
            .await
            .map_err(map_db_error)?;
    let seats = lock_route_seats(conn, t, route).await?;
    let mut h = head(conn, &T_ALLOCATION, t, alloc, true).await?;
    step(&h, TransportAllocationStatus::Active)?;
    if !seats_available(seats.capacity, used_seats(conn, route).await?, 1) {
        return Ok(false);
    }
    apply(
        conn,
        scope,
        &T_ALLOCATION,
        &mut h,
        Move {
            to: TransportAllocationStatus::Active.as_str(),
            event: Some((
                TRANSPORT_ALLOCATION_ACTIVATED,
                TRANSPORT_ALLOCATION_ACTIVATED_SCHEMA,
            )),
            action: "campus.transport_allocation.activate",
            reason: None,
            process_instance_id: None,
        },
        actor,
    )
    .await?;
    Ok(true)
}

/// Records a passenger request and activates it when a seat is free (otherwise it stays
/// `requested`). Returns the allocation id.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn request_allocation(
    st: &CampusState,
    ctx: &AuthContext,
    institution: InstitutionId,
    person: &PersonRef,
    route: Uuid,
    stop: Uuid,
    start: Option<NaiveDate>,
    channel: &'static str,
) -> Result<Uuid, KernelError> {
    let t = ctx.scope.tenant_id;
    let scope = institution_scope(ctx, institution);
    require_institution_writable(st, &scope, institution).await?;
    let start = match start {
        Some(d) => d,
        None => business_today(st, &scope, institution).await?,
    };
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let seats = lock_route_seats(&mut tx, t, route).await?;
    check_route_stop(&mut tx, &seats, institution, stop).await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.transport_allocation (tenant_id, institution_id, person_kind, person_id, \
         person_number, person_name, subject_ref, route_id, stop_id, channel, start_date) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(institution.as_uuid())
    .bind(person.kind.as_str())
    .bind(person.id)
    .bind(&person.number)
    .bind(&person.display_name)
    .bind(person.subject_ref.as_deref())
    .bind(route)
    .bind(stop)
    .bind(channel)
    .bind(start)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    created(
        &mut tx,
        &scope,
        &T_ALLOCATION,
        id,
        TransportAllocationStatus::Requested.as_str(),
        "campus.transport_allocation.request",
        serde_json::json!({ "person": person.number, "route": route, "stop": stop, "channel": channel }),
        &a,
    )
    .await?;
    try_activate(&mut tx, &scope, id, &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(id)
}

/// What a new pass is.
struct NewPass {
    allocation: Uuid,
    valid_from: NaiveDate,
    valid_to: NaiveDate,
    kind: &'static str,
    replaces: Option<Uuid>,
    fee: Decimal,
}

/// Issues a pass in the caller's transaction (number, history, `transport_pass.issued`) and
/// raises the fee for a student through `CampusChargePort` (idempotent on the pass id).
async fn issue_pass(
    st: &CampusState,
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    p: NewPass,
    actor: &Actor,
) -> Result<Uuid, KernelError> {
    let t = scope.tenant_id;
    let inst = crate::hostel::common::scope_institution(scope)?;
    let today = business_today(st, scope, inst).await?;
    let status = pass_status_on_issue(p.valid_from, today);
    let number = numbering::next(conn, t, inst, kind::TRANSPORT_PASS).await?;
    let id = Uuid::now_v7();
    let r = sqlx::query(
        "INSERT INTO campus.transport_pass (id, tenant_id, institution_id, number, allocation_id, \
         person_kind, person_id, person_number, person_name, subject_ref, route_id, valid_from, \
         valid_to, kind, status, fee_amount, replaces_id) \
         SELECT $1, tenant_id, institution_id, $2, id, person_kind, person_id, person_number, \
         person_name, subject_ref, route_id, $3, $4, $5, $6, $7, $8 FROM campus.transport_allocation \
         WHERE id = $9 RETURNING person_kind, person_id, person_name, subject_ref",
    )
    .bind(id)
    .bind(&number)
    .bind(p.valid_from)
    .bind(p.valid_to)
    .bind(p.kind)
    .bind(status.as_str())
    .bind(p.fee)
    .bind(p.replaces)
    .bind(p.allocation)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let person_kind: String = r.try_get("person_kind").map_err(map_db_error)?;
    let person_id: Uuid = r.try_get("person_id").map_err(map_db_error)?;
    let person_name: String = r.try_get("person_name").map_err(map_db_error)?;
    let subject: Option<String> = r.try_get("subject_ref").map_err(map_db_error)?;
    created(
        conn,
        scope,
        &PASS,
        id,
        status.as_str(),
        "campus.transport_pass.issue",
        serde_json::json!({ "number": number, "kind": p.kind, "validFrom": p.valid_from, "validTo": p.valid_to }),
        actor,
    )
    .await?;
    crate::shared::publish::event(
        conn,
        scope,
        TRANSPORT_PASS_ISSUED,
        TRANSPORT_PASS_ISSUED_SCHEMA,
        id.to_string(),
        &StatusChangedV1 {
            aggregate_id: id,
            institution_id: inst.as_uuid(),
            from_status: None,
            to_status: status.as_str().to_owned(),
            version: 1,
            reason: Some(p.kind.to_owned()),
            process_instance_id: None,
            number: Some(number.clone()),
            subject_ref: subject,
        },
        actor,
    )
    .await?;
    if let Some(charge) = raise_charge(
        st,
        scope,
        ChargeAsk {
            institution: inst,
            person: (person_kind.as_str(), person_id),
            fee_code: TRANSPORT_PASS,
            category: CampusChargeCategory::Transport,
            description: format!("Transport pass {number} ({}) — {person_name}", p.kind),
            amount: p.fee,
            due_date: Some(p.valid_from.max(today)),
            source_id: id,
        },
        actor,
    )
    .await?
    {
        sqlx::query(
            "UPDATE campus.transport_pass SET charge_id = $1, charge_number = $2 WHERE id = $3",
        )
        .bind(charge.charge_id.as_uuid())
        .bind(&charge.charge_number)
        .bind(id)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    Ok(id)
}

/// Moves a locked pass (`head`) to `to` with the given action / event.
async fn pass_move(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    pass: Uuid,
    to: PassStatus,
    action: &'static str,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let mut h = head(conn, &PASS, scope.tenant_id, pass, true).await?;
    step(&h, to)?;
    apply(
        conn,
        scope,
        &PASS,
        &mut h,
        Move {
            to: to.as_str(),
            event: if to == PassStatus::Suspended {
                Some((TRANSPORT_PASS_SUSPENDED, TRANSPORT_PASS_SUSPENDED_SCHEMA))
            } else {
                None
            },
            action,
            reason,
            process_instance_id: None,
        },
        actor,
    )
    .await?;
    Ok(())
}

/// The live pass of an allocation, if any.
async fn live_pass(conn: &mut PgConnection, allocation: Uuid) -> Result<Option<Uuid>, KernelError> {
    sqlx::query_scalar(
        "SELECT id FROM campus.transport_pass WHERE allocation_id = $1 \
         AND status IN ('issued', 'active', 'suspended') FOR UPDATE",
    )
    .bind(allocation)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)
}

/// Opens a transport complaint for an allocation (source `transport`).
pub(crate) async fn open_transport_complaint(
    st: &CampusState,
    ctx: &AuthContext,
    allocation: Uuid,
    b: &ComplaintCreate,
) -> Result<ComplaintView, KernelError> {
    let t = ctx.scope.tenant_id;
    let priority = b.priority()?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let al: TransportAllocationView = fetch(
        &mut tx,
        T_ALLOCATION_SELECT,
        t,
        allocation,
        T_ALLOCATION.entity,
    )
    .await?;
    let inst = InstitutionId(al.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(st, &scope, inst).await?;
    let subject: Option<String> =
        sqlx::query_scalar("SELECT subject_ref FROM campus.transport_allocation WHERE id = $1")
            .bind(allocation)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
    let person = PersonRef {
        kind: PersonKind::parse(&al.person_kind).unwrap_or(PersonKind::Student),
        id: al.person_id,
        number: al.person_number.clone(),
        display_name: al.person_name.clone(),
        subject_ref: subject,
    };
    let ticket = open_ticket(
        &mut tx,
        &scope,
        NewTicket {
            institution: inst,
            category: TicketCategory::Transport,
            source: TicketSource::Transport,
            source_ref_id: Some(allocation),
            is_complaint: true,
            node_id: None,
            requester: Some(&person),
            title: &b.title,
            description: b.description.as_deref(),
            priority,
            sla_hours: None,
        },
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(ComplaintView::from(&ticket))
}

// ------------------------------------------------------------------ handlers: allocations

/// Passenger allocations of an institution (`campus.read`; `?status=`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/transport-allocations", operation_id = "campus_transport_allocation_list", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), StatusFilter), security(("session_cookie" = [])),
    responses((status = 200, body = TransportAllocationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn allocation_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StatusFilter>,
) -> ApiResult<Json<TransportAllocationList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let status = status_filter::<TransportAllocationStatus>(q.status.as_deref())?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let items = list_of(
        &mut conn,
        T_ALLOCATION_SELECT,
        ctx.scope.tenant_id,
        "institution_id",
        id,
        status,
        "r.code, x.person_number",
    )
    .await?;
    Ok(Json(TransportAllocationList { items }))
}

/// Allocate a passenger to a route + stop (`campus.transport.manage`): active when a seat is
/// free, otherwise `requested` until one frees up (`/activate`).
#[utoipa::path(post, path = "/api/v1/campus/institutions/{id}/transport-allocations", operation_id = "campus_transport_allocation_create", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = TransportAllocationCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = TransportAllocationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn allocation_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<TransportAllocationCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    require_idempotency_key(&headers)?;
    let inst = InstitutionId(id);
    let kind = person_kind(&b.person_kind)?;
    let person = person_in(&st, &tenant_scope(ctx), kind, b.person_id, inst, "personId").await?;
    let alloc = request_allocation(
        &st,
        ctx,
        inst,
        &person,
        b.route_id,
        b.stop_id,
        b.start_date,
        "office",
    )
    .await?;
    Ok(t_allocation_reply(&st, ctx.scope.tenant_id, alloc, StatusCode::CREATED).await?)
}

/// One passenger allocation (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/transport-allocations/{id}", operation_id = "campus_transport_allocation_get", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = TransportAllocationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn allocation_get(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusRead)?;
    Ok(t_allocation_reply(&st, ctx.scope.tenant_id, id, StatusCode::OK).await?)
}

/// Activate a waiting request once a seat is free (`campus.transport.manage`; 409 while full).
#[utoipa::path(post, path = "/api/v1/campus/transport-allocations/{id}/activate", operation_id = "campus_transport_allocation_activate", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the allocation")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TransportAllocationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn allocation_activate(
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
    let h = head(&mut tx, &T_ALLOCATION, t, id, false).await?;
    ensure_version(v, h.version)?;
    let scope = institution_scope(ctx, h.institution);
    if !try_activate(&mut tx, &scope, id, &a).await? {
        return Err(KernelError::Conflict("the route has no free seat".into()).into());
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(t_allocation_reply(&st, t, id, StatusCode::OK).await?)
}

/// Change the stop / route (`campus.transport.manage`; G-100): this allocation → changed (ends
/// the day before), a new active allocation starts; a live pass follows the passenger.
#[utoipa::path(post, path = "/api/v1/campus/transport-allocations/{id}/change", operation_id = "campus_transport_allocation_change", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the allocation")),
    request_body = TransportAllocationChange, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TransportAllocationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn allocation_change(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<TransportAllocationChange>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let reason = optional_text("reason", b.reason.clone(), 1000)?;
    let cur: TransportAllocationView = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        fetch(&mut conn, T_ALLOCATION_SELECT, t, id, T_ALLOCATION.entity).await?
    };
    let inst = InstitutionId(cur.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let from = match b.effective_from {
        Some(d) => d,
        None => business_today(&st, &scope, inst).await?,
    };
    if from <= cur.start_date {
        return Err(invalid(
            "effectiveFrom",
            "too_early",
            "the change starts after the current allocation started",
        )
        .into());
    }
    let route = b.route_id.unwrap_or(cur.route_id);
    if route == cur.route_id && b.stop_id == cur.stop_id {
        return Err(invalid("stopId", "unchanged", "the passenger already boards here").into());
    }
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    // Lock both routes in a stable order before the seat check.
    if route != cur.route_id {
        let (first, second) = if route < cur.route_id {
            (route, cur.route_id)
        } else {
            (cur.route_id, route)
        };
        lock_route_seats(&mut tx, t, first).await?;
        lock_route_seats(&mut tx, t, second).await?;
    }
    let seats = lock_route_seats(&mut tx, t, route).await?;
    let mut h = head(&mut tx, &T_ALLOCATION, t, id, true).await?;
    ensure_version(v, h.version)?;
    step(&h, TransportAllocationStatus::Changed)?;
    check_route_stop(&mut tx, &seats, inst, b.stop_id).await?;
    if route != cur.route_id
        && !seats_available(seats.capacity, used_seats(&mut tx, route).await?, 1)
    {
        return Err(KernelError::Conflict("the target route has no free seat".into()).into());
    }
    sqlx::query("UPDATE campus.transport_allocation SET end_date = $1 WHERE id = $2")
        .bind(from.pred_opt().unwrap_or(from))
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    apply(
        &mut tx,
        &scope,
        &T_ALLOCATION,
        &mut h,
        Move {
            to: TransportAllocationStatus::Changed.as_str(),
            event: None,
            action: "campus.transport_allocation.change",
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &a,
    )
    .await?;
    let new_id: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.transport_allocation (tenant_id, institution_id, person_kind, person_id, \
         person_number, person_name, subject_ref, route_id, stop_id, channel, start_date, status, previous_id) \
         SELECT tenant_id, institution_id, person_kind, person_id, person_number, person_name, subject_ref, \
         $1, $2, 'office', $3, 'active', id FROM campus.transport_allocation WHERE id = $4 RETURNING id",
    )
    .bind(route)
    .bind(b.stop_id)
    .bind(from)
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    created(
        &mut tx,
        &scope,
        &T_ALLOCATION,
        new_id,
        TransportAllocationStatus::Active.as_str(),
        "campus.transport_allocation.reassign",
        serde_json::json!({ "previous": id, "route": route, "stop": b.stop_id }),
        &a,
    )
    .await?;
    crate::shared::publish::event(
        &mut tx,
        &scope,
        TRANSPORT_ALLOCATION_ACTIVATED,
        TRANSPORT_ALLOCATION_ACTIVATED_SCHEMA,
        new_id.to_string(),
        &StatusChangedV1 {
            aggregate_id: new_id,
            institution_id: inst.as_uuid(),
            from_status: None,
            to_status: TransportAllocationStatus::Active.as_str().to_owned(),
            version: 1,
            reason: Some("allocation changed".into()),
            process_instance_id: None,
            number: Some(cur.person_number.clone()),
            subject_ref: h.subject.clone(),
        },
        &a,
    )
    .await?;
    if let Some(pass) = live_pass(&mut tx, id).await? {
        sqlx::query(
            "UPDATE campus.transport_pass SET allocation_id = $1, route_id = $2, version = version + 1, \
             updated_at = now() WHERE id = $3",
        )
        .bind(new_id)
        .bind(route)
        .bind(pass)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(t_allocation_reply(&st, t, new_id, StatusCode::OK).await?)
}

/// Withdraw a passenger (`campus.transport.manage`; G-101): requested / active → withdrawn, a live
/// pass is cancelled.
#[utoipa::path(post, path = "/api/v1/campus/transport-allocations/{id}/withdraw", operation_id = "campus_transport_allocation_withdraw", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the allocation")),
    request_body = ReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TransportAllocationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn allocation_withdraw(
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
        head(&mut conn, &T_ALLOCATION, t, id, false)
            .await?
            .institution
    };
    let scope = institution_scope(ctx, inst);
    let today = business_today(&st, &scope, inst).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut h = head(&mut tx, &T_ALLOCATION, t, id, true).await?;
    ensure_version(v, h.version)?;
    step(&h, TransportAllocationStatus::Withdrawn)?;
    if let Some(pass) = live_pass(&mut tx, id).await? {
        pass_move(
            &mut tx,
            &scope,
            pass,
            PassStatus::Cancelled,
            "campus.transport_pass.cancel",
            Some("allocation withdrawn"),
            &a,
        )
        .await?;
    }
    sqlx::query("UPDATE campus.transport_allocation SET end_date = GREATEST(start_date, $1::date) WHERE id = $2")
        .bind(today)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    apply(
        &mut tx,
        &scope,
        &T_ALLOCATION,
        &mut h,
        Move {
            to: TransportAllocationStatus::Withdrawn.as_str(),
            event: None,
            action: "campus.transport_allocation.withdraw",
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(t_allocation_reply(&st, t, id, StatusCode::OK).await?)
}

// ------------------------------------------------------------------ handlers: passes

/// Passes of an institution (`campus.read`; `?status=`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/transport-passes", operation_id = "campus_transport_pass_list", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), StatusFilter), security(("session_cookie" = [])),
    responses((status = 200, body = TransportPassList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn pass_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StatusFilter>,
) -> ApiResult<Json<TransportPassList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let status = status_filter::<PassStatus>(q.status.as_deref())?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let items = list_of(
        &mut conn,
        PASS_SELECT,
        ctx.scope.tenant_id,
        "institution_id",
        id,
        status,
        "x.number DESC",
    )
    .await?;
    Ok(Json(TransportPassList { items }))
}

/// Issue a pass (`campus.transport.manage`; G-113): the route's fare is charged to a student as
/// `TRANSPORT-PASS` through `CampusChargePort`.
#[utoipa::path(post, path = "/api/v1/campus/transport-allocations/{id}/pass", operation_id = "campus_transport_pass_issue", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = TransportPassIssue, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = TransportPassView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn pass_issue(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<TransportPassIssue>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    require_idempotency_key(&headers)?;
    if b.valid_to < b.valid_from {
        return Err(invalid("validTo", "before_start", "validTo is before validFrom").into());
    }
    let t = ctx.scope.tenant_id;
    let inst = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        head(&mut conn, &T_ALLOCATION, t, id, false)
            .await?
            .institution
    };
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let h = head(&mut tx, &T_ALLOCATION, t, id, true).await?;
    if h.status != TransportAllocationStatus::Active.as_str() {
        return Err(KernelError::Conflict(format!(
            "the allocation is {}; passes are issued for active allocations",
            h.status
        ))
        .into());
    }
    if live_pass(&mut tx, id).await?.is_some() {
        return Err(KernelError::Conflict("the allocation already has a live pass".into()).into());
    }
    let route: Uuid =
        sqlx::query_scalar("SELECT route_id FROM campus.transport_allocation WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
    let fare = lock_route_seats(&mut tx, t, route).await?.fare;
    let pass = issue_pass(
        &st,
        &mut tx,
        &scope,
        NewPass {
            allocation: id,
            valid_from: b.valid_from,
            valid_to: b.valid_to,
            kind: "new",
            replaces: None,
            fee: fare,
        },
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(pass_reply(&st, t, pass, StatusCode::CREATED).await?)
}

/// One pass (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/transport-passes/{id}", operation_id = "campus_transport_pass_get", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = TransportPassView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn pass_get(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusRead)?;
    Ok(pass_reply(&st, ctx.scope.tenant_id, id, StatusCode::OK).await?)
}

/// Renew a pass (`campus.transport.manage`; G-114): a renewal pass from the day after this one
/// ends up to `validTo` (fare charged again); this pass expires.
#[utoipa::path(post, path = "/api/v1/campus/transport-passes/{id}/renew", operation_id = "campus_transport_pass_renew", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the pass")),
    request_body = TransportPassRenew, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TransportPassView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn pass_renew(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<TransportPassRenew>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let cur: TransportPassView = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        fetch(&mut conn, PASS_SELECT, t, id, PASS.entity).await?
    };
    let inst = InstitutionId(cur.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let from = cur.valid_to.succ_opt().unwrap_or(cur.valid_to);
    if b.valid_to < from {
        return Err(invalid(
            "validTo",
            "before_start",
            "the renewal must end after the current pass",
        )
        .into());
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let ah = head(&mut tx, &T_ALLOCATION, t, cur.allocation_id, true).await?;
    if ah.status != TransportAllocationStatus::Active.as_str() {
        return Err(KernelError::Conflict("the allocation is not active".into()).into());
    }
    let h = head(&mut tx, &PASS, t, id, true).await?;
    ensure_version(v, h.version)?;
    if !matches!(h.status.as_str(), "issued" | "active") {
        return Err(KernelError::Conflict(format!(
            "pass {} is {}; it cannot be renewed",
            cur.number, h.status
        ))
        .into());
    }
    pass_move(
        &mut tx,
        &scope,
        id,
        PassStatus::Expired,
        "campus.transport_pass.expire",
        Some("renewed"),
        &a,
    )
    .await?;
    let fare = lock_route_seats(&mut tx, t, cur.route_id).await?.fare;
    let pass = issue_pass(
        &st,
        &mut tx,
        &scope,
        NewPass {
            allocation: cur.allocation_id,
            valid_from: from,
            valid_to: b.valid_to,
            kind: "renewal",
            replaces: Some(id),
            fee: fare,
        },
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(pass_reply(&st, t, pass, StatusCode::OK).await?)
}

/// Replace a lost / damaged pass (`campus.transport.manage`; G-115): this pass → replaced, a
/// replacement pass with the same validity is issued (optional replacement fee).
#[utoipa::path(post, path = "/api/v1/campus/transport-passes/{id}/replace", operation_id = "campus_transport_pass_replace", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the pass")),
    request_body = TransportPassReplace, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TransportPassView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn pass_replace(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<TransportPassReplace>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let reason = crate::hostel::common::required_text("reason", &b.reason, 1000)?;
    let fee = match b.fee.as_deref() {
        Some(f) => money("fee", f)?,
        None => Decimal::ZERO,
    };
    let a = actor(ctx);
    let cur: TransportPassView = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        fetch(&mut conn, PASS_SELECT, t, id, PASS.entity).await?
    };
    let inst = InstitutionId(cur.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let h = head(&mut tx, &PASS, t, id, true).await?;
    ensure_version(v, h.version)?;
    pass_move(
        &mut tx,
        &scope,
        id,
        PassStatus::Replaced,
        "campus.transport_pass.replace",
        Some(reason.as_str()),
        &a,
    )
    .await?;
    let pass = issue_pass(
        &st,
        &mut tx,
        &scope,
        NewPass {
            allocation: cur.allocation_id,
            valid_from: cur.valid_from,
            valid_to: cur.valid_to,
            kind: "replacement",
            replaces: Some(id),
            fee,
        },
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(pass_reply(&st, t, pass, StatusCode::OK).await?)
}

/// Suspend a pass (`campus.transport.manage`; G-116): transport access restricted.
#[utoipa::path(post, path = "/api/v1/campus/transport-passes/{id}/suspend", operation_id = "campus_transport_pass_suspend", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the pass")),
    request_body = ReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TransportPassView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn pass_suspend(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReasonBody>,
) -> ApiResult<Response> {
    pass_command(
        st,
        auth,
        id,
        headers,
        b,
        PassStatus::Suspended,
        "campus.transport_pass.suspend",
    )
    .await
}

/// Restore a suspended pass (`campus.transport.manage`).
#[utoipa::path(post, path = "/api/v1/campus/transport-passes/{id}/restore", operation_id = "campus_transport_pass_restore", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the pass")),
    request_body = ReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TransportPassView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn pass_restore(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReasonBody>,
) -> ApiResult<Response> {
    pass_command(
        st,
        auth,
        id,
        headers,
        b,
        PassStatus::Active,
        "campus.transport_pass.restore",
    )
    .await
}

async fn pass_command(
    st: CampusState,
    auth: Authenticated,
    id: Uuid,
    headers: HeaderMap,
    b: ReasonBody,
    to: PassStatus,
    action: &'static str,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    let v = expected!(headers);
    let reason = b.text()?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let h = head(&mut tx, &PASS, t, id, true).await?;
    ensure_version(v, h.version)?;
    let scope = institution_scope(ctx, h.institution);
    pass_move(&mut tx, &scope, id, to, action, reason.as_deref(), &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(pass_reply(&st, t, id, StatusCode::OK).await?)
}

// ------------------------------------------------------------------ handlers: complaints

/// Complaints raised for an allocation (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/transport-allocations/{id}/complaints", operation_id = "campus_transport_complaint_list", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ComplaintList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn complaint_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ComplaintList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    head(&mut conn, &T_ALLOCATION, ctx.scope.tenant_id, id, false).await?;
    Ok(Json(
        complaints_of(&mut conn, ctx.scope.tenant_id, "transport", id).await?,
    ))
}

/// Raise a transport complaint for a passenger (`campus.transport.manage`): a service desk ticket
/// (source `transport`).
#[utoipa::path(post, path = "/api/v1/campus/transport-allocations/{id}/complaints", operation_id = "campus_transport_complaint_create", tag = "campus-transport",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ComplaintCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ComplaintView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn complaint_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ComplaintCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TransportManage)?;
    require_idempotency_key(&headers)?;
    let c = open_transport_complaint(&st, ctx, id, &b).await?;
    Ok(reply(StatusCode::CREATED, &c, c.version))
}
