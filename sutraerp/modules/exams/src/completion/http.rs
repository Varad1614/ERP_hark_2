//! HTTP API of completion evaluation and session closure (tag `exam-completion`).

use super::repo::{self, ClosureRow, CompletionRow};
use crate::results::common::{opt2, Decision, DecisionBody};
use crate::results::http::expected;
use crate::shared::grading::fmt2;
use crate::shared::guard::require_institution;
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::scope::institution_scope;
use crate::shared::ExamsState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sutra_contracts::exams::ids::ExamSessionId;
use sutra_contracts::exams::status::CompletionEvaluationStatus;
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::scope::InstitutionId;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// A completion evaluation and its evidence record.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompletionView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub student_id: Uuid,
    pub student_number: String,
    pub display_name: String,
    pub curriculum_version_id: Option<Uuid>,
    #[schema(example = "160.00")]
    pub credits_required: String,
    #[schema(example = "164.00")]
    pub credits_earned: String,
    pub components_required: i32,
    pub components_passed: i32,
    #[schema(example = "8.12")]
    pub cgpa: Option<String>,
    pub status: CompletionEvaluationStatus,
    pub status_reason: Option<String>,
    /// Published results and figures the evaluation rests on (L3-I-153).
    #[schema(value_type = Object)]
    pub evidence: serde_json::Value,
    pub supersedes_id: Option<Uuid>,
    pub evaluated_at: DateTime<Utc>,
    pub version: i64,
}

impl From<&CompletionRow> for CompletionView {
    fn from(c: &CompletionRow) -> Self {
        Self {
            id: c.id,
            institution_id: c.institution_id.as_uuid(),
            student_id: c.student_id,
            student_number: c.student_number.clone(),
            display_name: c.display_name.clone(),
            curriculum_version_id: c.curriculum_version_id,
            credits_required: fmt2(c.credits_required),
            credits_earned: fmt2(c.credits_earned),
            components_required: c.components_required,
            components_passed: c.components_passed,
            cgpa: opt2(c.cgpa),
            status: c.status,
            status_reason: c.status_reason.clone(),
            evidence: c.evidence.clone(),
            supersedes_id: c.supersedes_id,
            evaluated_at: c.created_at,
            version: c.version,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CompletionList {
    pub items: Vec<CompletionView>,
}

/// Evaluate a student's completion.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompletionCreate {
    pub student_id: Uuid,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct CompletionQuery {
    pub student_id: Option<Uuid>,
    pub include_superseded: Option<bool>,
}

/// A session closure.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClosureView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub session_id: Uuid,
    /// `closing` | `closed` | `archived` | `reopened`
    pub status: String,
    pub status_reason: Option<String>,
    /// Blockers found at the last check (`unpublishedResults`, `liveReviews`,
    /// `openPublications`, `ready`, `blockers[]`).
    #[schema(value_type = Object)]
    pub checks: serde_json::Value,
    pub completed_registrations: i32,
    pub process_instance_id: Option<String>,
    pub closed_at: Option<DateTime<Utc>>,
    pub archived_at: Option<DateTime<Utc>>,
    pub version: i64,
}

impl From<&ClosureRow> for ClosureView {
    fn from(c: &ClosureRow) -> Self {
        Self {
            id: c.id,
            institution_id: c.institution_id.as_uuid(),
            session_id: c.session_id.as_uuid(),
            status: c.status.clone(),
            status_reason: c.status_reason.clone(),
            checks: c.checks.clone(),
            completed_registrations: c.completed_registrations,
            process_instance_id: c.process_instance_id.clone(),
            closed_at: c.closed_at,
            archived_at: c.archived_at,
            version: c.version,
        }
    }
}

fn closure(c: &ClosureRow) -> Response {
    with_etag(StatusCode::OK, &ClosureView::from(c), Version(c.version))
}

/// Completion evaluations of an institution (`exam.read`; `?studentId=`).
#[utoipa::path(get, path = "/api/v1/exams/institutions/{id}/completion-evaluations", operation_id = "exam_completion_list", tag = "exam-completion",
    params(("id" = String, Path, format = Uuid), CompletionQuery), security(("session_cookie" = [])),
    responses((status = 200, body = CompletionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn completion_list(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<CompletionQuery>,
) -> ApiResult<Json<CompletionList>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let scope = institution_scope(ctx, InstitutionId(id));
    require_institution(&st, &scope, InstitutionId(id)).await?;
    let items = repo::list_completions(
        &st,
        ctx.scope.tenant_id,
        InstitutionId(id),
        q.student_id,
        q.include_superseded.unwrap_or(false),
    )
    .await?
    .iter()
    .map(CompletionView::from)
    .collect();
    Ok(Json(CompletionList { items }))
}

/// Evaluate (or re-evaluate) a student's academic completion (`exam.result.manage`).
#[utoipa::path(post, path = "/api/v1/exams/institutions/{id}/completion-evaluations", operation_id = "exam_completion_evaluate", tag = "exam-completion",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = CompletionCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = CompletionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn completion_evaluate(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<CompletionCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultManage)?;
    require_idempotency_key(&headers)?;
    let c = repo::evaluate_completion(
        &st,
        ctx.scope.tenant_id,
        InstitutionId(id),
        b.student_id,
        &actor(ctx),
    )
    .await?;
    Ok(with_etag(
        StatusCode::CREATED,
        &CompletionView::from(&c),
        Version(c.version),
    ))
}

/// One completion evaluation (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/completion-evaluations/{id}", operation_id = "exam_completion_get", tag = "exam-completion",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = CompletionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn completion_get(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRead)?;
    let c = repo::get_completion(&st, ctx.scope.tenant_id, id).await?;
    Ok(with_etag(
        StatusCode::OK,
        &CompletionView::from(&c),
        Version(c.version),
    ))
}

/// Re-evaluate after a revised result (`exam.result.manage`; L3-I-150) — supersedes this one.
#[utoipa::path(post, path = "/api/v1/exams/completion-evaluations/{id}/reevaluate", operation_id = "exam_completion_reevaluate", tag = "exam-completion",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the evaluation")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CompletionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn completion_reevaluate(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let cur = repo::get_completion(&st, t, id).await?;
    crate::results::common::ensure_version(v, cur.version)?;
    if cur.status == CompletionEvaluationStatus::Superseded {
        return Err(sutra_kernel::error::KernelError::Conflict(
            "this evaluation is superseded; re-evaluate the current one".into(),
        )
        .into());
    }
    let c =
        repo::evaluate_completion(&st, t, cur.institution_id, cur.student_id, &actor(ctx)).await?;
    Ok(with_etag(
        StatusCode::OK,
        &CompletionView::from(&c),
        Version(c.version),
    ))
}

