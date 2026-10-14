//! Residency (L3-G-082 resident leave → decision → out → verified return (late flag, overdue
//! check), G-087 room transfer → decision (`ROOM_TRANSFER_APPROVAL`) → completed bed swap,
//! G-089 room inspection → damage → `HOSTEL-DAMAGE` charge → closed, G-081 vacating → clearance
//! (no open / unpaid damage) → closed occupancy (bed released, waiting list offered), G-088 hostel
//! complaints → service desk tickets through `shared::tickets::open_ticket`).

use super::admission::HostelAllocationView;
use super::admission::{allocation_reply, move_bed, offer_vacancy, ALLOCATION, ALLOCATION_SELECT};
use super::common::{
    apply, complaints_of, created, ensure_version, expected, fetch, head, invalid, list_of, money,
    optional_text, raise_charge, reply, status_filter, step, Agg, BulkFailure, BulkSummary,
    ChargeAsk, ComplaintCreate, ComplaintList, ComplaintView, DecisionBody, Move, ReasonBody,
    StatusFilter,
};
use super::domain::{
    returned_late, BedStatus, HostelAllocationStatus, InspectionStatus, MessAllocationStatus,
    ResidentLeaveStatus, RoomTransferStatus,
};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::actor;
use crate::shared::me::PersonRef;
use crate::shared::rows::{load_node, lock_node};
use crate::shared::scope::institution_scope;
use crate::shared::tickets::{open_ticket, NewTicket};
use crate::shared::time::business_today;
use crate::shared::{Actor, CampusState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::campus::events::residence::{
    HOSTEL_ALLOCATION_CLOSED, HOSTEL_ALLOCATION_CLOSED_SCHEMA, RESIDENT_LEAVE_DECIDED,
    RESIDENT_LEAVE_DECIDED_SCHEMA, ROOM_INSPECTION_COMPLETED, ROOM_INSPECTION_COMPLETED_SCHEMA,
    ROOM_TRANSFER_COMPLETED, ROOM_TRANSFER_COMPLETED_SCHEMA,
};
use sutra_contracts::campus::ids::InfrastructureNodeId;
use sutra_contracts::campus::status::{
    InfrastructureNodeType, PersonKind, SpaceType, TicketCategory, TicketSource,
};
use sutra_contracts::finance::ids::ChargeId;
use sutra_contracts::finance::ports::CampusChargeCategory;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use uuid::Uuid;

/// Fee code of the room damage charge (contract §7).
pub const HOSTEL_DAMAGE: &str = "HOSTEL-DAMAGE";

pub(crate) const LEAVE: Agg = Agg {
    table: "resident_leave",
    entity: "resident leave",
    number: "(SELECT a.person_number FROM campus.hostel_allocation a WHERE a.id = allocation_id)",
    subject: "(SELECT a.subject_ref FROM campus.hostel_allocation a WHERE a.id = allocation_id)",
};

pub(crate) const TRANSFER: Agg = Agg {
    table: "room_transfer",
    entity: "room transfer",
    number: "(SELECT a.person_number FROM campus.hostel_allocation a WHERE a.id = allocation_id)",
    subject: "(SELECT a.subject_ref FROM campus.hostel_allocation a WHERE a.id = allocation_id)",
};

pub(crate) const INSPECTION: Agg = Agg {
    table: "room_inspection",
    entity: "room inspection",
    number: "(SELECT n.code FROM campus.infrastructure_node n WHERE n.id = room_node_id)",
    subject: "(SELECT a.subject_ref FROM campus.hostel_allocation a WHERE a.id = allocation_id)",
};

// ------------------------------------------------------------------ views

/// A resident leave / out-pass.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ResidentLeaveView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub allocation_id: Uuid,
    pub person_number: String,
    pub person_name: String,
    pub from_date: NaiveDate,
    pub to_date: NaiveDate,
    pub destination: Option<String>,
    pub reason: Option<String>,
    /// `requested | approved | rejected | out | returned | overdue`.
    pub status: String,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub departed_at: Option<DateTime<Utc>>,
    pub returned_at: Option<DateTime<Utc>>,
    /// Returned after the last leave day.
    pub returned_late: bool,
    pub version: i64,
}

pub(crate) const LEAVE_SELECT: &str =
    "SELECT x.id, x.institution_id, x.allocation_id, a.person_number, \
    a.person_name, x.from_date, x.to_date, x.destination, x.reason, x.status, x.status_reason, \
    x.process_instance_id, x.departed_at, x.returned_at, x.returned_late, x.version \
    FROM campus.resident_leave x JOIN campus.hostel_allocation a ON a.id = x.allocation_id";

/// Resident leaves.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ResidentLeaveList {
    pub items: Vec<ResidentLeaveView>,
}

/// A leave request of a resident.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResidentLeaveCreate {
    pub from_date: NaiveDate,
    pub to_date: NaiveDate,
    pub destination: Option<String>,
    pub reason: Option<String>,
}

/// A room transfer request.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct RoomTransferView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub allocation_id: Uuid,
    pub person_number: String,
    pub from_bed_id: Uuid,
    pub from_bed_code: String,
    pub from_room_code: String,
    pub to_bed_id: Uuid,
    pub to_bed_code: String,
    pub to_room_code: String,
    pub reason: Option<String>,
    /// `requested | approved | rejected | completed`.
    pub status: String,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub version: i64,
}

