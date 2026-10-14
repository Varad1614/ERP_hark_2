//! Governed dashboards (L3-H-048 requirement → governed published dashboard, H-050 role
//! assignment → available workspace, H-052 change → versioned publication or rollback; F-076 the
//! finance KPI dashboard). A dashboard holds immutable versions of its widget layout: a draft
//! version is submitted for review (`DASHBOARD_PUBLICATION`), approved, then published by the
//! publication authority (`services.report.approve`); publishing supersedes the previous version
//! (kept for reproducibility) and rollback re-publishes an earlier one. Widgets bind live
//! figures of the `OperationalMetricsPort` catalogue; `finance` dashboards bind only `finance.*`.

use super::common::{
    check_version, clean, decode, expected, invalid, optional_text, required_reason, required_text,
    transition, valid_code, ReportDecision, ReportDecisionBody, ReportReasonBody, Transition,
};
use super::domain::{
    binding_problem, figure, DashboardAssignmentStatus, DashboardCategory, DashboardStatus,
    DashboardVersionStatus, WidgetStatus, WidgetVisualization,
};
use super::widgets::load_widget;
use crate::directory::http::MetricValueView;
use crate::shared::approval::{
    business_key, process, request_approval, ApprovalOutcome, AUTO_APPROVER,
};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::lifecycle::illegal;
use crate::shared::publish;
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, ServicesState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use sutra_contracts::services::events::reporting as ev;
use sutra_contracts::services::ports::MetricValue;
use sutra_kernel::auth::{AuthContext, Permission, Role};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

// ================================================================ rows and views

/// One widget slot of a frozen dashboard version layout.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DashboardWidgetSlot {
    pub widget_id: Uuid,
    pub code: String,
    pub title: String,
    pub metric_key: String,
    pub visualization: WidgetVisualization,
    pub position: i32,
}

/// One version of a dashboard.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DashboardVersionView {
    pub id: Uuid,
    pub version_no: i32,
    pub status: DashboardVersionStatus,
    pub change_note: Option<String>,
    pub widgets: Vec<DashboardWidgetSlot>,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub published_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Availability of a dashboard to a role (L3-H-050).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DashboardAssignmentView {
    pub id: Uuid,
    pub dashboard_id: Uuid,
    #[schema(example = "administrator")]
    pub role: String,
    pub org_unit_id: Option<Uuid>,
    pub status: DashboardAssignmentStatus,
    pub version: i64,
}

/// A governed dashboard with its versions and assignments.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DashboardView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "FIN-KPI")]
    pub code: String,
    pub name: String,
    pub purpose: String,
    pub category: DashboardCategory,
    pub status: DashboardStatus,
    pub status_reason: Option<String>,
    pub current_version_id: Option<Uuid>,
    pub current_version_no: Option<i32>,
    pub process_instance_id: Option<String>,
    pub versions: Vec<DashboardVersionView>,
    pub assignments: Vec<DashboardAssignmentView>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DashboardList {
    pub items: Vec<DashboardView>,
}

struct Board {
    id: Uuid,
    institution_id: InstitutionId,
    code: String,
    name: String,
    purpose: String,
    category: DashboardCategory,
    status: DashboardStatus,
    status_reason: Option<String>,
    current_version_id: Option<Uuid>,
    process_instance_id: Option<String>,
    version: i64,
}

const BOARD_SELECT: &str = "SELECT id, institution_id, code, name, purpose, category, status, \
    status_reason, current_version_id, process_instance_id, version FROM services.dashboard";

