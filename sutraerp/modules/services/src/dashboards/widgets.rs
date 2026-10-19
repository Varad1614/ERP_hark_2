//! Widget definitions (L3-H-054 widget definition → governed availability or retirement): a widget
//! binds one operational figure (`<domain>.<key>` of the live `OperationalMetricsPort` catalogue);
//! draft → active (`widget_definition.activated`) → retired (409 while a published dashboard
//! version shows it).

use super::common::{
    check_version, clean, decode, expected, invalid, optional_text, required_reason, required_text,
    transition, valid_code, ReportReasonBody, Transition,
};
use super::domain::{binding_problem, WidgetStatus, WidgetVisualization};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::lifecycle::illegal;
use crate::shared::scope::institution_scope;
use crate::shared::ServicesState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use sutra_contracts::services::events::reporting as ev;
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// A widget definition.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WidgetView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "FIN-RECEIVABLES")]
    pub code: String,
    pub title: String,
    #[schema(example = "finance.receivables_outstanding")]
    pub metric_key: String,
    pub visualization: WidgetVisualization,
    pub description: Option<String>,
    pub status: WidgetStatus,
    pub status_reason: Option<String>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WidgetList {
    pub items: Vec<WidgetView>,
}

pub const SELECT: &str = "SELECT id, institution_id, code, title, metric_key, visualization, \
    description, status, status_reason, version FROM services.widget_definition";

pub fn from_row(r: &PgRow) -> Result<WidgetView, sqlx::Error> {
    Ok(WidgetView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        code: r.try_get("code")?,
        title: r.try_get("title")?,
        metric_key: r.try_get("metric_key")?,
        visualization: decode(r.try_get("visualization")?, WidgetVisualization::parse)?,
        description: r.try_get("description")?,
        status: decode(r.try_get("status")?, WidgetStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        version: r.try_get("version")?,
    })
}

/// Loads one widget of the tenant (404), optionally locked.
pub async fn load_widget(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<WidgetView, KernelError> {
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
        .ok_or(KernelError::NotFound { entity: "widget" })?;
    from_row(&row).map_err(map_db_error)
}

/// A widget to define.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WidgetCreate {
    pub code: String,
    pub title: String,
    /// `<domain>.<key>` from `GET /services/institutions/{id}/metrics`.
    pub metric_key: String,
    pub visualization: Option<WidgetVisualization>,
    pub description: Option<String>,
}

/// Define a widget (`services.report.manage`; Idempotency-Key). 422 when the metric key is not
/// in the live catalogue; 409 for a duplicate code.
#[utoipa::path(post, path = "/api/v1/services/institutions/{id}/widgets", operation_id = "services_dashboard_widget_create", tag = "services-dashboards",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = WidgetCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = WidgetView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<WidgetCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let code = valid_code("code", &b.code)?;
    let title = required_text("title", &b.title, 200)?;
    let description = optional_text("description", b.description, 1000)?;
    let key = b.metric_key.trim().to_owned();
    let catalogue = st.all_metrics(&scope, institution).await?;
    if let Some(problem) = binding_problem(&key, None, &catalogue) {
        return Err(invalid("metricKey", "unknown_metric", &problem).into());
    }
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let wid: Uuid = sqlx::query_scalar(
        "INSERT INTO services.widget_definition (tenant_id, institution_id, code, title, metric_key, \
         visualization, description) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(&code)
    .bind(&title)
    .bind(&key)
    .bind(b.visualization.unwrap_or(WidgetVisualization::Stat).as_str())
    .bind(description.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match map_db_error(e) {
        KernelError::Conflict(_) => KernelError::Conflict(format!("widget {code} already exists")),
        other => other,
    })?;
    transition(
        &mut tx,
        &scope,
        &Transition {
            aggregate_type: "widget_definition",
            id: wid,
            institution,
            from: None,
            to: "draft",
            version: 1,
            reason: None,
            process_instance_id: None,
            number: Some(&code),
            subject_ref: None,
        },
        None,
        "services.widget_definition.create",
        &actor,
    )
    .await?;
    let w = load_widget(&mut tx, ctx.scope.tenant_id, wid, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &w, Version(w.version)))
}

