//! Reporting cycles (L3-H-056 initiation → governed collection plan), snapshots (H-057 source
//! data collection → validated snapshot: figures read through `ServicesState::all_metrics` and
//! **frozen** in `report_snapshot_value`), submission packages (H-058 validated snapshot →
//! approved package, `SUBMISSION_APPROVAL`; H-059 submitted → acknowledged with the regulator's
//! reference) and H-060 correction (new snapshot + corrected package) or closure of the cycle.

use super::domain::{
    snapshot_issues, ReportSnapshotStatus, ReportingCycleStatus, ReportingFrameworkStatus,
    ReportingIssue, ReportingManualField, SubmissionPackageStatus,
};
use super::frameworks::load_framework;
use crate::dashboards::common::{
    check_version, clean, decode, expected, invalid, required_reason, required_text, transition,
    ReportDecision, ReportDecisionBody, ReportReasonBody, Transition,
};
use crate::shared::approval::{
    business_key, process, request_approval, ApprovalOutcome, AUTO_APPROVER,
};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::lifecycle::illegal;
use crate::shared::numbering::{self, kind};
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, ServicesState};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::services::events::reporting as ev;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

// ================================================================ cycles

/// The collection plan frozen into a cycle from its framework version.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReportingCollectionPlan {
    pub metric_keys: Vec<String>,
    pub manual_fields: Vec<ReportingManualField>,
}

/// A reporting cycle.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReportingCycleView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub framework_id: Uuid,
    pub framework_code: String,
    pub framework_version_no: i32,
    #[schema(example = "RC-000001")]
    pub number: String,
    #[schema(example = "2025-26")]
    pub period_label: String,
    pub due_on: Option<NaiveDate>,
    pub collection_plan: ReportingCollectionPlan,
    pub status: ReportingCycleStatus,
    pub status_reason: Option<String>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReportingCycleList {
    pub items: Vec<ReportingCycleView>,
}

/// A cycle with its snapshots and packages.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReportingCycleDetail {
    pub cycle: ReportingCycleView,
    pub snapshots: Vec<ReportSnapshotView>,
    pub packages: Vec<SubmissionPackageView>,
}

const CYCLE_SELECT: &str =
    "SELECT c.id, c.institution_id, c.framework_id, f.code AS framework_code, \
    f.version_no AS framework_version_no, c.number, c.period_label, c.due_on, c.collection_plan, \
    c.status, c.status_reason, c.version FROM services.reporting_cycle c \
    JOIN services.reporting_framework f ON f.id = c.framework_id";

fn cycle_from_row(r: &PgRow) -> Result<ReportingCycleView, sqlx::Error> {
    let plan: serde_json::Value = r.try_get("collection_plan")?;
    Ok(ReportingCycleView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        framework_id: r.try_get("framework_id")?,
        framework_code: r.try_get("framework_code")?,
        framework_version_no: r.try_get("framework_version_no")?,
        number: r.try_get("number")?,
        period_label: r.try_get("period_label")?,
        due_on: r.try_get("due_on")?,
        collection_plan: serde_json::from_value(plan)
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
        status: decode(r.try_get("status")?, ReportingCycleStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        version: r.try_get("version")?,
    })
}