fn board_from_row(r: &PgRow) -> Result<Board, sqlx::Error> {
    Ok(Board {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        code: r.try_get("code")?,
        name: r.try_get("name")?,
        purpose: r.try_get("purpose")?,
        category: decode(r.try_get("category")?, DashboardCategory::parse)?,
        status: decode(r.try_get("status")?, DashboardStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        current_version_id: r.try_get("current_version_id")?,
        process_instance_id: r.try_get("process_instance_id")?,
        version: r.try_get("version")?,
    })
}

async fn load_board(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<Board, KernelError> {
    let sql = format!(
        "{BOARD_SELECT} WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "dashboard",
        })?;
    board_from_row(&row).map_err(map_db_error)
}

const VERSION_SELECT: &str = "SELECT id, version_no, status, change_note, widgets, status_reason, \
    process_instance_id, published_at, published_by, created_at FROM services.dashboard_version";

fn version_from_row(r: &PgRow) -> Result<DashboardVersionView, sqlx::Error> {
    let widgets: serde_json::Value = r.try_get("widgets")?;
    Ok(DashboardVersionView {
        id: r.try_get("id")?,
        version_no: r.try_get("version_no")?,
        status: decode(r.try_get("status")?, DashboardVersionStatus::parse)?,
        change_note: r.try_get("change_note")?,
        widgets: serde_json::from_value(widgets).map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
        status_reason: r.try_get("status_reason")?,
        process_instance_id: r.try_get("process_instance_id")?,
        published_at: r.try_get("published_at")?,
        published_by: r.try_get("published_by")?,
        created_at: r.try_get("created_at")?,
    })
}

async fn versions_of(
    conn: &mut PgConnection,
    dashboard: Uuid,
) -> Result<Vec<DashboardVersionView>, KernelError> {
    let rows = sqlx::query(&format!(
        "{VERSION_SELECT} WHERE dashboard_id = $1 ORDER BY version_no DESC"
    ))
    .bind(dashboard)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(version_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)
}

const ASSIGNMENT_SELECT: &str =
    "SELECT id, dashboard_id, role, org_unit_id, status, version FROM services.dashboard_assignment";

fn assignment_from_row(r: &PgRow) -> Result<DashboardAssignmentView, sqlx::Error> {
    Ok(DashboardAssignmentView {
        id: r.try_get("id")?,
        dashboard_id: r.try_get("dashboard_id")?,
        role: r.try_get("role")?,
        org_unit_id: r.try_get("org_unit_id")?,
        status: decode(r.try_get("status")?, DashboardAssignmentStatus::parse)?,
        version: r.try_get("version")?,
    })
}

async fn assignments_of(
    conn: &mut PgConnection,
    dashboard: Uuid,
) -> Result<Vec<DashboardAssignmentView>, KernelError> {
    let rows = sqlx::query(&format!(
        "{ASSIGNMENT_SELECT} WHERE dashboard_id = $1 ORDER BY created_at"
    ))
    .bind(dashboard)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(assignment_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)
}

async fn view(conn: &mut PgConnection, b: Board) -> Result<DashboardView, KernelError> {
    let versions = versions_of(conn, b.id).await?;
    let assignments = assignments_of(conn, b.id).await?;
    let current_version_no = b
        .current_version_id
        .and_then(|c| versions.iter().find(|v| v.id == c))
        .map(|v| v.version_no);
    Ok(DashboardView {
        id: b.id,
        institution_id: b.institution_id.as_uuid(),
        code: b.code,
        name: b.name,
        purpose: b.purpose,
        category: b.category,
        status: b.status,
        status_reason: b.status_reason,
        current_version_id: b.current_version_id,
        current_version_no,
        process_instance_id: b.process_instance_id,
        versions,
        assignments,
        version: b.version,
    })
}

async fn load_view(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> Result<DashboardView, KernelError> {
    let b = load_board(conn, tenant, id, false).await?;
    view(conn, b).await
}

fn respond(status: StatusCode, d: &DashboardView) -> Response {
    with_etag(status, d, Version(d.version))
}

/// Updates the dashboard row (status / current version / process id), bumps its version and
/// records the transition.
#[allow(clippy::too_many_arguments)]
async fn set_board(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    b: &Board,
    to: DashboardStatus,
    current_version: Option<Uuid>,
    pid: Option<&str>,
    reason: Option<&str>,
    event: Option<(&'static str, &'static str)>,
    action: &'static str,
    actor: &Actor,
) -> Result<Board, KernelError> {
    sqlx::query(
        "UPDATE services.dashboard SET status = $2, current_version_id = COALESCE($3, current_version_id), \
         process_instance_id = COALESCE($4, process_instance_id), status_reason = $5, \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(b.id)
    .bind(to.as_str())
    .bind(current_version)
    .bind(pid)
    .bind(reason)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    transition(
        conn,
        scope,
        &Transition {
            aggregate_type: "dashboard",
            id: b.id,
            institution: b.institution_id,
            from: Some(b.status.as_str()),
            to: to.as_str(),
            version: b.version + 1,
            reason,
            process_instance_id: pid.or(b.process_instance_id.as_deref()),
            number: Some(&b.code),
            subject_ref: None,
        },
        event,
        action,
        actor,
    )
    .await?;
    load_board(conn, scope.tenant_id, b.id, true).await
}

/// Moves one version (`dashboard_version` history; illegal → 409).
#[allow(clippy::too_many_arguments)]
async fn set_version(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    b: &Board,
    v: &DashboardVersionView,
    to: DashboardVersionStatus,
    reason: Option<&str>,
    pid: Option<&str>,
    published_by: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    if !v.status.can_transition(to) {
        return Err(illegal(
            "dashboard version",
            &format!("{} v{}", b.code, v.version_no),
            v.status.as_str(),
            to.as_str(),
        ));
    }
    let publishing = to == DashboardVersionStatus::Published;
    sqlx::query(
        "UPDATE services.dashboard_version SET status = $2, status_reason = $3, \
         process_instance_id = COALESCE($4, process_instance_id), \
         published_at = CASE WHEN $5 THEN now() ELSE published_at END, \
         published_by = CASE WHEN $5 THEN $6 ELSE published_by END, updated_at = now() WHERE id = $1",
    )
    .bind(v.id)
    .bind(to.as_str())
    .bind(reason)
    .bind(pid)
    .bind(publishing)
    .bind(published_by)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let label = format!("v{}", v.version_no);
    transition(
        conn,
        scope,
        &Transition {
            aggregate_type: "dashboard_version",
            id: v.id,
            institution: b.institution_id,
            from: Some(v.status.as_str()),
            to: to.as_str(),
            version: i64::from(v.version_no),
            reason,
            process_instance_id: pid.or(v.process_instance_id.as_deref()),
            number: Some(&label),
            subject_ref: None,
        },
        None,
        "services.dashboard_version.transition",
        actor,
    )
    .await
}

async fn version_in(
    conn: &mut PgConnection,
    dashboard: Uuid,
    statuses: &[DashboardVersionStatus],
) -> Result<Option<DashboardVersionView>, KernelError> {
    let wanted: Vec<&str> = statuses.iter().map(|s| s.as_str()).collect();
    let row = sqlx::query(&format!(
        "{VERSION_SELECT} WHERE dashboard_id = $1 AND status = ANY($2) ORDER BY version_no DESC LIMIT 1 FOR UPDATE"
    ))
    .bind(dashboard)
    .bind(&wanted)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    row.as_ref()
        .map(version_from_row)
        .transpose()
        .map_err(map_db_error)
}

// ================================================================ dashboards

/// A dashboard requirement (L3-H-048).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DashboardCreate {
    pub code: String,
    pub name: String,
    /// What the dashboard must answer, for whom.
    pub purpose: String,
    pub category: Option<DashboardCategory>,
}

/// Record a dashboard requirement as a draft dashboard (`services.report.manage`;
/// Idempotency-Key). 409 for a duplicate code.
#[utoipa::path(post, path = "/api/v1/services/institutions/{id}/dashboards", operation_id = "services_dashboard_create", tag = "services-dashboards",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = DashboardCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = DashboardView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<DashboardCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let code = valid_code("code", &b.code)?;
    let name = required_text("name", &b.name, 200)?;
    let purpose = required_text("purpose", &b.purpose, 2000)?;
    let category = b.category.unwrap_or(DashboardCategory::Operational);
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let did: Uuid = sqlx::query_scalar(
        "INSERT INTO services.dashboard (tenant_id, institution_id, code, name, purpose, category) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(&code)
    .bind(&name)
    .bind(&purpose)
    .bind(category.as_str())
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match map_db_error(e) {
        KernelError::Conflict(_) => {
            KernelError::Conflict(format!("dashboard {code} already exists"))
        }
        other => other,
    })?;
    transition(
        &mut tx,
        &scope,
        &Transition {
            aggregate_type: "dashboard",
            id: did,
            institution,
            from: None,
            to: "draft",
            version: 1,
            reason: Some(&purpose),
            process_instance_id: None,
            number: Some(&code),
            subject_ref: None,
        },
        None,
        "services.dashboard.create",
        &actor,
    )
    .await?;
    let d = load_view(&mut tx, ctx.scope.tenant_id, did).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::CREATED, &d))
}

