//! HTTP API of results, publication and grade cards (tag `exam-results`; contract §5.2).

use super::common::{parse_opt, Decision, DecisionBody, ReasonBody};
use super::repo::{self, CorrectionRequest};
use super::views::*;
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::me::my_registrations;
use crate::shared::ExamsState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::NaiveDate;
use serde::Deserialize;
use sutra_contracts::exams::ids::{ExamPaperId, ExamSessionId, TermResultId};
use sutra_contracts::exams::status::ResultStatus;
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// `If-Match` → expected version, or the ready 428 response.
macro_rules! expected {
    ($headers:expr) => {
        match if_match(&$headers)? {
            Ok(v) => v.0,
            Err(resp) => return Ok(resp),
        }
    };
}
pub(crate) use expected;

fn required_reason(r: Option<String>) -> Result<String, KernelError> {
    r.ok_or_else(|| super::common::invalid("reason", "required", "a reason is required"))
}

// ------------------------------------------------------------------ bodies / queries

/// Validation notes.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct ValidateBody {
    pub notes: Option<String>,
}

/// Approved correction of final marks of one paper (L3-I-088/100/117).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CorrectionBody {
    pub paper_id: Uuid,
    #[schema(example = "28.00")]
    pub internal_marks: Option<String>,
    #[schema(example = "64.00")]
    pub external_marks: Option<String>,
    #[schema(example = "2.00")]
    pub grace_marks: Option<String>,
    pub reason: String,
}

/// New publication.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PublicationCreate {
    /// Planned publication date (business date); omitted → `planned`.
    pub scheduled_on: Option<NaiveDate>,
}

/// Delay of a publication.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DelayBody {
    pub reason: String,
    pub until: Option<NaiveDate>,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct ResultQuery {
    pub status: Option<ResultStatus>,
    /// Include superseded revisions (history).
    pub include_superseded: Option<bool>,
}

// ------------------------------------------------------------------ batches

/// Result computation batches of a session (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/exam-sessions/{id}/result-batches", operation_id = "exam_result_batch_list", tag = "exam-results",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ResultBatchList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn batch_list(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ResultBatchList>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let t = ctx.scope.tenant_id;
    repo::session_of(&st, t, ExamSessionId(id)).await?;
    let items = repo::list_batches(&st, t, ExamSessionId(id))
        .await?
        .iter()
        .map(ResultBatchView::from)
        .collect();
    Ok(Json(ResultBatchList { items }))
}

/// Open the result computation batch of an active session (`exam.result.manage`; one live batch
/// per session → 409).
#[utoipa::path(post, path = "/api/v1/exams/exam-sessions/{id}/result-batches", operation_id = "exam_result_batch_create", tag = "exam-results",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ResultBatchView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn batch_create(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultManage)?;
    require_idempotency_key(&headers)?;
    let b = repo::create_batch(&st, ctx.scope.tenant_id, ExamSessionId(id), &actor(ctx)).await?;
    Ok(with_etag(
        StatusCode::CREATED,
        &ResultBatchView::from(&b),
        Version(b.version),
    ))
}

/// One result batch (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/result-batches/{id}", operation_id = "exam_result_batch_get", tag = "exam-results",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ResultBatchView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn batch_get(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRead)?;
    let b = repo::get_batch(&st, ctx.scope.tenant_id, id).await?;
    Ok(with_etag(
        StatusCode::OK,
        &ResultBatchView::from(&b),
        Version(b.version),
    ))
}

/// Compute (or recompute before approval) every effective candidate of the session from final
/// marks (`exam.result.manage`; idempotent re-runs).
#[utoipa::path(post, path = "/api/v1/exams/result-batches/{id}/compute", operation_id = "exam_result_batch_compute", tag = "exam-results",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the batch"), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ComputeSummaryView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn batch_compute(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultManage)?;
    require_idempotency_key(&headers)?;
    let v = expected!(headers);
    let (b, s) = repo::compute_batch(&st, ctx.scope.tenant_id, id, v, &actor(ctx)).await?;
    Ok(with_etag(
        StatusCode::OK,
        &ComputeSummaryView {
            processed: s.processed,
            computed: s.computed,
            withheld: s.withheld,
            unchanged: s.unchanged,
            skipped: s.skipped,
            failed: Vec::new(),
            batch: ResultBatchView::from(&b),
        },
        Version(b.version),
    ))
}

