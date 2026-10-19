//! Space reservations / room booking (L2-G10; L3-G-291…293): request (space active, inside its
//! availability, capacity ≥ attendees, no overlap with a requested / confirmed booking → 409) →
//! confirmed (`RESERVATION_APPROVAL`, auto) → reschedule (same checks) → cancel; office bookings
//! and `/me/reservations` for `campus.self` requesters. Times are business-local.

use crate::infrastructure::common::{
    audit, clean, emit_status, ensure_version, expected, fmt_time, history, illegal, invalid,
    limit, parse_date, parse_filter, parse_time, required, EventExtras, FacilityDecisionBody,
    FacilityPersonInput, FacilityReasonBody, Transition,
};
use crate::infrastructure::domain::{outside_availability, overlaps, ReservationStatus};
use crate::infrastructure::nodes::load_availability;
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::me::{my_person, require_person, PersonRef};
use crate::shared::numbering::{self, kind};
use crate::shared::rows::{decode, decode_opt, load_node, lock_node};
use crate::shared::scope::institution_scope;
use crate::shared::time::business_today;
use crate::shared::{Actor, CampusState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::campus::events::facilities::{
    SPACE_RESERVATION_CANCELLED, SPACE_RESERVATION_CANCELLED_SCHEMA, SPACE_RESERVATION_CONFIRMED,
    SPACE_RESERVATION_CONFIRMED_SCHEMA, SPACE_RESERVATION_RESCHEDULED,
    SPACE_RESERVATION_RESCHEDULED_SCHEMA,
};
use sutra_contracts::campus::ids::InfrastructureNodeId;
use sutra_contracts::campus::status::{InfrastructureNodeType, PersonKind};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

// ------------------------------------------------------------------ rows

/// A stored reservation (+ its space's code / name).
#[derive(Clone, Debug)]
pub struct ReservationRow {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub number: String,
    pub node_id: Uuid,
    pub node_code: String,
    pub node_name: String,
    pub requester_kind: Option<PersonKind>,
    pub requester_id: Option<Uuid>,
    pub requester_subject: Option<String>,
    pub purpose: String,
    pub attendees: i32,
    pub reservation_date: NaiveDate,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    pub status: ReservationStatus,
    pub status_reason: Option<String>,
    pub flagged: bool,
    pub flag_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

const COLUMNS: &str = "r.id, r.institution_id, r.number, r.node_id, n.code AS node_code, \
    n.name AS node_name, r.requester_kind, r.requester_id, r.requester_subject, r.purpose, \
    r.attendees, r.reservation_date, r.start_time, r.end_time, r.status, r.status_reason, \
    r.flagged, r.flag_reason, r.process_instance_id, r.created_at, r.version";

const FROM: &str =
    "campus.space_reservation r JOIN campus.infrastructure_node n ON n.id = r.node_id";

fn from_row(r: &PgRow) -> Result<ReservationRow, sqlx::Error> {
    Ok(ReservationRow {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        number: r.try_get("number")?,
        node_id: r.try_get("node_id")?,
        node_code: r.try_get("node_code")?,
        node_name: r.try_get("node_name")?,
        requester_kind: decode_opt(r.try_get("requester_kind")?, PersonKind::parse)?,
        requester_id: r.try_get("requester_id")?,
        requester_subject: r.try_get("requester_subject")?,
        purpose: r.try_get("purpose")?,
        attendees: r.try_get("attendees")?,
        reservation_date: r.try_get("reservation_date")?,
        start_time: r.try_get("start_time")?,
        end_time: r.try_get("end_time")?,
        status: decode(r.try_get("status")?, ReservationStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        flagged: r.try_get("flagged")?,
        flag_reason: r.try_get("flag_reason")?,
        process_instance_id: r.try_get("process_instance_id")?,
        created_at: r.try_get("created_at")?,
        version: r.try_get("version")?,
    })
}

async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<ReservationRow, KernelError> {
    let sql = format!(
        "SELECT {COLUMNS} FROM {FROM} WHERE r.id = $1 AND r.tenant_id = $2{}",
        if lock { " FOR UPDATE OF r" } else { "" }
    );
    sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .as_ref()
        .map(from_row)
        .transpose()
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "space reservation",
        })
}

// ------------------------------------------------------------------ views + bodies

/// A space reservation.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReservationView {
    pub id: Uuid,
    #[schema(example = "RSV-000001")]
    pub number: String,
    pub institution_id: Uuid,
    pub node_id: Uuid,
    pub node_code: String,
    pub node_name: String,
    pub requester_kind: Option<PersonKind>,
    pub requester_id: Option<Uuid>,
    pub purpose: String,
    pub attendees: i32,
    pub date: NaiveDate,
    #[schema(example = "09:00")]
    pub start_time: String,
    #[schema(example = "11:00")]
    pub end_time: String,
    pub status: ReservationStatus,
    pub status_reason: Option<String>,
    /// The space was suspended after the booking.
    pub flagged: bool,
    pub flag_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

impl From<&ReservationRow> for ReservationView {
    fn from(r: &ReservationRow) -> Self {
        Self {
            id: r.id,
            number: r.number.clone(),
            institution_id: r.institution_id.as_uuid(),
            node_id: r.node_id,
            node_code: r.node_code.clone(),
            node_name: r.node_name.clone(),
            requester_kind: r.requester_kind,
            requester_id: r.requester_id,
            purpose: r.purpose.clone(),
            attendees: r.attendees,
            date: r.reservation_date,
            start_time: fmt_time(r.start_time),
            end_time: fmt_time(r.end_time),
            status: r.status,
            status_reason: r.status_reason.clone(),
            flagged: r.flagged,
            flag_reason: r.flag_reason.clone(),
            process_instance_id: r.process_instance_id.clone(),
            created_at: r.created_at,
            version: r.version,
        }
    }
}

/// Reservations.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReservationList {
    pub items: Vec<ReservationView>,
}

