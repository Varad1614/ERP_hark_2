//! Versioned governance configuration (L3-G-014 governance configuration change → effective
//! state): a version of one key (`readiness_threshold`, `risk_appetite`, `review_cadence`,
//! `kpi_thresholds`) is drafted, submitted (`GOVERNANCE_CONFIG_APPROVAL`) and, once approved,
//! becomes **effective**, superseding the previous effective version of the key
//! (`governance_config.effective`). The closure rules read the effective versions.

use super::domain::{config_problem, ApprovalState, ConfigKey, ConfigStatus, GovernancePolicy};
use super::policy::effective_policy;
use crate::infrastructure::common::{
    audit, clean, emit_status, ensure_version, expected, history, invalid, limit, parse_filter,
    EventExtras, FacilityDecisionBody, FacilityReasonBody, Transition,
};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::rows::decode;
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, CampusState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::campus::events::governance::{
    GOVERNANCE_CONFIG_EFFECTIVE, GOVERNANCE_CONFIG_EFFECTIVE_SCHEMA,
};
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

/// One configuration version.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceConfigView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub key: ConfigKey,
    pub version_no: i32,
    #[schema(value_type = Object)]
    pub settings: serde_json::Value,
    pub status: ConfigStatus,
    pub approval_state: ApprovalState,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub predecessor_id: Option<Uuid>,
    pub effective_at: Option<chrono::DateTime<chrono::Utc>>,
    pub version: i64,
}

/// Configuration versions.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[schema(as = CampusGovernanceConfigList)]
pub struct GovernanceConfigList {
    pub items: Vec<GovernanceConfigView>,
}

/// The effective policy and the versions behind it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GovernancePolicyView {
    pub policy: GovernancePolicy,
    pub effective: Vec<GovernanceConfigView>,
}

/// Draft a configuration version.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceConfigCreate {
    pub key: ConfigKey,
    /// `readiness_threshold` `{minCriterionScore 1..5}`, `risk_appetite` `{maxResidualScore 1..25}`,
    /// `review_cadence` `{operationalDays, executiveDays}`, `kpi_thresholds` `{<campus figure>: number}`.
    #[schema(value_type = Object)]
    pub settings: serde_json::Value,
}

/// Filters of the version list.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceConfigQuery {
    pub key: Option<String>,
    pub limit: Option<i64>,
}

const SELECT: &str = "SELECT id, institution_id, key, version_no, settings, status, \
    approval_state, status_reason, process_instance_id, predecessor_id, effective_at, version \
    FROM campus.governance_config_version";

fn from_row(r: &PgRow) -> Result<GovernanceConfigView, sqlx::Error> {
    Ok(GovernanceConfigView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        key: decode(r.try_get("key")?, ConfigKey::parse)?,
        version_no: r.try_get("version_no")?,
        settings: r.try_get("settings")?,
        status: decode(r.try_get("status")?, ConfigStatus::parse)?,
        approval_state: decode(r.try_get("approval_state")?, ApprovalState::parse)?,
        status_reason: r.try_get("status_reason")?,
        process_instance_id: r.try_get("process_instance_id")?,
        predecessor_id: r.try_get("predecessor_id")?,
        effective_at: r.try_get("effective_at")?,
        version: r.try_get("version")?,
    })
}

async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<GovernanceConfigView, KernelError> {
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
            entity: "governance configuration",
        })?;
    from_row(&row).map_err(map_db_error)
}

fn respond(status: StatusCode, v: &GovernanceConfigView) -> Response {
    with_etag(status, v, Version(v.version))
}

