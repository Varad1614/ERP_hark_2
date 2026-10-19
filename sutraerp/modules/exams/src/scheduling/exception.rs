//! Examination exception cases (L2-I15; L3-I-172 examination exception → governed exception
//! case): raised (`EXC-` number) → classified → under review → resolved → closed, every move in
//! the status history.

use crate::setup::common::{
    check_version, clean, illegal, invalid, move_status, reason_of, require_reason, Move,
    ReasonBody,
};
use crate::setup::domain::exception_can;
use crate::setup::session::load_session;
use crate::shared::guard::require_institution_writable;
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::lifecycle::{record_history, HistoryEntry, StatusChange};
use crate::shared::numbering::{self, kind};
use crate::shared::publish::audit;
use crate::shared::scope::institution_scope;
use crate::shared::ExamsState;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sutra_contracts::exams::status::ExamExceptionStatus;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use uuid::Uuid;

/// An examination exception case.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ExamExceptionView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub session_id: Uuid,
    #[schema(example = "EXC-000001")]
    pub number: String,
    pub sitting_id: Option<Uuid>,
    pub paper_id: Option<Uuid>,
    pub registration_id: Option<Uuid>,
    /// `conduct | venue | paper | candidate | technical | other`.
    pub category: String,
    pub classification: Option<String>,
    /// `low | medium | high | critical`.
    pub severity: Option<String>,
    pub description: String,
    pub resolution: Option<String>,
    /// `raised | classified | under_review | resolved | closed`.
    pub status: String,
    pub status_reason: Option<String>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ExamExceptionList {
    pub items: Vec<ExamExceptionView>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamExceptionCreate {
    pub category: String,
    pub description: String,
    pub sitting_id: Option<Uuid>,
    pub paper_id: Option<Uuid>,
    pub registration_id: Option<Uuid>,
    pub severity: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamExceptionClassify {
    pub classification: String,
    pub severity: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamExceptionResolve {
    pub resolution: String,
}

const COLUMNS: &str = "id, institution_id, session_id, number, sitting_id, paper_id, \
    registration_id, category, classification, severity, description, resolution, status, \
    status_reason, version";

const CATEGORIES: &[&str] = &[
    "conduct",
    "venue",
    "paper",
    "candidate",
    "technical",
    "other",
];
const SEVERITIES: &[&str] = &["low", "medium", "high", "critical"];

async fn load<'c, E>(
    executor: E,
    tenant: TenantId,
    id: Uuid,
) -> Result<ExamExceptionView, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    sqlx::query_as::<_, ExamExceptionView>(&format!(
        "SELECT {COLUMNS} FROM exams.exam_exception WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(executor)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "examination exception",
    })
}

fn check_severity(s: Option<&str>) -> Result<(), KernelError> {
    match s {
        Some(v) if !SEVERITIES.contains(&v) => Err(invalid(
            "severity",
            "invalid",
            "low, medium, high or critical",
        )),
        _ => Ok(()),
    }
}

/// Exception cases of a session (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/exam-sessions/{id}/exceptions", operation_id = "exam_exception_list", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExamExceptionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_list(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ExamExceptionList>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let tenant = ctx.scope.tenant_id;
    load_session(&st.pool, tenant, id).await?;
    let items = sqlx::query_as::<_, ExamExceptionView>(&format!(
        "SELECT {COLUMNS} FROM exams.exam_exception WHERE tenant_id = $1 AND session_id = $2 ORDER BY number"
    ))
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(Json(ExamExceptionList { items }))
}

