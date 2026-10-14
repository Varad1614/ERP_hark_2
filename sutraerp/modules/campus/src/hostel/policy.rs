//! Hostel policy versions (L3-G-092: draft → approved (`HOSTEL_POLICY_APPROVAL`) → active →
//! superseded, one active per institution) and the beds of `hostel_room` spaces (≤ the room's
//! capacity, room must be an active node).

use super::common::{
    apply, clock, created, ensure_version, expected, fetch, head, invalid, list_of, mark_pending,
    money, optional_text, reply, required_text, status_filter, Agg, DecisionBody, Move,
    StatusFilter,
};
use super::domain::{bed_fits, HostelPolicyStatus};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::actor;
use crate::shared::rows::{load_node, lock_node};
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, CampusState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use sutra_contracts::campus::ids::InfrastructureNodeId;
use sutra_contracts::campus::status::{InfrastructureNodeType, SpaceType};
use sutra_kernel::auth::Permission;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use uuid::Uuid;

pub(crate) const POLICY: Agg = Agg {
    table: "hostel_policy",
    entity: "hostel policy",
    number: "name",
    subject: "NULL",
};

pub(crate) const BED: Agg = Agg {
    table: "hostel_bed",
    entity: "hostel bed",
    number: "bed_code",
    subject: "NULL",
};

// ------------------------------------------------------------------ views

/// A hostel policy version.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct HostelPolicyView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub name: String,
    /// Hostel fee charged per term on allocation (students).
    #[schema(example = "45000.00")]
    pub fee_per_term: String,
    #[schema(example = "5000.00")]
    pub deposit: String,
    /// `HH:MM`.
    pub curfew_time: Option<String>,
    pub eligibility_note: Option<String>,
    pub max_stay_terms: Option<i32>,
    pub effective_from: NaiveDate,
    /// `draft | approved | active | superseded` (a draft with a `processInstanceId` awaits a decision).
    pub status: String,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub activated_at: Option<DateTime<Utc>>,
    pub version: i64,
}

const POLICY_SELECT: &str = "SELECT x.id, x.institution_id, x.name, x.fee_per_term::text AS fee_per_term, \
    x.deposit::text AS deposit, x.curfew_time, x.eligibility_note, x.max_stay_terms, x.effective_from, \
    x.status, x.status_reason, x.process_instance_id, x.activated_at, x.version FROM campus.hostel_policy x";

/// Policy versions of an institution.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HostelPolicyList {
    pub items: Vec<HostelPolicyView>,
}

/// A new policy version (draft).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HostelPolicyCreate {
    pub name: String,
    #[schema(example = "45000.00")]
    pub fee_per_term: String,
    #[schema(example = "5000.00")]
    pub deposit: Option<String>,
    #[schema(example = "21:30")]
    pub curfew_time: Option<String>,
    pub eligibility_note: Option<String>,
    pub max_stay_terms: Option<i32>,
    pub effective_from: NaiveDate,
}

/// A bed of a hostel room, with its occupant.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct HostelBedView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub room_node_id: Uuid,
    #[schema(example = "HST-B1-101")]
    pub room_code: String,
    pub room_name: String,
    #[schema(example = "A")]
    pub bed_code: String,
    /// `available | occupied | out_of_service`.
    pub status: String,
    pub allocation_id: Option<Uuid>,
    pub occupant_number: Option<String>,
    pub occupant_name: Option<String>,
    pub version: i64,
}

const BED_SELECT: &str = "SELECT x.id, x.institution_id, x.room_node_id, n.code AS room_code, \
    n.name AS room_name, x.bed_code, x.status, a.id AS allocation_id, a.person_number AS occupant_number, \
    a.person_name AS occupant_name, x.version FROM campus.hostel_bed x \
    JOIN campus.infrastructure_node n ON n.id = x.room_node_id \
    LEFT JOIN campus.hostel_allocation a ON a.bed_id = x.id AND a.status <> 'closed'";

/// Beds (with occupancy).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HostelBedList {
    pub items: Vec<HostelBedView>,
}

/// A new bed in a hostel room.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HostelBedCreate {
    #[schema(example = "A")]
    pub bed_code: String,
}

