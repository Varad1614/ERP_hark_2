//! Preventive maintenance (L3-G-023, 024): plans for a node + frequency (draft → active through
//! `MAINTENANCE_PLAN_APPROVAL` → suspended / retired), `generate` creates one cycle + one
//! preventive work order per period (idempotent per period), cycles are deferred / rescheduled
//! with a reason or completed once their work order is done.

use super::workorders::{insert_work_order, load_wo, NewWorkOrder};
use crate::infrastructure::common::{
    audit, clean, emit_status, ensure_version, expected, history, illegal, invalid, limit,
    parse_date, parse_filter, required, EventExtras, FacilityBulkSummary, FacilityDecisionBody,
    FacilityReasonBody, Transition,
};
use crate::infrastructure::domain::{
    period_starts, MaintenanceCycleStatus, MaintenanceFrequency, MaintenancePlanStatus,
    PlanApprovalState, WorkOrderKind, WorkOrderStatus,
};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::rows::{decode, load_node};
use crate::shared::scope::institution_scope;
use crate::shared::time::business_today;
use crate::shared::{Actor, CampusState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{Duration, NaiveDate};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::campus::events::facilities::{
    MAINTENANCE_PLAN_ACTIVATED, MAINTENANCE_PLAN_ACTIVATED_SCHEMA,
};
use sutra_contracts::campus::ids::InfrastructureNodeId;
use sutra_contracts::campus::status::InfrastructureStatus;
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

#[derive(Clone, Debug)]
pub struct PlanRow {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub node_id: Uuid,
    pub node_code: String,
    pub title: String,
    pub description: Option<String>,
    pub frequency: MaintenanceFrequency,
    pub start_date: NaiveDate,
    pub end_date: Option<NaiveDate>,
    pub default_assignee: Option<String>,
    pub status: MaintenancePlanStatus,
    pub approval_state: PlanApprovalState,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub version: i64,
}

const PLAN_COLUMNS: &str = "p.id, p.institution_id, p.node_id, n.code AS node_code, p.title, \
    p.description, p.frequency, p.start_date, p.end_date, p.default_assignee, p.status, \
    p.approval_state, p.status_reason, p.process_instance_id, p.version";

const PLAN_FROM: &str =
    "campus.maintenance_plan p JOIN campus.infrastructure_node n ON n.id = p.node_id";

fn plan_from_row(r: &PgRow) -> Result<PlanRow, sqlx::Error> {
    Ok(PlanRow {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        node_id: r.try_get("node_id")?,
        node_code: r.try_get("node_code")?,
        title: r.try_get("title")?,
        description: r.try_get("description")?,
        frequency: decode(r.try_get("frequency")?, MaintenanceFrequency::parse)?,
        start_date: r.try_get("start_date")?,
        end_date: r.try_get("end_date")?,
        default_assignee: r.try_get("default_assignee")?,
        status: decode(r.try_get("status")?, MaintenancePlanStatus::parse)?,
        approval_state: decode(r.try_get("approval_state")?, PlanApprovalState::parse)?,
        status_reason: r.try_get("status_reason")?,
        process_instance_id: r.try_get("process_instance_id")?,
        version: r.try_get("version")?,
    })
}

async fn load_plan(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<PlanRow, KernelError> {
    let sql = format!(
        "SELECT {PLAN_COLUMNS} FROM {PLAN_FROM} WHERE p.id = $1 AND p.tenant_id = $2{}",
        if lock { " FOR UPDATE OF p" } else { "" }
    );
    sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .as_ref()
        .map(plan_from_row)
        .transpose()
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "maintenance plan",
        })
}

// ------------------------------------------------------------------ views + bodies

/// A preventive maintenance plan.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MaintenancePlanView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub node_id: Uuid,
    pub node_code: String,
    pub title: String,
    pub description: Option<String>,
    pub frequency: MaintenanceFrequency,
    pub start_date: NaiveDate,
    pub end_date: Option<NaiveDate>,
    pub default_assignee: Option<String>,
    pub status: MaintenancePlanStatus,
    pub approval_state: PlanApprovalState,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub version: i64,
}