/// Book a space (office booking may name the requester).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReservationCreate {
    pub node_id: Uuid,
    #[schema(example = "2026-12-01")]
    pub date: String,
    #[schema(example = "09:00")]
    pub start_time: String,
    #[schema(example = "11:00")]
    pub end_time: String,
    pub purpose: Option<String>,
    pub attendees: i32,
    /// Office bookings only: the student / staff member the booking is for.
    pub requester: Option<FacilityPersonInput>,
}

/// Move a reservation.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReservationReschedule {
    pub date: String,
    pub start_time: String,
    pub end_time: String,
    pub attendees: Option<i32>,
    pub reason: Option<String>,
}

/// Filters of the reservation list.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct ReservationQuery {
    pub status: Option<String>,
    pub node_id: Option<Uuid>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub limit: Option<i64>,
}

// ------------------------------------------------------------------ rules

/// A requested slot.
struct Slot {
    date: NaiveDate,
    start: NaiveTime,
    end: NaiveTime,
    attendees: i32,
}

fn parse_slot(date: &str, start: &str, end: &str, attendees: i32) -> Result<Slot, KernelError> {
    let date = parse_date("date", date)?;
    let start = parse_time("startTime", start)?;
    let end = parse_time("endTime", end)?;
    if start >= end {
        return Err(invalid(
            "endTime",
            "invalid_window",
            "endTime must be after startTime",
        ));
    }
    if attendees < 1 {
        return Err(invalid("attendees", "invalid", "at least one attendee"));
    }
    Ok(Slot {
        date,
        start,
        end,
        attendees,
    })
}