/// Filters of the dashboard list.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DashboardQuery {
    pub status: Option<DashboardStatus>,
}

/// Dashboards of an institution with their versions (`services.report.read`).
#[utoipa::path(get, path = "/api/v1/services/institutions/{id}/dashboards", operation_id = "services_dashboard_list", tag = "services-dashboards",
    params(("id" = String, Path, format = Uuid), DashboardQuery), security(("session_cookie" = [])),
    responses((status = 200, body = DashboardList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<DashboardQuery>,
) -> ApiResult<Json<DashboardList>> {
    let ctx = auth.require(Permission::ReportRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let rows = sqlx::query(&format!(
        "{BOARD_SELECT} WHERE tenant_id = $1 AND institution_id = $2 AND ($3::text IS NULL OR status = $3) ORDER BY code"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(q.status.map(|s| s.as_str()))
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let boards = rows
        .iter()
        .map(board_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(boards.len());
    for b in boards {
        items.push(view(&mut conn, b).await?);
    }
    Ok(Json(DashboardList { items }))
}

/// One dashboard (`services.report.read`).
#[utoipa::path(get, path = "/api/v1/services/dashboards/{id}", operation_id = "services_dashboard_get", tag = "services-dashboards",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = DashboardView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let d = load_view(&mut conn, ctx.scope.tenant_id, id).await?;
    Ok(respond(StatusCode::OK, &d))
}

/// A new layout version (widget ids in display order).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DashboardVersionCreate {
    pub widget_ids: Vec<Uuid>,
    pub change_note: Option<String>,
}

/// Prepare a new layout version (`services.report.manage`; Idempotency-Key; L3-H-052): a draft
/// version is replaced in place, otherwise version n + 1 is created (409 while one is in review
/// or approved). 422 unless every widget is active and its figure exists in the live catalogue;
/// a `finance` dashboard binds only `finance.*` figures (F-076).
#[utoipa::path(post, path = "/api/v1/services/dashboards/{id}/versions", operation_id = "services_dashboard_version_create", tag = "services-dashboards",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = DashboardVersionCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = DashboardView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create_version(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<DashboardVersionCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let note = optional_text("changeNote", b.change_note, 1000)?;
    if b.widget_ids.is_empty() || b.widget_ids.len() > 12 {
        return Err(invalid("widgetIds", "count", "between 1 and 12 widgets").into());
    }
    let mut seen = std::collections::HashSet::new();
    if !b.widget_ids.iter().all(|w| seen.insert(*w)) {
        return Err(invalid("widgetIds", "duplicate", "each widget at most once").into());
    }
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_board(&mut conn, t, id, false).await?
    };
    let scope = institution_scope(ctx, pre.institution_id);
    require_institution_writable(&st, &scope, pre.institution_id).await?;
    let catalogue = st.all_metrics(&scope, pre.institution_id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let board = load_board(&mut tx, t, id, true).await?;
    if board.status == DashboardStatus::Retired {
        return Err(illegal("dashboard", &board.code, "retired", "changed").into());
    }
    let mut slots = Vec::with_capacity(b.widget_ids.len());
    for (i, wid) in b.widget_ids.iter().enumerate() {
        let w = load_widget(&mut tx, t, *wid, false)
            .await
            .ok()
            .filter(|w| w.institution_id == board.institution_id.as_uuid())
            .ok_or_else(|| invalid("widgetIds", "unknown_widget", &format!("no widget {wid}")))?;
        if w.status != WidgetStatus::Active {
            return Err(invalid(
                "widgetIds",
                "widget_not_active",
                &format!("widget {} is {}", w.code, w.status),
            )
            .into());
        }
        if let Some(problem) = binding_problem(&w.metric_key, Some(board.category), &catalogue) {
            return Err(invalid("widgetIds", "invalid_binding", &problem).into());
        }
        slots.push(DashboardWidgetSlot {
            widget_id: w.id,
            code: w.code,
            title: w.title,
            metric_key: w.metric_key,
            visualization: w.visualization,
            position: i32::try_from(i + 1).unwrap_or(i32::MAX),
        });
    }
    let layout = serde_json::to_value(&slots).map_err(KernelError::internal)?;
    let open = version_in(
        &mut tx,
        id,
        &[
            DashboardVersionStatus::Draft,
            DashboardVersionStatus::InReview,
            DashboardVersionStatus::Approved,
        ],
    )
    .await?;
    let (vid, version_no) = match open {
        Some(v) if v.status == DashboardVersionStatus::Draft => {
            sqlx::query(
                "UPDATE services.dashboard_version SET widgets = $2, change_note = $3, updated_at = now() \
                 WHERE id = $1",
            )
            .bind(v.id)
            .bind(&layout)
            .bind(note.as_deref())
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
            (v.id, v.version_no)
        }
        Some(v) => {
            return Err(KernelError::Conflict(format!(
                "version {} of {} is {}; decide or publish it first",
                v.version_no, board.code, v.status
            ))
            .into())
        }
        None => {
            let next: i32 = sqlx::query_scalar(
                "SELECT COALESCE(max(version_no), 0) + 1 FROM services.dashboard_version WHERE dashboard_id = $1",
            )
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
            let vid: Uuid = sqlx::query_scalar(
                "INSERT INTO services.dashboard_version (tenant_id, institution_id, dashboard_id, version_no, \
                 widgets, change_note) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
            )
            .bind(t.as_uuid())
            .bind(board.institution_id.as_uuid())
            .bind(id)
            .bind(next)
            .bind(&layout)
            .bind(note.as_deref())
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
            (vid, next)
        }
    };
    sqlx::query(
        "UPDATE services.dashboard SET version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    publish::audit(
        &mut tx,
        &scope,
        "services.dashboard.prepare_version",
        "dashboard",
        id.to_string(),
        serde_json::json!({ "versionId": vid, "versionNo": version_no, "widgets": layout }),
        &actor,
    )
    .await?;
    let d = load_view(&mut tx, t, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::CREATED, &d))
}

/// Versions of a dashboard, newest first (`services.report.read`).
#[utoipa::path(get, path = "/api/v1/services/dashboards/{id}/versions", operation_id = "services_dashboard_version_list", tag = "services-dashboards",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = DashboardVersionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list_versions(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<DashboardVersionList>> {
    let ctx = auth.require(Permission::ReportRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load_board(&mut conn, ctx.scope.tenant_id, id, false).await?;
    Ok(Json(DashboardVersionList {
        items: versions_of(&mut conn, id).await?,
    }))
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DashboardVersionList {
    pub items: Vec<DashboardVersionView>,
}

struct Command<'a> {
    st: &'a ServicesState,
    ctx: &'a AuthContext,
    id: Uuid,
    expected: i64,
}

impl Command<'_> {
    async fn begin(
        &self,
    ) -> Result<
        (
            ExecutionScope,
            sqlx::Transaction<'static, sqlx::Postgres>,
            Board,
        ),
        KernelError,
    > {
        let t = self.ctx.scope.tenant_id;
        let pre = {
            let mut conn = self.st.pool.acquire().await.map_err(map_db_error)?;
            load_board(&mut conn, t, self.id, false).await?
        };
        let scope = institution_scope(self.ctx, pre.institution_id);
        require_institution_writable(self.st, &scope, pre.institution_id).await?;
        let mut tx = self.st.pool.begin().await.map_err(map_db_error)?;
        let b = load_board(&mut tx, t, self.id, true).await?;
        check_version(self.expected, b.version)?;
        Ok((scope, tx, b))
    }
}