impl From<&PlanRow> for MaintenancePlanView {
    fn from(p: &PlanRow) -> Self {
        Self {
            id: p.id,
            institution_id: p.institution_id.as_uuid(),
            node_id: p.node_id,
            node_code: p.node_code.clone(),
            title: p.title.clone(),
            description: p.description.clone(),
            frequency: p.frequency,
            start_date: p.start_date,
            end_date: p.end_date,
            default_assignee: p.default_assignee.clone(),
            status: p.status,
            approval_state: p.approval_state,
            status_reason: p.status_reason.clone(),
            process_instance_id: p.process_instance_id.clone(),
            version: p.version,
        }
    }
}

/// One generated cycle of a plan.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MaintenanceCycleView {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub period_start: NaiveDate,
    pub due_on: NaiveDate,
    pub status: MaintenanceCycleStatus,
    pub status_reason: Option<String>,
    pub work_order_id: Option<Uuid>,
    pub work_order_number: Option<String>,
    pub work_order_status: Option<WorkOrderStatus>,
    pub version: i64,
}

/// A plan with its cycles.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MaintenancePlanDetail {
    pub plan: MaintenancePlanView,
    pub cycles: Vec<MaintenanceCycleView>,
    pub version: i64,
}

/// Plans.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MaintenancePlanList {
    pub items: Vec<MaintenancePlanView>,
}

/// Create a draft plan.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MaintenancePlanCreate {
    pub node_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub frequency: MaintenanceFrequency,
    #[schema(example = "2026-12-01")]
    pub start_date: String,
    pub end_date: Option<String>,
    pub default_assignee: Option<String>,
}

/// Generate cycles + work orders through a date.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MaintenanceGenerate {
    /// Last period start to generate (≤ one year ahead).
    #[schema(example = "2027-02-28")]
    pub through: String,
}

/// Defer / reschedule a cycle.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MaintenanceDefer {
    pub due_on: String,
    pub reason: Option<String>,
}

/// Filters of the plan list.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct MaintenancePlanQuery {
    pub status: Option<String>,
    pub limit: Option<i64>,
}

// ------------------------------------------------------------------ helpers

const CYCLE_SQL: &str = "SELECT c.id, c.plan_id, c.period_start, c.due_on, c.status, \
    c.status_reason, c.work_order_id, w.number AS wo_number, w.status AS wo_status, c.version \
    FROM campus.maintenance_cycle c LEFT JOIN campus.work_order w ON w.id = c.work_order_id";

fn cycle_from_row(r: &PgRow) -> Result<MaintenanceCycleView, sqlx::Error> {
    let wo_status: Option<String> = r.try_get("wo_status")?;
    Ok(MaintenanceCycleView {
        id: r.try_get("id")?,
        plan_id: r.try_get("plan_id")?,
        period_start: r.try_get("period_start")?,
        due_on: r.try_get("due_on")?,
        status: decode(r.try_get("status")?, MaintenanceCycleStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        work_order_id: r.try_get("work_order_id")?,
        work_order_number: r.try_get("wo_number")?,
        work_order_status: wo_status.as_deref().and_then(WorkOrderStatus::parse),
        version: r.try_get("version")?,
    })
}

async fn detail(
    st: &CampusState,
    tenant: TenantId,
    id: Uuid,
) -> Result<MaintenancePlanDetail, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let p = load_plan(&mut conn, tenant, id, false).await?;
    let sql =
        format!("{CYCLE_SQL} WHERE c.tenant_id = $1 AND c.plan_id = $2 ORDER BY c.period_start");
    let rows = sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(id)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let mut cycles = Vec::with_capacity(rows.len());
    for r in &rows {
        cycles.push(cycle_from_row(r).map_err(map_db_error)?);
    }
    Ok(MaintenancePlanDetail {
        version: p.version,
        plan: MaintenancePlanView::from(&p),
        cycles,
    })
}

fn respond(status: StatusCode, d: &MaintenancePlanDetail) -> Response {
    with_etag(status, d, Version(d.version))
}