/// Checks a slot on a **locked** space: active (409), capacity (409), availability (409), no
/// overlap with a reservation holding the slot (409; `exclude` = the reservation being moved).
async fn check_slot(
    conn: &mut PgConnection,
    tenant: TenantId,
    node_id: Uuid,
    slot: &Slot,
    exclude: Option<Uuid>,
) -> Result<(), KernelError> {
    let node = lock_node(conn, tenant, InfrastructureNodeId(node_id)).await?;
    if node.node_type != InfrastructureNodeType::Space {
        return Err(invalid("nodeId", "not_a_space", "only spaces are booked"));
    }
    if !node.status.is_operational() {
        return Err(KernelError::Conflict(format!(
            "space {} is {}; it takes no bookings",
            node.code, node.status
        )));
    }
    if node.capacity.is_some_and(|c| slot.attendees > c) {
        return Err(KernelError::Conflict(format!(
            "space {} seats {}; {} attendees do not fit",
            node.code,
            node.capacity.unwrap_or_default(),
            slot.attendees
        )));
    }
    let profile = load_availability(conn, tenant, node.id).await?.map(|x| x.0);
    if let Some(why) = outside_availability(profile.as_ref(), slot.date, slot.start, slot.end) {
        return Err(KernelError::Conflict(format!("space {}: {why}", node.code)));
    }
    let others: Vec<(Uuid, String, NaiveTime, NaiveTime)> = sqlx::query_as(
        "SELECT id, number, start_time, end_time FROM campus.space_reservation \
         WHERE tenant_id = $1 AND node_id = $2 AND reservation_date = $3 \
         AND status IN ('requested', 'confirmed', 'rescheduled') AND ($4::uuid IS NULL OR id <> $4)",
    )
    .bind(tenant.as_uuid())
    .bind(node_id)
    .bind(slot.date)
    .bind(exclude)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if let Some((_, number, s, e)) = others
        .iter()
        .find(|(_, _, s, e)| overlaps(slot.start, slot.end, *s, *e))
    {
        return Err(KernelError::Conflict(format!(
            "space {} is already booked {}–{} ({number})",
            node.code,
            fmt_time(*s),
            fmt_time(*e)
        )));
    }
    Ok(())
}

/// Moves a locked reservation (history + optional event).
#[allow(clippy::too_many_arguments)]
async fn move_reservation(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    r: &ReservationRow,
    to: ReservationStatus,
    reason: Option<&str>,
    pid: Option<&str>,
    event: Option<(&'static str, &'static str)>,
    actor: &Actor,
) -> Result<i64, KernelError> {
    if !r.status.can_transition(to) {
        return Err(illegal(
            &format!("reservation {}", r.number),
            r.status.as_str(),
            to.as_str(),
        ));
    }
    let v: i64 = sqlx::query_scalar(
        "UPDATE campus.space_reservation SET status = $3, status_reason = $4, \
         process_instance_id = COALESCE($5, process_instance_id), version = version + 1, \
         updated_at = now() WHERE id = $1 AND tenant_id = $2 RETURNING version",
    )
    .bind(r.id)
    .bind(scope.tenant_id.as_uuid())
    .bind(to.as_str())
    .bind(reason)
    .bind(pid)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let t = Transition {
        tenant: scope.tenant_id,
        institution: r.institution_id,
        aggregate_type: "space_reservation",
        aggregate_id: r.id,
        from: Some(r.status.as_str()),
        to: to.as_str(),
        reason,
        process_instance_id: pid,
        cause: event.map(|e| e.0).unwrap_or(""),
    };
    history(conn, &t, actor).await?;
    if let Some(e) = event {
        emit_status(
            conn,
            scope,
            e,
            &t,
            v,
            &EventExtras {
                number: Some(r.number.clone()),
                subject_ref: r.requester_subject.clone(),
            },
            actor,
        )
        .await?;
    }
    Ok(v)
}

async fn view(
    st: &CampusState,
    tenant: TenantId,
    id: Uuid,
) -> Result<ReservationView, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(ReservationView::from(
        &load(&mut conn, tenant, id, false).await?,
    ))
}

fn respond(status: StatusCode, v: &ReservationView) -> Response {
    with_etag(status, v, Version(v.version))
}

/// Whether the caller may act on a reservation: the office (`campus.facility.manage`) or its
/// own requester (`campus.self`); anyone else sees 404.
async fn may_act(
    st: &CampusState,
    ctx: &AuthContext,
    r: &ReservationRow,
    office: Permission,
) -> Result<(), KernelError> {
    if ctx.has_permission(office) {
        return Ok(());
    }
    let me = my_person(st, ctx).await?;
    if r.requester_kind == Some(me.kind) && r.requester_id == Some(me.id) {
        Ok(())
    } else {
        Err(KernelError::NotFound {
            entity: "space reservation",
        })
    }
}