/// Verify the computed results and request result approval (`exam.result.manage`;
/// `RESULT_APPROVAL`, auto-approved in Batch 6).
#[utoipa::path(post, path = "/api/v1/exams/result-batches/{id}/validate", operation_id = "exam_result_batch_validate", tag = "exam-results",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the batch")),
    request_body(content = ValidateBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ResultBatchView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn batch_validate(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ValidateBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultManage)?;
    let v = expected!(headers);
    let notes = body
        .and_then(|Json(b)| b.notes)
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty());
    let b = repo::validate_batch(&st, ctx.scope.tenant_id, id, v, notes, &actor(ctx)).await?;
    Ok(with_etag(
        StatusCode::OK,
        &ResultBatchView::from(&b),
        Version(b.version),
    ))
}

/// Approve / reject a validated batch awaiting its decision (`exam.result.publish`, step-up).
#[utoipa::path(post, path = "/api/v1/exams/result-batches/{id}/decision", operation_id = "exam_result_batch_decide", tag = "exam-results",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the batch")),
    request_body = DecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ResultBatchView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn batch_decide(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<DecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultPublish)?;
    let v = expected!(headers);
    let out = repo::decide_batch(
        &st,
        ctx.scope.tenant_id,
        id,
        v,
        b.decision == Decision::Approve,
        b.reason,
        &actor(ctx),
    )
    .await?;
    Ok(with_etag(
        StatusCode::OK,
        &ResultBatchView::from(&out),
        Version(out.version),
    ))
}

/// Finalise an approved batch → ready for publication (`exam.result.publish`).
#[utoipa::path(post, path = "/api/v1/exams/result-batches/{id}/finalise", operation_id = "exam_result_batch_finalise", tag = "exam-results",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the batch")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ResultBatchView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn batch_finalise(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultPublish)?;
    let v = expected!(headers);
    let b = repo::finalise_batch(&st, ctx.scope.tenant_id, id, v, &actor(ctx)).await?;
    Ok(with_etag(
        StatusCode::OK,
        &ResultBatchView::from(&b),
        Version(b.version),
    ))
}

/// Reopen a validated / approved batch before publication (`exam.result.manage`, reason
/// required, L3-I-099).
#[utoipa::path(post, path = "/api/v1/exams/result-batches/{id}/reopen", operation_id = "exam_result_batch_reopen", tag = "exam-results",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the batch")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ResultBatchView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn batch_reopen(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultManage)?;
    let v = expected!(headers);
    let reason = required_reason(ReasonBody::text(body))?;
    let b = repo::reopen_batch(&st, ctx.scope.tenant_id, id, v, &reason, &actor(ctx)).await?;
    Ok(with_etag(
        StatusCode::OK,
        &ResultBatchView::from(&b),
        Version(b.version),
    ))
}

// ------------------------------------------------------------------ results

/// Current results of a session (`exam.read`; `?status=`, `?includeSuperseded=true`).
#[utoipa::path(get, path = "/api/v1/exams/exam-sessions/{id}/results", operation_id = "exam_result_list", tag = "exam-results",
    params(("id" = String, Path, format = Uuid), ResultQuery), security(("session_cookie" = [])),
    responses((status = 200, body = TermResultList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn result_list(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ResultQuery>,
) -> ApiResult<Json<TermResultList>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let t = ctx.scope.tenant_id;
    repo::session_of(&st, t, ExamSessionId(id)).await?;
    let items = repo::list_results(
        &st,
        t,
        ExamSessionId(id),
        q.status,
        q.include_superseded.unwrap_or(false),
    )
    .await?
    .iter()
    .map(TermResultView::from)
    .collect();
    Ok(Json(TermResultList { items }))
}

/// One term result with its lines (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/term-results/{id}", operation_id = "exam_result_get", tag = "exam-results",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = TermResultView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn result_get(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRead)?;
    let r = repo::get_result(&st, ctx.scope.tenant_id, TermResultId(id)).await?;
    Ok(with_etag(
        StatusCode::OK,
        &TermResultView::from(&r),
        Version(r.version),
    ))
}