const TRANSFER_SELECT: &str = "SELECT x.id, x.institution_id, x.allocation_id, a.person_number, \
    x.from_bed_id, fb.bed_code AS from_bed_code, fr.code AS from_room_code, x.to_bed_id, \
    tb.bed_code AS to_bed_code, tn.code AS to_room_code, x.reason, x.status, x.status_reason, \
    x.process_instance_id, x.completed_at, x.version FROM campus.room_transfer x \
    JOIN campus.hostel_allocation a ON a.id = x.allocation_id \
    JOIN campus.hostel_bed fb ON fb.id = x.from_bed_id \
    JOIN campus.infrastructure_node fr ON fr.id = fb.room_node_id \
    JOIN campus.hostel_bed tb ON tb.id = x.to_bed_id \
    JOIN campus.infrastructure_node tn ON tn.id = tb.room_node_id";

/// Room transfers.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RoomTransferList {
    pub items: Vec<RoomTransferView>,
}

/// Request a move to another free bed.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoomTransferCreate {
    pub to_bed_id: Uuid,
    pub reason: Option<String>,
}

/// A room inspection.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct RoomInspectionView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub room_node_id: Uuid,
    pub room_code: String,
    pub allocation_id: Option<Uuid>,
    pub person_number: Option<String>,
    pub scheduled_on: NaiveDate,
    /// `scheduled | completed | damage_found | charged | closed`.
    pub status: String,
    pub status_reason: Option<String>,
    pub findings: Option<String>,
    #[schema(example = "1200.00")]
    pub damage_amount: Option<String>,
    pub charge_id: Option<Uuid>,
    pub charge_number: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub version: i64,
}

const INSPECTION_SELECT: &str = "SELECT x.id, x.institution_id, x.room_node_id, n.code AS room_code, \
    x.allocation_id, a.person_number, x.scheduled_on, x.status, x.status_reason, x.findings, \
    x.damage_amount::text AS damage_amount, x.charge_id, x.charge_number, x.completed_at, x.version \
    FROM campus.room_inspection x JOIN campus.infrastructure_node n ON n.id = x.room_node_id \
    LEFT JOIN campus.hostel_allocation a ON a.id = x.allocation_id";

/// Room inspections.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RoomInspectionList {
    pub items: Vec<RoomInspectionView>,
}

/// Schedule an inspection of a hostel room (optionally for its resident).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoomInspectionCreate {
    pub scheduled_on: NaiveDate,
    /// The residency inspected (damage is charged to this resident).
    pub allocation_id: Option<Uuid>,
}

/// Inspection outcome.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoomInspectionComplete {
    pub findings: Option<String>,
    /// Damage to charge (`"0"` / absent = no damage).
    #[schema(example = "1200.00")]
    pub damage_amount: Option<String>,
}

/// Start vacating a residency.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HostelVacateBody {
    /// Last day of residency (default: today).
    pub end_date: Option<NaiveDate>,
    pub reason: Option<String>,
}

// ------------------------------------------------------------------ helpers

async fn leave_reply(
    st: &CampusState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v: ResidentLeaveView = fetch(&mut conn, LEAVE_SELECT, tenant, id, LEAVE.entity).await?;
    Ok(reply(status, &v, v.version))
}

async fn transfer_reply(
    st: &CampusState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v: RoomTransferView =
        fetch(&mut conn, TRANSFER_SELECT, tenant, id, TRANSFER.entity).await?;
    Ok(reply(status, &v, v.version))
}

async fn inspection_reply(
    st: &CampusState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v: RoomInspectionView =
        fetch(&mut conn, INSPECTION_SELECT, tenant, id, INSPECTION.entity).await?;
    Ok(reply(status, &v, v.version))
}

fn alloc_move<'a>(
    to: HostelAllocationStatus,
    action: &'static str,
    reason: Option<&'a str>,
) -> Move<'a> {
    Move {
        to: to.as_str(),
        event: None,
        action,
        reason,
        process_instance_id: None,
    }
}

fn leave_decided<'a>(approve: bool, reason: Option<&'a str>, pid: Option<&'a str>) -> Move<'a> {
    Move {
        to: if approve {
            ResidentLeaveStatus::Approved.as_str()
        } else {
            ResidentLeaveStatus::Rejected.as_str()
        },
        event: Some((RESIDENT_LEAVE_DECIDED, RESIDENT_LEAVE_DECIDED_SCHEMA)),
        action: if approve {
            "campus.resident_leave.approve"
        } else {
            "campus.resident_leave.reject"
        },
        reason,
        process_instance_id: pid,
    }
}