/// Validates, requests approval, inserts the reservation and (auto-approval) confirms it.
async fn book(
    st: &CampusState,
    scope: &ExecutionScope,
    institution: InstitutionId,
    b: ReservationCreate,
    requester: Option<PersonRef>,
    a: &Actor,
) -> Result<Uuid, KernelError> {
    let t = scope.tenant_id;
    let slot = parse_slot(&b.date, &b.start_time, &b.end_time, b.attendees)?;
    let purpose = required("purpose", b.purpose)?;
    if purpose.len() > 300 {
        return Err(invalid("purpose", "too_long", "at most 300 characters"));
    }
    let today = business_today(st, scope, institution).await?;
    if slot.date < today {
        return Err(invalid(
            "date",
            "in_the_past",
            "a booking starts today or later",
        ));
    }
    {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let node = load_node(&mut conn, t, InfrastructureNodeId(b.node_id))
            .await?
            .filter(|n| n.institution_id == institution)
            .ok_or_else(|| invalid("nodeId", "unknown_node", "no space with this id"))?;
        if node.node_type != InfrastructureNodeType::Space {
            return Err(invalid("nodeId", "not_a_space", "only spaces are booked"));
        }
    }
    // Read-only pre-check, so no approval starts for a slot that cannot be booked.
    {
        let mut tx = st.pool.begin().await.map_err(map_db_error)?;
        check_slot(&mut tx, t, b.node_id, &slot, None).await?;
        tx.rollback().await.map_err(map_db_error)?;
    }
    let id = Uuid::now_v7();
    let outcome = request_approval(
        st,
        scope,
        process::RESERVATION_APPROVAL,
        business_key("space_reservation", id, 1),
        BTreeMap::from([
            ("nodeId".to_owned(), serde_json::json!(b.node_id)),
            ("date".to_owned(), serde_json::json!(slot.date)),
            ("attendees".to_owned(), serde_json::json!(slot.attendees)),
        ]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    check_slot(&mut tx, t, b.node_id, &slot, None).await?;
    let number = numbering::next(&mut tx, t, institution, kind::RESERVATION).await?;
    sqlx::query(
        "INSERT INTO campus.space_reservation (id, tenant_id, institution_id, number, node_id, \
         requester_kind, requester_id, requester_subject, purpose, attendees, reservation_date, \
         start_time, end_time, process_instance_id, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(institution.as_uuid())
    .bind(&number)
    .bind(b.node_id)
    .bind(requester.as_ref().map(|p| p.kind.as_str()))
    .bind(requester.as_ref().map(|p| p.id))
    .bind(requester.as_ref().and_then(|p| p.subject_ref.clone()))
    .bind(&purpose)
    .bind(slot.attendees)
    .bind(slot.date)
    .bind(slot.start)
    .bind(slot.end)
    .bind(&pid)
    .bind(a.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        &Transition {
            tenant: t,
            institution,
            aggregate_type: "space_reservation",
            aggregate_id: id,
            from: None,
            to: ReservationStatus::Requested.as_str(),
            reason: None,
            process_instance_id: Some(&pid),
            cause: "sutraerp.campus.space_reservation.requested",
        },
        a,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let row = load(&mut tx, t, id, true).await?;
        move_reservation(
            &mut tx,
            scope,
            &row,
            ReservationStatus::Confirmed,
            Some("auto-approved"),
            Some(&pid),
            Some((
                SPACE_RESERVATION_CONFIRMED,
                SPACE_RESERVATION_CONFIRMED_SCHEMA,
            )),
            &Actor::system(a.correlation_id.clone()),
        )
        .await?;
    }
    audit(
        &mut tx,
        scope,
        "campus.space_reservation.request",
        "space_reservation",
        id,
        serde_json::json!({"number": number, "nodeId": b.node_id, "date": slot.date,
            "startTime": fmt_time(slot.start), "endTime": fmt_time(slot.end),
            "attendees": slot.attendees}),
        a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(id)
}

async fn list(
    st: &CampusState,
    tenant: TenantId,
    institution: Option<InstitutionId>,
    requester: Option<&PersonRef>,
    q: &ReservationQuery,
) -> Result<ReservationList, KernelError> {
    let status = parse_filter("status", q.status.as_deref(), ReservationStatus::parse)?;
    let from = q
        .from
        .as_deref()
        .map(|r| parse_date("from", r))
        .transpose()?;
    let to = q.to.as_deref().map(|r| parse_date("to", r)).transpose()?;
    let sql = format!(
        "SELECT {COLUMNS} FROM {FROM} WHERE r.tenant_id = $1 \
         AND ($2::uuid IS NULL OR r.institution_id = $2) \
         AND ($3::text IS NULL OR r.requester_kind = $3) AND ($4::uuid IS NULL OR r.requester_id = $4) \
         AND ($5::text IS NULL OR r.status = $5) AND ($6::uuid IS NULL OR r.node_id = $6) \
         AND ($7::date IS NULL OR r.reservation_date >= $7) \
         AND ($8::date IS NULL OR r.reservation_date <= $8) \
         ORDER BY r.reservation_date DESC, r.start_time DESC LIMIT $9"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(institution.map(|i| i.as_uuid()))
        .bind(requester.map(|p| p.kind.as_str()))
        .bind(requester.map(|p| p.id))
        .bind(status.map(|s| s.as_str()))
        .bind(q.node_id)
        .bind(from)
        .bind(to)
        .bind(limit(q.limit))
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        items.push(ReservationView::from(&from_row(r).map_err(map_db_error)?));
    }
    Ok(ReservationList { items })
}

// ------------------------------------------------------------------ handlers

/// Book a space for the office or a named student / staff member (`campus.facility.manage`).
#[utoipa::path(post, path = "/api/v1/campus/institutions/{id}/reservations", operation_id = "campus_reservation_create", tag = "campus-infrastructure",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ReservationCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ReservationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn reservation_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReservationCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let requester = match b.requester {
        None => None,
        Some(p) => Some(require_person(&st, &scope, p.kind, p.id, "requester").await?),
    };
    let rid = book(&st, &scope, institution, b, requester, &actor(ctx)).await?;
    let v = view(&st, ctx.scope.tenant_id, rid).await?;
    Ok(respond(StatusCode::CREATED, &v))
}

/// Reservations of an institution (`campus.read`; `?status=&nodeId=&from=&to=&limit=`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/reservations", operation_id = "campus_reservation_list", tag = "campus-infrastructure",
    params(("id" = String, Path, format = Uuid), ReservationQuery), security(("session_cookie" = [])),
    responses((status = 200, body = ReservationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn reservation_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ReservationQuery>,
) -> ApiResult<Json<ReservationList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution(&st, &scope, institution).await?;
    Ok(Json(
        list(&st, ctx.scope.tenant_id, Some(institution), None, &q).await?,
    ))
}

/// One reservation (`campus.read`, or its own requester).
#[utoipa::path(get, path = "/api/v1/campus/reservations/{id}", operation_id = "campus_reservation_get", tag = "campus-infrastructure",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ReservationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn reservation_get(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    let t = ctx.scope.tenant_id;
    let row = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load(&mut conn, t, id, false).await?
    };
    may_act(&st, ctx, &row, Permission::CampusRead).await?;
    Ok(respond(StatusCode::OK, &ReservationView::from(&row)))
}