/// Withhold an unpublished result (`exam.result.manage`, reason required; L3-I-097/102).
#[utoipa::path(post, path = "/api/v1/exams/term-results/{id}/withhold", operation_id = "exam_result_withhold", tag = "exam-results",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the result")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TermResultView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn result_withhold(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultManage)?;
    let v = expected!(headers);
    let reason = required_reason(ReasonBody::text(body))?;
    let r = repo::withhold_result(
        &st,
        ctx.scope.tenant_id,
        TermResultId(id),
        v,
        &reason,
        &actor(ctx),
    )
    .await?;
    Ok(with_etag(
        StatusCode::OK,
        &TermResultView::from(&r),
        Version(r.version),
    ))
}

/// Release a withheld result once its blocking condition is resolved (`exam.result.manage`;
/// L3-I-086/098). The response is the current result (a new revision when it changed).
#[utoipa::path(post, path = "/api/v1/exams/term-results/{id}/release", operation_id = "exam_result_release", tag = "exam-results",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the result")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TermResultView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn result_release(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultManage)?;
    let v = expected!(headers);
    let reason = ReasonBody::text(body);
    let r = repo::recompute_result(
        &st,
        ctx.scope.tenant_id,
        TermResultId(id),
        v,
        true,
        reason.as_deref(),
        &actor(ctx),
    )
    .await?;
    Ok(with_etag(
        StatusCode::OK,
        &TermResultView::from(&r),
        Version(r.version),
    ))
}

/// Recompute one result before approval (`exam.result.manage`; L3-I-087).
#[utoipa::path(post, path = "/api/v1/exams/term-results/{id}/recompute", operation_id = "exam_result_recompute", tag = "exam-results",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the result")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TermResultView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn result_recompute(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultManage)?;
    let v = expected!(headers);
    let reason = ReasonBody::text(body);
    let r = repo::recompute_result(
        &st,
        ctx.scope.tenant_id,
        TermResultId(id),
        v,
        false,
        reason.as_deref(),
        &actor(ctx),
    )
    .await?;
    Ok(with_etag(
        StatusCode::OK,
        &TermResultView::from(&r),
        Version(r.version),
    ))
}

/// Request an approved correction of one paper's final marks (`exam.result.manage`;
/// `RESULT_CORRECTION`): final marks are revised through `revise_final` and the result is
/// replaced at the same level (published → `term_result.revised`).
#[utoipa::path(post, path = "/api/v1/exams/term-results/{id}/correct", operation_id = "exam_result_correct", tag = "exam-results",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the result")),
    request_body = CorrectionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ResultDecisionOutcome), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn result_correct(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<CorrectionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultManage)?;
    let v = expected!(headers);
    let c = CorrectionRequest {
        paper_id: ExamPaperId(b.paper_id),
        internal_marks: parse_opt("internalMarks", b.internal_marks.as_deref())?,
        external_marks: parse_opt("externalMarks", b.external_marks.as_deref())?,
        grace_marks: parse_opt("graceMarks", b.grace_marks.as_deref())?,
        reason: b.reason.trim().to_owned(),
    };
    let (d, r) = repo::request_decision(
        &st,
        ctx.scope.tenant_id,
        TermResultId(id),
        v,
        Some(c),
        None,
        &actor(ctx),
    )
    .await?;
    Ok(with_etag(
        StatusCode::OK,
        &ResultDecisionOutcome {
            decision: ResultDecisionView::from(&d),
            result: TermResultView::from(&r),
        },
        Version(r.version),
    ))
}