/// Approves the version under review (in the caller's tx).
async fn approve_version(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    b: Board,
    v: &DashboardVersionView,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<Board, KernelError> {
    set_version(
        conn,
        scope,
        &b,
        v,
        DashboardVersionStatus::Approved,
        reason,
        None,
        None,
        actor,
    )
    .await?;
    sqlx::query(
        "UPDATE services.dashboard SET version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(b.id)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    load_board(conn, scope.tenant_id, b.id, true).await
}

/// Submit the draft version for review (`services.report.manage`; If-Match on the dashboard;
/// `DASHBOARD_PUBLICATION`). Auto-approval leaves the version `approved`, ready to publish.
#[utoipa::path(post, path = "/api/v1/services/dashboards/{id}/submit", operation_id = "services_dashboard_submit", tag = "services-dashboards",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = ReportReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = DashboardView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn submit(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<ReportReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportManage)?;
    let v = expected!(headers);
    let reason = clean(body.reason);
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_board(&mut conn, ctx.scope.tenant_id, id, false).await?
    };
    let scope = institution_scope(ctx, pre.institution_id);
    let mut vars = BTreeMap::new();
    vars.insert("dashboard".to_owned(), serde_json::json!(pre.code));
    vars.insert(
        "category".to_owned(),
        serde_json::json!(pre.category.as_str()),
    );
    let outcome = request_approval(
        &st,
        &scope,
        process::DASHBOARD_PUBLICATION,
        business_key("dashboard", pre.id, pre.version),
        vars,
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let actor = actor(ctx);
    let (scope, mut tx, b) = Command {
        st: &st,
        ctx,
        id,
        expected: v,
    }
    .begin()
    .await?;
    let draft = version_in(&mut tx, id, &[DashboardVersionStatus::Draft])
        .await?
        .ok_or_else(|| {
            KernelError::Conflict(format!(
                "dashboard {} has no draft version to submit",
                b.code
            ))
        })?;
    if draft.widgets.is_empty() {
        return Err(KernelError::Conflict("an empty version cannot be submitted".into()).into());
    }
    set_version(
        &mut tx,
        &scope,
        &b,
        &draft,
        DashboardVersionStatus::InReview,
        reason.as_deref(),
        Some(&pid),
        None,
        &actor,
    )
    .await?;
    let to = if b.status == DashboardStatus::Published {
        DashboardStatus::Published
    } else {
        DashboardStatus::InReview
    };
    let b = set_board(
        &mut tx,
        &scope,
        &b,
        to,
        None,
        Some(&pid),
        reason.as_deref(),
        None,
        "services.dashboard.submit",
        &actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let in_review = version_in(&mut tx, id, &[DashboardVersionStatus::InReview])
            .await?
            .ok_or_else(|| KernelError::internal_msg("submitted version vanished"))?;
        approve_version(
            &mut tx,
            &scope,
            b,
            &in_review,
            None,
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?;
    }
    let d = load_view(&mut tx, ctx.scope.tenant_id, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &d))
}

/// Decide the version under review (`services.report.approve`, step-up; If-Match): approve →
/// `approved` (ready to publish), reject → `rejected` (reason required).
#[utoipa::path(post, path = "/api/v1/services/dashboards/{id}/decision", operation_id = "services_dashboard_decide", tag = "services-dashboards",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = ReportDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = DashboardView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn decide(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<ReportDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportApprove)?;
    let v = expected!(headers);
    let reason = clean(body.reason);
    if body.decision == ReportDecision::Reject && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let actor = actor(ctx);
    let (scope, mut tx, b) = Command {
        st: &st,
        ctx,
        id,
        expected: v,
    }
    .begin()
    .await?;
    let in_review = version_in(&mut tx, id, &[DashboardVersionStatus::InReview])
        .await?
        .ok_or_else(|| {
            KernelError::Conflict(format!("dashboard {} has no version in review", b.code))
        })?;
    match body.decision {
        ReportDecision::Approve => {
            approve_version(&mut tx, &scope, b, &in_review, reason.as_deref(), &actor).await?;
        }
        ReportDecision::Reject => {
            set_version(
                &mut tx,
                &scope,
                &b,
                &in_review,
                DashboardVersionStatus::Rejected,
                reason.as_deref(),
                None,
                None,
                &actor,
            )
            .await?;
            let to = if b.current_version_id.is_some() {
                DashboardStatus::Published
            } else {
                DashboardStatus::Draft
            };
            set_board(
                &mut tx,
                &scope,
                &b,
                to,
                None,
                None,
                reason.as_deref(),
                None,
                "services.dashboard.reject",
                &actor,
            )
            .await?;
        }
    }
    let d = load_view(&mut tx, ctx.scope.tenant_id, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &d))
}