async fn decide(
    st: &CampusState,
    ctx: &AuthContext,
    id: Uuid,
    expected_version: i64,
    approve: bool,
    reason: Option<String>,
) -> Result<ReservationView, KernelError> {
    if !approve && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason"));
    }
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load(&mut tx, t, id, true).await?;
    ensure_version(expected_version, r.version)?;
    let scope = institution_scope(ctx, r.institution_id);
    if approve {
        if r.status != ReservationStatus::Requested {
            return Err(illegal(
                &format!("reservation {}", r.number),
                r.status.as_str(),
                "confirmed",
            ));
        }
        // The slot may have changed since the request (suspension, profile, capacity).
        let slot = Slot {
            date: r.reservation_date,
            start: r.start_time,
            end: r.end_time,
            attendees: r.attendees,
        };
        check_slot(&mut tx, t, r.node_id, &slot, Some(r.id)).await?;
        move_reservation(
            &mut tx,
            &scope,
            &r,
            ReservationStatus::Confirmed,
            reason.as_deref(),
            r.process_instance_id.as_deref(),
            Some((
                SPACE_RESERVATION_CONFIRMED,
                SPACE_RESERVATION_CONFIRMED_SCHEMA,
            )),
            &a,
        )
        .await?;
    } else {
        move_reservation(
            &mut tx,
            &scope,
            &r,
            ReservationStatus::Rejected,
            reason.as_deref(),
            r.process_instance_id.as_deref(),
            None,
            &a,
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "campus.space_reservation.decide",
        "space_reservation",
        id,
        serde_json::json!({"approve": approve, "reason": reason}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    view(st, t, id).await
}

/// Confirm a requested reservation (`campus.facility.manage`; = approve).
#[utoipa::path(post, path = "/api/v1/campus/reservations/{id}/confirm", operation_id = "campus_reservation_confirm", tag = "campus-infrastructure",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the reservation")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ReservationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn reservation_confirm(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let out = decide(&st, ctx, id, v, true, clean(b.reason)).await?;
    Ok(respond(StatusCode::OK, &out))
}

/// Decide a requested reservation (`RESERVATION_APPROVAL`; routine bookings are decided with
/// `campus.facility.manage`): approve → confirmed (slot re-checked), reject → rejected.
#[utoipa::path(post, path = "/api/v1/campus/reservations/{id}/decision", operation_id = "campus_reservation_decide", tag = "campus-infrastructure",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the reservation")),
    request_body = FacilityDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ReservationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn reservation_decide(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let approve = b.approve();
    let out = decide(&st, ctx, id, v, approve, clean(b.reason)).await?;
    Ok(respond(StatusCode::OK, &out))
}

/// Move a reservation (office or its requester): the new slot passes the same checks; a
/// confirmed booking becomes `rescheduled`, a requested one stays requested.
#[utoipa::path(post, path = "/api/v1/campus/reservations/{id}/reschedule", operation_id = "campus_reservation_reschedule", tag = "campus-infrastructure",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the reservation")),
    request_body = ReservationReschedule, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ReservationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn reservation_reschedule(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReservationReschedule>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let probe = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load(&mut conn, t, id, false).await?
    };
    may_act(&st, ctx, &probe, Permission::FacilityManage).await?;
    let scope = institution_scope(ctx, probe.institution_id);
    require_institution_writable(&st, &scope, probe.institution_id).await?;
    let slot = parse_slot(
        &b.date,
        &b.start_time,
        &b.end_time,
        b.attendees.unwrap_or(probe.attendees),
    )?;
    let today = business_today(&st, &scope, probe.institution_id).await?;
    if slot.date < today {
        return Err(invalid("date", "in_the_past", "a booking starts today or later").into());
    }
    let reason = clean(b.reason);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load(&mut tx, t, id, true).await?;
    ensure_version(v, r.version)?;
    if !r.status.holds_slot() {
        return Err(illegal(
            &format!("reservation {}", r.number),
            r.status.as_str(),
            "rescheduled",
        )
        .into());
    }
    check_slot(&mut tx, t, r.node_id, &slot, Some(r.id)).await?;
    sqlx::query(
        "UPDATE campus.space_reservation SET reservation_date = $3, start_time = $4, \
         end_time = $5, attendees = $6, flagged = false, flag_reason = NULL, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(slot.date)
    .bind(slot.start)
    .bind(slot.end)
    .bind(slot.attendees)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if r.status == ReservationStatus::Requested {
        sqlx::query(
            "UPDATE campus.space_reservation SET version = version + 1 WHERE id = $1 AND tenant_id = $2",
        )
        .bind(id)
        .bind(t.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    } else {
        move_reservation(
            &mut tx,
            &scope,
            &r,
            ReservationStatus::Rescheduled,
            reason.as_deref(),
            None,
            Some((
                SPACE_RESERVATION_RESCHEDULED,
                SPACE_RESERVATION_RESCHEDULED_SCHEMA,
            )),
            &a,
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "campus.space_reservation.reschedule",
        "space_reservation",
        id,
        serde_json::json!({"from": {"date": r.reservation_date, "startTime": fmt_time(r.start_time),
            "endTime": fmt_time(r.end_time)}, "to": {"date": slot.date,
            "startTime": fmt_time(slot.start), "endTime": fmt_time(slot.end)}, "reason": reason}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let out = view(&st, t, id).await?;
    Ok(respond(StatusCode::OK, &out))
}

/// Cancel a reservation (office or its requester).
#[utoipa::path(post, path = "/api/v1/campus/reservations/{id}/cancel", operation_id = "campus_reservation_cancel", tag = "campus-infrastructure",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the reservation")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ReservationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn reservation_cancel(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let reason = clean(b.reason);
    let probe = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load(&mut conn, t, id, false).await?
    };
    may_act(&st, ctx, &probe, Permission::FacilityManage).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load(&mut tx, t, id, true).await?;
    ensure_version(v, r.version)?;
    let scope = institution_scope(ctx, r.institution_id);
    move_reservation(
        &mut tx,
        &scope,
        &r,
        ReservationStatus::Cancelled,
        reason.as_deref(),
        None,
        Some((
            SPACE_RESERVATION_CANCELLED,
            SPACE_RESERVATION_CANCELLED_SCHEMA,
        )),
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "campus.space_reservation.cancel",
        "space_reservation",
        id,
        serde_json::json!({"reason": reason}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let out = view(&st, t, id).await?;
    Ok(respond(StatusCode::OK, &out))
}

/// Book a space for myself (`campus.self`: faculty book rooms, students study rooms).
#[utoipa::path(post, path = "/api/v1/campus/me/reservations", operation_id = "campus_reservation_self_create", tag = "campus-infrastructure",
    params(("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ReservationCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ReservationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_reservation_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(b): Json<ReservationCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    require_idempotency_key(&headers)?;
    let me = my_person(&st, ctx).await?;
    if b.requester.is_some() {
        return Err(invalid("requester", "not_allowed", "you book for yourself").into());
    }
    let t = ctx.scope.tenant_id;
    let institution = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_node(&mut conn, t, InfrastructureNodeId(b.node_id))
            .await?
            .ok_or_else(|| invalid("nodeId", "unknown_node", "no space with this id"))?
            .institution_id
    };
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let rid = book(&st, &scope, institution, b, Some(me), &actor(ctx)).await?;
    let v = view(&st, t, rid).await?;
    Ok(respond(StatusCode::CREATED, &v))
}

/// My reservations (`campus.self`).
#[utoipa::path(get, path = "/api/v1/campus/me/reservations", operation_id = "campus_reservation_self_list", tag = "campus-infrastructure",
    params(ReservationQuery), security(("session_cookie" = [])),
    responses((status = 200, body = ReservationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_reservation_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Query(q): Query<ReservationQuery>,
) -> ApiResult<Json<ReservationList>> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    let me = my_person(&st, ctx).await?;
    Ok(Json(
        list(&st, ctx.scope.tenant_id, None, Some(&me), &q).await?,
    ))
}

pub fn router(state: CampusState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(reservation_create, reservation_list))
        .routes(routes!(reservation_get))
        .routes(routes!(reservation_confirm))
        .routes(routes!(reservation_decide))
        .routes(routes!(reservation_reschedule))
        .routes(routes!(reservation_cancel))
        .routes(routes!(my_reservation_create, my_reservation_list))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_validated() {
        assert!(parse_slot("2026-12-01", "09:00", "11:00", 10).is_ok());
        assert!(parse_slot("2026-12-01", "11:00", "09:00", 10).is_err());
        assert!(parse_slot("2026-12-01", "09:00", "11:00", 0).is_err());
        assert!(parse_slot("01/12/2026", "09:00", "11:00", 1).is_err());
    }
}
