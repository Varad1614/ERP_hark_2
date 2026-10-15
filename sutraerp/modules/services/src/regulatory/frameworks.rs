//! Regulatory reporting frameworks (L3-H-055 framework change → active reporting definition):
//! an AISHE / NIRF / NAAC data definition is a versioned list of operational figures (validated
//! against the live catalogue) plus manual fields. Draft → submitted (`REPORTING_FRAMEWORK_APPROVAL`)
//! → approved → active (`reporting_framework.activated`); the previous active version of the same
//! code is superseded, cycles keep the plan they were opened with.

use super::domain::{
    valid_field_key, ReportingApprovalState, ReportingFrameworkStatus, ReportingManualField,
};
use crate::dashboards::common::{
    check_version, clean, decode, expected, invalid, required_text, transition, valid_code,
    ReportDecision, ReportDecisionBody, ReportReasonBody, Transition,
};
use crate::dashboards::domain::binding_problem;
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
use sutra_contracts::services::events::reporting as ev;
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// A reporting framework version.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReportingFrameworkView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "AISHE")]
    pub code: String,
    pub name: String,
    pub regulator: String,
    pub version_no: i32,
    pub metric_keys: Vec<String>,
    pub manual_fields: Vec<ReportingManualField>,
    pub status: ReportingFrameworkStatus,
    pub approval_state: ReportingApprovalState,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub predecessor_id: Option<Uuid>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReportingFrameworkList {
    pub items: Vec<ReportingFrameworkView>,
}

const SELECT: &str = "SELECT id, institution_id, code, name, regulator, version_no, metric_keys, \
    manual_fields, status, approval_state, status_reason, process_instance_id, predecessor_id, \
    version FROM services.reporting_framework";

fn from_row(r: &PgRow) -> Result<ReportingFrameworkView, sqlx::Error> {
    let fields: serde_json::Value = r.try_get("manual_fields")?;
    Ok(ReportingFrameworkView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        code: r.try_get("code")?,
        name: r.try_get("name")?,
        regulator: r.try_get("regulator")?,
        version_no: r.try_get("version_no")?,
        metric_keys: r.try_get("metric_keys")?,
        manual_fields: serde_json::from_value(fields)
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
        status: decode(r.try_get("status")?, ReportingFrameworkStatus::parse)?,
        approval_state: decode(r.try_get("approval_state")?, ReportingApprovalState::parse)?,
        status_reason: r.try_get("status_reason")?,
        process_instance_id: r.try_get("process_instance_id")?,
        predecessor_id: r.try_get("predecessor_id")?,
        version: r.try_get("version")?,
    })
}

/// Loads one framework of the tenant (404), optionally locked.
pub async fn load_framework(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<ReportingFrameworkView, KernelError> {
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
            entity: "reporting framework",
        })?;
    from_row(&row).map_err(map_db_error)
}

/// A reporting definition (new code or new version of an existing code).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReportingFrameworkCreate {
    pub code: String,
    pub name: String,
    /// `AISHE`, `NIRF`, `NAAC` … (the receiving body).
    pub regulator: String,
    /// Operational figures (`<domain>.<key>`) the return needs.
    pub metric_keys: Vec<String>,
    #[serde(default)]
    pub manual_fields: Vec<ReportingManualField>,
}

/// Define a reporting framework version (`services.report.manage`; Idempotency-Key): version n + 1
/// of the code, its predecessor the active version. 422 for unknown figures or invalid fields.
#[utoipa::path(post, path = "/api/v1/services/institutions/{id}/reporting-frameworks", operation_id = "services_regulatory_framework_create", tag = "services-regulatory",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = ReportingFrameworkCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ReportingFrameworkView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReportingFrameworkCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let code = valid_code("code", &b.code)?;
    let name = required_text("name", &b.name, 200)?;
    let regulator = required_text("regulator", &b.regulator, 100)?;
    let mut keys: Vec<String> = b.metric_keys.iter().map(|k| k.trim().to_owned()).collect();
    keys.sort();
    keys.dedup();
    if keys.is_empty() && b.manual_fields.is_empty() {
        return Err(invalid(
            "metricKeys",
            "empty",
            "a framework needs at least one figure or manual field",
        )
        .into());
    }
    let catalogue = st.all_metrics(&scope, institution).await?;
    for k in &keys {
        if let Some(problem) = binding_problem(k, None, &catalogue) {
            return Err(invalid("metricKeys", "unknown_metric", &problem).into());
        }
    }
    let mut fields = Vec::with_capacity(b.manual_fields.len());
    for f in b.manual_fields {
        let key = f.key.trim().to_owned();
        if !valid_field_key(&key) || fields.iter().any(|x: &ReportingManualField| x.key == key) {
            return Err(invalid(
                "manualFields",
                "invalid_key",
                "unique keys matching ^[a-z][a-z0-9_]{0,47}$",
            )
            .into());
        }
        fields.push(ReportingManualField {
            key,
            label: required_text("manualFields.label", &f.label, 200)?,
            required: f.required,
        });
    }
    let actor = actor(ctx);
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let (next, predecessor): (i32, Option<Uuid>) = sqlx::query_as(
        "SELECT COALESCE(max(version_no), 0) + 1, (SELECT id FROM services.reporting_framework \
         WHERE tenant_id = $1 AND institution_id = $2 AND code = $3 AND status = 'active') \
         FROM services.reporting_framework WHERE tenant_id = $1 AND institution_id = $2 AND code = $3",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&code)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let fid: Uuid = sqlx::query_scalar(
        "INSERT INTO services.reporting_framework (tenant_id, institution_id, code, name, regulator, \
         version_no, metric_keys, manual_fields, predecessor_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&code)
    .bind(&name)
    .bind(&regulator)
    .bind(next)
    .bind(&keys)
    .bind(serde_json::to_value(&fields).map_err(KernelError::internal)?)
    .bind(predecessor)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let label = format!("{code} v{next}");
    transition(
        &mut tx,
        &scope,
        &Transition {
            aggregate_type: "reporting_framework",
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
        "services.reporting_framework.create",
        &actor,
    )
    .await?;
    let f = load_framework(&mut tx, t, fid, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &f, Version(f.version)))
}