async fn load_cycle(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<ReportingCycleView, KernelError> {
    if lock {
        sqlx::query(
            "SELECT id FROM services.reporting_cycle WHERE id = $1 AND tenant_id = $2 FOR UPDATE",
        )
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    let row = sqlx::query(&format!(
        "{CYCLE_SELECT} WHERE c.id = $1 AND c.tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "reporting cycle",
    })?;
    cycle_from_row(&row).map_err(map_db_error)
}

async fn cycles_where(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: Option<Uuid>,
    framework: Option<Uuid>,
) -> Result<Vec<ReportingCycleView>, KernelError> {
    let rows = sqlx::query(&format!(
        "{CYCLE_SELECT} WHERE c.tenant_id = $1 AND ($2::uuid IS NULL OR c.institution_id = $2) \
         AND ($3::uuid IS NULL OR c.framework_id = $3) ORDER BY c.created_at DESC LIMIT 200"
    ))
    .bind(tenant.as_uuid())
    .bind(institution)
    .bind(framework)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(cycle_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)
}

#[allow(clippy::too_many_arguments)]
async fn set_cycle(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    c: &ReportingCycleView,
    to: ReportingCycleStatus,
    reason: Option<&str>,
    event: Option<(&'static str, &'static str)>,
    action: &'static str,
    actor: &Actor,
) -> Result<(), KernelError> {
    if !c.status.can_transition(to) {
        return Err(illegal(
            "reporting cycle",
            &c.number,
            c.status.as_str(),
            to.as_str(),
        ));
    }
    sqlx::query(
        "UPDATE services.reporting_cycle SET status = $2, status_reason = $3, version = version + 1, \
         updated_at = now() WHERE id = $1",
    )
    .bind(c.id)
    .bind(to.as_str())
    .bind(reason)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    transition(
        conn,
        scope,
        &Transition {
            aggregate_type: "reporting_cycle",
            id: c.id,
            institution: InstitutionId(c.institution_id),
            from: Some(c.status.as_str()),
            to: to.as_str(),
            version: c.version + 1,
            reason,
            process_instance_id: None,
            number: Some(&c.number),
            subject_ref: None,
        },
        event,
        action,
        actor,
    )
    .await
}

/// A cycle to open.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReportingCycleCreate {
    pub period_label: String,
    pub due_on: Option<NaiveDate>,
}

/// Open a reporting cycle of an active framework (`services.report.manage`; Idempotency-Key):
/// `RC-…`, collection plan frozen from the framework version. 409 for an inactive framework or
/// a period already opened.
#[utoipa::path(post, path = "/api/v1/services/reporting-frameworks/{id}/cycles", operation_id = "services_regulatory_cycle_create", tag = "services-regulatory",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = ReportingCycleCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ReportingCycleView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create_cycle(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReportingCycleCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportManage)?;
    require_idempotency_key(&headers)?;
    let period = required_text("periodLabel", &b.period_label, 40)?;
    let t = ctx.scope.tenant_id;
    let f = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_framework(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(f.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    if f.status != ReportingFrameworkStatus::Active {
        return Err(KernelError::Conflict(format!(
            "framework {} v{} is {}; cycles open on the active version",
            f.code, f.version_no, f.status
        ))
        .into());
    }
    let plan = ReportingCollectionPlan {
        metric_keys: f.metric_keys.clone(),
        manual_fields: f.manual_fields.clone(),
    };
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let number = numbering::next(&mut tx, t, institution, kind::REPORTING_CYCLE).await?;
    let cid: Uuid = sqlx::query_scalar(
        "INSERT INTO services.reporting_cycle (tenant_id, institution_id, framework_id, number, \
         period_label, due_on, collection_plan) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(f.institution_id)
    .bind(id)
    .bind(&number)
    .bind(&period)
    .bind(b.due_on)
    .bind(serde_json::to_value(&plan).map_err(KernelError::internal)?)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match map_db_error(e) {
        KernelError::Conflict(_) => {
            KernelError::Conflict(format!("a {} cycle for {period} already exists", f.code))
        }
        other => other,
    })?;
    transition(
        &mut tx,
        &scope,
        &Transition {
            aggregate_type: "reporting_cycle",
            id: cid,
            institution,
            from: None,
            to: "planned",
            version: 1,
            reason: Some(&period),
            process_instance_id: None,
            number: Some(&number),
            subject_ref: None,
        },
        None,
        "services.reporting_cycle.open",
        &actor,
    )
    .await?;
    let c = load_cycle(&mut tx, t, cid, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &c, Version(c.version)))
}

/// Cycles of a framework, newest first (`services.report.read`).
#[utoipa::path(get, path = "/api/v1/services/reporting-frameworks/{id}/cycles", operation_id = "services_regulatory_cycle_list", tag = "services-regulatory",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ReportingCycleList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list_cycles(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ReportingCycleList>> {
    let ctx = auth.require(Permission::ReportRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let items = cycles_where(&mut conn, ctx.scope.tenant_id, None, Some(id)).await?;
    Ok(Json(ReportingCycleList { items }))
}

/// Cycles of an institution, newest first (`services.report.read`).
#[utoipa::path(get, path = "/api/v1/services/institutions/{id}/reporting-cycles", operation_id = "services_regulatory_cycle_institution_list", tag = "services-regulatory",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ReportingCycleList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list_institution_cycles(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ReportingCycleList>> {
    let ctx = auth.require(Permission::ReportRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let items = cycles_where(&mut conn, ctx.scope.tenant_id, Some(id), None).await?;
    Ok(Json(ReportingCycleList { items }))
}

/// One cycle with its snapshots and packages (`services.report.read`).
#[utoipa::path(get, path = "/api/v1/services/reporting-cycles/{id}", operation_id = "services_regulatory_cycle_get", tag = "services-regulatory",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ReportingCycleDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get_cycle(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let d = detail(&mut conn, ctx.scope.tenant_id, id).await?;
    let v = d.cycle.version;
    Ok(with_etag(StatusCode::OK, &d, Version(v)))
}