/// Publish the approved version (`services.report.approve`, step-up; If-Match): the previous
/// published version becomes `superseded` (kept), the dashboard shows the new one
/// (`dashboard.published`).
#[utoipa::path(post, path = "/api/v1/services/dashboards/{id}/publish", operation_id = "services_dashboard_publish", tag = "services-dashboards",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = ReportReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = DashboardView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn publish_board(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<ReportReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportApprove)?;
    let v = expected!(headers);
    let reason = clean(body.reason);
    let actor = actor(ctx);
    let publisher = actor.id.clone().unwrap_or_else(|| AUTO_APPROVER.to_owned());
    let (scope, mut tx, b) = Command {
        st: &st,
        ctx,
        id,
        expected: v,
    }
    .begin()
    .await?;
    let approved = version_in(&mut tx, id, &[DashboardVersionStatus::Approved])
        .await?
        .ok_or_else(|| {
            KernelError::Conflict(format!("dashboard {} has no approved version", b.code))
        })?;
    if let Some(current) = version_in(&mut tx, id, &[DashboardVersionStatus::Published]).await? {
        set_version(
            &mut tx,
            &scope,
            &b,
            &current,
            DashboardVersionStatus::Superseded,
            Some(&format!("superseded by v{}", approved.version_no)),
            None,
            None,
            &actor,
        )
        .await?;
    }
    set_version(
        &mut tx,
        &scope,
        &b,
        &approved,
        DashboardVersionStatus::Published,
        reason.as_deref(),
        None,
        Some(&publisher),
        &actor,
    )
    .await?;
    let note = format!("v{}", approved.version_no);
    set_board(
        &mut tx,
        &scope,
        &b,
        DashboardStatus::Published,
        Some(approved.id),
        None,
        Some(reason.as_deref().unwrap_or(&note)),
        Some((ev::DASHBOARD_PUBLISHED, ev::DASHBOARD_PUBLISHED_SCHEMA)),
        "services.dashboard.publish",
        &actor,
    )
    .await?;
    let d = load_view(&mut tx, ctx.scope.tenant_id, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &d))
}

