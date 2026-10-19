//! Accreditation frameworks (L3-H-061 framework change → active framework): a NAAC / NBA / NIRF
//! criteria set is a versioned list of weighted criteria (weights add up to 100) whose
//! supporting figures are validated against the live metric catalogue. Draft → submitted
//! (`ACCREDITATION_FRAMEWORK_APPROVAL`) → active (`accreditation_framework.activated`); the
//! previous active version of the code is superseded, cycles keep the version they opened with.

use super::domain::{
    criteria_problems, AccreditationApprovalState, AccreditationCriterion, FrameworkStatus, BODIES,
};
use crate::dashboards::common::{
    check_version, clean, decode, expected, invalid, page_limit, required_text, transition,
    valid_code, ReportDecision, ReportDecisionBody, ReportReasonBody, Transition,
};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, ServicesState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::services::events::accreditation::{
    ACCREDITATION_FRAMEWORK_ACTIVATED, ACCREDITATION_FRAMEWORK_ACTIVATED_SCHEMA,
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

/// One framework version.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccreditationFrameworkView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "NAAC-2025")]
    pub code: String,
    pub name: String,
    /// `NAAC`, `NBA`, `NIRF` or `OTHER`.
    pub body: String,
    pub version_no: i32,
    pub criteria: Vec<AccreditationCriterion>,
    pub status: FrameworkStatus,
    pub approval_state: AccreditationApprovalState,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub predecessor_id: Option<Uuid>,
    pub version: i64,
}

/// Framework versions.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AccreditationFrameworkList {
    pub items: Vec<AccreditationFrameworkView>,
}

/// Define a framework version (new code or version n + 1 of a code).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccreditationFrameworkCreate {
    pub code: String,
    pub name: String,
    pub body: String,
    pub criteria: Vec<AccreditationCriterion>,
}

/// Filters of the framework list.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct AccreditationFrameworkQuery {
    pub status: Option<String>,
    pub limit: Option<i64>,
}

const SELECT: &str = "SELECT id, institution_id, code, name, body, version_no, criteria, status, \
    approval_state, status_reason, process_instance_id, predecessor_id, version \
    FROM services.accreditation_framework";

fn from_row(r: &PgRow) -> Result<AccreditationFrameworkView, sqlx::Error> {
    let criteria: serde_json::Value = r.try_get("criteria")?;
    Ok(AccreditationFrameworkView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        code: r.try_get("code")?,
        name: r.try_get("name")?,
        body: r.try_get("body")?,
        version_no: r.try_get("version_no")?,
        criteria: serde_json::from_value(criteria).map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
        status: decode(r.try_get("status")?, FrameworkStatus::parse)?,
        approval_state: decode(
            r.try_get("approval_state")?,
            AccreditationApprovalState::parse,
        )?,
        status_reason: r.try_get("status_reason")?,
        process_instance_id: r.try_get("process_instance_id")?,
        predecessor_id: r.try_get("predecessor_id")?,
        version: r.try_get("version")?,
    })
}

/// Loads one framework version of the tenant (404), optionally locked.
pub async fn load_framework(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<AccreditationFrameworkView, KernelError> {
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
            entity: "accreditation framework",
        })?;
    from_row(&row).map_err(map_db_error)
}

fn respond(status: StatusCode, f: &AccreditationFrameworkView) -> Response {
    with_etag(status, f, Version(f.version))
}

/// Draft a framework version (`services.accreditation.manage`; Idempotency-Key). 422 for invalid
/// criteria, weights that do not add up to 100 or unknown figures.
#[utoipa::path(post, path = "/api/v1/services/institutions/{id}/accreditation-frameworks", operation_id = "services_accreditation_framework_create", tag = "services-accreditation",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = AccreditationFrameworkCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = AccreditationFrameworkView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn framework_create(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<AccreditationFrameworkCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AccreditationManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let code = valid_code("code", &b.code)?;
    let name = required_text("name", &b.name, 200)?;
    let body = b.body.trim().to_uppercase();
    if !BODIES.contains(&body.as_str()) {
        return Err(invalid("body", "unknown_body", "NAAC, NBA, NIRF or OTHER").into());
    }
    let criteria: Vec<AccreditationCriterion> = b
        .criteria
        .into_iter()
        .map(|c| AccreditationCriterion {
            code: c.code.trim().to_uppercase(),
            title: c.title.trim().to_owned(),
            weight: c.weight,
            metric_keys: c.metric_keys.iter().map(|k| k.trim().to_owned()).collect(),
        })
        .collect();
    let catalogue = st.all_metrics(&scope, institution).await?;
    let known = |k: &str| catalogue.iter().any(|m| m.key == k);
    let problems = criteria_problems(&criteria, &known);
    if !problems.is_empty() {
        return Err(KernelError::Validation(
            problems
                .into_iter()
                .map(|p| FieldViolation::new("criteria", "invalid", p))
                .collect(),
        )
        .into());
    }
    let a = actor(ctx);
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let (next, predecessor): (i32, Option<Uuid>) = sqlx::query_as(
        "SELECT COALESCE(max(version_no), 0) + 1, (SELECT id FROM services.accreditation_framework \
         WHERE tenant_id = $1 AND institution_id = $2 AND code = $3 AND status = 'active') \
         FROM services.accreditation_framework WHERE tenant_id = $1 AND institution_id = $2 AND code = $3",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&code)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let fid: Uuid = sqlx::query_scalar(
        "INSERT INTO services.accreditation_framework (tenant_id, institution_id, code, name, body, \
         version_no, criteria, predecessor_id, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&code)
    .bind(&name)
    .bind(&body)
    .bind(next)
    .bind(serde_json::to_value(&criteria).map_err(KernelError::internal)?)
    .bind(predecessor)
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let label = format!("{code} v{next}");
    transition(
        &mut tx,
        &scope,
        &Transition {
            aggregate_type: "accreditation_framework",
            id: fid,
            institution,
            from: None,
            to: "draft",
            version: 1,
            reason: None,
            process_instance_id: None,
            number: Some(&label),
            subject_ref: None,
        },
        None,
        "services.accreditation_framework.create",
        &a,
    )
    .await?;
    let f = load_framework(&mut tx, t, fid, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::CREATED, &f))
}