async fn detail(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> Result<ReportingCycleDetail, KernelError> {
    let cycle = load_cycle(conn, tenant, id, false).await?;
    let rows = sqlx::query(&format!(
        "{SNAPSHOT_SELECT} WHERE cycle_id = $1 ORDER BY seq DESC"
    ))
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let snapshots = rows
        .iter()
        .map(snapshot_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    let packages = packages_of(conn, id).await?;
    Ok(ReportingCycleDetail {
        cycle,
        snapshots,
        packages,
    })
}

// ================================================================ snapshots

/// One frozen value of a snapshot.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReportSnapshotValueView {
    pub key: String,
    pub label: String,
    /// `metric` (from the owning domain) or `manual`.
    pub source: String,
    #[schema(value_type = Option<String>, example = "1250")]
    pub value: Option<String>,
    pub value_text: Option<String>,
    pub unit: Option<String>,
}

/// A collected snapshot (values only on the single-snapshot reads).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReportSnapshotView {
    pub id: Uuid,
    pub cycle_id: Uuid,
    pub seq: i32,
    pub status: ReportSnapshotStatus,
    pub issues: Vec<ReportingIssue>,
    pub status_reason: Option<String>,
    pub collected_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
    pub values: Vec<ReportSnapshotValueView>,
    #[serde(skip)]
    pub institution_id: Uuid,
}

const SNAPSHOT_SELECT: &str = "SELECT id, institution_id, cycle_id, seq, status, issues, \
    status_reason, collected_by, created_at, version FROM services.report_snapshot";

fn snapshot_from_row(r: &PgRow) -> Result<ReportSnapshotView, sqlx::Error> {
    let issues: serde_json::Value = r.try_get("issues")?;
    Ok(ReportSnapshotView {
        id: r.try_get("id")?,
        cycle_id: r.try_get("cycle_id")?,
        seq: r.try_get("seq")?,
        status: decode(r.try_get("status")?, ReportSnapshotStatus::parse)?,
        issues: serde_json::from_value(issues).map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
        status_reason: r.try_get("status_reason")?,
        collected_by: r.try_get("collected_by")?,
        created_at: r.try_get("created_at")?,
        version: r.try_get("version")?,
        values: Vec::new(),
        institution_id: r.try_get("institution_id")?,
    })
}

async fn load_snapshot(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<ReportSnapshotView, KernelError> {
    let sql = format!(
        "{SNAPSHOT_SELECT} WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "report snapshot",
        })?;
    let mut s = snapshot_from_row(&row).map_err(map_db_error)?;
    let rows = sqlx::query(
        "SELECT key, label, source, value, value_text, unit FROM services.report_snapshot_value \
         WHERE snapshot_id = $1 ORDER BY source, key",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    for r in &rows {
        let value: Option<Decimal> = r.try_get("value").map_err(map_db_error)?;
        s.values.push(ReportSnapshotValueView {
            key: r.try_get("key").map_err(map_db_error)?,
            label: r.try_get("label").map_err(map_db_error)?,
            source: r.try_get("source").map_err(map_db_error)?,
            value: value.map(|v| v.normalize().to_string()),
            value_text: r.try_get("value_text").map_err(map_db_error)?,
            unit: r.try_get("unit").map_err(map_db_error)?,
        });
    }
    Ok(s)
}

/// Manual values of a collection.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReportingCollect {
    #[serde(default)]
    pub manual_values: BTreeMap<String, String>,
}