/// Request withdrawal of a published result (`exam.result.publish`; `RESULT_WITHDRAWAL`).
#[utoipa::path(post, path = "/api/v1/exams/term-results/{id}/withdraw", operation_id = "exam_result_withdraw", tag = "exam-results",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the result")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ResultDecisionOutcome), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn result_withdraw(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultPublish)?;
    let v = expected!(headers);
    let reason = required_reason(ReasonBody::text(body))?;
    let (d, r) = repo::request_decision(
        &st,
        ctx.scope.tenant_id,
        TermResultId(id),
        v,
        None,
        Some(reason),
        &actor(ctx),
    )
    .await?;
    Ok(with_etag(
        StatusCode::OK,
        &ResultDecisionOutcome {
            decision: ResultDecisionView::from(&d),
            result: TermResultView::from(&r),
        },
        Version(r.version),
    ))
}

/// Restore a withdrawn result (`exam.result.publish`; L3-I-110).
#[utoipa::path(post, path = "/api/v1/exams/term-results/{id}/restore", operation_id = "exam_result_restore", tag = "exam-results",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the result")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TermResultView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn result_restore(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultPublish)?;
    let v = expected!(headers);
    let reason = ReasonBody::text(body);
    let r = repo::restore_result(
        &st,
        ctx.scope.tenant_id,
        TermResultId(id),
        v,
        reason.as_deref(),
        &actor(ctx),
    )
    .await?;
    Ok(with_etag(
        StatusCode::OK,
        &TermResultView::from(&r),
        Version(r.version),
    ))
}

/// Decide a pending correction / withdrawal (`exam.result.publish`, step-up).
#[utoipa::path(post, path = "/api/v1/exams/result-decisions/{id}/decision", operation_id = "exam_result_decision_decide", tag = "exam-results",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the decision")),
    request_body = DecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ResultDecisionOutcome), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn decision_decide(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<DecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultPublish)?;
    let v = expected!(headers);
    let (d, r) = repo::decide_decision(
        &st,
        ctx.scope.tenant_id,
        id,
        v,
        b.decision == Decision::Approve,
        b.reason,
        &actor(ctx),
    )
    .await?;
    Ok(with_etag(
        StatusCode::OK,
        &ResultDecisionOutcome {
            decision: ResultDecisionView::from(&d),
            result: TermResultView::from(&r),
        },
        Version(d.version),
    ))
}

// ------------------------------------------------------------------ publications

/// Publications of a session (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/exam-sessions/{id}/publications", operation_id = "exam_publication_list", tag = "exam-results",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = PublicationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn publication_list(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PublicationList>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let t = ctx.scope.tenant_id;
    repo::session_of(&st, t, ExamSessionId(id)).await?;
    let items = repo::list_publications(&st, t, ExamSessionId(id))
        .await?
        .iter()
        .map(PublicationView::from)
        .collect();
    Ok(Json(PublicationList { items }))
}

/// Plan a publication of approved results (`exam.result.publish`).
#[utoipa::path(post, path = "/api/v1/exams/exam-sessions/{id}/publications", operation_id = "exam_publication_create", tag = "exam-results",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body(content = PublicationCreate, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = PublicationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn publication_create(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<PublicationCreate>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultPublish)?;
    require_idempotency_key(&headers)?;
    let scheduled = body.and_then(|Json(b)| b.scheduled_on);
    let p = repo::create_publication(
        &st,
        ctx.scope.tenant_id,
        ExamSessionId(id),
        scheduled,
        &actor(ctx),
    )
    .await?;
    Ok(with_etag(
        StatusCode::CREATED,
        &PublicationView::from(&p),
        Version(p.version),
    ))
}

/// One publication (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/publications/{id}", operation_id = "exam_publication_get", tag = "exam-results",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = PublicationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn publication_get(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRead)?;
    let p = repo::get_publication(&st, ctx.scope.tenant_id, id).await?;
    Ok(with_etag(
        StatusCode::OK,
        &PublicationView::from(&p),
        Version(p.version),
    ))
}