/// Requests leave for a residency (office or the resident). Returns the leave id.
pub(crate) async fn request_leave(
    st: &CampusState,
    ctx: &AuthContext,
    allocation: Uuid,
    b: ResidentLeaveCreate,
) -> Result<Uuid, KernelError> {
    let t = ctx.scope.tenant_id;
    if b.to_date < b.from_date {
        return Err(invalid(
            "toDate",
            "before_start",
            "toDate must not be before fromDate",
        ));
    }
    let destination = optional_text("destination", b.destination, 200)?;
    let reason = optional_text("reason", b.reason, 1000)?;
    let ah = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        head(&mut conn, &ALLOCATION, t, allocation, false).await?
    };
    let scope = institution_scope(ctx, ah.institution);
    require_institution_writable(st, &scope, ah.institution).await?;
    if ah.status != HostelAllocationStatus::Active.as_str() {
        return Err(KernelError::Conflict(format!(
            "the residency of {} is {}; leave needs an active residency",
            ah.number.clone().unwrap_or_default(),
            ah.status
        )));
    }
    let id = Uuid::now_v7();
    let outcome = request_approval(
        st,
        &scope,
        process::RESIDENT_LEAVE_APPROVAL,
        business_key("resident_leave", id, 1),
        BTreeMap::new(),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let ah = head(&mut tx, &ALLOCATION, t, allocation, true).await?;
    if ah.status != HostelAllocationStatus::Active.as_str() {
        return Err(KernelError::Conflict(
            "the residency is no longer active".into(),
        ));
    }
    let overlap: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM campus.resident_leave WHERE allocation_id = $1 \
         AND status IN ('requested', 'approved', 'out', 'overdue') AND from_date <= $3 AND to_date >= $2)",
    )
    .bind(allocation)
    .bind(b.from_date)
    .bind(b.to_date)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if overlap {
        return Err(KernelError::Conflict(
            "an open leave already covers part of these dates".into(),
        ));
    }
    sqlx::query(
        "INSERT INTO campus.resident_leave (id, tenant_id, institution_id, allocation_id, from_date, \
         to_date, destination, reason, process_instance_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(ah.institution.as_uuid())
    .bind(allocation)
    .bind(b.from_date)
    .bind(b.to_date)
    .bind(destination)
    .bind(reason)
    .bind(&pid)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    created(
        &mut tx,
        &scope,
        &LEAVE,
        id,
        ResidentLeaveStatus::Requested.as_str(),
        "campus.resident_leave.request",
        serde_json::json!({ "from": b.from_date, "to": b.to_date }),
        &a,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let mut h = head(&mut tx, &LEAVE, t, id, true).await?;
        let sys = Actor::system(a.correlation_id.clone());
        apply(
            &mut tx,
            &scope,
            &LEAVE,
            &mut h,
            leave_decided(true, None, Some(&pid)),
            &sys,
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(id)
}

/// Opens a hostel complaint ticket for a residency (source `hostel`, `source_ref_id` = the
/// allocation, node = the room, requester = the resident).
pub(crate) async fn open_complaint(
    st: &CampusState,
    ctx: &AuthContext,
    allocation: Uuid,
    b: &ComplaintCreate,
) -> Result<ComplaintView, KernelError> {
    let t = ctx.scope.tenant_id;
    let priority = b.priority()?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let al: HostelAllocationView =
        fetch(&mut tx, ALLOCATION_SELECT, t, allocation, ALLOCATION.entity).await?;
    let inst = InstitutionId(al.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(st, &scope, inst).await?;
    if al.status == HostelAllocationStatus::Closed.as_str() {
        return Err(KernelError::Conflict(
            "the residency is closed; complaints need a live residency".into(),
        ));
    }
    let subject: Option<String> =
        sqlx::query_scalar("SELECT subject_ref FROM campus.hostel_allocation WHERE id = $1")
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
            category: TicketCategory::Hostel,
            source: TicketSource::Hostel,
            source_ref_id: Some(allocation),
            is_complaint: true,
            node_id: Some(InfrastructureNodeId(al.room_node_id)),
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

/// `true` when a damage charge raised for the residency is still unpaid.
async fn unpaid_damage(
    st: &CampusState,
    scope: &ExecutionScope,
    conn: &mut PgConnection,
    allocation: Uuid,
) -> Result<Option<String>, KernelError> {
    let rows = sqlx::query(
        "SELECT charge_id, charge_number FROM campus.room_inspection WHERE allocation_id = $1 \
         AND charge_id IS NOT NULL",
    )
    .bind(allocation)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    for r in rows {
        let id: Uuid = r.try_get("charge_id").map_err(map_db_error)?;
        let number: Option<String> = r.try_get("charge_number").map_err(map_db_error)?;
        if let Some(c) = st.finance.charge(scope, ChargeId(id)).await? {
            if c.outstanding_amount > Decimal::ZERO {
                return Ok(Some(number.unwrap_or(c.charge_number)));
            }
        }
    }
    Ok(None)
}

// ------------------------------------------------------------------ handlers: leave

/// Leaves of a residency (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/hostel-allocations/{id}/leaves", operation_id = "campus_hostel_leave_list", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), StatusFilter), security(("session_cookie" = [])),
    responses((status = 200, body = ResidentLeaveList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn leave_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StatusFilter>,
) -> ApiResult<Json<ResidentLeaveList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let status = status_filter::<ResidentLeaveStatus>(q.status.as_deref())?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    head(&mut conn, &ALLOCATION, ctx.scope.tenant_id, id, false).await?;
    let items = list_of(
        &mut conn,
        LEAVE_SELECT,
        ctx.scope.tenant_id,
        "allocation_id",
        id,
        status,
        "x.from_date DESC",
    )
    .await?;
    Ok(Json(ResidentLeaveList { items }))
}

/// Resident leaves of an institution (`campus.read`; `?status=`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/resident-leaves", operation_id = "campus_hostel_leave_queue", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), StatusFilter), security(("session_cookie" = [])),
    responses((status = 200, body = ResidentLeaveList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn leave_queue(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StatusFilter>,
) -> ApiResult<Json<ResidentLeaveList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let status = status_filter::<ResidentLeaveStatus>(q.status.as_deref())?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let items = list_of(
        &mut conn,
        LEAVE_SELECT,
        ctx.scope.tenant_id,
        "institution_id",
        id,
        status,
        "x.from_date DESC",
    )
    .await?;
    Ok(Json(ResidentLeaveList { items }))
}

/// Request leave for a resident (`campus.hostel.manage`; `RESIDENT_LEAVE_APPROVAL`).
#[utoipa::path(post, path = "/api/v1/campus/hostel-allocations/{id}/leave", operation_id = "campus_hostel_leave_request", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ResidentLeaveCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ResidentLeaveView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn leave_request(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ResidentLeaveCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::HostelManage)?;
    require_idempotency_key(&headers)?;
    let leave = request_leave(&st, ctx, id, b).await?;
    Ok(leave_reply(&st, ctx.scope.tenant_id, leave, StatusCode::CREATED).await?)
}

/// Decide a leave request (`campus.hostel.manage`: routine decision, contract §9).
#[utoipa::path(post, path = "/api/v1/campus/resident-leaves/{id}/decision", operation_id = "campus_hostel_leave_decide", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the leave")),
    request_body = DecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ResidentLeaveView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn leave_decide(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<DecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::HostelManage)?;
    let v = expected!(headers);
    let reason = b.checked_reason()?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut h = head(&mut tx, &LEAVE, t, id, true).await?;
    ensure_version(v, h.version)?;
    step(
        &h,
        if b.approve() {
            ResidentLeaveStatus::Approved
        } else {
            ResidentLeaveStatus::Rejected
        },
    )?;
    let scope = institution_scope(ctx, h.institution);
    apply(
        &mut tx,
        &scope,
        &LEAVE,
        &mut h,
        leave_decided(b.approve(), reason.as_deref(), None),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(leave_reply(&st, t, id, StatusCode::OK).await?)
}

/// The resident leaves the hostel (`campus.hostel.manage`): approved → out, residency → on leave.
#[utoipa::path(post, path = "/api/v1/campus/resident-leaves/{id}/depart", operation_id = "campus_hostel_leave_depart", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the leave")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ResidentLeaveView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn leave_depart(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::HostelManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut h = head(&mut tx, &LEAVE, t, id, true).await?;
    ensure_version(v, h.version)?;
    step(&h, ResidentLeaveStatus::Out)?;
    let alloc: Uuid =
        sqlx::query_scalar("SELECT allocation_id FROM campus.resident_leave WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
    let mut ah = head(&mut tx, &ALLOCATION, t, alloc, true).await?;
    step(&ah, HostelAllocationStatus::OnLeave)?;
    let scope = institution_scope(ctx, h.institution);
    apply(
        &mut tx,
        &scope,
        &LEAVE,
        &mut h,
        Move {
            to: ResidentLeaveStatus::Out.as_str(),
            event: None,
            action: "campus.resident_leave.depart",
            reason: None,
            process_instance_id: None,
        },
        &a,
    )
    .await?;
    sqlx::query("UPDATE campus.resident_leave SET departed_at = now() WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    apply(
        &mut tx,
        &scope,
        &ALLOCATION,
        &mut ah,
        alloc_move(
            HostelAllocationStatus::OnLeave,
            "campus.hostel_allocation.leave",
            None,
        ),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(leave_reply(&st, t, id, StatusCode::OK).await?)
}

/// Verified return (`campus.hostel.manage`): out / overdue → returned (late flag when after the
/// last leave day), residency → active.
#[utoipa::path(post, path = "/api/v1/campus/resident-leaves/{id}/return", operation_id = "campus_hostel_leave_return", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the leave")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ResidentLeaveView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn leave_return(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::HostelManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let inst = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        head(&mut conn, &LEAVE, t, id, false).await?.institution
    };
    let scope = institution_scope(ctx, inst);
    let today = business_today(&st, &scope, inst).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut h = head(&mut tx, &LEAVE, t, id, true).await?;
    ensure_version(v, h.version)?;
    step(&h, ResidentLeaveStatus::Returned)?;
    let r = sqlx::query("SELECT allocation_id, to_date FROM campus.resident_leave WHERE id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let alloc: Uuid = r.try_get("allocation_id").map_err(map_db_error)?;
    let to_date: NaiveDate = r.try_get("to_date").map_err(map_db_error)?;
    let late = returned_late(to_date, today);
    let mut ah = head(&mut tx, &ALLOCATION, t, alloc, true).await?;
    apply(
        &mut tx,
        &scope,
        &LEAVE,
        &mut h,
        Move {
            to: ResidentLeaveStatus::Returned.as_str(),
            event: None,
            action: "campus.resident_leave.return",
            reason: if late { Some("returned late") } else { None },
            process_instance_id: None,
        },
        &a,
    )
    .await?;
    sqlx::query(
        "UPDATE campus.resident_leave SET returned_at = now(), returned_late = $1 WHERE id = $2",
    )
    .bind(late)
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if ah.status == HostelAllocationStatus::OnLeave.as_str() {
        apply(
            &mut tx,
            &scope,
            &ALLOCATION,
            &mut ah,
            alloc_move(
                HostelAllocationStatus::Active,
                "campus.hostel_allocation.return",
                None,
            ),
            &a,
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(leave_reply(&st, t, id, StatusCode::OK).await?)
}

/// Flag residents who are out past their last leave day (`campus.hostel.manage`; bulk: out →
/// overdue).
#[utoipa::path(post, path = "/api/v1/campus/institutions/{id}/resident-leaves/overdue-check", operation_id = "campus_hostel_leave_overdue_check", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = BulkSummary), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn leave_overdue_check(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Json<BulkSummary>> {
    let ctx = auth.require(Permission::HostelManage)?;
    require_idempotency_key(&headers)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let t = ctx.scope.tenant_id;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM campus.resident_leave WHERE tenant_id = $1 AND institution_id = $2 \
         AND status = 'out' AND to_date < $3 ORDER BY to_date LIMIT 1000",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(today)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let sys = Actor::system(actor(ctx).correlation_id);
    let mut out = BulkSummary::default();
    for leave in ids {
        let r: Result<bool, KernelError> = async {
            let mut tx = st.pool.begin().await.map_err(map_db_error)?;
            let mut h = head(&mut tx, &LEAVE, t, leave, true).await?;
            if h.status != ResidentLeaveStatus::Out.as_str() {
                return Ok(false);
            }
            apply(
                &mut tx,
                &scope,
                &LEAVE,
                &mut h,
                Move {
                    to: ResidentLeaveStatus::Overdue.as_str(),
                    event: None,
                    action: "campus.resident_leave.overdue",
                    reason: Some("not back after the last leave day"),
                    process_instance_id: None,
                },
                &sys,
            )
            .await?;
            tx.commit().await.map_err(map_db_error)?;
            Ok(true)
        }
        .await;
        match r {
            Ok(true) => out.processed += 1,
            Ok(false) => out.skipped += 1,
            Err(e) => out.failed.push(BulkFailure {
                id: leave,
                error: e.to_string(),
            }),
        }
    }
    Ok(Json(out))
}

// ------------------------------------------------------------------ handlers: transfers

async fn complete_transfer(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    transfer: Uuid,
    pid: Option<&str>,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let t = scope.tenant_id;
    let mut th = head(conn, &TRANSFER, t, transfer, true).await?;
    step(&th, RoomTransferStatus::Approved)?;
    let r = sqlx::query(
        "SELECT allocation_id, from_bed_id, to_bed_id FROM campus.room_transfer WHERE id = $1",
    )
    .bind(transfer)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let alloc: Uuid = r.try_get("allocation_id").map_err(map_db_error)?;
    let from_bed: Uuid = r.try_get("from_bed_id").map_err(map_db_error)?;
    let to_bed: Uuid = r.try_get("to_bed_id").map_err(map_db_error)?;
    let mut ah = head(conn, &ALLOCATION, t, alloc, true).await?;
    let target = sqlx::query(
        "SELECT b.status, b.room_node_id, n.status AS room_status FROM campus.hostel_bed b \
         JOIN campus.infrastructure_node n ON n.id = b.room_node_id WHERE b.id = $1 FOR UPDATE OF b",
    )
    .bind(to_bed)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let bed_status: String = target.try_get("status").map_err(map_db_error)?;
    let room_status: String = target.try_get("room_status").map_err(map_db_error)?;
    let to_room: Uuid = target.try_get("room_node_id").map_err(map_db_error)?;
    if bed_status != BedStatus::Available.as_str() || room_status != "active" {
        return Err(KernelError::Conflict(format!(
            "the target bed is {bed_status} in a room that is {room_status}; the transfer cannot complete"
        )));
    }
    apply(
        conn,
        scope,
        &TRANSFER,
        &mut th,
        Move {
            to: RoomTransferStatus::Approved.as_str(),
            event: None,
            action: "campus.room_transfer.approve",
            reason,
            process_instance_id: pid,
        },
        actor,
    )
    .await?;
    sqlx::query("SELECT id FROM campus.hostel_bed WHERE id = $1 FOR UPDATE")
        .bind(from_bed)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    move_bed(
        conn,
        scope,
        from_bed,
        BedStatus::Available,
        Some("room transfer"),
        actor,
    )
    .await?;
    move_bed(
        conn,
        scope,
        to_bed,
        BedStatus::Occupied,
        Some("room transfer"),
        actor,
    )
    .await?;
    sqlx::query("UPDATE campus.hostel_allocation SET bed_id = $1, room_node_id = $2 WHERE id = $3")
        .bind(to_bed)
        .bind(to_room)
        .bind(alloc)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    apply(
        conn,
        scope,
        &ALLOCATION,
        &mut ah,
        alloc_move(
            HostelAllocationStatus::Active,
            "campus.hostel_allocation.transfer",
            Some("room transfer completed"),
        ),
        actor,
    )
    .await?;
    apply(
        conn,
        scope,
        &TRANSFER,
        &mut th,
        Move {
            to: RoomTransferStatus::Completed.as_str(),
            event: Some((ROOM_TRANSFER_COMPLETED, ROOM_TRANSFER_COMPLETED_SCHEMA)),
            action: "campus.room_transfer.complete",
            reason: None,
            process_instance_id: pid,
        },
        actor,
    )
    .await?;
    sqlx::query("UPDATE campus.room_transfer SET completed_at = now() WHERE id = $1")
        .bind(transfer)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    Ok(())
}

/// Transfers of a residency (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/hostel-allocations/{id}/transfers", operation_id = "campus_hostel_transfer_list", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = RoomTransferList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn transfer_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<RoomTransferList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    head(&mut conn, &ALLOCATION, ctx.scope.tenant_id, id, false).await?;
    let items = list_of(
        &mut conn,
        TRANSFER_SELECT,
        ctx.scope.tenant_id,
        "allocation_id",
        id,
        None,
        "x.created_at DESC",
    )
    .await?;
    Ok(Json(RoomTransferList { items }))
}

/// Request a room transfer (`campus.hostel.manage`; `ROOM_TRANSFER_APPROVAL`): residency →
/// transfer pending; once approved the beds are swapped and the residency is active again.
#[utoipa::path(post, path = "/api/v1/campus/hostel-allocations/{id}/transfer", operation_id = "campus_hostel_transfer_request", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = RoomTransferCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = RoomTransferView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn transfer_request(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<RoomTransferCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::HostelManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let reason = optional_text("reason", b.reason.clone(), 1000)?;
    let ah = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        head(&mut conn, &ALLOCATION, t, id, false).await?
    };
    let scope = institution_scope(ctx, ah.institution);
    require_institution_writable(&st, &scope, ah.institution).await?;
    step(&ah, HostelAllocationStatus::TransferPending)?;
    let transfer = Uuid::now_v7();
    let outcome = request_approval(
        &st,
        &scope,
        process::ROOM_TRANSFER_APPROVAL,
        business_key("room_transfer", transfer, 1),
        BTreeMap::new(),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut ah = head(&mut tx, &ALLOCATION, t, id, true).await?;
    step(&ah, HostelAllocationStatus::TransferPending)?;
    let from_bed: Uuid =
        sqlx::query_scalar("SELECT bed_id FROM campus.hostel_allocation WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
    let target: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT b.institution_id, b.status FROM campus.hostel_bed b WHERE b.id = $1 AND b.tenant_id = $2",
    )
    .bind(b.to_bed_id)
    .bind(t.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    match target {
        Some((inst, _)) if inst != ah.institution.as_uuid() => {
            return Err(invalid(
                "toBedId",
                "unknown_bed",
                "no bed with this id in the institution",
            )
            .into())
        }
        None => {
            return Err(invalid(
                "toBedId",
                "unknown_bed",
                "no bed with this id in the institution",
            )
            .into())
        }
        Some((_, status)) if status != BedStatus::Available.as_str() => {
            return Err(KernelError::Conflict(format!("the target bed is {status}")).into())
        }
        _ => {}
    }
    if b.to_bed_id == from_bed {
        return Err(invalid("toBedId", "same_bed", "the resident already has this bed").into());
    }
    sqlx::query(
        "INSERT INTO campus.room_transfer (id, tenant_id, institution_id, allocation_id, from_bed_id, \
         to_bed_id, reason, process_instance_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(transfer)
    .bind(t.as_uuid())
    .bind(ah.institution.as_uuid())
    .bind(id)
    .bind(from_bed)
    .bind(b.to_bed_id)
    .bind(reason.as_deref())
    .bind(&pid)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    created(
        &mut tx,
        &scope,
        &TRANSFER,
        transfer,
        RoomTransferStatus::Requested.as_str(),
        "campus.room_transfer.request",
        serde_json::json!({ "fromBed": from_bed, "toBed": b.to_bed_id }),
        &a,
    )
    .await?;
    apply(
        &mut tx,
        &scope,
        &ALLOCATION,
        &mut ah,
        alloc_move(
            HostelAllocationStatus::TransferPending,
            "campus.hostel_allocation.request_transfer",
            reason.as_deref(),
        ),
        &a,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let sys = Actor::system(a.correlation_id.clone());
        complete_transfer(&mut tx, &scope, transfer, Some(&pid), None, &sys).await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(transfer_reply(&st, t, transfer, StatusCode::CREATED).await?)
}

/// Decide a transfer (`campus.approve`): approve → completed (bed swap); reject → residency active.
#[utoipa::path(post, path = "/api/v1/campus/room-transfers/{id}/decision", operation_id = "campus_hostel_transfer_decide", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the transfer")),
    request_body = DecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RoomTransferView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn transfer_decide(
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
    let mut h = head(&mut tx, &TRANSFER, t, id, true).await?;
    ensure_version(v, h.version)?;
    let scope = institution_scope(ctx, h.institution);
    if b.approve() {
        complete_transfer(&mut tx, &scope, id, None, reason.as_deref(), &a).await?;
    } else {
        step(&h, RoomTransferStatus::Rejected)?;
        let alloc: Uuid =
            sqlx::query_scalar("SELECT allocation_id FROM campus.room_transfer WHERE id = $1")
                .bind(id)
                .fetch_one(&mut *tx)
                .await
                .map_err(map_db_error)?;
        let mut ah = head(&mut tx, &ALLOCATION, t, alloc, true).await?;
        apply(
            &mut tx,
            &scope,
            &TRANSFER,
            &mut h,
            Move {
                to: RoomTransferStatus::Rejected.as_str(),
                event: None,
                action: "campus.room_transfer.reject",
                reason: reason.as_deref(),
                process_instance_id: None,
            },
            &a,
        )
        .await?;
        if ah.status == HostelAllocationStatus::TransferPending.as_str() {
            apply(
                &mut tx,
                &scope,
                &ALLOCATION,
                &mut ah,
                alloc_move(
                    HostelAllocationStatus::Active,
                    "campus.hostel_allocation.transfer_rejected",
                    reason.as_deref(),
                ),
                &a,
            )
            .await?;
        }
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(transfer_reply(&st, t, id, StatusCode::OK).await?)
}

// ------------------------------------------------------------------ handlers: inspections

/// Inspections of a hostel room (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/hostel-rooms/{id}/inspections", operation_id = "campus_hostel_inspection_list", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), StatusFilter), security(("session_cookie" = [])),
    responses((status = 200, body = RoomInspectionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn inspection_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StatusFilter>,
) -> ApiResult<Json<RoomInspectionList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let status = status_filter::<InspectionStatus>(q.status.as_deref())?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let items = list_of(
        &mut conn,
        INSPECTION_SELECT,
        ctx.scope.tenant_id,
        "room_node_id",
        id,
        status,
        "x.scheduled_on DESC",
    )
    .await?;
    Ok(Json(RoomInspectionList { items }))
}

/// Inspections of an institution (`campus.read`; `?status=`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/room-inspections", operation_id = "campus_hostel_inspection_queue", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), StatusFilter), security(("session_cookie" = [])),
    responses((status = 200, body = RoomInspectionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn inspection_queue(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StatusFilter>,
) -> ApiResult<Json<RoomInspectionList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let status = status_filter::<InspectionStatus>(q.status.as_deref())?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let items = list_of(
        &mut conn,
        INSPECTION_SELECT,
        ctx.scope.tenant_id,
        "institution_id",
        id,
        status,
        "x.scheduled_on DESC",
    )
    .await?;
    Ok(Json(RoomInspectionList { items }))
}

/// Schedule a room inspection (`campus.hostel.manage`).
#[utoipa::path(post, path = "/api/v1/campus/hostel-rooms/{id}/inspections", operation_id = "campus_hostel_inspection_create", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = RoomInspectionCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = RoomInspectionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn inspection_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<RoomInspectionCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::HostelManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let inst = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_node(&mut conn, t, InfrastructureNodeId(id))
            .await?
            .ok_or(KernelError::NotFound {
                entity: "hostel room",
            })?
            .institution_id
    };
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let room = lock_node(&mut tx, t, InfrastructureNodeId(id)).await?;
    if room.node_type != InfrastructureNodeType::Space
        || room.space_type != Some(SpaceType::HostelRoom)
    {
        return Err(invalid(
            "id",
            "not_a_hostel_room",
            "inspections are of hostel_room spaces",
        )
        .into());
    }
    if let Some(alloc) = b.allocation_id {
        let ok: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM campus.hostel_allocation WHERE id = $1 AND tenant_id = $2 \
             AND room_node_id = $3 AND status <> 'closed')",
        )
        .bind(alloc)
        .bind(t.as_uuid())
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if !ok {
            return Err(invalid(
                "allocationId",
                "not_in_room",
                "no live residency with this id in the room",
            )
            .into());
        }
    }
    let insp: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.room_inspection (tenant_id, institution_id, room_node_id, allocation_id, \
         scheduled_on) VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(id)
    .bind(b.allocation_id)
    .bind(b.scheduled_on)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    created(
        &mut tx,
        &scope,
        &INSPECTION,
        insp,
        InspectionStatus::Scheduled.as_str(),
        "campus.room_inspection.schedule",
        serde_json::json!({ "room": room.code, "scheduledOn": b.scheduled_on }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(inspection_reply(&st, t, insp, StatusCode::CREATED).await?)
}

/// Record the inspection outcome (`campus.hostel.manage`): no damage → completed; damage →
/// damage found and, for a student resident, the `HOSTEL-DAMAGE` charge → charged.
#[utoipa::path(post, path = "/api/v1/campus/room-inspections/{id}/complete", operation_id = "campus_hostel_inspection_complete", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the inspection")),
    request_body = RoomInspectionComplete, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RoomInspectionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn inspection_complete(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<RoomInspectionComplete>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::HostelManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let findings = optional_text("findings", b.findings.clone(), 4000)?;
    let damage = match b.damage_amount.as_deref() {
        Some(d) => money("damageAmount", d)?,
        None => Decimal::ZERO,
    };
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut h = head(&mut tx, &INSPECTION, t, id, true).await?;
    ensure_version(v, h.version)?;
    let to = if damage > Decimal::ZERO {
        InspectionStatus::DamageFound
    } else {
        InspectionStatus::Completed
    };
    step(&h, to)?;
    let scope = institution_scope(ctx, h.institution);
    sqlx::query(
        "UPDATE campus.room_inspection SET findings = $1, damage_amount = $2, completed_at = now() \
         WHERE id = $3",
    )
    .bind(findings.as_deref())
    .bind(if damage > Decimal::ZERO { Some(damage) } else { None })
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    apply(
        &mut tx,
        &scope,
        &INSPECTION,
        &mut h,
        Move {
            to: to.as_str(),
            event: Some((ROOM_INSPECTION_COMPLETED, ROOM_INSPECTION_COMPLETED_SCHEMA)),
            action: "campus.room_inspection.complete",
            reason: findings.as_deref(),
            process_instance_id: None,
        },
        &a,
    )
    .await?;
    if to == InspectionStatus::DamageFound {
        let resident = sqlx::query(
            "SELECT a.person_kind, a.person_id, a.person_name FROM campus.room_inspection i \
             JOIN campus.hostel_allocation a ON a.id = i.allocation_id WHERE i.id = $1",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if let Some(r) = resident {
            let kind: String = r.try_get("person_kind").map_err(map_db_error)?;
            let person: Uuid = r.try_get("person_id").map_err(map_db_error)?;
            let name: String = r.try_get("person_name").map_err(map_db_error)?;
            let today = business_today(&st, &scope, h.institution).await?;
            if let Some(charge) = raise_charge(
                &st,
                &scope,
                ChargeAsk {
                    institution: h.institution,
                    person: (kind.as_str(), person),
                    fee_code: HOSTEL_DAMAGE,
                    category: CampusChargeCategory::Hostel,
                    description: format!(
                        "Hostel room damage — {name} ({})",
                        h.number.clone().unwrap_or_default()
                    ),
                    amount: damage,
                    due_date: Some(today),
                    source_id: id,
                },
                &a,
            )
            .await?
            {
                sqlx::query(
                    "UPDATE campus.room_inspection SET charge_id = $1, charge_number = $2 WHERE id = $3",
                )
                .bind(charge.charge_id.as_uuid())
                .bind(&charge.charge_number)
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(map_db_error)?;
                apply(
                    &mut tx,
                    &scope,
                    &INSPECTION,
                    &mut h,
                    Move {
                        to: InspectionStatus::Charged.as_str(),
                        event: None,
                        action: "campus.room_inspection.charge",
                        reason: Some(charge.charge_number.as_str()),
                        process_instance_id: None,
                    },
                    &a,
                )
                .await?;
            }
        }
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(inspection_reply(&st, t, id, StatusCode::OK).await?)
}

/// Close an inspection (`campus.hostel.manage`).
#[utoipa::path(post, path = "/api/v1/campus/room-inspections/{id}/close", operation_id = "campus_hostel_inspection_close", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the inspection")),
    request_body = ReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RoomInspectionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn inspection_close(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::HostelManage)?;
    let v = expected!(headers);
    let reason = b.text()?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut h = head(&mut tx, &INSPECTION, t, id, true).await?;
    ensure_version(v, h.version)?;
    step(&h, InspectionStatus::Closed)?;
    let scope = institution_scope(ctx, h.institution);
    apply(
        &mut tx,
        &scope,
        &INSPECTION,
        &mut h,
        Move {
            to: InspectionStatus::Closed.as_str(),
            event: None,
            action: "campus.room_inspection.close",
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(inspection_reply(&st, t, id, StatusCode::OK).await?)
}

// ------------------------------------------------------------------ handlers: vacating

/// Start vacating (`campus.hostel.manage`): active → vacating with the last day.
#[utoipa::path(post, path = "/api/v1/campus/hostel-allocations/{id}/vacate", operation_id = "campus_hostel_allocation_vacate", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the residency")),
    request_body = HostelVacateBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = HostelAllocationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn allocation_vacate(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<HostelVacateBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::HostelManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let reason = optional_text("reason", b.reason.clone(), 1000)?;
    let a = actor(ctx);
    let inst = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        head(&mut conn, &ALLOCATION, t, id, false)
            .await?
            .institution
    };
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let end = match b.end_date {
        Some(d) => d,
        None => business_today(&st, &scope, inst).await?,
    };
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut h = head(&mut tx, &ALLOCATION, t, id, true).await?;
    ensure_version(v, h.version)?;
    step(&h, HostelAllocationStatus::Vacating)?;
    let start: NaiveDate =
        sqlx::query_scalar("SELECT start_date FROM campus.hostel_allocation WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
    if end < start {
        return Err(invalid(
            "endDate",
            "before_start",
            "the last day is before the residency start",
        )
        .into());
    }
    sqlx::query("UPDATE campus.hostel_allocation SET end_date = $1 WHERE id = $2")
        .bind(end)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    apply(
        &mut tx,
        &scope,
        &ALLOCATION,
        &mut h,
        alloc_move(
            HostelAllocationStatus::Vacating,
            "campus.hostel_allocation.vacate",
            reason.as_deref(),
        ),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(allocation_reply(&st, t, id, StatusCode::OK).await?)
}

/// Clear and close a vacating residency (`campus.hostel.manage`): no inspection still open with
/// damage, no unpaid damage charge; the bed is released, the mess allocation ends and the
/// oldest waitlisted application is offered the vacancy.
#[utoipa::path(post, path = "/api/v1/campus/hostel-allocations/{id}/close", operation_id = "campus_hostel_allocation_close", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the residency")),
    request_body = ReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = HostelAllocationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn allocation_close(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::HostelManage)?;
    let v = expected!(headers);
    let reason = b.text()?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut h = head(&mut tx, &ALLOCATION, t, id, true).await?;
    ensure_version(v, h.version)?;
    step(&h, HostelAllocationStatus::Closed)?;
    let scope = institution_scope(ctx, h.institution);
    let open_damage: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM campus.room_inspection WHERE allocation_id = $1 \
         AND status IN ('scheduled', 'damage_found'))",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if open_damage {
        return Err(KernelError::Conflict(
            "clearance refused: an inspection of this residency is still open or has unbilled damage"
                .into(),
        )
        .into());
    }
    if let Some(charge) = unpaid_damage(&st, &scope, &mut tx, id).await? {
        return Err(KernelError::Conflict(format!(
            "clearance refused: damage charge {charge} is unpaid"
        ))
        .into());
    }
    let bed: Uuid = sqlx::query_scalar("SELECT bed_id FROM campus.hostel_allocation WHERE id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    sqlx::query("SELECT id FROM campus.hostel_bed WHERE id = $1 FOR UPDATE")
        .bind(bed)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    move_bed(
        &mut tx,
        &scope,
        bed,
        BedStatus::Available,
        Some("residency closed"),
        &a,
    )
    .await?;
    let mess: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM campus.mess_allocation WHERE hostel_allocation_id = $1 AND status = 'active' FOR UPDATE",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;
    for m in mess {
        let mut mh = head(&mut tx, &super::mess::MESS_ALLOCATION, t, m, true).await?;
        sqlx::query("UPDATE campus.mess_allocation SET end_date = COALESCE(end_date, current_date) WHERE id = $1")
            .bind(m)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        apply(
            &mut tx,
            &scope,
            &super::mess::MESS_ALLOCATION,
            &mut mh,
            Move {
                to: MessAllocationStatus::Ended.as_str(),
                event: None,
                action: "campus.mess_allocation.end",
                reason: Some("residency closed"),
                process_instance_id: None,
            },
            &a,
        )
        .await?;
    }
    sqlx::query("UPDATE campus.hostel_allocation SET closed_at = now() WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    apply(
        &mut tx,
        &scope,
        &ALLOCATION,
        &mut h,
        Move {
            to: HostelAllocationStatus::Closed.as_str(),
            event: Some((HOSTEL_ALLOCATION_CLOSED, HOSTEL_ALLOCATION_CLOSED_SCHEMA)),
            action: "campus.hostel_allocation.close",
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &a,
    )
    .await?;
    offer_vacancy(&mut tx, &scope, h.institution, &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(allocation_reply(&st, t, id, StatusCode::OK).await?)
}

// ------------------------------------------------------------------ handlers: complaints

/// Complaints raised for a residency (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/hostel-allocations/{id}/complaints", operation_id = "campus_hostel_complaint_list", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ComplaintList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn complaint_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ComplaintList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    head(&mut conn, &ALLOCATION, ctx.scope.tenant_id, id, false).await?;
    Ok(Json(
        complaints_of(&mut conn, ctx.scope.tenant_id, "hostel", id).await?,
    ))
}

/// Raise a hostel complaint for a resident (`campus.hostel.manage`): a service desk ticket
/// (source `hostel`) followed in the facilities service desk.
#[utoipa::path(post, path = "/api/v1/campus/hostel-allocations/{id}/complaints", operation_id = "campus_hostel_complaint_create", tag = "campus-hostel",
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
    let ctx = auth.require(Permission::HostelManage)?;
    require_idempotency_key(&headers)?;
    let c = open_complaint(&st, ctx, id, &b).await?;
    Ok(reply(StatusCode::CREATED, &c, c.version))
}
