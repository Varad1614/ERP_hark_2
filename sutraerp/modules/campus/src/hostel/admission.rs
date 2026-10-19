//! Hostel admission → allocation (L3-G-078 application → decision (`HOSTEL_ADMISSION`, the
//! Batch 8 reference approval), G-079 approved admission → active residency with the hostel fee
//! raised through `CampusChargePort`, G-080 no free bed → waiting list, auto-offer on vacancy),
//! withdrawal / rejection.

use super::common::{
    apply, created, ensure_version, expected, fetch, head, invalid, list_of, optional_text,
    person_in, person_kind, raise_charge, reply, status_filter, step, Agg, ChargeAsk, DecisionBody,
    Head, Move, ReasonBody, StatusFilter,
};
use super::domain::{BedStatus, HostelAllocationStatus, HostelApplicationStatus};
use super::policy::{active_policy, BED};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::actor;
use crate::shared::me::PersonRef;
use crate::shared::numbering::{self, kind};
use crate::shared::publish;
use crate::shared::scope::{institution_scope, tenant_scope};
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
    HOSTEL_ALLOCATION_ACTIVATED, HOSTEL_ALLOCATION_ACTIVATED_SCHEMA, HOSTEL_APPLICATION_DECIDED,
    HOSTEL_APPLICATION_DECIDED_SCHEMA,
};
use sutra_contracts::campus::events::StatusChangedV1;
use sutra_contracts::finance::ports::CampusChargeCategory;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use uuid::Uuid;

/// Fee code of the hostel fee charge (contract §7).
pub const HOSTEL_FEE: &str = "HOSTEL-FEE";

pub(crate) const APPLICATION: Agg = Agg {
    table: "hostel_application",
    entity: "hostel application",
    number: "number",
    subject: "subject_ref",
};

pub(crate) const ALLOCATION: Agg = Agg {
    table: "hostel_allocation",
    entity: "hostel allocation",
    number: "person_number",
    subject: "subject_ref",
};

// ------------------------------------------------------------------ views

/// A hostel admission application.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct HostelApplicationView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "HAP-000001")]
    pub number: String,
    /// `student | staff`.
    pub person_kind: String,
    pub person_id: Uuid,
    pub person_number: String,
    pub person_name: String,
    pub policy_id: Uuid,
    pub preference: Option<String>,
    /// `self_service | office`.
    pub channel: String,
    /// `submitted | under_review | approved | waitlisted | rejected | withdrawn | allocated`.
    pub status: String,
    pub status_reason: Option<String>,
    /// 9E: approvalState — canonical approval state (`not_requested | pending | approved |
    /// rejected`, contract batch9 §6.4), derived from the status.
    pub approval_state: String,
    pub process_instance_id: Option<String>,
    pub waitlisted_at: Option<DateTime<Utc>>,
    pub decided_at: Option<DateTime<Utc>>,
    pub allocation_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

pub(crate) const APPLICATION_SELECT: &str =
    "SELECT x.id, x.institution_id, x.number, x.person_kind, \
    x.person_id, x.person_number, x.person_name, x.policy_id, x.preference, x.channel, x.status, \
    x.status_reason, x.process_instance_id, x.waitlisted_at, x.decided_at, x.allocation_id, \
    x.created_at, x.version, \
    CASE WHEN x.status IN ('submitted', 'under_review') THEN 'pending' WHEN x.status IN ('approved', 'waitlisted', 'allocated') THEN 'approved' WHEN x.status = 'rejected' THEN 'rejected' ELSE 'not_requested' END AS approval_state /* 9E: approvalState */ FROM campus.hostel_application x";

/// Applications of an institution.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HostelApplicationList {
    pub items: Vec<HostelApplicationView>,
}

/// An application made by the office on behalf of a student / staff member.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HostelApplicationCreate {
    /// `student | staff`.
    pub person_kind: String,
    pub person_id: Uuid,
    pub preference: Option<String>,
}

/// Allocate a bed to an approved (or waitlisted) application.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HostelAllocateBody {
    /// A specific free bed; otherwise the first free bed of an active hostel room.
    pub bed_id: Option<Uuid>,
    /// Residency start (default: today in the institution timezone).
    pub start_date: Option<NaiveDate>,
}

