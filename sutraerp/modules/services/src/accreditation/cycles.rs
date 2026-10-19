//! Accreditation cycles (L3-H-062 accreditation cycle → approved assessment plan): a cycle opens
//! against the **active** version of a framework, its assessment plan assigns every criterion
//! exactly once to an owner with a due date inside the cycle period, the plan is approved
//! (`ACCREDITATION_PLAN_APPROVAL` → `accreditation_cycle.plan_approved`) and the cycle becomes
//! `active`; the hand-off of the approved self-study report moves it to `submitted` (H-071) and
//! it is closed afterwards.

use super::domain::{
    plan_problems, AccreditationApprovalState, AccreditationCriterion, CycleStatus,
    FrameworkStatus, PlanEntry,
};
use super::frameworks::load_framework;
use crate::dashboards::common::{
    check_version, clean, decode, expected, invalid, page_limit, required_text, transition,
    ReportDecision, ReportDecisionBody, ReportReasonBody, Transition,
};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::numbering;
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, ServicesState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::services::events::accreditation::{
    ACCREDITATION_PLAN_APPROVED, ACCREDITATION_PLAN_APPROVED_SCHEMA,
};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Number kind of accreditation cycles (`AC-000001`).
pub const CYCLE_NUMBER: (&str, &str) = ("accreditation_cycle", "AC");

/// Records of one kind / status in a cycle.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CycleRecordCount {
    pub kind: String,
    pub status: String,
    pub count: i64,
}

/// An accreditation cycle.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccreditationCycleView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "AC-000001")]
    pub number: String,
    pub framework_id: Uuid,
    pub framework_code: String,
    pub framework_version_no: i32,
    pub criteria: Vec<AccreditationCriterion>,
    pub title: String,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub plan: Vec<PlanEntry>,
    pub status: CycleStatus,
    pub approval_state: AccreditationApprovalState,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub records: Vec<CycleRecordCount>,
    pub version: i64,
}

/// Cycles.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AccreditationCycleList {
    pub items: Vec<AccreditationCycleView>,
}

/// Open a cycle.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccreditationCycleCreate {
    pub framework_id: Uuid,
    pub title: String,
    #[schema(example = "2026-07-01")]
    pub period_start: String,
    #[schema(example = "2027-06-30")]
    pub period_end: String,
}

/// Replace the assessment plan of a planning cycle.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccreditationPlanBody {
    pub entries: Vec<PlanEntry>,
}

/// Filters of the cycle list.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct AccreditationCycleQuery {
    pub limit: Option<i64>,
}

/// Plain cycle row.
#[derive(Debug, Clone)]
pub struct CycleRow {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub number: String,
    pub framework_id: Uuid,
    pub title: String,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub plan: Vec<PlanEntry>,
    pub status: CycleStatus,
    pub approval_state: AccreditationApprovalState,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub version: i64,
}

const SELECT: &str = "SELECT id, institution_id, number, framework_id, title, period_start, \
    period_end, plan, status, approval_state, status_reason, process_instance_id, version \
    FROM services.accreditation_cycle";

fn from_row(r: &PgRow) -> Result<CycleRow, sqlx::Error> {
    let plan: serde_json::Value = r.try_get("plan")?;
    Ok(CycleRow {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        number: r.try_get("number")?,
        framework_id: r.try_get("framework_id")?,
        title: r.try_get("title")?,
        period_start: r.try_get("period_start")?,
        period_end: r.try_get("period_end")?,
        plan: serde_json::from_value(plan).map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
        status: decode(r.try_get("status")?, CycleStatus::parse)?,
        approval_state: decode(
            r.try_get("approval_state")?,
            AccreditationApprovalState::parse,
        )?,
        status_reason: r.try_get("status_reason")?,
        process_instance_id: r.try_get("process_instance_id")?,
        version: r.try_get("version")?,
    })
}