/// Collect a snapshot (`services.report.manage`; Idempotency-Key; L3-H-057): the plan's figures
/// are read from every domain through the metrics port and frozen with the manual values; the
/// issues found are stored (a snapshot with issues cannot be validated). An earlier unvalidated
/// snapshot is rejected; the cycle moves to `validating`.
#[utoipa::path(post, path = "/api/v1/services/reporting-cycles/{id}/collect", operation_id = "services_regulatory_cycle_collect", tag = "services-regulatory",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = ReportingCollect, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ReportSnapshotView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn collect(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReportingCollect>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_cycle(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let manual: BTreeMap<String, String> = b
        .manual_values
        .into_iter()
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect();
    if manual.values().any(|v| v.chars().count() > 2000) {
        return Err(invalid("manualValues", "too_long", "at most 2000 characters each").into());
    }
    let catalogue = st.all_metrics(&scope, institution).await?;
    let plan = &pre.collection_plan;
    let issues = snapshot_issues(&plan.metric_keys, &plan.manual_fields, &catalogue, &manual);
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = load_cycle(&mut tx, t, id, true).await?;
    if !matches!(
        c.status,
        ReportingCycleStatus::Planned
            | ReportingCycleStatus::Collecting
            | ReportingCycleStatus::Validating
    ) {
        return Err(illegal("reporting cycle", &c.number, c.status.as_str(), "collected").into());
    }
    let open: Vec<(Uuid, i64)> = sqlx::query_as(
        "SELECT id, version FROM services.report_snapshot WHERE cycle_id = $1 AND status = 'collected' FOR UPDATE",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let seq: i32 = sqlx::query_scalar(
        "SELECT COALESCE(max(seq), 0) + 1 FROM services.report_snapshot WHERE cycle_id = $1",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    for (old, old_version) in open {
        let note = format!("superseded by snapshot {seq}");
        sqlx::query(
            "UPDATE services.report_snapshot SET status = 'rejected', status_reason = $2, \
             version = version + 1, updated_at = now() WHERE id = $1",
        )
        .bind(old)
        .bind(&note)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        transition(
            &mut tx,
            &scope,
            &Transition {
                aggregate_type: "report_snapshot",
                id: old,
                institution,
                from: Some("collected"),
                to: "rejected",
                version: old_version + 1,
                reason: Some(&note),
                process_instance_id: None,
                number: Some(&c.number),
                subject_ref: None,
            },
            None,
            "services.report_snapshot.reject",
            &actor,
        )
        .await?;
    }
    let sid: Uuid = sqlx::query_scalar(
        "INSERT INTO services.report_snapshot (tenant_id, institution_id, cycle_id, seq, issues, collected_by) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(c.institution_id)
    .bind(id)
    .bind(seq)
    .bind(serde_json::to_value(&issues).map_err(KernelError::internal)?)
    .bind(actor.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    for k in &plan.metric_keys {
        if let Some(m) = catalogue.iter().find(|m| &m.key == k) {
            sqlx::query(
                "INSERT INTO services.report_snapshot_value (tenant_id, snapshot_id, key, label, source, value, unit) \
                 VALUES ($1, $2, $3, $4, 'metric', $5, $6)",
            )
            .bind(t.as_uuid())
            .bind(sid)
            .bind(&m.key)
            .bind(&m.label)
            .bind(m.value)
            .bind(&m.unit)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        }
    }
    for f in &plan.manual_fields {
        if let Some(v) = manual.get(&f.key).filter(|v| !v.is_empty()) {
            sqlx::query(
                "INSERT INTO services.report_snapshot_value (tenant_id, snapshot_id, key, label, source, value_text) \
                 VALUES ($1, $2, $3, $4, 'manual', $5)",
            )
            .bind(t.as_uuid())
            .bind(sid)
            .bind(&f.key)
            .bind(&f.label)
            .bind(v)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        }
    }
    let note = format!("snapshot {seq} collected ({} issue(s))", issues.len());
    transition(
        &mut tx,
        &scope,
        &Transition {
            aggregate_type: "report_snapshot",
            id: sid,
            institution,
            from: None,
            to: "collected",
            version: 1,
            reason: Some(&note),
            process_instance_id: None,
            number: Some(&c.number),
            subject_ref: None,
        },
        None,
        "services.report_snapshot.collect",
        &actor,
    )
    .await?;
    set_cycle(
        &mut tx,
        &scope,
        &c,
        ReportingCycleStatus::Validating,
        Some(&note),
        None,
        "services.reporting_cycle.collect",
        &actor,
    )
    .await?;
    let s = load_snapshot(&mut tx, t, sid, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &s, Version(s.version)))
}

/// One snapshot with its frozen values (`services.report.read`).
#[utoipa::path(get, path = "/api/v1/services/report-snapshots/{id}", operation_id = "services_regulatory_snapshot_get", tag = "services-regulatory",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ReportSnapshotView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get_snapshot(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let s = load_snapshot(&mut conn, ctx.scope.tenant_id, id, false).await?;
    Ok(with_etag(StatusCode::OK, &s, Version(s.version)))
}

async fn decide_snapshot(
    st: &ServicesState,
    ctx: &AuthContext,
    id: Uuid,
    expected_version: i64,
    to: ReportSnapshotStatus,
    reason: Option<String>,
) -> Result<ReportSnapshotView, KernelError> {
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_snapshot(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(st, &scope, institution).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = load_cycle(&mut tx, t, pre.cycle_id, true).await?;
    let s = load_snapshot(&mut tx, t, id, true).await?;
    check_version(expected_version, s.version)?;
    if s.status != ReportSnapshotStatus::Collected || c.status != ReportingCycleStatus::Validating {
        return Err(illegal(
            "report snapshot of",
            &c.number,
            s.status.as_str(),
            to.as_str(),
        ));
    }
    if to == ReportSnapshotStatus::Validated && !s.issues.is_empty() {
        return Err(KernelError::Conflict(format!(
            "snapshot {} of {} has {} validation issue(s); reject it and collect again",
            s.seq,
            c.number,
            s.issues.len()
        )));
    }
    sqlx::query(
        "UPDATE services.report_snapshot SET status = $2, status_reason = $3, version = version + 1, \
         updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(to.as_str())
    .bind(reason.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let validated = to == ReportSnapshotStatus::Validated;
    transition(
        &mut tx,
        &scope,
        &Transition {
            aggregate_type: "report_snapshot",
            id,
            institution,
            from: Some("collected"),
            to: to.as_str(),
            version: s.version + 1,
            reason: reason.as_deref(),
            process_instance_id: None,
            number: Some(&c.number),
            subject_ref: None,
        },
        validated.then_some((
            ev::REPORT_SNAPSHOT_VALIDATED,
            ev::REPORT_SNAPSHOT_VALIDATED_SCHEMA,
        )),
        if validated {
            "services.report_snapshot.validate"
        } else {
            "services.report_snapshot.reject"
        },
        &actor,
    )
    .await?;
    if !validated {
        set_cycle(
            &mut tx,
            &scope,
            &c,
            ReportingCycleStatus::Collecting,
            reason.as_deref(),
            None,
            "services.reporting_cycle.recollect",
            &actor,
        )
        .await?;
    }
    let out = load_snapshot(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Validate a collected snapshot (`services.report.manage`; If-Match): 409 while it has issues.
#[utoipa::path(post, path = "/api/v1/services/report-snapshots/{id}/validate", operation_id = "services_regulatory_snapshot_validate", tag = "services-regulatory",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = ReportReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ReportSnapshotView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn validate(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReportReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportManage)?;
    let v = expected!(headers);
    let s = decide_snapshot(
        &st,
        ctx,
        id,
        v,
        ReportSnapshotStatus::Validated,
        clean(b.reason),
    )
    .await?;
    Ok(with_etag(StatusCode::OK, &s, Version(s.version)))
}

/// Reject a collected snapshot (`services.report.manage`; If-Match; reason required): the cycle
/// returns to `collecting`.
#[utoipa::path(post, path = "/api/v1/services/report-snapshots/{id}/reject", operation_id = "services_regulatory_snapshot_reject", tag = "services-regulatory",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = ReportReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ReportSnapshotView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn reject(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReportReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportManage)?;
    let v = expected!(headers);
    let reason = required_reason(b.reason)?;
    let s = decide_snapshot(
        &st,
        ctx,
        id,
        v,
        ReportSnapshotStatus::Rejected,
        Some(reason),
    )
    .await?;
    Ok(with_etag(StatusCode::OK, &s, Version(s.version)))
}

// ================================================================ packages

/// A submission package.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubmissionPackageView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub cycle_id: Uuid,
    pub snapshot_id: Uuid,
    #[schema(example = "SUB-000001")]
    pub number: String,
    pub status: SubmissionPackageStatus,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub approved_by: Option<String>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub acknowledgement_ref: Option<String>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub corrected_from_id: Option<Uuid>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SubmissionPackageList {
    pub items: Vec<SubmissionPackageView>,
}

const PACKAGE_SELECT: &str = "SELECT id, institution_id, cycle_id, snapshot_id, number, status, \
    status_reason, process_instance_id, approved_by, submitted_at, acknowledgement_ref, \
    acknowledged_at, corrected_from_id, version FROM services.submission_package";

fn package_from_row(r: &PgRow) -> Result<SubmissionPackageView, sqlx::Error> {
    Ok(SubmissionPackageView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        cycle_id: r.try_get("cycle_id")?,
        snapshot_id: r.try_get("snapshot_id")?,
        number: r.try_get("number")?,
        status: decode(r.try_get("status")?, SubmissionPackageStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        process_instance_id: r.try_get("process_instance_id")?,
        approved_by: r.try_get("approved_by")?,
        submitted_at: r.try_get("submitted_at")?,
        acknowledgement_ref: r.try_get("acknowledgement_ref")?,
        acknowledged_at: r.try_get("acknowledged_at")?,
        corrected_from_id: r.try_get("corrected_from_id")?,
        version: r.try_get("version")?,
    })
}

async fn load_package(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<SubmissionPackageView, KernelError> {
    let sql = format!(
        "{PACKAGE_SELECT} WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "submission package",
        })?;
    package_from_row(&row).map_err(map_db_error)
}

async fn packages_of(
    conn: &mut PgConnection,
    cycle: Uuid,
) -> Result<Vec<SubmissionPackageView>, KernelError> {
    let rows = sqlx::query(&format!(
        "{PACKAGE_SELECT} WHERE cycle_id = $1 ORDER BY created_at DESC"
    ))
    .bind(cycle)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(package_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)
}

/// What a package write changes besides the status.
#[derive(Default)]
struct PackageChange<'a> {
    reason: Option<&'a str>,
    pid: Option<&'a str>,
    approved_by: Option<&'a str>,
    submitted: bool,
    acknowledgement_ref: Option<&'a str>,
}

#[allow(clippy::too_many_arguments)]
async fn set_package(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    p: &SubmissionPackageView,
    to: SubmissionPackageStatus,
    change: PackageChange<'_>,
    event: Option<(&'static str, &'static str)>,
    action: &'static str,
    actor: &Actor,
) -> Result<SubmissionPackageView, KernelError> {
    if !p.status.can_transition(to) {
        return Err(illegal(
            "submission package",
            &p.number,
            p.status.as_str(),
            to.as_str(),
        ));
    }
    sqlx::query(
        "UPDATE services.submission_package SET status = $2, status_reason = COALESCE($3, status_reason), \
         process_instance_id = COALESCE($4, process_instance_id), approved_by = COALESCE($5, approved_by), \
         submitted_at = CASE WHEN $6 THEN now() ELSE submitted_at END, \
         acknowledgement_ref = COALESCE($7, acknowledgement_ref), \
         acknowledged_at = CASE WHEN $7::text IS NOT NULL THEN now() ELSE acknowledged_at END, \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(p.id)
    .bind(to.as_str())
    .bind(change.reason)
    .bind(change.pid)
    .bind(change.approved_by)
    .bind(change.submitted)
    .bind(change.acknowledgement_ref)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    transition(
        conn,
        scope,
        &Transition {
            aggregate_type: "submission_package",
            id: p.id,
            institution: InstitutionId(p.institution_id),
            from: Some(p.status.as_str()),
            to: to.as_str(),
            version: p.version + 1,
            reason: change.reason.or(change.acknowledgement_ref),
            process_instance_id: change.pid.or(p.process_instance_id.as_deref()),
            number: Some(&p.number),
            subject_ref: None,
        },
        event,
        action,
        actor,
    )
    .await?;
    load_package(conn, scope.tenant_id, p.id, true).await
}

/// A package to assemble.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubmissionPackageCreate {
    /// Defaults to the latest validated snapshot of the cycle.
    pub snapshot_id: Option<Uuid>,
}

/// Assemble a submission package from a validated snapshot (`services.report.manage`;
/// Idempotency-Key): `SUB-…`, linked to the corrected package after a correction; the cycle
/// moves to `packaged`.
#[utoipa::path(post, path = "/api/v1/services/reporting-cycles/{id}/packages", operation_id = "services_regulatory_package_create", tag = "services-regulatory",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = SubmissionPackageCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = SubmissionPackageView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create_package(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<SubmissionPackageCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_cycle(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = load_cycle(&mut tx, t, id, true).await?;
    if c.status != ReportingCycleStatus::Validating {
        return Err(illegal("reporting cycle", &c.number, c.status.as_str(), "packaged").into());
    }
    let snapshot: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM services.report_snapshot WHERE cycle_id = $1 AND status = 'validated' \
         AND ($2::uuid IS NULL OR id = $2) ORDER BY seq DESC LIMIT 1",
    )
    .bind(id)
    .bind(b.snapshot_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let Some(snapshot) = snapshot else {
        return Err(KernelError::Conflict(format!(
            "cycle {} has no validated snapshot to package",
            c.number
        ))
        .into());
    };
    let corrected_from: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM services.submission_package WHERE cycle_id = $1 AND status = 'corrected' \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let number = numbering::next(&mut tx, t, institution, kind::SUBMISSION_PACKAGE).await?;
    let pid: Uuid = sqlx::query_scalar(
        "INSERT INTO services.submission_package (tenant_id, institution_id, cycle_id, snapshot_id, \
         number, corrected_from_id) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(c.institution_id)
    .bind(id)
    .bind(snapshot)
    .bind(&number)
    .bind(corrected_from)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    transition(
        &mut tx,
        &scope,
        &Transition {
            aggregate_type: "submission_package",
            id: pid,
            institution,
            from: None,
            to: "draft",
            version: 1,
            reason: corrected_from.is_some().then_some("corrected package"),
            process_instance_id: None,
            number: Some(&number),
            subject_ref: None,
        },
        None,
        "services.submission_package.assemble",
        &actor,
    )
    .await?;
    set_cycle(
        &mut tx,
        &scope,
        &c,
        ReportingCycleStatus::Packaged,
        Some(&number),
        None,
        "services.reporting_cycle.package",
        &actor,
    )
    .await?;
    let p = load_package(&mut tx, t, pid, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &p, Version(p.version)))
}

/// Packages of a cycle, newest first (`services.report.read`).
#[utoipa::path(get, path = "/api/v1/services/reporting-cycles/{id}/packages", operation_id = "services_regulatory_package_list", tag = "services-regulatory",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = SubmissionPackageList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list_packages(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<SubmissionPackageList>> {
    let ctx = auth.require(Permission::ReportRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load_cycle(&mut conn, ctx.scope.tenant_id, id, false).await?;
    Ok(Json(SubmissionPackageList {
        items: packages_of(&mut conn, id).await?,
    }))
}

/// One package (`services.report.read`).
#[utoipa::path(get, path = "/api/v1/services/submission-packages/{id}", operation_id = "services_regulatory_package_get", tag = "services-regulatory",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = SubmissionPackageView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get_package(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let p = load_package(&mut conn, ctx.scope.tenant_id, id, false).await?;
    Ok(with_etag(StatusCode::OK, &p, Version(p.version)))
}

/// Submit a package (If-Match): a **draft** goes for approval (`services.report.manage`;
/// `SUBMISSION_APPROVAL`; auto-approval → `approved`, `submission_package.approved`); an
/// **approved** package is submitted to the regulator (`services.report.approve`, step-up; the
/// cycle becomes `submitted`).
#[utoipa::path(post, path = "/api/v1/services/submission-packages/{id}/submit", operation_id = "services_regulatory_package_submit", tag = "services-regulatory",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = ReportReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SubmissionPackageView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn submit_package(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReportReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportManage)?;
    let v = expected!(headers);
    let reason = clean(b.reason);
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_package(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    check_version(v, pre.version)?;
    let actor = actor(ctx);
    match pre.status {
        SubmissionPackageStatus::Draft => {
            let mut vars = BTreeMap::new();
            vars.insert("package".to_owned(), serde_json::json!(pre.number));
            let outcome = request_approval(
                &st,
                &scope,
                process::SUBMISSION_APPROVAL,
                business_key("submission_package", pre.id, pre.version),
                vars,
            )
            .await?;
            let mut tx = st.pool.begin().await.map_err(map_db_error)?;
            let p = load_package(&mut tx, t, id, true).await?;
            check_version(v, p.version)?;
            let p = set_package(
                &mut tx,
                &scope,
                &p,
                SubmissionPackageStatus::PendingApproval,
                PackageChange {
                    reason: reason.as_deref(),
                    pid: Some(outcome.process_instance_id()),
                    ..PackageChange::default()
                },
                None,
                "services.submission_package.submit",
                &actor,
            )
            .await?;
            let p = if let ApprovalOutcome::Approved { .. } = outcome {
                set_package(
                    &mut tx,
                    &scope,
                    &p,
                    SubmissionPackageStatus::Approved,
                    PackageChange {
                        approved_by: Some(AUTO_APPROVER),
                        ..PackageChange::default()
                    },
                    Some((
                        ev::SUBMISSION_PACKAGE_APPROVED,
                        ev::SUBMISSION_PACKAGE_APPROVED_SCHEMA,
                    )),
                    "services.submission_package.approve",
                    &Actor::system(actor.correlation_id.clone()),
                )
                .await?
            } else {
                p
            };
            tx.commit().await.map_err(map_db_error)?;
            Ok(with_etag(StatusCode::OK, &p, Version(p.version)))
        }
        SubmissionPackageStatus::Approved => {
            auth.require(Permission::ReportApprove)?;
            let mut tx = st.pool.begin().await.map_err(map_db_error)?;
            let c = load_cycle(&mut tx, t, pre.cycle_id, true).await?;
            let p = load_package(&mut tx, t, id, true).await?;
            check_version(v, p.version)?;
            let p = set_package(
                &mut tx,
                &scope,
                &p,
                SubmissionPackageStatus::Submitted,
                PackageChange {
                    reason: reason.as_deref(),
                    submitted: true,
                    ..PackageChange::default()
                },
                None,
                "services.submission_package.submit_to_regulator",
                &actor,
            )
            .await?;
            set_cycle(
                &mut tx,
                &scope,
                &c,
                ReportingCycleStatus::Submitted,
                Some(&p.number),
                None,
                "services.reporting_cycle.submit",
                &actor,
            )
            .await?;
            tx.commit().await.map_err(map_db_error)?;
            Ok(with_etag(StatusCode::OK, &p, Version(p.version)))
        }
        s => Err(illegal("submission package", &pre.number, s.as_str(), "submitted").into()),
    }
}

/// Decide a package awaiting approval (`services.report.approve`, step-up; If-Match): approve →
/// `approved` (`submission_package.approved`), reject → back to `draft` (reason required).
#[utoipa::path(post, path = "/api/v1/services/submission-packages/{id}/decision", operation_id = "services_regulatory_package_decide", tag = "services-regulatory",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = ReportDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SubmissionPackageView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn decide_package(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReportDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportApprove)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "submission package").await?;
    let v = expected!(headers);
    let reason = clean(b.reason);
    if b.decision == ReportDecision::Reject && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_package(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let actor = actor(ctx);
    let decider = actor.id.clone().unwrap_or_default();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = load_package(&mut tx, t, id, true).await?;
    check_version(v, p.version)?;
    if p.status != SubmissionPackageStatus::PendingApproval {
        return Err(KernelError::Conflict(format!(
            "package {} is not awaiting a decision ({})",
            p.number, p.status
        ))
        .into());
    }
    let p = match b.decision {
        ReportDecision::Approve => {
            set_package(
                &mut tx,
                &scope,
                &p,
                SubmissionPackageStatus::Approved,
                PackageChange {
                    reason: reason.as_deref(),
                    approved_by: Some(&decider),
                    ..PackageChange::default()
                },
                Some((
                    ev::SUBMISSION_PACKAGE_APPROVED,
                    ev::SUBMISSION_PACKAGE_APPROVED_SCHEMA,
                )),
                "services.submission_package.approve",
                &actor,
            )
            .await?
        }
        ReportDecision::Reject => {
            set_package(
                &mut tx,
                &scope,
                &p,
                SubmissionPackageStatus::Draft,
                PackageChange {
                    reason: reason.as_deref(),
                    ..PackageChange::default()
                },
                None,
                "services.submission_package.reject",
                &actor,
            )
            .await?
        }
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &p, Version(p.version)))
}