/// Which earlier version to re-publish.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DashboardRollback {
    pub version_no: i32,
    pub reason: Option<String>,
}

/// Roll back to an earlier (superseded) version (`services.report.approve`, step-up; If-Match;
/// reason required; L3-H-052): the current version is superseded, the earlier one re-published
/// (`dashboard.rolled_back`). History is never rewritten.
#[utoipa::path(post, path = "/api/v1/services/dashboards/{id}/rollback", operation_id = "services_dashboard_rollback", tag = "services-dashboards",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = DashboardRollback, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = DashboardView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn rollback(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<DashboardRollback>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportApprove)?;
    let v = expected!(headers);
    let reason = required_reason(body.reason)?;
    let actor = actor(ctx);
    let publisher = actor.id.clone().unwrap_or_default();
    let (scope, mut tx, b) = Command {
        st: &st,
        ctx,
        id,
        expected: v,
    }
    .begin()
    .await?;
    let row = sqlx::query(&format!(
        "{VERSION_SELECT} WHERE dashboard_id = $1 AND version_no = $2 FOR UPDATE"
    ))
    .bind(id)
    .bind(body.version_no)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| invalid("versionNo", "unknown_version", "no such version"))?;
    let target = version_from_row(&row).map_err(map_db_error)?;
    if target.status != DashboardVersionStatus::Superseded {
        return Err(KernelError::Conflict(format!(
            "version {} of {} is {}; only a superseded version can be restored",
            target.version_no, b.code, target.status
        ))
        .into());
    }
    let current = version_in(&mut tx, id, &[DashboardVersionStatus::Published])
        .await?
        .ok_or_else(|| KernelError::Conflict(format!("dashboard {} is not published", b.code)))?;
    set_version(
        &mut tx,
        &scope,
        &b,
        &current,
        DashboardVersionStatus::Superseded,
        Some(&reason),
        None,
        None,
        &actor,
    )
    .await?;
    set_version(
        &mut tx,
        &scope,
        &b,
        &target,
        DashboardVersionStatus::Published,
        Some(&reason),
        None,
        Some(&publisher),
        &actor,
    )
    .await?;
    set_board(
        &mut tx,
        &scope,
        &b,
        DashboardStatus::Published,
        Some(target.id),
        None,
        Some(&reason),
        Some((ev::DASHBOARD_ROLLED_BACK, ev::DASHBOARD_ROLLED_BACK_SCHEMA)),
        "services.dashboard.rollback",
        &actor,
    )
    .await?;
    let d = load_view(&mut tx, ctx.scope.tenant_id, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &d))
}

// ================================================================ assignments

/// Make a dashboard available to a role.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DashboardAssignmentCreate {
    /// `administrator`, `faculty` or `student`.
    pub role: String,
    pub org_unit_id: Option<Uuid>,
}