/// A residency (bed allocation).
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct HostelAllocationView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub application_id: Uuid,
    pub person_kind: String,
    pub person_id: Uuid,
    pub person_number: String,
    pub person_name: String,
    pub policy_id: Uuid,
    pub bed_id: Uuid,
    pub bed_code: String,
    pub room_node_id: Uuid,
    #[schema(example = "HST-B1-101")]
    pub room_code: String,
    pub start_date: NaiveDate,
    pub end_date: Option<NaiveDate>,
    /// `active | on_leave | transfer_pending | vacating | closed`.
    pub status: String,
    pub status_reason: Option<String>,
    /// Hostel fee raised on allocation (students; staff: recorded only).
    #[schema(example = "45000.00")]
    pub fee_amount: String,
    pub charge_id: Option<Uuid>,
    pub charge_number: Option<String>,
    pub closed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

pub(crate) const ALLOCATION_SELECT: &str = "SELECT x.id, x.institution_id, x.application_id, \
    x.person_kind, x.person_id, x.person_number, x.person_name, x.policy_id, x.bed_id, \
    b.bed_code, x.room_node_id, n.code AS room_code, x.start_date, x.end_date, x.status, \
    x.status_reason, x.fee_amount::text AS fee_amount, x.charge_id, x.charge_number, x.closed_at, \
    x.created_at, x.version FROM campus.hostel_allocation x \
    JOIN campus.hostel_bed b ON b.id = x.bed_id \
    JOIN campus.infrastructure_node n ON n.id = x.room_node_id";

/// Residencies of an institution.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HostelAllocationList {
    pub items: Vec<HostelAllocationView>,
}

// ------------------------------------------------------------------ shared logic

pub(crate) async fn application_reply(
    st: &CampusState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v: HostelApplicationView = fetch(
        &mut conn,
        APPLICATION_SELECT,
        tenant,
        id,
        APPLICATION.entity,
    )
    .await?;
    Ok(reply(status, &v, v.version))
}

pub(crate) async fn allocation_reply(
    st: &CampusState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v: HostelAllocationView =
        fetch(&mut conn, ALLOCATION_SELECT, tenant, id, ALLOCATION.entity).await?;
    Ok(reply(status, &v, v.version))
}

fn decided<'a>(
    to: HostelApplicationStatus,
    reason: Option<&'a str>,
    pid: Option<&'a str>,
) -> Move<'a> {
    Move {
        to: to.as_str(),
        event: Some((
            HOSTEL_APPLICATION_DECIDED,
            HOSTEL_APPLICATION_DECIDED_SCHEMA,
        )),
        action: match to {
            HostelApplicationStatus::Approved => "campus.hostel_application.approve",
            HostelApplicationStatus::Rejected => "campus.hostel_application.reject",
            _ => "campus.hostel_application.waitlist",
        },
        reason,
        process_instance_id: pid,
    }
}

async fn stamp_decided(conn: &mut PgConnection, id: Uuid) -> Result<(), KernelError> {
    sqlx::query("UPDATE campus.hostel_application SET decided_at = now() WHERE id = $1")
        .bind(id)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    Ok(())
}