/// The regulator's acknowledgement.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubmissionAcknowledge {
    /// Acknowledgement / application reference issued by the regulator.
    pub reference: String,
}

/// Record the regulator's acknowledgement (`services.report.manage`; If-Match; L3-H-059).
#[utoipa::path(post, path = "/api/v1/services/submission-packages/{id}/acknowledge", operation_id = "services_regulatory_package_acknowledge", tag = "services-regulatory",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = SubmissionAcknowledge, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SubmissionPackageView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn acknowledge(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<SubmissionAcknowledge>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportManage)?;
    let v = expected!(headers);
    let reference = required_text("reference", &b.reference, 100)?;
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_package(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = load_package(&mut tx, t, id, true).await?;
    check_version(v, p.version)?;
    let p = set_package(
        &mut tx,
        &scope,
        &p,
        SubmissionPackageStatus::Acknowledged,
        PackageChange {
            acknowledgement_ref: Some(&reference),
            ..PackageChange::default()
        },
        Some((
            ev::SUBMISSION_PACKAGE_ACKNOWLEDGED,
            ev::SUBMISSION_PACKAGE_ACKNOWLEDGED_SCHEMA,
        )),
        "services.submission_package.acknowledge",
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &p, Version(p.version)))
}

/// Open a correction of a submitted / acknowledged package (`services.report.manage`; If-Match;
/// reason required; L3-H-060): the package becomes `corrected`, the cycle returns to
/// `collecting` for a new snapshot and a corrected package.
#[utoipa::path(post, path = "/api/v1/services/submission-packages/{id}/correct", operation_id = "services_regulatory_package_correct", tag = "services-regulatory",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = ReportReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SubmissionPackageView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn correct(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReportReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportManage)?;
    let v = expected!(headers);
    let reason = required_reason(b.reason)?;
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_package(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = load_cycle(&mut tx, t, pre.cycle_id, true).await?;
    let p = load_package(&mut tx, t, id, true).await?;
    check_version(v, p.version)?;
    let p = set_package(
        &mut tx,
        &scope,
        &p,
        SubmissionPackageStatus::Corrected,
        PackageChange {
            reason: Some(&reason),
            ..PackageChange::default()
        },
        None,
        "services.submission_package.correct",
        &actor,
    )
    .await?;
    set_cycle(
        &mut tx,
        &scope,
        &c,
        ReportingCycleStatus::Collecting,
        Some(&reason),
        None,
        "services.reporting_cycle.correct",
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &p, Version(p.version)))
}