/// Request authorisation of a publication (`exam.result.publish`; `RESULT_PUBLICATION`).
#[utoipa::path(post, path = "/api/v1/exams/publications/{id}/authorise", operation_id = "exam_publication_authorise", tag = "exam-results",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the publication")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PublicationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn publication_authorise(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultPublish)?;
    let v = expected!(headers);
    let p = repo::authorise_publication(&st, ctx.scope.tenant_id, id, v, &actor(ctx)).await?;
    Ok(with_etag(
        StatusCode::OK,
        &PublicationView::from(&p),
        Version(p.version),
    ))
}

/// Decide a pending publication authorisation (`exam.result.publish`, step-up).
#[utoipa::path(post, path = "/api/v1/exams/publications/{id}/decision", operation_id = "exam_publication_decide", tag = "exam-results",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the publication")),
    request_body = DecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PublicationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn publication_decide(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<DecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultPublish)?;
    let v = expected!(headers);
    let p = repo::decide_publication(
        &st,
        ctx.scope.tenant_id,
        id,
        v,
        b.decision == Decision::Approve,
        b.reason,
        &actor(ctx),
    )
    .await?;
    Ok(with_etag(
        StatusCode::OK,
        &PublicationView::from(&p),
        Version(p.version),
    ))
}

async fn run_publish(
    st: &ExamsState,
    auth: &Authenticated,
    id: Uuid,
    headers: &HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultPublish)?;
    require_idempotency_key(headers)?;
    let v = expected!(headers);
    let (p, s) = repo::publish_publication(st, ctx.scope.tenant_id, id, v, &actor(ctx)).await?;
    Ok(with_etag(
        StatusCode::OK,
        &PublishSummaryView {
            processed: s.published + s.revised + s.skipped,
            published: s.published,
            revised: s.revised,
            skipped: s.skipped,
            failed: Vec::new(),
            publication: PublicationView::from(&p),
        },
        Version(p.version),
    ))
}

/// Publish the session's approved results (`exam.result.publish`): results → `published`,
/// `term_result.published` (or `.revised` for a replacement revision).
#[utoipa::path(post, path = "/api/v1/exams/publications/{id}/publish", operation_id = "exam_publication_publish", tag = "exam-results",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the publication"), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PublishSummaryView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn publication_publish(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    run_publish(&st, &auth, id, &headers).await
}

/// Retry a failed publication (safe recovery, L3-I-116).
#[utoipa::path(post, path = "/api/v1/exams/publications/{id}/retry", operation_id = "exam_publication_retry", tag = "exam-results",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the publication"), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PublishSummaryView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn publication_retry(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    run_publish(&st, &auth, id, &headers).await
}

/// Delay a pending publication (`exam.result.publish`; L3-I-104).
#[utoipa::path(post, path = "/api/v1/exams/publications/{id}/delay", operation_id = "exam_publication_delay", tag = "exam-results",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the publication")),
    request_body = DelayBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PublicationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn publication_delay(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<DelayBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultPublish)?;
    let v = expected!(headers);
    let reason = required_reason(Some(b.reason.trim().to_owned()).filter(|r| !r.is_empty()))?;
    let p = repo::delay_publication(
        &st,
        ctx.scope.tenant_id,
        id,
        v,
        &reason,
        b.until,
        &actor(ctx),
    )
    .await?;
    Ok(with_etag(
        StatusCode::OK,
        &PublicationView::from(&p),
        Version(p.version),
    ))
}

/// Resolve a publication delay (`exam.result.publish`).
#[utoipa::path(post, path = "/api/v1/exams/publications/{id}/resume", operation_id = "exam_publication_resume", tag = "exam-results",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the publication")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PublicationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn publication_resume(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultPublish)?;
    let v = expected!(headers);
    let p = repo::resume_publication(&st, ctx.scope.tenant_id, id, v, &actor(ctx)).await?;
    Ok(with_etag(
        StatusCode::OK,
        &PublicationView::from(&p),
        Version(p.version),
    ))
}