/// Filters of the framework list.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ReportingFrameworkQuery {
    pub status: Option<ReportingFrameworkStatus>,
}

/// Reporting frameworks of an institution (`services.report.read`).
#[utoipa::path(get, path = "/api/v1/services/institutions/{id}/reporting-frameworks", operation_id = "services_regulatory_framework_list", tag = "services-regulatory",
    params(("id" = String, Path, format = Uuid), ReportingFrameworkQuery), security(("session_cookie" = [])),
    responses((status = 200, body = ReportingFrameworkList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ReportingFrameworkQuery>,
) -> ApiResult<Json<ReportingFrameworkList>> {
    let ctx = auth.require(Permission::ReportRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let rows = sqlx::query(&format!(
        "{SELECT} WHERE tenant_id = $1 AND institution_id = $2 AND ($3::text IS NULL OR status = $3) \
         ORDER BY code, version_no DESC"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(q.status.map(|s| s.as_str()))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(ReportingFrameworkList { items }))
}

/// One framework version (`services.report.read`).
#[utoipa::path(get, path = "/api/v1/services/reporting-frameworks/{id}", operation_id = "services_regulatory_framework_get", tag = "services-regulatory",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ReportingFrameworkView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let f = load_framework(&mut conn, ctx.scope.tenant_id, id, false).await?;
    Ok(with_etag(StatusCode::OK, &f, Version(f.version)))
}

#[allow(clippy::too_many_arguments)]
async fn set_approval(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    f: &ReportingFrameworkView,
    to: ReportingApprovalState,
    reason: Option<&str>,
    pid: Option<&str>,
    action: &'static str,
    actor: &Actor,
) -> Result<ReportingFrameworkView, KernelError> {
    sqlx::query(
        "UPDATE services.reporting_framework SET approval_state = $2, status_reason = COALESCE($3, status_reason), \
         process_instance_id = COALESCE($4, process_instance_id), version = version + 1, updated_at = now() \
         WHERE id = $1",
    )
    .bind(f.id)
    .bind(to.as_str())
    .bind(reason)
    .bind(pid)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let label = format!("{} v{}", f.code, f.version_no);
    transition(
        conn,
        scope,
        &Transition {
            aggregate_type: "reporting_framework",
            id: f.id,
            institution: InstitutionId(f.institution_id),
            from: Some(f.approval_state.as_str()),
            to: to.as_str(),
            version: f.version + 1,
            reason,
            process_instance_id: pid.or(f.process_instance_id.as_deref()),
            number: Some(&label),
            subject_ref: None,
        },
        None,
        action,
        actor,
    )
    .await?;
    load_framework(conn, scope.tenant_id, f.id, true).await
}

/// Submit a draft framework for approval (`services.report.manage`; If-Match;
/// `REPORTING_FRAMEWORK_APPROVAL`). Auto-approval leaves it `approved`, ready to activate.
#[utoipa::path(post, path = "/api/v1/services/reporting-frameworks/{id}/submit", operation_id = "services_regulatory_framework_submit", tag = "services-regulatory",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = ReportReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ReportingFrameworkView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn submit(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReportReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_framework(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    check_version(v, pre.version)?;
    let open = matches!(
        pre.approval_state,
        ReportingApprovalState::NotRequested | ReportingApprovalState::Rejected
    );
    if pre.status != ReportingFrameworkStatus::Draft || !open {
        return Err(KernelError::Conflict(format!(
            "framework {} v{} is {} / {}; only an unsubmitted draft can be submitted",
            pre.code, pre.version_no, pre.status, pre.approval_state
        ))
        .into());
    }
    let mut vars = BTreeMap::new();
    vars.insert("framework".to_owned(), serde_json::json!(pre.code));
    vars.insert("versionNo".to_owned(), serde_json::json!(pre.version_no));
    let outcome = request_approval(
        &st,
        &scope,
        process::REPORTING_FRAMEWORK_APPROVAL,
        business_key("reporting_framework", pre.id, pre.version),
        vars,
    )
    .await?;
    let actor = actor(ctx);
    let reason = clean(b.reason);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let f = load_framework(&mut tx, t, id, true).await?;
    check_version(v, f.version)?;
    let f = set_approval(
        &mut tx,
        &scope,
        &f,
        ReportingApprovalState::Pending,
        reason.as_deref(),
        Some(outcome.process_instance_id()),
        "services.reporting_framework.submit",
        &actor,
    )
    .await?;
    let f = if let ApprovalOutcome::Approved { .. } = outcome {
        set_approval(
            &mut tx,
            &scope,
            &f,
            ReportingApprovalState::Approved,
            None,
            None,
            "services.reporting_framework.approve",
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?
    } else {
        f
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &f, Version(f.version)))
}

/// Decide a submitted framework (`services.report.approve`, step-up; If-Match).
#[utoipa::path(post, path = "/api/v1/services/reporting-frameworks/{id}/decision", operation_id = "services_regulatory_framework_decide", tag = "services-regulatory",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = ReportDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ReportingFrameworkView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn decide(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReportDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportApprove)?;
    let v = expected!(headers);
    let reason = clean(b.reason);
    if b.decision == ReportDecision::Reject && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_framework(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let f = load_framework(&mut tx, t, id, true).await?;
    check_version(v, f.version)?;
    if f.approval_state != ReportingApprovalState::Pending {
        return Err(KernelError::Conflict(format!(
            "framework {} v{} is not awaiting a decision ({})",
            f.code, f.version_no, f.approval_state
        ))
        .into());
    }
    let (to, action) = match b.decision {
        ReportDecision::Approve => (
            ReportingApprovalState::Approved,
            "services.reporting_framework.approve",
        ),
        ReportDecision::Reject => (
            ReportingApprovalState::Rejected,
            "services.reporting_framework.reject",
        ),
    };
    let f = set_approval(
        &mut tx,
        &scope,
        &f,
        to,
        reason.as_deref(),
        None,
        action,
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &f, Version(f.version)))
}