/// Close a cycle whose latest package is acknowledged (`services.report.manage`; If-Match;
/// L3-H-060; `reporting_cycle.closed`).
#[utoipa::path(post, path = "/api/v1/services/reporting-cycles/{id}/close", operation_id = "services_regulatory_cycle_close", tag = "services-regulatory",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = ReportReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ReportingCycleDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn close(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReportReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportManage)?;
    let v = expected!(headers);
    let reason = clean(b.reason);
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_cycle(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = load_cycle(&mut tx, t, id, true).await?;
    check_version(v, c.version)?;
    let latest: Option<String> = sqlx::query_scalar(
        "SELECT status FROM services.submission_package WHERE cycle_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if latest.as_deref() != Some("acknowledged") {
        return Err(KernelError::Conflict(format!(
            "cycle {} has no acknowledged submission to close on",
            c.number
        ))
        .into());
    }
    set_cycle(
        &mut tx,
        &scope,
        &c,
        ReportingCycleStatus::Closed,
        reason.as_deref(),
        Some((
            ev::REPORTING_CYCLE_CLOSED,
            ev::REPORTING_CYCLE_CLOSED_SCHEMA,
        )),
        "services.reporting_cycle.close",
        &actor,
    )
    .await?;
    let d = detail(&mut tx, t, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    let version = d.cycle.version;
    Ok(with_etag(StatusCode::OK, &d, Version(version)))
}

pub fn router(state: ServicesState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(create_cycle, list_cycles))
        .routes(routes!(list_institution_cycles))
        .routes(routes!(get_cycle))
        .routes(routes!(collect))
        .routes(routes!(get_snapshot))
        .routes(routes!(validate))
        .routes(routes!(reject))
        .routes(routes!(create_package, list_packages))
        .routes(routes!(get_package))
        .routes(routes!(submit_package))
        .routes(routes!(decide_package))
        .routes(routes!(acknowledge))
        .routes(routes!(correct))
        .routes(routes!(close))
        .with_state(state)
}