// ------------------------------------------------------------------ helpers

async fn policy_reply(
    st: &CampusState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v: HostelPolicyView = fetch(&mut conn, POLICY_SELECT, tenant, id, POLICY.entity).await?;
    Ok(reply(status, &v, v.version))
}

/// The active policy of an institution, if any.
pub async fn active_policy(
    conn: &mut sqlx::PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
) -> Result<Option<HostelPolicyView>, KernelError> {
    let sql = format!(
        "{POLICY_SELECT} WHERE x.tenant_id = $1 AND x.institution_id = $2 AND x.status = 'active'"
    );
    sqlx::query_as::<_, HostelPolicyView>(&sql)
        .bind(tenant.as_uuid())
        .bind(institution.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)
}

fn approve_move<'a>(pid: Option<&'a str>, reason: Option<&'a str>) -> Move<'a> {
    Move {
        to: HostelPolicyStatus::Approved.as_str(),
        event: None,
        action: "campus.hostel_policy.approve",
        reason,
        process_instance_id: pid,
    }
}

// ------------------------------------------------------------------ handlers: policies

/// Policy versions of an institution (`campus.read`; `?status=`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/hostel-policies", operation_id = "campus_hostel_policy_list", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), StatusFilter), security(("session_cookie" = [])),
    responses((status = 200, body = HostelPolicyList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StatusFilter>,
) -> ApiResult<Json<HostelPolicyList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let status = status_filter::<HostelPolicyStatus>(q.status.as_deref())?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let items = list_of(
        &mut conn,
        POLICY_SELECT,
        ctx.scope.tenant_id,
        "institution_id",
        id,
        status,
        "x.effective_from DESC, x.created_at DESC",
    )
    .await?;
    Ok(Json(HostelPolicyList { items }))
}

/// Draft a policy version (`campus.hostel.manage`).
#[utoipa::path(post, path = "/api/v1/campus/institutions/{id}/hostel-policies", operation_id = "campus_hostel_policy_create", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = HostelPolicyCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = HostelPolicyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<HostelPolicyCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::HostelManage)?;
    require_idempotency_key(&headers)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let name = required_text("name", &b.name, 200)?;
    let fee = money("feePerTerm", &b.fee_per_term)?;
    let deposit = match b.deposit.as_deref() {
        Some(d) => money("deposit", d)?,
        None => rust_decimal::Decimal::ZERO,
    };
    let curfew = clock("curfewTime", b.curfew_time.clone())?;
    let note = optional_text("eligibilityNote", b.eligibility_note.clone(), 2000)?;
    if b.max_stay_terms.is_some_and(|m| !(1..=40).contains(&m)) {
        return Err(invalid("maxStayTerms", "out_of_range", "between 1 and 40").into());
    }
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let pid: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.hostel_policy (tenant_id, institution_id, name, fee_per_term, deposit, \
         curfew_time, eligibility_note, max_stay_terms, effective_from) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(&name)
    .bind(fee)
    .bind(deposit)
    .bind(curfew)
    .bind(note)
    .bind(b.max_stay_terms)
    .bind(b.effective_from)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    created(
        &mut tx,
        &scope,
        &POLICY,
        pid,
        "draft",
        "campus.hostel_policy.create",
        serde_json::json!({ "name": name }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(policy_reply(&st, ctx.scope.tenant_id, pid, StatusCode::CREATED).await?)
}

/// One policy version (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/hostel-policies/{id}", operation_id = "campus_hostel_policy_get", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = HostelPolicyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_get(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusRead)?;
    Ok(policy_reply(&st, ctx.scope.tenant_id, id, StatusCode::OK).await?)
}