/// Cancel a pending publication (`exam.result.publish`, reason required; L3-I-105).
#[utoipa::path(post, path = "/api/v1/exams/publications/{id}/cancel", operation_id = "exam_publication_cancel", tag = "exam-results",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the publication")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PublicationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn publication_cancel(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultPublish)?;
    let v = expected!(headers);
    let reason = required_reason(ReasonBody::text(body))?;
    let p = repo::cancel_publication(&st, ctx.scope.tenant_id, id, v, &reason, &actor(ctx)).await?;
    Ok(with_etag(
        StatusCode::OK,
        &PublicationView::from(&p),
        Version(p.version),
    ))
}

// ------------------------------------------------------------------ grade cards

/// The valid grade card of a result's candidate (`exam.read`; 404 when none).
#[utoipa::path(get, path = "/api/v1/exams/term-results/{id}/grade-card", operation_id = "exam_grade_card_of_result", tag = "exam-results",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = GradeCardView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn card_of_result(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRead)?;
    let t = ctx.scope.tenant_id;
    let r = repo::get_result(&st, t, TermResultId(id)).await?;
    let card = repo::cards_of_registrations(&st, t, &[r.registration_id.as_uuid()])
        .await?
        .into_iter()
        .find(|c| c.status != sutra_contracts::exams::status::GradeCardStatus::Withdrawn)
        .ok_or(KernelError::NotFound {
            entity: "grade card",
        })?;
    let result = repo::get_result(&st, t, card.result_id).await?;
    Ok(with_etag(
        StatusCode::OK,
        &GradeCardView::of(&card, Some(&result)),
        Version(card.version),
    ))
}

/// Issue the grade card of a published result (`exam.result.publish`; `GC-` number).
#[utoipa::path(post, path = "/api/v1/exams/term-results/{id}/grade-card", operation_id = "exam_grade_card_issue", tag = "exam-results",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = GradeCardView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn card_issue(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultPublish)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let c = repo::issue_card(&st, t, TermResultId(id), &actor(ctx)).await?;
    let r = repo::get_result(&st, t, c.result_id).await?;
    Ok(with_etag(
        StatusCode::CREATED,
        &GradeCardView::of(&c, Some(&r)),
        Version(c.version),
    ))
}

/// One grade card (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/grade-cards/{id}", operation_id = "exam_grade_card_get", tag = "exam-results",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = GradeCardView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn card_get(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRead)?;
    let t = ctx.scope.tenant_id;
    let c = repo::get_card(&st, t, id).await?;
    let r = repo::get_result(&st, t, c.result_id).await?;
    Ok(with_etag(
        StatusCode::OK,
        &GradeCardView::of(&c, Some(&r)),
        Version(c.version),
    ))
}

/// Reissue a grade card from the current published result (`exam.result.publish`).
#[utoipa::path(post, path = "/api/v1/exams/grade-cards/{id}/reissue", operation_id = "exam_grade_card_reissue", tag = "exam-results",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the grade card")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = GradeCardView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn card_reissue(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultPublish)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let reason = ReasonBody::text(body);
    let c = repo::reissue_card(&st, t, id, v, reason.as_deref(), &actor(ctx)).await?;
    let r = repo::get_result(&st, t, c.result_id).await?;
    Ok(with_etag(
        StatusCode::OK,
        &GradeCardView::of(&c, Some(&r)),
        Version(c.version),
    ))
}

/// Withdraw a grade card (`exam.result.publish`, reason required).
#[utoipa::path(post, path = "/api/v1/exams/grade-cards/{id}/withdraw", operation_id = "exam_grade_card_withdraw", tag = "exam-results",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the grade card")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = GradeCardView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn card_withdraw(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultPublish)?;
    let v = expected!(headers);
    let reason = required_reason(ReasonBody::text(body))?;
    let c = repo::withdraw_card(&st, ctx.scope.tenant_id, id, v, &reason, &actor(ctx)).await?;
    Ok(with_etag(
        StatusCode::OK,
        &GradeCardView::of(&c, None),
        Version(c.version),
    ))
}