/// The closure of a session (`exam.read`; 404 before initiation).
#[utoipa::path(get, path = "/api/v1/exams/exam-sessions/{id}/closure", operation_id = "exam_closure_get", tag = "exam-completion",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ClosureView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn closure_get(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRead)?;
    let c = repo::get_closure(&st, ctx.scope.tenant_id, ExamSessionId(id)).await?;
    Ok(closure(&c))
}

/// Start closing an active session (`exam.result.manage`; session → `closing`).
#[utoipa::path(post, path = "/api/v1/exams/exam-sessions/{id}/closure/initiate", operation_id = "exam_closure_initiate", tag = "exam-completion",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ClosureView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn closure_initiate(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultManage)?;
    require_idempotency_key(&headers)?;
    let c =
        repo::initiate_closure(&st, ctx.scope.tenant_id, ExamSessionId(id), &actor(ctx)).await?;
    Ok(closure(&c))
}

/// Close the session when nothing blocks it (`exam.result.manage`; `SESSION_CLOSURE`).
#[utoipa::path(post, path = "/api/v1/exams/exam-sessions/{id}/closure/close", operation_id = "exam_closure_close", tag = "exam-completion",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the closure")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ClosureView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn closure_close(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultManage)?;
    let v = expected!(headers);
    let c =
        repo::close_session(&st, ctx.scope.tenant_id, ExamSessionId(id), v, &actor(ctx)).await?;
    Ok(closure(&c))
}

/// Decide a pending closure (`exam.approve`, step-up): approve closes, reject reopens.
#[utoipa::path(post, path = "/api/v1/exams/exam-sessions/{id}/closure/decision", operation_id = "exam_closure_decide", tag = "exam-completion",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the closure")),
    request_body = DecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ClosureView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn closure_decide(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<DecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamApprove)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision_for_row(
        &st.pool,
        ctx,
        "exams.session_closure",
        "session_id",
        id,
        &["initiated_by"],
        "session closure",
    )
    .await?;
    let v = expected!(headers);
    let c = repo::decide_closure(
        &st,
        ctx.scope.tenant_id,
        ExamSessionId(id),
        v,
        b.decision == Decision::Approve,
        b.reason,
        &actor(ctx),
    )
    .await?;
    Ok(closure(&c))
}

/// Archive a closed session (`exam.result.manage`; L3-I-192).
#[utoipa::path(post, path = "/api/v1/exams/exam-sessions/{id}/closure/archive", operation_id = "exam_closure_archive", tag = "exam-completion",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the closure")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ClosureView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn closure_archive(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultManage)?;
    let v = expected!(headers);
    let c =
        repo::archive_session(&st, ctx.scope.tenant_id, ExamSessionId(id), v, &actor(ctx)).await?;
    Ok(closure(&c))
}

pub fn router(state: ExamsState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(completion_list, completion_evaluate))
        .routes(routes!(completion_get))
        .routes(routes!(completion_reevaluate))
        .routes(routes!(closure_get))
        .routes(routes!(closure_initiate))
        .routes(routes!(closure_close))
        .routes(routes!(closure_decide))
        .routes(routes!(closure_archive))
        .with_state(state)
}