/// Submits an application for `person` (office or self-service) and runs the
/// `HOSTEL_ADMISSION` approval step (auto-approved in Batch 8). Returns the application id.
pub(crate) async fn submit_application(
    st: &CampusState,
    ctx: &AuthContext,
    institution: InstitutionId,
    person: &PersonRef,
    preference: Option<String>,
    channel: &'static str,
) -> Result<Uuid, KernelError> {
    let t = ctx.scope.tenant_id;
    let scope = institution_scope(ctx, institution);
    require_institution_writable(st, &scope, institution).await?;
    let preference = optional_text("preference", preference, 500)?;
    let policy = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let live: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM campus.hostel_allocation WHERE tenant_id = $1 \
             AND person_kind = $2 AND person_id = $3 AND status <> 'closed')",
        )
        .bind(t.as_uuid())
        .bind(person.kind.as_str())
        .bind(person.id)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
        if live {
            return Err(KernelError::Conflict(format!(
                "{} already has a live hostel residency",
                person.number
            )));
        }
        active_policy(&mut conn, t, institution)
            .await?
            .ok_or_else(|| {
                KernelError::Conflict(
                    "the institution has no active hostel policy; admissions are closed".into(),
                )
            })?
    };
    let id = Uuid::now_v7();
    let mut vars = BTreeMap::new();
    vars.insert(
        "personKind".to_owned(),
        serde_json::json!(person.kind.as_str()),
    );
    vars.insert("personNumber".to_owned(), serde_json::json!(person.number));
    let outcome = request_approval(
        st,
        &scope,
        process::HOSTEL_ADMISSION,
        business_key("hostel_application", id, 1),
        vars,
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let number = numbering::next(&mut tx, t, institution, kind::HOSTEL_APPLICATION).await?;
    sqlx::query(
        "INSERT INTO campus.hostel_application (id, tenant_id, institution_id, number, person_kind, \
         person_id, person_number, person_name, subject_ref, policy_id, preference, channel) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(institution.as_uuid())
    .bind(&number)
    .bind(person.kind.as_str())
    .bind(person.id)
    .bind(&person.number)
    .bind(&person.display_name)
    .bind(person.subject_ref.as_deref())
    .bind(policy.id)
    .bind(preference)
    .bind(channel)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    created(
        &mut tx,
        &scope,
        &APPLICATION,
        id,
        "submitted",
        "campus.hostel_application.submit",
        serde_json::json!({ "number": number, "person": person.number, "channel": channel }),
        &a,
    )
    .await?;
    let mut h = head(&mut tx, &APPLICATION, t, id, true).await?;
    apply(
        &mut tx,
        &scope,
        &APPLICATION,
        &mut h,
        Move {
            to: HostelApplicationStatus::UnderReview.as_str(),
            event: None,
            action: "campus.hostel_application.review",
            reason: None,
            process_instance_id: Some(&pid),
        },
        &a,
    )
    .await?;
    sqlx::query("UPDATE campus.hostel_application SET process_instance_id = $1 WHERE id = $2")
        .bind(&pid)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let sys = Actor::system(a.correlation_id.clone());
        apply(
            &mut tx,
            &scope,
            &APPLICATION,
            &mut h,
            decided(HostelApplicationStatus::Approved, None, Some(&pid)),
            &sys,
        )
        .await?;
        stamp_decided(&mut tx, id).await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(id)
}

/// Withdraws an open application (office or the applicant).
pub(crate) async fn withdraw_application(
    st: &CampusState,
    ctx: &AuthContext,
    id: Uuid,
    expected_version: i64,
    reason: Option<&str>,
) -> Result<(), KernelError> {
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut h = head(&mut tx, &APPLICATION, t, id, true).await?;
    ensure_version(expected_version, h.version)?;
    step(&h, HostelApplicationStatus::Withdrawn)?;
    let scope = institution_scope(ctx, h.institution);
    apply(
        &mut tx,
        &scope,
        &APPLICATION,
        &mut h,
        Move {
            to: HostelApplicationStatus::Withdrawn.as_str(),
            event: None,
            action: "campus.hostel_application.withdraw",
            reason,
            process_instance_id: None,
        },
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(())
}

/// Offers the next free bed to the oldest waitlisted application of the institution (waitlisted →
/// approved, `hostel_application.decided` so the applicant is notified; the office then allocates).
pub(crate) async fn offer_vacancy(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    institution: InstitutionId,
    actor: &Actor,
) -> Result<Option<Uuid>, KernelError> {
    let next: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM campus.hostel_application WHERE tenant_id = $1 AND institution_id = $2 \
         AND status = 'waitlisted' ORDER BY waitlisted_at, created_at LIMIT 1 FOR UPDATE SKIP LOCKED",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(institution.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let Some(id) = next else {
        return Ok(None);
    };
    let mut h = head(conn, &APPLICATION, scope.tenant_id, id, true).await?;
    apply(
        conn,
        scope,
        &APPLICATION,
        &mut h,
        decided(
            HostelApplicationStatus::Approved,
            Some("a bed became available: offered from the waiting list"),
            None,
        ),
        actor,
    )
    .await?;
    Ok(Some(id))
}

/// The bed to allocate (locked): the requested one (must be available in an active room of the
/// institution) or the first free one.
async fn pick_bed(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    requested: Option<Uuid>,
) -> Result<Option<(Uuid, Uuid)>, KernelError> {
    match requested {
        Some(bed) => {
            let r = sqlx::query(
                "SELECT b.id, b.room_node_id, b.status, n.status AS room_status FROM campus.hostel_bed b \
                 JOIN campus.infrastructure_node n ON n.id = b.room_node_id \
                 WHERE b.id = $1 AND b.tenant_id = $2 AND b.institution_id = $3 FOR UPDATE OF b",
            )
            .bind(bed)
            .bind(tenant.as_uuid())
            .bind(institution.as_uuid())
            .fetch_optional(&mut *conn)
            .await
            .map_err(map_db_error)?
            .ok_or_else(|| invalid("bedId", "unknown_bed", "no bed with this id in the institution"))?;
            let status: String = r.try_get("status").map_err(map_db_error)?;
            let room_status: String = r.try_get("room_status").map_err(map_db_error)?;
            if status != BedStatus::Available.as_str() || room_status != "active" {
                return Err(KernelError::Conflict(format!(
                    "the bed is {status} in a room that is {room_status}; it cannot be allocated"
                )));
            }
            Ok(Some((
                r.try_get("id").map_err(map_db_error)?,
                r.try_get("room_node_id").map_err(map_db_error)?,
            )))
        }
        None => {
            let r = sqlx::query(
                "SELECT b.id, b.room_node_id FROM campus.hostel_bed b \
                 JOIN campus.infrastructure_node n ON n.id = b.room_node_id \
                 WHERE b.tenant_id = $1 AND b.institution_id = $2 AND b.status = 'available' \
                 AND n.status = 'active' ORDER BY n.code, b.bed_code LIMIT 1 FOR UPDATE OF b SKIP LOCKED",
            )
            .bind(tenant.as_uuid())
            .bind(institution.as_uuid())
            .fetch_optional(&mut *conn)
            .await
            .map_err(map_db_error)?;
            match r {
                None => Ok(None),
                Some(r) => Ok(Some((
                    r.try_get("id").map_err(map_db_error)?,
                    r.try_get("room_node_id").map_err(map_db_error)?,
                ))),
            }
        }
    }
}

/// Moves a locked bed (its own row lock is already held by the caller).
pub(crate) async fn move_bed(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    bed: Uuid,
    to: BedStatus,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let mut h: Head = head(conn, &BED, scope.tenant_id, bed, true).await?;
    step(&h, to)?;
    apply(
        conn,
        scope,
        &BED,
        &mut h,
        Move {
            to: to.as_str(),
            event: None,
            action: match to {
                BedStatus::Occupied => "campus.hostel_bed.occupy",
                BedStatus::Available => "campus.hostel_bed.release",
                BedStatus::OutOfService => "campus.hostel_bed.out_of_service",
            },
            reason,
            process_instance_id: None,
        },
        actor,
    )
    .await?;
    Ok(())
}

// ------------------------------------------------------------------ handlers

/// Applications of an institution (`campus.read`; `?status=`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/hostel-applications", operation_id = "campus_hostel_application_list", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), StatusFilter), security(("session_cookie" = [])),
    responses((status = 200, body = HostelApplicationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn application_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StatusFilter>,
) -> ApiResult<Json<HostelApplicationList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let status = status_filter::<HostelApplicationStatus>(q.status.as_deref())?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let items = list_of(
        &mut conn,
        APPLICATION_SELECT,
        ctx.scope.tenant_id,
        "institution_id",
        id,
        status,
        "x.created_at DESC",
    )
    .await?;
    Ok(Json(HostelApplicationList { items }))
}

/// Apply on behalf of a student / staff member (`campus.hostel.manage`).
#[utoipa::path(post, path = "/api/v1/campus/institutions/{id}/hostel-applications", operation_id = "campus_hostel_application_create", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = HostelApplicationCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = HostelApplicationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn application_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<HostelApplicationCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::HostelManage)?;
    require_idempotency_key(&headers)?;
    let inst = InstitutionId(id);
    let kind = person_kind(&b.person_kind)?;
    let person = person_in(&st, &tenant_scope(ctx), kind, b.person_id, inst, "personId").await?;
    let app = submit_application(&st, ctx, inst, &person, b.preference, "office").await?;
    Ok(application_reply(&st, ctx.scope.tenant_id, app, StatusCode::CREATED).await?)
}