/// Loads one cycle of the tenant (404), optionally locked.
pub async fn load_cycle(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<CycleRow, KernelError> {
    let sql = format!(
        "{SELECT} WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "accreditation cycle",
        })?;
    from_row(&row).map_err(map_db_error)
}

async fn cycle_view(
    conn: &mut PgConnection,
    tenant: TenantId,
    c: CycleRow,
) -> Result<AccreditationCycleView, KernelError> {
    let f = load_framework(&mut *conn, tenant, c.framework_id, false).await?;
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT kind, status, count(*) FROM services.accreditation_record \
         WHERE tenant_id = $1 AND cycle_id = $2 GROUP BY kind, status ORDER BY kind, status",
    )
    .bind(tenant.as_uuid())
    .bind(c.id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(AccreditationCycleView {
        id: c.id,
        institution_id: c.institution_id,
        number: c.number,
        framework_id: c.framework_id,
        framework_code: f.code,
        framework_version_no: f.version_no,
        criteria: f.criteria,
        title: c.title,
        period_start: c.period_start,
        period_end: c.period_end,
        plan: c.plan,
        status: c.status,
        approval_state: c.approval_state,
        status_reason: c.status_reason,
        process_instance_id: c.process_instance_id,
        records: rows
            .into_iter()
            .map(|(kind, status, count)| CycleRecordCount {
                kind,
                status,
                count,
            })
            .collect(),
        version: c.version,
    })
}

async fn detail(
    st: &ServicesState,
    tenant: TenantId,
    id: Uuid,
) -> Result<AccreditationCycleView, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let c = load_cycle(&mut conn, tenant, id, false).await?;
    cycle_view(&mut conn, tenant, c).await
}

fn respond(status: StatusCode, v: &AccreditationCycleView) -> Response {
    with_etag(status, v, Version(v.version))
}

fn parse_day(field: &str, raw: &str) -> Result<NaiveDate, KernelError> {
    NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
        .map_err(|_| invalid(field, "invalid_date", "must be a date YYYY-MM-DD"))
}

async fn begin(
    st: &ServicesState,
    ctx: &AuthContext,
    id: Uuid,
    v: i64,
) -> Result<
    (
        sqlx::Transaction<'static, sqlx::Postgres>,
        CycleRow,
        ExecutionScope,
    ),
    KernelError,
> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = load_cycle(&mut tx, ctx.scope.tenant_id, id, true).await?;
    check_version(v, c.version)?;
    let inst = InstitutionId(c.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(st, &scope, inst).await?;
    Ok((tx, c, scope))
}

#[allow(clippy::too_many_arguments)]
async fn move_cycle(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    c: &CycleRow,
    to: CycleStatus,
    approval: Option<AccreditationApprovalState>,
    reason: Option<&str>,
    event: Option<(&'static str, &'static str)>,
    action: &'static str,
    actor: &Actor,
) -> Result<(), KernelError> {
    let v: i64 = sqlx::query_scalar(
        "UPDATE services.accreditation_cycle SET status = $3, \
         approval_state = COALESCE($4, approval_state), status_reason = $5, \
         version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2 RETURNING version",
    )
    .bind(c.id)
    .bind(scope.tenant_id.as_uuid())
    .bind(to.as_str())
    .bind(approval.map(|a| a.as_str()))
    .bind(reason)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    transition(
        conn,
        scope,
        &Transition {
            aggregate_type: "accreditation_cycle",
            id: c.id,
            institution: InstitutionId(c.institution_id),
            from: Some(c.status.as_str()),
            to: to.as_str(),
            version: v,
            reason,
            process_instance_id: c.process_instance_id.as_deref(),
            number: Some(&c.number),
            subject_ref: None,
        },
        event,
        action,
        actor,
    )
    .await
}

/// Moves a cycle (used by the self-study hand-off, H-071).
pub async fn set_cycle_status(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    c: &CycleRow,
    to: CycleStatus,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    move_cycle(
        conn,
        scope,
        c,
        to,
        None,
        reason,
        None,
        "services.accreditation_cycle.submit",
        actor,
    )
    .await
}