#[allow(clippy::too_many_arguments)]
async fn move_plan(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    p: &PlanRow,
    to: MaintenancePlanStatus,
    approval: Option<PlanApprovalState>,
    reason: Option<&str>,
    pid: Option<&str>,
    actor: &Actor,
) -> Result<i64, KernelError> {
    if p.status != to && !p.status.can_transition(to) {
        return Err(illegal(
            &format!("maintenance plan {}", p.title),
            p.status.as_str(),
            to.as_str(),
        ));
    }
    let v: i64 = sqlx::query_scalar(
        "UPDATE campus.maintenance_plan SET status = $3, approval_state = COALESCE($4, approval_state), \
         status_reason = $5, process_instance_id = COALESCE($6, process_instance_id), \
         version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2 RETURNING version",
    )
    .bind(p.id)
    .bind(scope.tenant_id.as_uuid())
    .bind(to.as_str())
    .bind(approval.map(|x| x.as_str()))
    .bind(reason)
    .bind(pid)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let activated = to == MaintenancePlanStatus::Active && p.status != to;
    let t = Transition {
        tenant: scope.tenant_id,
        institution: p.institution_id,
        aggregate_type: "maintenance_plan",
        aggregate_id: p.id,
        from: Some(p.status.as_str()),
        to: to.as_str(),
        reason,
        process_instance_id: pid,
        cause: if activated {
            MAINTENANCE_PLAN_ACTIVATED
        } else {
            ""
        },
    };
    history(conn, &t, actor).await?;
    if activated {
        emit_status(
            conn,
            scope,
            (
                MAINTENANCE_PLAN_ACTIVATED,
                MAINTENANCE_PLAN_ACTIVATED_SCHEMA,
            ),
            &t,
            v,
            &EventExtras::default(),
            actor,
        )
        .await?;
    }
    Ok(v)
}

async fn begin(
    st: &CampusState,
    ctx: &AuthContext,
    id: Uuid,
    expected_version: i64,
) -> Result<
    (
        sqlx::Transaction<'static, sqlx::Postgres>,
        PlanRow,
        ExecutionScope,
    ),
    KernelError,
> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = load_plan(&mut tx, ctx.scope.tenant_id, id, true).await?;
    ensure_version(expected_version, p.version)?;
    let scope = institution_scope(ctx, p.institution_id);
    require_institution_writable(st, &scope, p.institution_id).await?;
    Ok((tx, p, scope))
}

// ------------------------------------------------------------------ handlers