/// One application (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/hostel-applications/{id}", operation_id = "campus_hostel_application_get", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = HostelApplicationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn application_get(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusRead)?;
    Ok(application_reply(&st, ctx.scope.tenant_id, id, StatusCode::OK).await?)
}

/// Decide an application under review (`campus.approve`, step-up; `HOSTEL_ADMISSION`).
#[utoipa::path(post, path = "/api/v1/campus/hostel-applications/{id}/decision", operation_id = "campus_hostel_application_decide", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the application")),
    request_body = DecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = HostelApplicationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn application_decide(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<DecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusApprove)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "hostel application").await?;
    let v = expected!(headers);
    let reason = b.checked_reason()?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let to = if b.approve() {
        HostelApplicationStatus::Approved
    } else {
        HostelApplicationStatus::Rejected
    };
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut h = head(&mut tx, &APPLICATION, t, id, true).await?;
    ensure_version(v, h.version)?;
    step(&h, to)?;
    let scope = institution_scope(ctx, h.institution);
    apply(
        &mut tx,
        &scope,
        &APPLICATION,
        &mut h,
        decided(to, reason.as_deref(), None),
        &a,
    )
    .await?;
    stamp_decided(&mut tx, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(application_reply(&st, t, id, StatusCode::OK).await?)
}