/// Makes a pending version effective (supersedes the previous effective one) in the caller's tx.
async fn make_effective(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    c: &GovernanceConfigView,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let t = scope.tenant_id;
    let inst = InstitutionId(c.institution_id);
    let previous: Option<Uuid> = sqlx::query_scalar(
        "UPDATE campus.governance_config_version SET status = 'superseded', version = version + 1, \
         updated_at = now() WHERE tenant_id = $1 AND institution_id = $2 AND key = $3 \
         AND status = 'effective' RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(c.institution_id)
    .bind(c.key.as_str())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if let Some(p) = previous {
        history(
            conn,
            &Transition {
                tenant: t,
                institution: inst,
                aggregate_type: "governance_config_version",
                aggregate_id: p,
                from: Some("effective"),
                to: "superseded",
                reason: Some("superseded by a newer version"),
                process_instance_id: None,
                cause: GOVERNANCE_CONFIG_EFFECTIVE,
            },
            actor,
        )
        .await?;
    }
    let v: i64 = sqlx::query_scalar(
        "UPDATE campus.governance_config_version SET status = 'effective', \
         approval_state = 'approved', status_reason = $3, effective_at = now(), \
         predecessor_id = COALESCE(predecessor_id, $4), version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 RETURNING version",
    )
    .bind(c.id)
    .bind(t.as_uuid())
    .bind(reason)
    .bind(previous)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let tr = Transition {
        tenant: t,
        institution: inst,
        aggregate_type: "governance_config_version",
        aggregate_id: c.id,
        from: Some("draft"),
        to: "effective",
        reason,
        process_instance_id: c.process_instance_id.as_deref(),
        cause: GOVERNANCE_CONFIG_EFFECTIVE,
    };
    history(conn, &tr, actor).await?;
    emit_status(
        conn,
        scope,
        (
            GOVERNANCE_CONFIG_EFFECTIVE,
            GOVERNANCE_CONFIG_EFFECTIVE_SCHEMA,
        ),
        &tr,
        v,
        &EventExtras {
            number: Some(format!("{} v{}", c.key, c.version_no)),
            subject_ref: None,
        },
        actor,
    )
    .await
}

async fn begin(
    st: &CampusState,
    ctx: &AuthContext,
    id: Uuid,
    expected_version: i64,
) -> Result<
    (
        sqlx::Transaction<'static, sqlx::Postgres>,
        GovernanceConfigView,
        ExecutionScope,
    ),
    KernelError,
> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = load(&mut tx, ctx.scope.tenant_id, id, true).await?;
    ensure_version(expected_version, c.version)?;
    let inst = InstitutionId(c.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(st, &scope, inst).await?;
    Ok((tx, c, scope))
}

/// Draft a configuration version (`campus.governance.manage`; Idempotency-Key): version n + 1
/// of the key; 422 for invalid settings.
#[utoipa::path(post, path = "/api/v1/campus/institutions/{id}/governance-configs", operation_id = "campus_governance_config_create", tag = "campus-governance",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = GovernanceConfigCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = GovernanceConfigView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn config_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<GovernanceConfigCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GovernanceManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    if let Some(p) = config_problem(b.key, &b.settings) {
        return Err(invalid("settings", "invalid", &p).into());
    }
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let next: i32 = sqlx::query_scalar(
        "SELECT COALESCE(max(version_no), 0) + 1 FROM campus.governance_config_version \
         WHERE tenant_id = $1 AND institution_id = $2 AND key = $3",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(b.key.as_str())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let cid: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.governance_config_version (tenant_id, institution_id, key, version_no, \
         settings, created_by) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(b.key.as_str())
    .bind(next)
    .bind(&b.settings)
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        &Transition {
            tenant: t,
            institution,
            aggregate_type: "governance_config_version",
            aggregate_id: cid,
            from: None,
            to: "draft",
            reason: None,
            process_instance_id: None,
            cause: "sutraerp.campus.governance_config.created",
        },
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "campus.governance_config.create",
        "governance_config_version",
        cid,
        serde_json::json!({"key": b.key, "versionNo": next, "settings": b.settings}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v = load(&mut conn, t, cid, false).await?;
    Ok(respond(StatusCode::CREATED, &v))
}

/// Configuration versions of an institution (`campus.governance.manage`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/governance-configs", operation_id = "campus_governance_config_list", tag = "campus-governance",
    params(("id" = String, Path, format = Uuid), GovernanceConfigQuery), security(("session_cookie" = [])),
    responses((status = 200, body = GovernanceConfigList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn config_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<GovernanceConfigQuery>,
) -> ApiResult<Json<GovernanceConfigList>> {
    let ctx = auth.require(Permission::GovernanceManage)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution(&st, &scope, institution).await?;
    let key = parse_filter("key", q.key.as_deref(), ConfigKey::parse)?;
    let sql = format!(
        "{SELECT} WHERE tenant_id = $1 AND institution_id = $2 AND ($3::text IS NULL OR key = $3) \
         ORDER BY key, version_no DESC LIMIT $4"
    );
    let rows = sqlx::query(&sql)
        .bind(ctx.scope.tenant_id.as_uuid())
        .bind(id)
        .bind(key.map(|k| k.as_str()))
        .bind(limit(q.limit))
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(GovernanceConfigList { items }))
}

/// The effective governance policy of an institution (`campus.governance.manage`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/governance-policy", operation_id = "campus_governance_policy_get", tag = "campus-governance",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = GovernancePolicyView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_get(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<GovernancePolicyView>> {
    let ctx = auth.require(Permission::GovernanceManage)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution(&st, &scope, institution).await?;
    let t = ctx.scope.tenant_id;
    let policy = effective_policy(&st.pool, t, institution).await?;
    let sql = format!(
        "{SELECT} WHERE tenant_id = $1 AND institution_id = $2 AND status = 'effective' ORDER BY key"
    );
    let rows = sqlx::query(&sql)
        .bind(t.as_uuid())
        .bind(id)
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
    let effective = rows
        .iter()
        .map(from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(GovernancePolicyView { policy, effective }))
}