/// Create a draft preventive plan for a node (`campus.facility.manage`).
#[utoipa::path(post, path = "/api/v1/campus/institutions/{id}/maintenance-plans", operation_id = "campus_maintenance_plan_create", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = MaintenancePlanCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = MaintenancePlanDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn plan_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<MaintenancePlanCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let title = required("title", Some(b.title))?;
    if title.len() > 200 {
        return Err(invalid("title", "too_long", "at most 200 characters").into());
    }
    let start = parse_date("startDate", &b.start_date)?;
    let end = b
        .end_date
        .as_deref()
        .map(|r| parse_date("endDate", r))
        .transpose()?;
    if end.is_some_and(|e| e < start) {
        return Err(invalid("endDate", "before_start", "endDate is before startDate").into());
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let node = load_node(&mut tx, t, InfrastructureNodeId(b.node_id))
        .await?
        .filter(|n| n.institution_id == institution)
        .ok_or_else(|| {
            invalid(
                "nodeId",
                "unknown_node",
                "no infrastructure node with this id",
            )
        })?;
    if matches!(
        node.status,
        InfrastructureStatus::Decommissioned | InfrastructureStatus::Archived
    ) {
        return Err(KernelError::Conflict(format!(
            "infrastructure node {} is {}; it is no longer maintained",
            node.code, node.status
        ))
        .into());
    }
    let pid: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.maintenance_plan (tenant_id, institution_id, node_id, title, \
         description, frequency, start_date, end_date, default_assignee, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(b.node_id)
    .bind(&title)
    .bind(clean(b.description))
    .bind(b.frequency.as_str())
    .bind(start)
    .bind(end)
    .bind(clean(b.default_assignee))
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        &Transition {
            tenant: t,
            institution,
            aggregate_type: "maintenance_plan",
            aggregate_id: pid,
            from: None,
            to: MaintenancePlanStatus::Draft.as_str(),
            reason: None,
            process_instance_id: None,
            cause: "sutraerp.campus.maintenance_plan.created",
        },
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "campus.maintenance_plan.create",
        "maintenance_plan",
        pid,
        serde_json::json!({"title": title, "nodeId": b.node_id, "frequency": b.frequency}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = detail(&st, t, pid).await?;
    Ok(respond(StatusCode::CREATED, &d))
}

/// Preventive plans of an institution (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/maintenance-plans", operation_id = "campus_maintenance_plan_list", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), MaintenancePlanQuery), security(("session_cookie" = [])),
    responses((status = 200, body = MaintenancePlanList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn plan_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<MaintenancePlanQuery>,
) -> ApiResult<Json<MaintenancePlanList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution(&st, &scope, institution).await?;
    let status = parse_filter("status", q.status.as_deref(), MaintenancePlanStatus::parse)?;
    let sql = format!(
        "SELECT {PLAN_COLUMNS} FROM {PLAN_FROM} WHERE p.tenant_id = $1 AND p.institution_id = $2 \
         AND ($3::text IS NULL OR p.status = $3) ORDER BY p.created_at DESC LIMIT $4"
    );
    let rows = sqlx::query(&sql)
        .bind(ctx.scope.tenant_id.as_uuid())
        .bind(id)
        .bind(status.map(|s| s.as_str()))
        .bind(limit(q.limit))
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        items.push(MaintenancePlanView::from(
            &plan_from_row(r).map_err(map_db_error)?,
        ));
    }
    Ok(Json(MaintenancePlanList { items }))
}

/// One plan with its cycles (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/maintenance-plans/{id}", operation_id = "campus_maintenance_plan_get", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = MaintenancePlanDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn plan_get(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusRead)?;
    let d = detail(&st, ctx.scope.tenant_id, id).await?;
    Ok(respond(StatusCode::OK, &d))
}