/// Allocate a bed (`campus.hostel.manage`): approved / waitlisted → allocated with an active
/// residency and the hostel fee (`HOSTEL-FEE`, students) raised through `CampusChargePort`; an
/// approved application with no free bed goes to the waiting list (G-080).
#[utoipa::path(post, path = "/api/v1/campus/hostel-applications/{id}/allocate", operation_id = "campus_hostel_application_allocate", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the application")),
    request_body = HostelAllocateBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = HostelApplicationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn application_allocate(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<HostelAllocateBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::HostelManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let inst = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        head(&mut conn, &APPLICATION, t, id, false)
            .await?
            .institution
    };
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let start = b.start_date.unwrap_or(today);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut h = head(&mut tx, &APPLICATION, t, id, true).await?;
    ensure_version(v, h.version)?;
    step(&h, HostelApplicationStatus::Allocated)?;
    let app: HostelApplicationView =
        fetch(&mut tx, APPLICATION_SELECT, t, id, APPLICATION.entity).await?;
    let Some((bed, room)) = pick_bed(&mut tx, t, inst, b.bed_id).await? else {
        if h.status == HostelApplicationStatus::Waitlisted.as_str() {
            return Err(KernelError::Conflict(format!(
                "no free bed for {}; the application stays on the waiting list",
                app.number
            ))
            .into());
        }
        apply(
            &mut tx,
            &scope,
            &APPLICATION,
            &mut h,
            decided(
                HostelApplicationStatus::Waitlisted,
                Some("no free bed: placed on the waiting list"),
                None,
            ),
            &a,
        )
        .await?;
        sqlx::query("UPDATE campus.hostel_application SET waitlisted_at = now() WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        tx.commit().await.map_err(map_db_error)?;
        return Ok(application_reply(&st, t, id, StatusCode::OK).await?);
    };
    let policy = active_policy(&mut tx, t, inst).await?;
    let (policy_id, fee) = match &policy {
        Some(p) => (
            p.id,
            crate::shared::decimal::parse_decimal("feePerTerm", &p.fee_per_term)?,
        ),
        None => (app.policy_id, {
            let f: Decimal =
                sqlx::query_scalar("SELECT fee_per_term FROM campus.hostel_policy WHERE id = $1")
                    .bind(app.policy_id)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(map_db_error)?;
            f
        }),
    };
    let alloc = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO campus.hostel_allocation (id, tenant_id, institution_id, application_id, \
         person_kind, person_id, person_number, person_name, subject_ref, policy_id, bed_id, \
         room_node_id, start_date, fee_amount) \
         SELECT $1, tenant_id, institution_id, id, person_kind, person_id, person_number, person_name, \
         subject_ref, $2, $3, $4, $5, $6 FROM campus.hostel_application WHERE id = $7",
    )
    .bind(alloc)
    .bind(policy_id)
    .bind(bed)
    .bind(room)
    .bind(start)
    .bind(fee)
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    created(
        &mut tx,
        &scope,
        &ALLOCATION,
        alloc,
        HostelAllocationStatus::Active.as_str(),
        "campus.hostel_allocation.activate",
        serde_json::json!({ "application": app.number, "bed": bed }),
        &a,
    )
    .await?;
    publish::event(
        &mut tx,
        &scope,
        HOSTEL_ALLOCATION_ACTIVATED,
        HOSTEL_ALLOCATION_ACTIVATED_SCHEMA,
        alloc.to_string(),
        &StatusChangedV1 {
            aggregate_id: alloc,
            institution_id: inst.as_uuid(),
            from_status: None,
            to_status: HostelAllocationStatus::Active.as_str().to_owned(),
            version: 1,
            reason: None,
            process_instance_id: app.process_instance_id.clone(),
            number: Some(app.number.clone()),
            subject_ref: h.subject.clone(),
        },
        &a,
    )
    .await?;
    move_bed(&mut tx, &scope, bed, BedStatus::Occupied, None, &a).await?;
    sqlx::query("UPDATE campus.hostel_application SET allocation_id = $1 WHERE id = $2")
        .bind(alloc)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    apply(
        &mut tx,
        &scope,
        &APPLICATION,
        &mut h,
        Move {
            to: HostelApplicationStatus::Allocated.as_str(),
            event: None,
            action: "campus.hostel_application.allocate",
            reason: None,
            process_instance_id: None,
        },
        &a,
    )
    .await?;
    // Hostel fee: raised in Finance (own transaction, idempotent on the allocation id) while this
    // transaction holds the allocation; a failure rolls the allocation back.
    if let Some(charge) = raise_charge(
        &st,
        &scope,
        ChargeAsk {
            institution: inst,
            person: (app.person_kind.as_str(), app.person_id),
            fee_code: HOSTEL_FEE,
            category: CampusChargeCategory::Hostel,
            description: format!("Hostel fee — {} ({})", app.person_name, app.number),
            amount: fee,
            due_date: Some(start),
            source_id: alloc,
        },
        &a,
    )
    .await?
    {
        sqlx::query(
            "UPDATE campus.hostel_allocation SET charge_id = $1, charge_number = $2 WHERE id = $3",
        )
        .bind(charge.charge_id.as_uuid())
        .bind(&charge.charge_number)
        .bind(alloc)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(application_reply(&st, t, id, StatusCode::OK).await?)
}