/// Open a cycle on the active version of a framework (`services.accreditation.manage`;
/// Idempotency-Key).
#[utoipa::path(post, path = "/api/v1/services/institutions/{id}/accreditation-cycles", operation_id = "services_accreditation_cycle_create", tag = "services-accreditation",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = AccreditationCycleCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = AccreditationCycleView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn cycle_create(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<AccreditationCycleCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AccreditationManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let title = required_text("title", &b.title, 200)?;
    let start = parse_day("periodStart", &b.period_start)?;
    let end = parse_day("periodEnd", &b.period_end)?;
    if end < start {
        return Err(invalid(
            "periodEnd",
            "before_start",
            "the period ends before it starts",
        )
        .into());
    }
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let f = load_framework(&mut tx, t, b.framework_id, false)
        .await
        .ok()
        .filter(|f| f.institution_id == id)
        .ok_or_else(|| invalid("frameworkId", "unknown_framework", "no such framework"))?;
    if f.status != FrameworkStatus::Active {
        return Err(KernelError::Conflict(format!(
            "framework {} v{} is {}; a cycle opens on the active version",
            f.code, f.version_no, f.status
        ))
        .into());
    }
    let number = numbering::next(&mut tx, t, institution, CYCLE_NUMBER).await?;
    let cid: Uuid = sqlx::query_scalar(
        "INSERT INTO services.accreditation_cycle (tenant_id, institution_id, number, framework_id, \
         title, period_start, period_end, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&number)
    .bind(f.id)
    .bind(&title)
    .bind(start)
    .bind(end)
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    transition(
        &mut tx,
        &scope,
        &Transition {
            aggregate_type: "accreditation_cycle",
            id: cid,
            institution,
            from: None,
            to: "planning",
            version: 1,
            reason: None,
            process_instance_id: None,
            number: Some(&number),
            subject_ref: None,
        },
        None,
        "services.accreditation_cycle.create",
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = detail(&st, t, cid).await?;
    Ok(respond(StatusCode::CREATED, &d))
}

/// Cycles of an institution (`services.accreditation.manage`).
#[utoipa::path(get, path = "/api/v1/services/institutions/{id}/accreditation-cycles", operation_id = "services_accreditation_cycle_list", tag = "services-accreditation",
    params(("id" = String, Path, format = Uuid), AccreditationCycleQuery), security(("session_cookie" = [])),
    responses((status = 200, body = AccreditationCycleList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn cycle_list(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<AccreditationCycleQuery>,
) -> ApiResult<Json<AccreditationCycleList>> {
    let ctx = auth.require(Permission::AccreditationManage)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let rows = sqlx::query(&format!(
        "{SELECT} WHERE tenant_id = $1 AND institution_id = $2 ORDER BY created_at DESC LIMIT $3"
    ))
    .bind(t.as_uuid())
    .bind(id)
    .bind(page_limit(q.limit))
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        let c = from_row(r).map_err(map_db_error)?;
        items.push(cycle_view(&mut conn, t, c).await?);
    }
    Ok(Json(AccreditationCycleList { items }))
}

/// One cycle with its plan and record counts (`services.accreditation.manage`).
#[utoipa::path(get, path = "/api/v1/services/accreditation-cycles/{id}", operation_id = "services_accreditation_cycle_get", tag = "services-accreditation",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = AccreditationCycleView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn cycle_get(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AccreditationManage)?;
    let d = detail(&st, ctx.scope.tenant_id, id).await?;
    Ok(respond(StatusCode::OK, &d))
}

/// Replace the assessment plan of a planning cycle (`services.accreditation.manage`): every
/// criterion exactly once with an owner and a due date inside the period (422 otherwise). A
/// rejected plan returns to `not_requested`.
#[utoipa::path(post, path = "/api/v1/services/accreditation-cycles/{id}/plan", operation_id = "services_accreditation_cycle_plan", tag = "services-accreditation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = AccreditationPlanBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AccreditationCycleView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn cycle_plan(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<AccreditationPlanBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AccreditationManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let (mut tx, c, scope) = begin(&st, ctx, id, v).await?;
    if c.status != CycleStatus::Planning || c.approval_state == AccreditationApprovalState::Pending
    {
        return Err(KernelError::Conflict(format!(
            "cycle {} is {} ({}); its plan is frozen",
            c.number, c.status, c.approval_state
        ))
        .into());
    }
    let f = load_framework(&mut tx, t, c.framework_id, false).await?;
    let entries: Vec<PlanEntry> = b
        .entries
        .into_iter()
        .map(|e| PlanEntry {
            criterion_code: e.criterion_code.trim().to_uppercase(),
            owner: e.owner.trim().to_owned(),
            due_on: e.due_on.trim().to_owned(),
        })
        .collect();
    let mut problems = plan_problems(&entries, &f.criteria);
    for e in &entries {
        match NaiveDate::parse_from_str(&e.due_on, "%Y-%m-%d") {
            Ok(d) if d >= c.period_start && d <= c.period_end => {}
            _ => problems.push(format!(
                "criterion `{}` is due outside the cycle period",
                e.criterion_code
            )),
        }
    }
    if !problems.is_empty() {
        return Err(KernelError::Validation(
            problems
                .into_iter()
                .map(|p| FieldViolation::new("entries", "invalid_plan", p))
                .collect(),
        )
        .into());
    }
    let nv: i64 = sqlx::query_scalar(
        "UPDATE services.accreditation_cycle SET plan = $3, approval_state = 'not_requested', \
         version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2 RETURNING version",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(serde_json::to_value(&entries).map_err(KernelError::internal)?)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    transition(
        &mut tx,
        &scope,
        &Transition {
            aggregate_type: "accreditation_cycle",
            id,
            institution: InstitutionId(c.institution_id),
            from: Some("planning"),
            to: "planning",
            version: nv,
            reason: Some("assessment plan updated"),
            process_instance_id: None,
            number: Some(&c.number),
            subject_ref: None,
        },
        None,
        "services.accreditation_cycle.plan",
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = detail(&st, t, id).await?;
    Ok(respond(StatusCode::OK, &d))
}

/// Submit the assessment plan (`services.accreditation.manage`; `ACCREDITATION_PLAN_APPROVAL`):
/// auto-approval activates the cycle (`accreditation_cycle.plan_approved`).
#[utoipa::path(post, path = "/api/v1/services/accreditation-cycles/{id}/submit", operation_id = "services_accreditation_cycle_submit", tag = "services-accreditation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = ReportReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AccreditationCycleView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn cycle_submit(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReportReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AccreditationManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let reason = clean(b.reason);
    let probe = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_cycle(&mut conn, t, id, false).await?
    };
    check_version(v, probe.version)?;
    if probe.status != CycleStatus::Planning
        || probe.approval_state == AccreditationApprovalState::Pending
    {
        return Err(KernelError::Conflict(format!(
            "cycle {} is {} ({}); only a planning cycle submits its plan once",
            probe.number, probe.status, probe.approval_state
        ))
        .into());
    }
    if probe.plan.is_empty() {
        return Err(invalid("plan", "empty", "set the assessment plan first").into());
    }
    let inst = InstitutionId(probe.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let outcome = request_approval(
        &st,
        &scope,
        process::ACCREDITATION_PLAN_APPROVAL,
        business_key("accreditation_cycle", id, probe.version),
        BTreeMap::from([("cycleId".to_owned(), serde_json::json!(id))]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let (mut tx, c, scope) = begin(&st, ctx, id, v).await?;
    let nv: i64 = sqlx::query_scalar(
        "UPDATE services.accreditation_cycle SET approval_state = 'pending', \
         process_instance_id = $3, status_reason = $4, version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 RETURNING version",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(&pid)
    .bind(reason.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    transition(
        &mut tx,
        &scope,
        &Transition {
            aggregate_type: "accreditation_cycle",
            id,
            institution: inst,
            from: Some("planning"),
            to: "planning",
            version: nv,
            reason: Some("assessment plan submitted"),
            process_instance_id: Some(&pid),
            number: Some(&c.number),
            subject_ref: None,
        },
        None,
        "services.accreditation_cycle.submit_plan",
        &a,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let cur = load_cycle(&mut tx, t, id, true).await?;
        move_cycle(
            &mut tx,
            &scope,
            &cur,
            CycleStatus::Active,
            Some(AccreditationApprovalState::Approved),
            Some("auto-approved"),
            Some((
                ACCREDITATION_PLAN_APPROVED,
                ACCREDITATION_PLAN_APPROVED_SCHEMA,
            )),
            "services.accreditation_cycle.approve_plan",
            &Actor::system(a.correlation_id.clone()),
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    let d = detail(&st, t, id).await?;
    Ok(respond(StatusCode::OK, &d))
}

/// Decide a submitted plan (`services.report.approve`, step-up): approve → cycle active; reject
/// (reason required) → planning, approval `rejected`.
#[utoipa::path(post, path = "/api/v1/services/accreditation-cycles/{id}/decision", operation_id = "services_accreditation_cycle_decide", tag = "services-accreditation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = ReportDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AccreditationCycleView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn cycle_decide(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReportDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportApprove)?;
    let v = expected!(headers);
    let reason = clean(b.reason.clone());
    let approve = b.decision == ReportDecision::Approve;
    if !approve && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let (mut tx, c, scope) = begin(&st, ctx, id, v).await?;
    if c.status != CycleStatus::Planning || c.approval_state != AccreditationApprovalState::Pending
    {
        return Err(KernelError::Conflict(format!(
            "cycle {} is not waiting for a plan decision",
            c.number
        ))
        .into());
    }
    if approve {
        move_cycle(
            &mut tx,
            &scope,
            &c,
            CycleStatus::Active,
            Some(AccreditationApprovalState::Approved),
            reason.as_deref(),
            Some((
                ACCREDITATION_PLAN_APPROVED,
                ACCREDITATION_PLAN_APPROVED_SCHEMA,
            )),
            "services.accreditation_cycle.approve_plan",
            &a,
        )
        .await?;
    } else {
        move_cycle(
            &mut tx,
            &scope,
            &c,
            CycleStatus::Planning,
            Some(AccreditationApprovalState::Rejected),
            reason.as_deref(),
            None,
            "services.accreditation_cycle.reject_plan",
            &a,
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    let d = detail(&st, t, id).await?;
    Ok(respond(StatusCode::OK, &d))
}

/// Close a submitted cycle (`services.accreditation.manage`): only after the self-study report
/// was handed off.
#[utoipa::path(post, path = "/api/v1/services/accreditation-cycles/{id}/close", operation_id = "services_accreditation_cycle_close", tag = "services-accreditation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = ReportReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AccreditationCycleView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn cycle_close(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReportReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AccreditationManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let reason = clean(b.reason);
    let (mut tx, c, scope) = begin(&st, ctx, id, v).await?;
    if c.status != CycleStatus::Submitted {
        return Err(KernelError::Conflict(format!(
            "cycle {} is {}; it closes after the self-study hand-off",
            c.number, c.status
        ))
        .into());
    }
    move_cycle(
        &mut tx,
        &scope,
        &c,
        CycleStatus::Closed,
        None,
        reason.as_deref(),
        None,
        "services.accreditation_cycle.close",
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = detail(&st, t, id).await?;
    Ok(respond(StatusCode::OK, &d))
}

pub fn router(state: ServicesState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(cycle_create, cycle_list))
        .routes(routes!(cycle_get))
        .routes(routes!(cycle_plan))
        .routes(routes!(cycle_submit))
        .routes(routes!(cycle_decide))
        .routes(routes!(cycle_close))
        .with_state(state)
}