/// One configuration version (`campus.governance.manage`).
#[utoipa::path(get, path = "/api/v1/campus/governance-configs/{id}", operation_id = "campus_governance_config_get", tag = "campus-governance",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = GovernanceConfigView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn config_get(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GovernanceManage)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v = load(&mut conn, ctx.scope.tenant_id, id, false).await?;
    Ok(respond(StatusCode::OK, &v))
}

/// Submit a draft version (`campus.governance.manage`; `GOVERNANCE_CONFIG_APPROVAL`):
/// auto-approval makes it effective at once.
#[utoipa::path(post, path = "/api/v1/campus/governance-configs/{id}/submit", operation_id = "campus_governance_config_submit", tag = "campus-governance",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = GovernanceConfigView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn config_submit(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GovernanceManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let reason = clean(b.reason);
    let probe = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load(&mut conn, t, id, false).await?
    };
    ensure_version(v, probe.version)?;
    if probe.status != ConfigStatus::Draft || probe.approval_state == ApprovalState::Pending {
        return Err(KernelError::Conflict(format!(
            "configuration {} v{} is {} ({}); only a draft is submitted once",
            probe.key, probe.version_no, probe.status, probe.approval_state
        ))
        .into());
    }
    let inst = InstitutionId(probe.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let outcome = request_approval(
        &st,
        &scope,
        process::GOVERNANCE_CONFIG_APPROVAL,
        business_key("governance_config_version", id, probe.version),
        BTreeMap::from([
            ("configId".to_owned(), serde_json::json!(id)),
            ("key".to_owned(), serde_json::json!(probe.key)),
        ]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let (mut tx, c, scope) = begin(&st, ctx, id, v).await?;
    sqlx::query(
        "UPDATE campus.governance_config_version SET approval_state = 'pending', \
         process_instance_id = $3, status_reason = $4, version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(&pid)
    .bind(reason.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        &Transition {
            tenant: t,
            institution: InstitutionId(c.institution_id),
            aggregate_type: "governance_config_version",
            aggregate_id: id,
            from: Some("draft"),
            to: "draft",
            reason: Some("submitted for approval"),
            process_instance_id: Some(&pid),
            cause: "",
        },
        &a,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let cur = load(&mut tx, t, id, true).await?;
        make_effective(
            &mut tx,
            &scope,
            &cur,
            Some("auto-approved"),
            &Actor::system(a.correlation_id.clone()),
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "campus.governance_config.submit",
        "governance_config_version",
        id,
        serde_json::json!({"processInstanceId": pid}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v = load(&mut conn, t, id, false).await?;
    Ok(respond(StatusCode::OK, &v))
}

/// Decide a submitted version (`campus.approve`, step-up): approve → effective (supersedes the
/// previous one); reject (reason required) → stays draft, approval `rejected`.
#[utoipa::path(post, path = "/api/v1/campus/governance-configs/{id}/decision", operation_id = "campus_governance_config_decide", tag = "campus-governance",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version")),
    request_body = FacilityDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = GovernanceConfigView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn config_decide(
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
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let (mut tx, c, scope) = begin(&st, ctx, id, v).await?;
    if c.approval_state != ApprovalState::Pending || c.status != ConfigStatus::Draft {
        return Err(KernelError::Conflict(format!(
            "configuration {} v{} is not waiting for a decision",
            c.key, c.version_no
        ))
        .into());
    }
    if b.approve() {
        make_effective(&mut tx, &scope, &c, reason.as_deref(), &a).await?;
    } else {
        sqlx::query(
            "UPDATE campus.governance_config_version SET approval_state = 'rejected', \
             status_reason = $3, version = version + 1, updated_at = now() \
             WHERE id = $1 AND tenant_id = $2",
        )
        .bind(id)
        .bind(t.as_uuid())
        .bind(reason.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        history(
            &mut tx,
            &Transition {
                tenant: t,
                institution: InstitutionId(c.institution_id),
                aggregate_type: "governance_config_version",
                aggregate_id: id,
                from: Some("draft"),
                to: "draft",
                reason: reason.as_deref(),
                process_instance_id: c.process_instance_id.as_deref(),
                cause: "",
            },
            &a,
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "campus.governance_config.decide",
        "governance_config_version",
        id,
        serde_json::json!({"decision": b.decision, "reason": reason}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v = load(&mut conn, t, id, false).await?;
    Ok(respond(StatusCode::OK, &v))
}

pub fn router(state: CampusState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(config_create, config_list))
        .routes(routes!(policy_get))
        .routes(routes!(config_get))
        .routes(routes!(config_submit))
        .routes(routes!(config_decide))
        .with_state(state)
}