/// Filters of the widget list.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct WidgetQuery {
    pub status: Option<WidgetStatus>,
}

/// Widget definitions of an institution (`services.report.read`).
#[utoipa::path(get, path = "/api/v1/services/institutions/{id}/widgets", operation_id = "services_dashboard_widget_list", tag = "services-dashboards",
    params(("id" = String, Path, format = Uuid), WidgetQuery), security(("session_cookie" = [])),
    responses((status = 200, body = WidgetList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<WidgetQuery>,
) -> ApiResult<Json<WidgetList>> {
    let ctx = auth.require(Permission::ReportRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let rows = sqlx::query(&format!(
        "{SELECT} WHERE tenant_id = $1 AND institution_id = $2 AND ($3::text IS NULL OR status = $3) ORDER BY code"
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
    Ok(Json(WidgetList { items }))
}

async fn change(
    st: &ServicesState,
    ctx: &sutra_kernel::auth::AuthContext,
    id: Uuid,
    expected_version: i64,
    to: WidgetStatus,
    reason: Option<String>,
) -> Result<WidgetView, KernelError> {
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_widget(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(st, &scope, institution).await?;
    if to == WidgetStatus::Active {
        let catalogue = st.all_metrics(&scope, institution).await?;
        if let Some(problem) = binding_problem(&pre.metric_key, None, &catalogue) {
            return Err(KernelError::Conflict(problem));
        }
    }
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let w = load_widget(&mut tx, t, id, true).await?;
    check_version(expected_version, w.version)?;
    if !w.status.can_transition(to) {
        return Err(illegal("widget", &w.code, w.status.as_str(), to.as_str()));
    }
    if to == WidgetStatus::Retired {
        let shown: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM services.dashboard_version WHERE status = 'published' \
             AND widgets @> jsonb_build_array(jsonb_build_object('widgetId', $1::text)))",
        )
        .bind(id.to_string())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if shown {
            return Err(KernelError::Conflict(format!(
                "widget {} is shown on a published dashboard; publish a version without it first",
                w.code
            )));
        }
    }
    sqlx::query(
        "UPDATE services.widget_definition SET status = $2, status_reason = $3, version = version + 1, \
         updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(to.as_str())
    .bind(reason.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let (event, action) = if to == WidgetStatus::Active {
        (
            Some((
                ev::WIDGET_DEFINITION_ACTIVATED,
                ev::WIDGET_DEFINITION_ACTIVATED_SCHEMA,
            )),
            "services.widget_definition.activate",
        )
    } else {
        (None, "services.widget_definition.retire")
    };
    transition(
        &mut tx,
        &scope,
        &Transition {
            aggregate_type: "widget_definition",
            id,
            institution,
            from: Some(w.status.as_str()),
            to: to.as_str(),
            version: w.version + 1,
            reason: reason.as_deref(),
            process_instance_id: None,
            number: Some(&w.code),
            subject_ref: None,
        },
        event,
        action,
        &actor,
    )
    .await?;
    let out = load_widget(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Make a draft widget available for dashboards (`services.report.manage`; If-Match).
#[utoipa::path(post, path = "/api/v1/services/widgets/{id}/activate", operation_id = "services_dashboard_widget_activate", tag = "services-dashboards",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = ReportReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = WidgetView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn activate(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReportReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportManage)?;
    let v = expected!(headers);
    let w = change(&st, ctx, id, v, WidgetStatus::Active, clean(b.reason)).await?;
    Ok(with_etag(StatusCode::OK, &w, Version(w.version)))
}

/// Retire a widget (`services.report.manage`; If-Match; reason required).
#[utoipa::path(post, path = "/api/v1/services/widgets/{id}/retire", operation_id = "services_dashboard_widget_retire", tag = "services-dashboards",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = ReportReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = WidgetView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn retire(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReportReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ReportManage)?;
    let v = expected!(headers);
    let reason = required_reason(b.reason)?;
    let w = change(&st, ctx, id, v, WidgetStatus::Retired, Some(reason)).await?;
    Ok(with_etag(StatusCode::OK, &w, Version(w.version)))
}

pub fn router(state: ServicesState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(create, list))
        .routes(routes!(activate))
        .routes(routes!(retire))
        .with_state(state)
}