/// Framework versions of an institution (`services.accreditation.manage`).
#[utoipa::path(get, path = "/api/v1/services/institutions/{id}/accreditation-frameworks", operation_id = "services_accreditation_framework_list", tag = "services-accreditation",
    params(("id" = String, Path, format = Uuid), AccreditationFrameworkQuery), security(("session_cookie" = [])),
    responses((status = 200, body = AccreditationFrameworkList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn framework_list(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<AccreditationFrameworkQuery>,
) -> ApiResult<Json<AccreditationFrameworkList>> {
    let ctx = auth.require(Permission::AccreditationManage)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let status = match q.status.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => Some(
            FrameworkStatus::parse(s)
                .ok_or_else(|| invalid("status", "invalid", "unknown value"))?,
        ),
        None => None,
    };
    let rows = sqlx::query(&format!(
        "{SELECT} WHERE tenant_id = $1 AND institution_id = $2 AND ($3::text IS NULL OR status = $3) \
         ORDER BY code, version_no DESC LIMIT $4"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(status.map(|s| s.as_str()))
    .bind(page_limit(q.limit))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(AccreditationFrameworkList { items }))
}

/// One framework version (`services.accreditation.manage`).
#[utoipa::path(get, path = "/api/v1/services/accreditation-frameworks/{id}", operation_id = "services_accreditation_framework_get", tag = "services-accreditation",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = AccreditationFrameworkView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn framework_get(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AccreditationManage)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let f = load_framework(&mut conn, ctx.scope.tenant_id, id, false).await?;
    Ok(respond(StatusCode::OK, &f))
}

async fn activate(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    f: &AccreditationFrameworkView,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let t = scope.tenant_id;
    let inst = InstitutionId(f.institution_id);
    let previous: Option<(Uuid, i64)> = sqlx::query_as(
        "UPDATE services.accreditation_framework SET status = 'superseded', version = version + 1, \
         updated_at = now() WHERE tenant_id = $1 AND institution_id = $2 AND code = $3 \
         AND status = 'active' RETURNING id, version",
    )
    .bind(t.as_uuid())
    .bind(f.institution_id)
    .bind(&f.code)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if let Some((pid, pv)) = previous {
        transition(
            conn,
            scope,
            &Transition {
                aggregate_type: "accreditation_framework",
                id: pid,
                institution: inst,
                from: Some("active"),
                to: "superseded",
                version: pv,
                reason: Some("superseded by a newer version"),
                process_instance_id: None,
                number: Some(&f.code),
                subject_ref: None,
            },
            None,
            "services.accreditation_framework.supersede",
            actor,
        )
        .await?;
    }
    let v: i64 = sqlx::query_scalar(
        "UPDATE services.accreditation_framework SET status = 'active', approval_state = 'approved', \
         status_reason = $3, version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 RETURNING version",
    )
    .bind(f.id)
    .bind(t.as_uuid())
    .bind(reason)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let label = format!("{} v{}", f.code, f.version_no);
    transition(
        conn,
        scope,
        &Transition {
            aggregate_type: "accreditation_framework",
            id: f.id,
            institution: inst,
            from: Some("draft"),
            to: "active",
            version: v,
            reason,
            process_instance_id: f.process_instance_id.as_deref(),
            number: Some(&label),
            subject_ref: None,
        },
        Some((
            ACCREDITATION_FRAMEWORK_ACTIVATED,
            ACCREDITATION_FRAMEWORK_ACTIVATED_SCHEMA,
        )),
        "services.accreditation_framework.activate",
        actor,
    )
    .await
}

async fn begin(
    st: &ServicesState,
    ctx: &AuthContext,
    id: Uuid,
    v: i64,
) -> Result<
    (
        sqlx::Transaction<'static, sqlx::Postgres>,
        AccreditationFrameworkView,
        ExecutionScope,
    ),
    KernelError,
> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let f = load_framework(&mut tx, ctx.scope.tenant_id, id, true).await?;
    check_version(v, f.version)?;
    let inst = InstitutionId(f.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(st, &scope, inst).await?;
    Ok((tx, f, scope))
}

/// Submit a draft framework version (`services.accreditation.manage`;
/// `ACCREDITATION_FRAMEWORK_APPROVAL`): auto-approval activates it at once.
#[utoipa::path(post, path = "/api/v1/services/accreditation-frameworks/{id}/submit", operation_id = "services_accreditation_framework_submit", tag = "services-accreditation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = ReportReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AccreditationFrameworkView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn framework_submit(
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
        load_framework(&mut conn, t, id, false).await?
    };
    check_version(v, probe.version)?;
    if probe.status != FrameworkStatus::Draft
        || probe.approval_state == AccreditationApprovalState::Pending
    {
        return Err(KernelError::Conflict(format!(
            "framework {} v{} is {} ({}); only a draft is submitted once",
            probe.code, probe.version_no, probe.status, probe.approval_state
        ))
        .into());
    }
    let inst = InstitutionId(probe.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let outcome = request_approval(
        &st,
        &scope,
        process::ACCREDITATION_FRAMEWORK_APPROVAL,
        business_key("accreditation_framework", id, probe.version),
        BTreeMap::from([
            ("frameworkId".to_owned(), serde_json::json!(id)),
            ("code".to_owned(), serde_json::json!(probe.code)),
        ]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let (mut tx, f, scope) = begin(&st, ctx, id, v).await?;
    let nv: i64 = sqlx::query_scalar(
        "UPDATE services.accreditation_framework SET approval_state = 'pending', \
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
    let label = format!("{} v{}", f.code, f.version_no);
    transition(
        &mut tx,
        &scope,
        &Transition {
            aggregate_type: "accreditation_framework",
            id,
            institution: inst,
            from: Some(f.approval_state.as_str()),
            to: "pending",
            version: nv,
            reason: reason.as_deref(),
            process_instance_id: Some(&pid),
            number: Some(&label),
            subject_ref: None,
        },
        None,
        "services.accreditation_framework.submit",
        &a,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let cur = load_framework(&mut tx, t, id, true).await?;
        activate(
            &mut tx,
            &scope,
            &cur,
            Some("auto-approved"),
            &Actor::system(a.correlation_id.clone()),
        )
        .await?;
    }
    let f = load_framework(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &f))
}

/// Decide a submitted framework version (`services.report.approve`, step-up): approve → active
/// (supersedes the previous version); reject (reason required) → draft, approval `rejected`.
#[utoipa::path(post, path = "/api/v1/services/accreditation-frameworks/{id}/decision", operation_id = "services_accreditation_framework_decide", tag = "services-accreditation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = ReportDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AccreditationFrameworkView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn framework_decide(
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
    let (mut tx, f, scope) = begin(&st, ctx, id, v).await?;
    if f.approval_state != AccreditationApprovalState::Pending || f.status != FrameworkStatus::Draft
    {
        return Err(KernelError::Conflict(format!(
            "framework {} v{} is not waiting for a decision",
            f.code, f.version_no
        ))
        .into());
    }
    if approve {
        activate(&mut tx, &scope, &f, reason.as_deref(), &a).await?;
    } else {
        let nv: i64 = sqlx::query_scalar(
            "UPDATE services.accreditation_framework SET approval_state = 'rejected', \
             status_reason = $3, version = version + 1, updated_at = now() \
             WHERE id = $1 AND tenant_id = $2 RETURNING version",
        )
        .bind(id)
        .bind(t.as_uuid())
        .bind(reason.as_deref())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        let label = format!("{} v{}", f.code, f.version_no);
        transition(
            &mut tx,
            &scope,
            &Transition {
                aggregate_type: "accreditation_framework",
                id,
                institution: InstitutionId(f.institution_id),
                from: Some("pending"),
                to: "rejected",
                version: nv,
                reason: reason.as_deref(),
                process_instance_id: f.process_instance_id.as_deref(),
                number: Some(&label),
                subject_ref: None,
            },
            None,
            "services.accreditation_framework.reject",
            &a,
        )
        .await?;
    }
    let f = load_framework(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &f))
}

pub fn router(state: ServicesState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(framework_create, framework_list))
        .routes(routes!(framework_get))
        .routes(routes!(framework_submit))
        .routes(routes!(framework_decide))
        .with_state(state)
}