/// Submit a draft for approval (`campus.hostel.manage`; `HOSTEL_POLICY_APPROVAL`: auto-approved
/// in Batch 8, else it waits for `/decision`).
#[utoipa::path(post, path = "/api/v1/campus/hostel-policies/{id}/submit", operation_id = "campus_hostel_policy_submit", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the policy")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = HostelPolicyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_submit(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::HostelManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let cur: HostelPolicyView = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        fetch(&mut conn, POLICY_SELECT, t, id, POLICY.entity).await?
    };
    ensure_version(v, cur.version)?;
    if cur.status != "draft" || cur.process_instance_id.is_some() {
        return Err(KernelError::Conflict(format!(
            "hostel policy {} is {}{}; only an unsubmitted draft can be submitted",
            cur.name,
            cur.status,
            if cur.process_instance_id.is_some() {
                " (awaiting a decision)"
            } else {
                ""
            }
        ))
        .into());
    }
    let inst = InstitutionId(cur.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let mut vars = BTreeMap::new();
    vars.insert("feePerTerm".to_owned(), serde_json::json!(cur.fee_per_term));
    let outcome = request_approval(
        &st,
        &scope,
        process::HOSTEL_POLICY_APPROVAL,
        business_key("hostel_policy", id, cur.version),
        vars,
    )
    .await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut h = head(&mut tx, &POLICY, t, id, true).await?;
    ensure_version(v, h.version)?;
    let pid = outcome.process_instance_id().to_owned();
    mark_pending(
        &mut tx,
        &scope,
        &POLICY,
        &mut h,
        Some(&pid),
        "campus.hostel_policy.submit",
        &a,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let sys = Actor::system(a.correlation_id.clone());
        apply(
            &mut tx,
            &scope,
            &POLICY,
            &mut h,
            approve_move(Some(&pid), None),
            &sys,
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(policy_reply(&st, t, id, StatusCode::OK).await?)
}

/// Decide a submitted policy (`campus.approve`, step-up): approve → `approved`; reject → back to
/// an unsubmitted draft with the reason.
#[utoipa::path(post, path = "/api/v1/campus/hostel-policies/{id}/decision", operation_id = "campus_hostel_policy_decide", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the policy")),
    request_body = DecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = HostelPolicyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_decide(
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
    let mut h = head(&mut tx, &POLICY, t, id, true).await?;
    ensure_version(v, h.version)?;
    let pid: Option<String> =
        sqlx::query_scalar("SELECT process_instance_id FROM campus.hostel_policy WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
    if h.status != "draft" || pid.is_none() {
        return Err(KernelError::Conflict(format!(
            "hostel policy {} is not awaiting a decision",
            h.number.clone().unwrap_or_default()
        ))
        .into());
    }
    let scope = institution_scope(ctx, h.institution);
    if b.approve() {
        apply(
            &mut tx,
            &scope,
            &POLICY,
            &mut h,
            approve_move(pid.as_deref(), reason.as_deref()),
            &a,
        )
        .await?;
    } else {
        sqlx::query(
            "UPDATE campus.hostel_policy SET status_reason = $1 WHERE id = $2 AND tenant_id = $3",
        )
        .bind(reason.as_deref())
        .bind(id)
        .bind(t.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        mark_pending(
            &mut tx,
            &scope,
            &POLICY,
            &mut h,
            None,
            "campus.hostel_policy.reject",
            &a,
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(policy_reply(&st, t, id, StatusCode::OK).await?)
}

/// Activate an approved version (`campus.approve`): the previous active version becomes
/// `superseded` (one active per institution).
#[utoipa::path(post, path = "/api/v1/campus/hostel-policies/{id}/activate", operation_id = "campus_hostel_policy_activate", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the policy")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = HostelPolicyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_activate(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusApprove)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let inst = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        head(&mut conn, &POLICY, t, id, false).await?.institution
    };
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    // Lock the current active version first (stable lock order: institution's active, then this).
    let current: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM campus.hostel_policy WHERE tenant_id = $1 AND institution_id = $2 \
         AND status = 'active' FOR UPDATE",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let mut h = head(&mut tx, &POLICY, t, id, true).await?;
    ensure_version(v, h.version)?;
    super::common::step(&h, HostelPolicyStatus::Active)?;
    if let Some(old) = current {
        let mut oh = head(&mut tx, &POLICY, t, old, true).await?;
        apply(
            &mut tx,
            &scope,
            &POLICY,
            &mut oh,
            Move {
                to: HostelPolicyStatus::Superseded.as_str(),
                event: None,
                action: "campus.hostel_policy.supersede",
                reason: Some("superseded by a newer policy version"),
                process_instance_id: None,
            },
            &a,
        )
        .await?;
        sqlx::query("UPDATE campus.hostel_policy SET superseded_at = now() WHERE id = $1")
            .bind(old)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
    }
    apply(
        &mut tx,
        &scope,
        &POLICY,
        &mut h,
        Move {
            to: HostelPolicyStatus::Active.as_str(),
            event: None,
            action: "campus.hostel_policy.activate",
            reason: None,
            process_instance_id: None,
        },
        &a,
    )
    .await?;
    sqlx::query("UPDATE campus.hostel_policy SET activated_at = now() WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(policy_reply(&st, t, id, StatusCode::OK).await?)
}

// ------------------------------------------------------------------ handlers: beds

/// Beds of a hostel room with their occupants (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/hostel-rooms/{id}/beds", operation_id = "campus_hostel_bed_list", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = HostelBedList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn bed_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<HostelBedList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load_node(&mut conn, ctx.scope.tenant_id, InfrastructureNodeId(id))
        .await?
        .ok_or(KernelError::NotFound {
            entity: "hostel room",
        })?;
    let items = list_of(
        &mut conn,
        BED_SELECT,
        ctx.scope.tenant_id,
        "room_node_id",
        id,
        None,
        "x.bed_code",
    )
    .await?;
    Ok(Json(HostelBedList { items }))
}

/// Every bed of an institution with occupancy (`campus.read`; `?status=`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/hostel-beds", operation_id = "campus_hostel_bed_occupancy", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), StatusFilter), security(("session_cookie" = [])),
    responses((status = 200, body = HostelBedList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn bed_occupancy(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StatusFilter>,
) -> ApiResult<Json<HostelBedList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let status = status_filter::<super::domain::BedStatus>(q.status.as_deref())?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let items = list_of(
        &mut conn,
        BED_SELECT,
        ctx.scope.tenant_id,
        "institution_id",
        id,
        status,
        "n.code, x.bed_code",
    )
    .await?;
    Ok(Json(HostelBedList { items }))
}

/// Add a bed to a hostel room (`campus.hostel.manage`): the node must be an active
/// `hostel_room` space and the room keeps at most `capacity` beds.
#[utoipa::path(post, path = "/api/v1/campus/hostel-rooms/{id}/beds", operation_id = "campus_hostel_bed_create", tag = "campus-hostel",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = HostelBedCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = HostelBedView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn bed_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<HostelBedCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::HostelManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let bed_code = super::common::code("bedCode", &b.bed_code, 16)?;
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
    // The room row lock serialises bed creation against the capacity check.
    let room = lock_node(&mut tx, t, InfrastructureNodeId(id)).await?;
    if room.node_type != InfrastructureNodeType::Space
        || room.space_type != Some(SpaceType::HostelRoom)
    {
        return Err(invalid(
            "id",
            "not_a_hostel_room",
            "beds belong to hostel_room spaces",
        )
        .into());
    }
    if !room.status.is_operational() {
        return Err(KernelError::Conflict(format!(
            "room {} is {}; only an active room takes beds",
            room.code, room.status
        ))
        .into());
    }
    let existing: i64 =
        sqlx::query_scalar("SELECT count(*) FROM campus.hostel_bed WHERE room_node_id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
    if !bed_fits(existing, room.capacity) {
        return Err(KernelError::Conflict(format!(
            "room {} already has {existing} bed(s) for a capacity of {}",
            room.code,
            room.capacity.unwrap_or(0)
        ))
        .into());
    }
    let bed: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.hostel_bed (tenant_id, institution_id, room_node_id, bed_code) \
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(id)
    .bind(&bed_code)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    created(
        &mut tx,
        &scope,
        &BED,
        bed,
        "available",
        "campus.hostel_bed.create",
        serde_json::json!({ "room": room.code, "bedCode": bed_code }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v: HostelBedView = fetch(&mut conn, BED_SELECT, t, bed, BED.entity).await?;
    Ok(reply(StatusCode::CREATED, &v, v.version))
}