/// Assign a dashboard to a role (`services.report.manage`; Idempotency-Key; L3-H-050). The
/// dashboard shows on `/me/dashboards` of that role once published. 409 for a duplicate.
#[utoipa::path(post, path = "/api/v1/services/dashboards/{id}/assignments", operation_id = "services_dashboard_assignment_create", tag = "services-dashboards",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = DashboardAssignmentCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = DashboardAssignmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn assign(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<DashboardAssignmentCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportManage)?;
    require_idempotency_key(&headers)?;
    let role = Role::parse(body.role.trim())
        .ok_or_else(|| invalid("role", "unknown_role", "administrator, faculty or student"))?;
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_board(&mut conn, t, id, false).await?
    };
    let scope = institution_scope(ctx, pre.institution_id);
    require_institution_writable(&st, &scope, pre.institution_id).await?;
    if pre.status == DashboardStatus::Retired {
        return Err(illegal("dashboard", &pre.code, "retired", "assigned").into());
    }
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let aid: Uuid = sqlx::query_scalar(
        "INSERT INTO services.dashboard_assignment (tenant_id, institution_id, dashboard_id, role, org_unit_id) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(pre.institution_id.as_uuid())
    .bind(id)
    .bind(role.as_str())
    .bind(body.org_unit_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match map_db_error(e) {
        KernelError::Conflict(_) => KernelError::Conflict(format!(
            "dashboard {} is already assigned to {}",
            pre.code,
            role.as_str()
        )),
        other => other,
    })?;
    transition(
        &mut tx,
        &scope,
        &Transition {
            aggregate_type: "dashboard_assignment",
            id: aid,
            institution: pre.institution_id,
            from: None,
            to: "active",
            version: 1,
            reason: Some(role.as_str()),
            process_instance_id: None,
            number: Some(&pre.code),
            subject_ref: None,
        },
        None,
        "services.dashboard_assignment.create",
        &actor,
    )
    .await?;
    let row = sqlx::query(&format!("{ASSIGNMENT_SELECT} WHERE id = $1"))
        .bind(aid)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let a = assignment_from_row(&row).map_err(map_db_error)?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &a, Version(a.version)))
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DashboardAssignmentList {
    pub items: Vec<DashboardAssignmentView>,
}

/// Assignments of a dashboard (`services.report.read`).
#[utoipa::path(get, path = "/api/v1/services/dashboards/{id}/assignments", operation_id = "services_dashboard_assignment_list", tag = "services-dashboards",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = DashboardAssignmentList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list_assignments(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<DashboardAssignmentList>> {
    let ctx = auth.require(Permission::ReportRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load_board(&mut conn, ctx.scope.tenant_id, id, false).await?;
    Ok(Json(DashboardAssignmentList {
        items: assignments_of(&mut conn, id).await?,
    }))
}