// ------------------------------------------------------------------ self-service

/// The caller's own results (`exam.self`): published results in full, withdrawn ones as a
/// notice; nothing before publication.
#[utoipa::path(get, path = "/api/v1/exams/me/results", operation_id = "exam_result_self_list", tag = "exam-results",
    security(("session_cookie" = [])),
    responses((status = 200, body = TermResultList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn me_results(
    State(st): State<ExamsState>,
    auth: Authenticated,
) -> ApiResult<Json<TermResultList>> {
    let ctx = auth.require(Permission::ExamSelfService)?;
    let mine = my_registrations(&st, ctx).await?;
    let t = ctx.scope.tenant_id;
    let mut items = Vec::new();
    for reg in &mine {
        let mut conn = st
            .pool
            .acquire()
            .await
            .map_err(sutra_persistence::map_db_error)?;
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM exams.term_result WHERE tenant_id = $1 AND registration_id = $2 \
             AND status IN ('published', 'withdrawn') ORDER BY revision DESC LIMIT 1",
        )
        .bind(t.as_uuid())
        .bind(reg.registration_id.as_uuid())
        .fetch_all(&mut *conn)
        .await
        .map_err(sutra_persistence::map_db_error)?;
        for id in ids {
            let r =
                crate::shared::results::load_result(&mut conn, t, TermResultId(id), false).await?;
            items.push(TermResultView::for_student(&r));
        }
    }
    Ok(Json(TermResultList { items }))
}

/// The caller's own grade cards (`exam.self`).
#[utoipa::path(get, path = "/api/v1/exams/me/grade-cards", operation_id = "exam_grade_card_self_list", tag = "exam-results",
    security(("session_cookie" = [])),
    responses((status = 200, body = GradeCardList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn me_grade_cards(
    State(st): State<ExamsState>,
    auth: Authenticated,
) -> ApiResult<Json<GradeCardList>> {
    let ctx = auth.require(Permission::ExamSelfService)?;
    let mine = my_registrations(&st, ctx).await?;
    let t = ctx.scope.tenant_id;
    let ids: Vec<Uuid> = mine.iter().map(|r| r.registration_id.as_uuid()).collect();
    let mut items = Vec::new();
    for c in repo::cards_of_registrations(&st, t, &ids).await? {
        let r = repo::get_result(&st, t, c.result_id).await?;
        let shown = (r.status == ResultStatus::Published).then_some(&r);
        items.push(GradeCardView::of(&c, shown));
    }
    Ok(Json(GradeCardList { items }))
}

pub fn router(state: ExamsState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(batch_list, batch_create))
        .routes(routes!(batch_get))
        .routes(routes!(batch_compute))
        .routes(routes!(batch_validate))
        .routes(routes!(batch_decide))
        .routes(routes!(batch_finalise))
        .routes(routes!(batch_reopen))
        .routes(routes!(result_list))
        .routes(routes!(result_get))
        .routes(routes!(result_withhold))
        .routes(routes!(result_release))
        .routes(routes!(result_recompute))
        .routes(routes!(result_correct))
        .routes(routes!(result_withdraw))
        .routes(routes!(result_restore))
        .routes(routes!(decision_decide))
        .routes(routes!(publication_list, publication_create))
        .routes(routes!(publication_get))
        .routes(routes!(publication_authorise))
        .routes(routes!(publication_decide))
        .routes(routes!(publication_publish))
        .routes(routes!(publication_retry))
        .routes(routes!(publication_delay))
        .routes(routes!(publication_resume))
        .routes(routes!(publication_cancel))
        .routes(routes!(card_of_result, card_issue))
        .routes(routes!(card_get))
        .routes(routes!(card_reissue))
        .routes(routes!(card_withdraw))
        .routes(routes!(me_results))
        .routes(routes!(me_grade_cards))
        .with_state(state)
}