/// Activate an approved framework version (`services.report.manage`; If-Match): the active
/// version of the same code is superseded.
#[utoipa::path(post, path = "/api/v1/services/reporting-frameworks/{id}/activate", operation_id = "services_regulatory_framework_activate", tag = "services-regulatory",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = ReportReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ReportingFrameworkView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn activate(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReportReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_framework(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let actor = actor(ctx);
    let reason = clean(b.reason);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let f = load_framework(&mut tx, t, id, true).await?;
    check_version(v, f.version)?;
    if f.status != ReportingFrameworkStatus::Draft
        || f.approval_state != ReportingApprovalState::Approved
    {
        return Err(KernelError::Conflict(format!(
            "framework {} v{} is {} / {}; only an approved draft can be activated",
            f.code, f.version_no, f.status, f.approval_state
        ))
        .into());
    }
    let previous: Vec<(Uuid, i64, i32)> = sqlx::query_as(
        "SELECT id, version, version_no FROM services.reporting_framework WHERE tenant_id = $1 \
         AND institution_id = $2 AND code = $3 AND status = 'active' FOR UPDATE",
    )
    .bind(t.as_uuid())
    .bind(f.institution_id)
    .bind(&f.code)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;
    for (old, old_version, old_no) in previous {
        sqlx::query(
            "UPDATE services.reporting_framework SET status = 'superseded', version = version + 1, \
             updated_at = now() WHERE id = $1",
        )
        .bind(old)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        let label = format!("{} v{old_no}", f.code);
        transition(
            &mut tx,
            &scope,
            &Transition {
                aggregate_type: "reporting_framework",
                id: old,
                institution,
                from: Some("active"),
                to: "superseded",
                version: old_version + 1,
                reason: Some("superseded by a newer version"),
                process_instance_id: None,
                number: Some(&label),
                subject_ref: None,
            },
            None,
            "services.reporting_framework.supersede",
            &actor,
        )
        .await?;
    }
    sqlx::query(
        "UPDATE services.reporting_framework SET status = 'active', activated_at = now(), \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let label = format!("{} v{}", f.code, f.version_no);
    transition(
        &mut tx,
        &scope,
        &Transition {
            aggregate_type: "reporting_framework",
            id,
            institution,
            from: Some("draft"),
            to: "active",
            version: f.version + 1,
            reason: reason.as_deref(),
            process_instance_id: f.process_instance_id.as_deref(),
            number: Some(&label),
            subject_ref: None,
        },
        Some((
            ev::REPORTING_FRAMEWORK_ACTIVATED,
            ev::REPORTING_FRAMEWORK_ACTIVATED_SCHEMA,
        )),
        "services.reporting_framework.activate",
        &actor,
    )
    .await?;
    let out = load_framework(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

pub fn router(state: ServicesState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(create, list))
        .routes(routes!(get))
        .routes(routes!(submit))
        .routes(routes!(decide))
        .routes(routes!(activate))
        .with_state(state)
}