/// Revoke an assignment (`services.report.manage`; If-Match on the assignment).
#[utoipa::path(post, path = "/api/v1/services/dashboards/{id}/assignments/{assignmentId}/revoke", operation_id = "services_dashboard_assignment_revoke", tag = "services-dashboards",
    params(("id" = String, Path, format = Uuid), ("assignmentId" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = ReportReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = DashboardAssignmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn revoke(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path((id, assignment_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    Json(body): Json<ReportReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_board(&mut conn, t, id, false).await?
    };
    let scope = institution_scope(ctx, pre.institution_id);
    require_institution_writable(&st, &scope, pre.institution_id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row = sqlx::query(&format!(
        "{ASSIGNMENT_SELECT} WHERE id = $1 AND dashboard_id = $2 AND tenant_id = $3 FOR UPDATE"
    ))
    .bind(assignment_id)
    .bind(id)
    .bind(t.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "dashboard assignment",
    })?;
    let a = assignment_from_row(&row).map_err(map_db_error)?;
    check_version(v, a.version)?;
    if a.status != DashboardAssignmentStatus::Active {
        return Err(illegal("dashboard assignment", &a.role, "revoked", "revoked").into());
    }
    let reason = clean(body.reason);
    sqlx::query(
        "UPDATE services.dashboard_assignment SET status = 'revoked', version = version + 1, \
         updated_at = now() WHERE id = $1",
    )
    .bind(assignment_id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    transition(
        &mut tx,
        &scope,
        &Transition {
            aggregate_type: "dashboard_assignment",
            id: assignment_id,
            institution: pre.institution_id,
            from: Some("active"),
            to: "revoked",
            version: a.version + 1,
            reason: reason.as_deref(),
            process_instance_id: None,
            number: Some(&pre.code),
            subject_ref: None,
        },
        None,
        "services.dashboard_assignment.revoke",
        &actor,
    )
    .await?;
    let row = sqlx::query(&format!("{ASSIGNMENT_SELECT} WHERE id = $1"))
        .bind(assignment_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let out = assignment_from_row(&row).map_err(map_db_error)?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

// ================================================================ rendering

/// One widget with its live figure.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RenderedWidgetView {
    pub widget_id: Uuid,
    pub code: String,
    pub title: String,
    pub metric_key: String,
    pub visualization: WidgetVisualization,
    pub position: i32,
    /// The figure (`None` when the owning domain no longer reports it).
    pub metric: Option<MetricValueView>,
}

/// A published dashboard with its figures, as of `generatedAt`.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DashboardRenderView {
    pub dashboard_id: Uuid,
    pub institution_id: Uuid,
    pub code: String,
    pub name: String,
    pub category: DashboardCategory,
    pub version_no: i32,
    pub generated_at: DateTime<Utc>,
    pub widgets: Vec<RenderedWidgetView>,
}

fn render(
    b: &Board,
    v: &DashboardVersionView,
    catalogue: &[MetricValue],
    at: DateTime<Utc>,
) -> DashboardRenderView {
    DashboardRenderView {
        dashboard_id: b.id,
        institution_id: b.institution_id.as_uuid(),
        code: b.code.clone(),
        name: b.name.clone(),
        category: b.category,
        version_no: v.version_no,
        generated_at: at,
        widgets: v
            .widgets
            .iter()
            .map(|w| RenderedWidgetView {
                widget_id: w.widget_id,
                code: w.code.clone(),
                title: w.title.clone(),
                metric_key: w.metric_key.clone(),
                visualization: w.visualization,
                position: w.position,
                metric: figure(&w.metric_key, catalogue).map(MetricValueView::from),
            })
            .collect(),
    }
}

async fn published_of(
    conn: &mut PgConnection,
    b: &Board,
) -> Result<Option<DashboardVersionView>, KernelError> {
    let Some(current) = b.current_version_id else {
        return Ok(None);
    };
    let row = sqlx::query(&format!("{VERSION_SELECT} WHERE id = $1"))
        .bind(current)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
    row.as_ref()
        .map(version_from_row)
        .transpose()
        .map_err(map_db_error)
}

/// The published version of a dashboard with its live figures (`services.report.read`). 409
/// before the first publication.
#[utoipa::path(get, path = "/api/v1/services/dashboards/{id}/view", operation_id = "services_dashboard_render", tag = "services-dashboards",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = DashboardRenderView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn render_board(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<DashboardRenderView>> {
    let ctx = auth.require(Permission::ReportRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let b = load_board(&mut conn, ctx.scope.tenant_id, id, false).await?;
    let v = published_of(&mut conn, &b)
        .await?
        .filter(|_| b.status == DashboardStatus::Published)
        .ok_or_else(|| KernelError::Conflict(format!("dashboard {} is not published", b.code)))?;
    let scope = institution_scope(ctx, b.institution_id);
    let catalogue = st.all_metrics(&scope, b.institution_id).await?;
    Ok(Json(render(&b, &v, &catalogue, Utc::now())))
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MyDashboardList {
    pub items: Vec<DashboardRenderView>,
}

/// The published dashboards assigned to my roles, with their live figures (any signed-in user).
#[utoipa::path(get, path = "/api/v1/services/me/dashboards", operation_id = "services_dashboard_self_list", tag = "services-dashboards",
    security(("session_cookie" = [])),
    responses((status = 200, body = MyDashboardList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_dashboards(
    State(st): State<ServicesState>,
    auth: Authenticated,
) -> ApiResult<Json<MyDashboardList>> {
    let ctx = &auth.0;
    let roles: Vec<String> = ctx.roles.iter().map(|r| r.as_str().to_owned()).collect();
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let rows = sqlx::query(&format!(
        "{BOARD_SELECT} WHERE tenant_id = $1 AND status = 'published' AND id IN (SELECT dashboard_id \
         FROM services.dashboard_assignment WHERE tenant_id = $1 AND status = 'active' AND role = ANY($2)) \
         ORDER BY name"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(&roles)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let boards = rows
        .iter()
        .map(board_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    let mut catalogues: HashMap<Uuid, Vec<MetricValue>> = HashMap::new();
    let now = Utc::now();
    let mut items = Vec::new();
    for b in boards {
        let Some(v) = published_of(&mut conn, &b).await? else {
            continue;
        };
        let inst = b.institution_id;
        if let Entry::Vacant(slot) = catalogues.entry(inst.as_uuid()) {
            let scope = ExecutionScope::institution(ctx.scope.tenant_id, inst);
            slot.insert(st.all_metrics(&scope, inst).await?);
        }
        let catalogue = catalogues
            .get(&inst.as_uuid())
            .map(Vec::as_slice)
            .unwrap_or_default();
        items.push(render(&b, &v, catalogue, now));
    }
    Ok(Json(MyDashboardList { items }))
}

pub fn router(state: ServicesState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(create, list))
        .routes(routes!(get))
        .routes(routes!(create_version, list_versions))
        .routes(routes!(submit))
        .routes(routes!(decide))
        .routes(routes!(publish_board))
        .routes(routes!(rollback))
        .routes(routes!(assign, list_assignments))
        .routes(routes!(revoke))
        .routes(routes!(render_board))
        .routes(routes!(my_dashboards))
        .with_state(state)
}