/// Raise an exception case (`exam.setup.manage`).
#[utoipa::path(post, path = "/api/v1/exams/exam-sessions/{id}/exceptions", operation_id = "exam_exception_create", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ExamExceptionCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ExamExceptionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_create(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamExceptionCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    require_idempotency_key(&headers)?;
    let tenant = ctx.scope.tenant_id;
    let session = load_session(&st.pool, tenant, id).await?;
    let scope = institution_scope(ctx, session.institution_id);
    require_institution_writable(&st, &scope, session.institution_id).await?;
    if !CATEGORIES.contains(&b.category.as_str()) {
        return Err(invalid(
            "category",
            "invalid",
            "conduct, venue, paper, candidate, technical or other",
        )
        .into());
    }
    check_severity(b.severity.as_deref())?;
    let description = clean(Some(&b.description))
        .filter(|d| d.chars().count() <= 2000)
        .ok_or_else(|| {
            invalid(
                "description",
                "invalid_length",
                "must be 1 … 2000 characters",
            )
        })?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let number = numbering::next(&mut tx, tenant, session.institution_id, kind::EXCEPTION).await?;
    let new_id: Uuid = sqlx::query_scalar(
        "INSERT INTO exams.exam_exception (tenant_id, institution_id, session_id, number, \
         sitting_id, paper_id, registration_id, category, severity, description, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(session.institution_id.as_uuid())
    .bind(id)
    .bind(&number)
    .bind(b.sitting_id)
    .bind(b.paper_id)
    .bind(b.registration_id)
    .bind(&b.category)
    .bind(b.severity.as_deref())
    .bind(&description)
    .bind(act.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    record_history(
        &mut tx,
        HistoryEntry {
            tenant,
            institution: session.institution_id,
            aggregate_type: "exam_exception",
            aggregate_id: new_id,
            from: None,
            to: "raised",
        },
        &StatusChange {
            cause_event_type: "exams.exam_exception.raise",
            reason: None,
            process_instance_id: None,
        },
        &act,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.exam_exception.raise",
        "exam_exception",
        new_id.to_string(),
        json!({"number": number, "category": b.category}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let e = load(&st.pool, tenant, new_id).await?;
    Ok(with_etag(StatusCode::CREATED, &e, Version(e.version)))
}

/// One exception case (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/exam-exceptions/{id}", operation_id = "exam_exception_get", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExamExceptionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_get(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRead)?;
    let e = load(&st.pool, ctx.scope.tenant_id, id).await?;
    Ok(with_etag(StatusCode::OK, &e, Version(e.version)))
}

/// Moves an exception (with optional attribute updates) in one transaction.
async fn move_exception(
    st: &ExamsState,
    ctx: &AuthContext,
    id: Uuid,
    headers: &HeaderMap,
    to: ExamExceptionStatus,
    reason: Option<String>,
    set: Option<(&'static str, Vec<Option<String>>)>,
) -> ApiResult<Response> {
    let expected = match if_match(headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let tenant = ctx.scope.tenant_id;
    let e = load(&st.pool, tenant, id).await?;
    let scope = institution_scope(ctx, InstitutionId(e.institution_id));
    require_institution_writable(st, &scope, InstitutionId(e.institution_id)).await?;
    check_version(expected, e.version)?;
    let from = ExamExceptionStatus::parse(&e.status)
        .ok_or_else(|| KernelError::internal_msg("bad exception status"))?;
    if !exception_can(from, to) {
        return Err(illegal(
            &format!("exception {}", e.number),
            from.as_str(),
            to.as_str(),
        )
        .into());
    }
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    if let Some((sql, values)) = set {
        let mut q = sqlx::query(sql).bind(id);
        for v in values {
            q = q.bind(v);
        }
        q.execute(&mut *tx).await.map_err(map_db_error)?;
    }
    move_status(
        &mut tx,
        Move {
            table: "exams.exam_exception",
            aggregate: "exam_exception",
            has_process: false,
            tenant,
            institution: InstitutionId(e.institution_id),
            id,
            version: e.version,
            from: &e.status,
            to: to.as_str(),
        },
        &StatusChange {
            cause_event_type: "exams.exam_exception.move",
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &act,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        &format!("exams.exam_exception.{}", to.as_str()),
        "exam_exception",
        id.to_string(),
        json!({"reason": reason}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let e = load(&st.pool, tenant, id).await?;
    Ok(with_etag(StatusCode::OK, &e, Version(e.version)))
}

/// Classify a raised exception (`exam.setup.manage`).
#[utoipa::path(post, path = "/api/v1/exams/exam-exceptions/{id}/classify", operation_id = "exam_exception_classify", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the exception")),
    request_body = ExamExceptionClassify, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamExceptionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_classify(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamExceptionClassify>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let classification = clean(Some(&b.classification))
        .ok_or_else(|| invalid("classification", "required", "a classification is required"))?;
    check_severity(b.severity.as_deref())?;
    move_exception(
        &st,
        ctx,
        id,
        &headers,
        ExamExceptionStatus::Classified,
        clean(b.reason.as_deref()),
        Some((
            "UPDATE exams.exam_exception SET classification = $2, severity = COALESCE($3, severity) WHERE id = $1",
            vec![Some(classification), b.severity.clone()],
        )),
    )
    .await
}

/// Take an exception under review (`exam.setup.manage`).
#[utoipa::path(post, path = "/api/v1/exams/exam-exceptions/{id}/review", operation_id = "exam_exception_review", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the exception")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamExceptionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_review(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    move_exception(
        &st,
        ctx,
        id,
        &headers,
        ExamExceptionStatus::UnderReview,
        reason_of(body),
        None,
    )
    .await
}

/// Resolve an exception with its resolution (`exam.setup.manage`).
#[utoipa::path(post, path = "/api/v1/exams/exam-exceptions/{id}/resolve", operation_id = "exam_exception_resolve", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the exception")),
    request_body = ExamExceptionResolve, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamExceptionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_resolve(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamExceptionResolve>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let resolution = clean(Some(&b.resolution));
    require_reason(&resolution)?;
    move_exception(
        &st,
        ctx,
        id,
        &headers,
        ExamExceptionStatus::Resolved,
        resolution.clone(),
        Some((
            "UPDATE exams.exam_exception SET resolution = $2 WHERE id = $1",
            vec![resolution],
        )),
    )
    .await
}

/// Close a resolved exception (`exam.setup.manage`).
#[utoipa::path(post, path = "/api/v1/exams/exam-exceptions/{id}/close", operation_id = "exam_exception_close", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the exception")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamExceptionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_close(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    move_exception(
        &st,
        ctx,
        id,
        &headers,
        ExamExceptionStatus::Closed,
        reason_of(body),
        None,
    )
    .await
}