/// Withdraw an open application (`campus.hostel.manage`).
#[utoipa::path(post, path = "/api/v1/campus/hostel-applications/{id}/withdraw", operation_id = "campus_hostel_application_withdraw", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the application")),
    request_body = ReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = HostelApplicationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn application_withdraw(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::HostelManage)?;
    let v = expected!(headers);
    let reason = b.text()?;
    withdraw_application(&st, ctx, id, v, reason.as_deref()).await?;
    Ok(application_reply(&st, ctx.scope.tenant_id, id, StatusCode::OK).await?)
}

/// Residencies of an institution (`campus.read`; `?status=`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/hostel-allocations", operation_id = "campus_hostel_allocation_list", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), StatusFilter), security(("session_cookie" = [])),
    responses((status = 200, body = HostelAllocationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn allocation_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StatusFilter>,
) -> ApiResult<Json<HostelAllocationList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let status = status_filter::<HostelAllocationStatus>(q.status.as_deref())?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let items = list_of(
        &mut conn,
        ALLOCATION_SELECT,
        ctx.scope.tenant_id,
        "institution_id",
        id,
        status,
        "n.code, b.bed_code, x.created_at DESC",
    )
    .await?;
    Ok(Json(HostelAllocationList { items }))
}

/// One residency (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/hostel-allocations/{id}", operation_id = "campus_hostel_allocation_get", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = HostelAllocationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn allocation_get(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusRead)?;
    Ok(allocation_reply(&st, ctx.scope.tenant_id, id, StatusCode::OK).await?)
}