/// Submit a draft plan for approval (`campus.facility.manage`; `MAINTENANCE_PLAN_APPROVAL`):
/// auto-approval activates it at once (`maintenance_plan.activated`).
#[utoipa::path(post, path = "/api/v1/campus/maintenance-plans/{id}/submit", operation_id = "campus_maintenance_plan_submit", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the plan")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MaintenancePlanDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn plan_submit(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let reason = clean(b.reason);
    let probe = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_plan(&mut conn, t, id, false).await?
    };
    ensure_version(v, probe.version)?;
    if probe.status != MaintenancePlanStatus::Draft
        || probe.approval_state == PlanApprovalState::Pending
    {
        return Err(KernelError::Conflict(format!(
            "maintenance plan {} is {} ({}); only a draft is submitted once",
            probe.title, probe.status, probe.approval_state
        ))
        .into());
    }
    let scope = institution_scope(ctx, probe.institution_id);
    require_institution_writable(&st, &scope, probe.institution_id).await?;
    let outcome = request_approval(
        &st,
        &scope,
        process::MAINTENANCE_PLAN_APPROVAL,
        business_key("maintenance_plan", id, probe.version),
        BTreeMap::from([
            ("planId".to_owned(), serde_json::json!(id)),
            ("nodeId".to_owned(), serde_json::json!(probe.node_id)),
        ]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let (mut tx, p, scope) = begin(&st, ctx, id, v).await?;
    sqlx::query(
        "UPDATE campus.maintenance_plan SET approval_state = 'pending', process_instance_id = $3, \
         status_reason = $4, version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(&pid)
    .bind(reason.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let cur = load_plan(&mut tx, t, id, true).await?;
        move_plan(
            &mut tx,
            &scope,
            &cur,
            MaintenancePlanStatus::Active,
            Some(PlanApprovalState::Approved),
            Some("auto-approved"),
            Some(&pid),
            &Actor::system(a.correlation_id.clone()),
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "campus.maintenance_plan.submit",
        "maintenance_plan",
        p.id,
        serde_json::json!({"processInstanceId": pid}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = detail(&st, t, id).await?;
    Ok(respond(StatusCode::OK, &d))
}

/// Decide a submitted plan (`campus.approve`, step-up): approve → active, reject → stays draft.
#[utoipa::path(post, path = "/api/v1/campus/maintenance-plans/{id}/decision", operation_id = "campus_maintenance_plan_decide", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the plan")),
    request_body = FacilityDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MaintenancePlanDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn plan_decide(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusApprove)?;
    let v = expected!(headers);
    let reason = clean(b.reason.clone());
    if !b.approve() && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let a = actor(ctx);
    let (mut tx, p, scope) = begin(&st, ctx, id, v).await?;
    if p.approval_state != PlanApprovalState::Pending {
        return Err(KernelError::Conflict(format!(
            "maintenance plan {} is not waiting for a decision",
            p.title
        ))
        .into());
    }
    if b.approve() {
        move_plan(
            &mut tx,
            &scope,
            &p,
            MaintenancePlanStatus::Active,
            Some(PlanApprovalState::Approved),
            reason.as_deref(),
            None,
            &a,
        )
        .await?;
    } else {
        move_plan(
            &mut tx,
            &scope,
            &p,
            MaintenancePlanStatus::Draft,
            Some(PlanApprovalState::Rejected),
            reason.as_deref(),
            None,
            &a,
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "campus.maintenance_plan.decide",
        "maintenance_plan",
        id,
        serde_json::json!({"decision": b.decision, "reason": reason}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = detail(&st, ctx.scope.tenant_id, id).await?;
    Ok(respond(StatusCode::OK, &d))
}

#[allow(clippy::too_many_arguments)]
async fn simple_move(
    st: &CampusState,
    ctx: &AuthContext,
    id: Uuid,
    v: i64,
    from: &[MaintenancePlanStatus],
    to: MaintenancePlanStatus,
    reason: Option<String>,
    action: &str,
) -> Result<MaintenancePlanDetail, KernelError> {
    let a = actor(ctx);
    let (mut tx, p, scope) = begin(st, ctx, id, v).await?;
    if !from.contains(&p.status) {
        return Err(illegal(
            &format!("maintenance plan {}", p.title),
            p.status.as_str(),
            to.as_str(),
        ));
    }
    move_plan(&mut tx, &scope, &p, to, None, reason.as_deref(), None, &a).await?;
    audit(
        &mut tx,
        &scope,
        action,
        "maintenance_plan",
        id,
        serde_json::json!({"reason": reason}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    detail(st, ctx.scope.tenant_id, id).await
}

/// Resume a suspended plan (`campus.facility.manage`).
#[utoipa::path(post, path = "/api/v1/campus/maintenance-plans/{id}/activate", operation_id = "campus_maintenance_plan_activate", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the plan")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MaintenancePlanDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn plan_activate(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let d = simple_move(
        &st,
        ctx,
        id,
        v,
        &[MaintenancePlanStatus::Suspended],
        MaintenancePlanStatus::Active,
        clean(b.reason),
        "campus.maintenance_plan.activate",
    )
    .await?;
    Ok(respond(StatusCode::OK, &d))
}

/// Suspend an active plan (`campus.facility.manage`; reason required): no new cycles.
#[utoipa::path(post, path = "/api/v1/campus/maintenance-plans/{id}/suspend", operation_id = "campus_maintenance_plan_suspend", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the plan")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MaintenancePlanDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn plan_suspend(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let reason = required("reason", b.reason)?;
    let d = simple_move(
        &st,
        ctx,
        id,
        v,
        &[MaintenancePlanStatus::Active],
        MaintenancePlanStatus::Suspended,
        Some(reason),
        "campus.maintenance_plan.suspend",
    )
    .await?;
    Ok(respond(StatusCode::OK, &d))
}

/// Retire a plan for good (`campus.facility.manage`).
#[utoipa::path(post, path = "/api/v1/campus/maintenance-plans/{id}/retire", operation_id = "campus_maintenance_plan_retire", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the plan")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MaintenancePlanDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn plan_retire(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let d = simple_move(
        &st,
        ctx,
        id,
        v,
        &[
            MaintenancePlanStatus::Draft,
            MaintenancePlanStatus::Active,
            MaintenancePlanStatus::Suspended,
        ],
        MaintenancePlanStatus::Retired,
        clean(b.reason),
        "campus.maintenance_plan.retire",
    )
    .await?;
    Ok(respond(StatusCode::OK, &d))
}

/// Generate cycles + preventive work orders for every period through `through`
/// (`campus.facility.manage`; bulk, `Idempotency-Key`; idempotent per period: existing periods
/// are skipped). The plan must be active.
#[utoipa::path(post, path = "/api/v1/campus/maintenance-plans/{id}/generate", operation_id = "campus_maintenance_plan_generate", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = MaintenanceGenerate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FacilityBulkSummary), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn plan_generate(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<MaintenanceGenerate>,
) -> ApiResult<Json<FacilityBulkSummary>> {
    let ctx = auth.require(Permission::FacilityManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let through = parse_date("through", &b.through)?;
    let probe = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_plan(&mut conn, t, id, false).await?
    };
    let scope = institution_scope(ctx, probe.institution_id);
    require_institution_writable(&st, &scope, probe.institution_id).await?;
    let today = business_today(&st, &scope, probe.institution_id).await?;
    if through > today + Duration::days(366) {
        return Err(invalid("through", "too_far", "at most one year ahead").into());
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = load_plan(&mut tx, t, id, true).await?;
    if p.status != MaintenancePlanStatus::Active {
        return Err(KernelError::Conflict(format!(
            "maintenance plan {} is {}; only an active plan generates work",
            p.title, p.status
        ))
        .into());
    }
    let mut summary = FacilityBulkSummary::default();
    for period in period_starts(
        p.start_date,
        p.end_date,
        p.frequency,
        p.start_date,
        through,
        400,
    ) {
        let cycle: Option<Uuid> = sqlx::query_scalar(
            "INSERT INTO campus.maintenance_cycle (tenant_id, institution_id, plan_id, \
             period_start, due_on, status) VALUES ($1, $2, $3, $4, $4, $5) \
             ON CONFLICT (tenant_id, plan_id, period_start) DO NOTHING RETURNING id",
        )
        .bind(t.as_uuid())
        .bind(p.institution_id.as_uuid())
        .bind(id)
        .bind(period)
        .bind(if period <= today {
            MaintenanceCycleStatus::Due.as_str()
        } else {
            MaintenanceCycleStatus::Scheduled.as_str()
        })
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db_error)?;
        let Some(cycle) = cycle else {
            summary.skipped += 1;
            continue;
        };
        let title = format!("PM {} — {} ({})", p.node_code, p.title, period);
        let title: String = title.chars().take(200).collect();
        let wo = insert_work_order(
            &mut tx,
            NewWorkOrder {
                tenant: t,
                institution: p.institution_id,
                kind: WorkOrderKind::Preventive,
                ticket_id: None,
                plan_id: Some(id),
                node_id: Some(p.node_id),
                title: &title,
                description: p.description.as_deref(),
                assignee: p.default_assignee.as_deref(),
                scheduled_for: Some(period),
            },
            Some("generated from the preventive plan"),
            &a,
        )
        .await?;
        sqlx::query("UPDATE campus.maintenance_cycle SET work_order_id = $2 WHERE id = $1")
            .bind(cycle)
            .bind(wo)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        summary.processed += 1;
    }
    audit(
        &mut tx,
        &scope,
        "campus.maintenance_plan.generate",
        "maintenance_plan",
        id,
        serde_json::json!({"through": through, "generated": summary.processed,
            "skipped": summary.skipped}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(summary))
}

async fn load_cycle(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> Result<(MaintenanceCycleView, InstitutionId), KernelError> {
    let sql = format!("{CYCLE_SQL} WHERE c.id = $1 AND c.tenant_id = $2 FOR UPDATE OF c");
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "maintenance cycle",
        })?;
    let inst: Uuid =
        sqlx::query_scalar("SELECT institution_id FROM campus.maintenance_cycle WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *conn)
            .await
            .map_err(map_db_error)?;
    Ok((
        cycle_from_row(&row).map_err(map_db_error)?,
        InstitutionId(inst),
    ))
}

async fn move_cycle(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    c: &MaintenanceCycleView,
    to: MaintenanceCycleStatus,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    if !c.status.can_transition(to) {
        return Err(illegal("maintenance cycle", c.status.as_str(), to.as_str()));
    }
    sqlx::query(
        "UPDATE campus.maintenance_cycle SET status = $2, status_reason = $3, \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(c.id)
    .bind(to.as_str())
    .bind(reason)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        &Transition {
            tenant,
            institution,
            aggregate_type: "maintenance_cycle",
            aggregate_id: c.id,
            from: Some(c.status.as_str()),
            to: to.as_str(),
            reason,
            process_instance_id: None,
            cause: "",
        },
        actor,
    )
    .await
}

/// Defer / reschedule a cycle to a new due date (`campus.facility.manage`; reason required):
/// its open work order is rescheduled too. Returns the plan.
#[utoipa::path(post, path = "/api/v1/campus/maintenance-cycles/{id}/defer", operation_id = "campus_maintenance_cycle_defer", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the cycle")),
    request_body = MaintenanceDefer, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MaintenancePlanDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn cycle_defer(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<MaintenanceDefer>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let reason = required("reason", b.reason)?;
    let due_on = parse_date("dueOn", &b.due_on)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let (c, inst) = load_cycle(&mut tx, t, id).await?;
    ensure_version(v, c.version)?;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    if due_on < today || due_on <= c.due_on {
        return Err(invalid(
            "dueOn",
            "not_later",
            "the new due date must be later than the current one and not in the past",
        )
        .into());
    }
    move_cycle(
        &mut tx,
        t,
        inst,
        &c,
        MaintenanceCycleStatus::Deferred,
        Some(&reason),
        &a,
    )
    .await?;
    sqlx::query("UPDATE campus.maintenance_cycle SET due_on = $2 WHERE id = $1")
        .bind(id)
        .bind(due_on)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    if let Some(wo) = c.work_order_id {
        sqlx::query(
            "UPDATE campus.work_order SET scheduled_for = $3, version = version + 1, \
             updated_at = now() WHERE id = $1 AND tenant_id = $2 \
             AND status NOT IN ('completed', 'verified', 'cancelled')",
        )
        .bind(wo)
        .bind(t.as_uuid())
        .bind(due_on)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    audit(
        &mut tx,
        &scope,
        "campus.maintenance_cycle.defer",
        "maintenance_cycle",
        id,
        serde_json::json!({"from": c.due_on, "to": due_on, "reason": reason}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = detail(&st, t, c.plan_id).await?;
    Ok(respond(StatusCode::OK, &d))
}

/// Complete a cycle whose work order is completed / verified (`campus.facility.manage`; 409
/// otherwise). Returns the plan.
#[utoipa::path(post, path = "/api/v1/campus/maintenance-cycles/{id}/complete", operation_id = "campus_maintenance_cycle_complete", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the cycle")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MaintenancePlanDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn cycle_complete(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let reason = clean(b.reason);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let (c, inst) = load_cycle(&mut tx, t, id).await?;
    ensure_version(v, c.version)?;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let done = match c.work_order_id {
        Some(wo) => matches!(
            load_wo(&mut tx, t, wo, false).await?.status,
            WorkOrderStatus::Completed | WorkOrderStatus::Verified
        ),
        None => false,
    };
    if !done {
        return Err(
            KernelError::Conflict("the cycle's work order is not completed yet".into()).into(),
        );
    }
    move_cycle(
        &mut tx,
        t,
        inst,
        &c,
        MaintenanceCycleStatus::Completed,
        reason.as_deref(),
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "campus.maintenance_cycle.complete",
        "maintenance_cycle",
        id,
        serde_json::json!({"reason": reason}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = detail(&st, t, c.plan_id).await?;
    Ok(respond(StatusCode::OK, &d))
}

pub fn router(state: CampusState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(plan_create, plan_list))
        .routes(routes!(plan_get))
        .routes(routes!(plan_submit))
        .routes(routes!(plan_decide))
        .routes(routes!(plan_activate))
        .routes(routes!(plan_suspend))
        .routes(routes!(plan_retire))
        .routes(routes!(plan_generate))
        .routes(routes!(cycle_defer))
        .routes(routes!(cycle_complete))
        .with_state(state)
}
