//! HTTP API of revaluation / rechecking and supplementary eligibility (tag `exam-revaluation`).

use super::repo::{self, EligibilityRow, RevaluationRow};
use crate::results::common::{opt2, parse_dec, Decision, DecisionBody, ReasonBody};
use crate::results::http::expected;
use crate::results::repo::session_of;
use crate::shared::grading::fmt2;
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::me::my_registrations;
use crate::shared::ExamsState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use serde::{Deserialize, Serialize};
use sutra_contracts::exams::ids::{ExamPaperId, ExamSessionId};
use sutra_contracts::exams::status::{
    RevaluationStatus, ReviewKind, SupplementaryEligibilityStatus,
};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::TenantId;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

// ------------------------------------------------------------------ views

/// Outcome of a completed review.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewOutcomeView {
    #[schema(example = "52.00")]
    pub from_marks: Option<String>,
    #[schema(example = "58.00")]
    pub to_marks: Option<String>,
    pub changed: bool,
}

/// A revaluation / rechecking request (contract §5.3).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RevaluationView {
    pub id: Uuid,
    #[schema(example = "REV-000001")]
    pub number: String,
    pub kind: ReviewKind,
    pub session_id: Uuid,
    pub registration_id: Uuid,
    pub paper_id: Uuid,
    pub paper_code: String,
    pub result_id: Uuid,
    pub student_number: String,
    pub display_name: String,
    pub status: RevaluationStatus,
    pub status_reason: Option<String>,
    pub reason: Option<String>,
    #[schema(example = "500.00")]
    pub fee_amount: String,
    pub fee_charge_id: Option<Uuid>,
    pub fee_charge_number: Option<String>,
    pub fee_settled: bool,
    pub outcome: Option<ReviewOutcomeView>,
    pub revised_result_id: Option<Uuid>,
    pub process_instance_id: Option<String>,
    pub version: i64,
}

impl RevaluationView {
    pub fn of(r: &RevaluationRow, fee_settled: bool) -> Self {
        Self {
            id: r.id,
            number: r.request_number.clone(),
            kind: r.kind,
            session_id: r.session_id.as_uuid(),
            registration_id: r.registration_id,
            paper_id: r.paper_id,
            paper_code: r.paper_code.clone(),
            result_id: r.result_id,
            student_number: r.student_number.clone(),
            display_name: r.display_name.clone(),
            status: r.status,
            status_reason: r.status_reason.clone(),
            reason: r.reason.clone(),
            fee_amount: fmt2(r.fee_amount),
            fee_charge_id: r.fee_charge_id,
            fee_charge_number: r.fee_charge_number.clone(),
            fee_settled,
            outcome: r.to_marks.map(|to| ReviewOutcomeView {
                from_marks: opt2(r.from_marks),
                to_marks: Some(fmt2(to)),
                changed: r.from_marks != Some(to),
            }),
            revised_result_id: r.revised_result_id,
            process_instance_id: r.process_instance_id.clone(),
            version: r.version,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RevaluationList {
    pub items: Vec<RevaluationView>,
}

/// A subsequent-attempt eligibility.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EligibilityView {
    pub id: Uuid,
    pub session_id: Uuid,
    pub registration_id: Uuid,
    pub result_id: Uuid,
    pub paper_id: Uuid,
    pub curriculum_component_id: Uuid,
    pub student_id: Uuid,
    pub student_number: String,
    pub display_name: String,
    pub paper_code: String,
    pub attempts_used: i32,
    pub max_attempts: i32,
    #[schema(example = "38.50")]
    pub best_percent: Option<String>,
    pub best_grade: Option<String>,
    pub status: SupplementaryEligibilityStatus,
    pub status_reason: Option<String>,
    pub authorised_session_id: Option<Uuid>,
    pub version: i64,
}

impl From<&EligibilityRow> for EligibilityView {
    fn from(e: &EligibilityRow) -> Self {
        Self {
            id: e.id,
            session_id: e.session_id.as_uuid(),
            registration_id: e.registration_id,
            result_id: e.result_id,
            paper_id: e.paper_id,
            curriculum_component_id: e.curriculum_component_id,
            student_id: e.student_id,
            student_number: e.student_number.clone(),
            display_name: e.display_name.clone(),
            paper_code: e.paper_code.clone(),
            attempts_used: e.attempts_used,
            max_attempts: e.max_attempts,
            best_percent: opt2(e.best_percent),
            best_grade: e.best_grade.clone(),
            status: e.status,
            status_reason: e.status_reason.clone(),
            authorised_session_id: e.authorised_session_id,
            version: e.version,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EligibilityList {
    pub items: Vec<EligibilityView>,
}

/// Summary of an eligibility determination (bulk command).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DetermineSummaryView {
    pub processed: i32,
    pub eligible: i32,
    pub exhausted: i32,
    pub skipped: i32,
    pub failed: Vec<String>,
}

// ------------------------------------------------------------------ bodies

/// A review request of one of the caller's own published papers.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RevaluationCreate {
    pub paper_id: Uuid,
    pub kind: ReviewKind,
    pub reason: Option<String>,
}

/// Review outcome.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewCompleteBody {
    /// Revised external marks after the review.
    #[schema(example = "58.00")]
    pub external_marks: String,
    pub notes: Option<String>,
}

/// Correction of a closed review's outcome.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewCorrectBody {
    #[schema(example = "56.00")]
    pub external_marks: String,
    pub reason: String,
}

/// Authorisation of a subsequent attempt.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthoriseBody {
    pub supplementary_session_id: Uuid,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct RevaluationQuery {
    pub status: Option<RevaluationStatus>,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct EligibilityQuery {
    pub status: Option<SupplementaryEligibilityStatus>,
}

async fn view(
    st: &ExamsState,
    tenant: TenantId,
    r: &RevaluationRow,
) -> Result<RevaluationView, KernelError> {
    let settled = repo::fee_settled_in(st, tenant, r).await?;
    Ok(RevaluationView::of(r, settled))
}

async fn respond(st: &ExamsState, tenant: TenantId, r: &RevaluationRow) -> ApiResult<Response> {
    let v = view(st, tenant, r).await?;
    Ok(with_etag(StatusCode::OK, &v, Version(r.version)))
}

// ------------------------------------------------------------------ office

/// Review requests of a session (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/exam-sessions/{id}/revaluation-requests", operation_id = "exam_revaluation_list", tag = "exam-revaluation",
    params(("id" = String, Path, format = Uuid), RevaluationQuery), security(("session_cookie" = [])),
    responses((status = 200, body = RevaluationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<RevaluationQuery>,
) -> ApiResult<Json<RevaluationList>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let t = ctx.scope.tenant_id;
    session_of(&st, t, ExamSessionId(id)).await?;
    let mut items = Vec::new();
    for r in repo::list_of_session(&st, t, ExamSessionId(id), q.status).await? {
        items.push(view(&st, t, &r).await?);
    }
    Ok(Json(RevaluationList { items }))
}

/// One review request (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/revaluation-requests/{id}", operation_id = "exam_revaluation_get", tag = "exam-revaluation",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = RevaluationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRead)?;
    let r = repo::get(&st, ctx.scope.tenant_id, id).await?;
    respond(&st, ctx.scope.tenant_id, &r).await
}

/// Verify the fee (raised in Finance; settled through Student Fees) and request the decision
/// (`exam.result.manage`; `REVALUATION_DECISION`). 409 while the fee is outstanding.
#[utoipa::path(post, path = "/api/v1/exams/revaluation-requests/{id}/verify", operation_id = "exam_revaluation_verify", tag = "exam-revaluation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the request")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RevaluationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn verify(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultManage)?;
    let v = expected!(headers);
    let r = repo::verify(&st, ctx.scope.tenant_id, id, v, &actor(ctx)).await?;
    respond(&st, ctx.scope.tenant_id, &r).await
}

/// Approve / reject a fee-verified request (`exam.approve`, step-up).
#[utoipa::path(post, path = "/api/v1/exams/revaluation-requests/{id}/decision", operation_id = "exam_revaluation_decide", tag = "exam-revaluation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the request")),
    request_body = DecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RevaluationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn decide(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<DecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamApprove)?;
    let v = expected!(headers);
    let r = repo::decide(
        &st,
        ctx.scope.tenant_id,
        id,
        v,
        b.decision == Decision::Approve,
        b.reason,
        &actor(ctx),
    )
    .await?;
    respond(&st, ctx.scope.tenant_id, &r).await
}

/// Start the review of an approved request (`exam.result.manage`).
#[utoipa::path(post, path = "/api/v1/exams/revaluation-requests/{id}/start-review", operation_id = "exam_revaluation_start_review", tag = "exam-revaluation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the request")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RevaluationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn start_review(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultManage)?;
    let v = expected!(headers);
    let r = repo::step(
        &st,
        ctx.scope.tenant_id,
        id,
        v,
        RevaluationStatus::ReviewInProgress,
        &actor(ctx),
    )
    .await?;
    respond(&st, ctx.scope.tenant_id, &r).await
}

/// Record the review outcome (`exam.result.manage`; revised external marks).
#[utoipa::path(post, path = "/api/v1/exams/revaluation-requests/{id}/complete", operation_id = "exam_revaluation_complete", tag = "exam-revaluation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the request")),
    request_body = ReviewCompleteBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RevaluationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn complete(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReviewCompleteBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultManage)?;
    let v = expected!(headers);
    let marks = parse_dec("externalMarks", &b.external_marks)?;
    let notes = b
        .notes
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty());
    let r = repo::complete(&st, ctx.scope.tenant_id, id, v, marks, notes, &actor(ctx)).await?;
    respond(&st, ctx.scope.tenant_id, &r).await
}

/// Verify the review outcome (`exam.result.manage`).
#[utoipa::path(post, path = "/api/v1/exams/revaluation-requests/{id}/verify-outcome", operation_id = "exam_revaluation_verify_outcome", tag = "exam-revaluation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the request")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RevaluationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn verify_outcome(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultManage)?;
    let v = expected!(headers);
    let r = repo::step(
        &st,
        ctx.scope.tenant_id,
        id,
        v,
        RevaluationStatus::OutcomeVerified,
        &actor(ctx),
    )
    .await?;
    respond(&st, ctx.scope.tenant_id, &r).await
}

/// Approve the revised marks: final marks revised and the result republished as a new revision
/// when they changed (`exam.result.manage`).
#[utoipa::path(post, path = "/api/v1/exams/revaluation-requests/{id}/approve-marks", operation_id = "exam_revaluation_approve_marks", tag = "exam-revaluation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the request")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RevaluationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn approve_marks(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultManage)?;
    let v = expected!(headers);
    let r = repo::approve_marks(&st, ctx.scope.tenant_id, id, v, &actor(ctx)).await?;
    respond(&st, ctx.scope.tenant_id, &r).await
}

/// Withdraw a request before its review starts (`exam.result.manage`).
#[utoipa::path(post, path = "/api/v1/exams/revaluation-requests/{id}/withdraw", operation_id = "exam_revaluation_withdraw", tag = "exam-revaluation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the request")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RevaluationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn withdraw(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultManage)?;
    let v = expected!(headers);
    let reason = ReasonBody::text(body);
    let r = repo::withdraw(
        &st,
        ctx.scope.tenant_id,
        id,
        Some(v),
        None,
        reason,
        &actor(ctx),
    )
    .await?;
    respond(&st, ctx.scope.tenant_id, &r).await
}

/// Correct a closed review's outcome (`exam.result.manage`; L3-I-132).
#[utoipa::path(post, path = "/api/v1/exams/revaluation-requests/{id}/correct", operation_id = "exam_revaluation_correct", tag = "exam-revaluation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the request")),
    request_body = ReviewCorrectBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RevaluationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn correct(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReviewCorrectBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultManage)?;
    let v = expected!(headers);
    let marks = parse_dec("externalMarks", &b.external_marks)?;
    let reason = b.reason.trim().to_owned();
    if reason.is_empty() {
        return Err(
            crate::results::common::invalid("reason", "required", "a reason is required").into(),
        );
    }
    let r = repo::correct(&st, ctx.scope.tenant_id, id, v, marks, &reason, &actor(ctx)).await?;
    respond(&st, ctx.scope.tenant_id, &r).await
}

// ------------------------------------------------------------------ self-service

/// The caller's own review requests (`exam.self`).
#[utoipa::path(get, path = "/api/v1/exams/me/revaluation-requests", operation_id = "exam_revaluation_self_list", tag = "exam-revaluation",
    security(("session_cookie" = [])),
    responses((status = 200, body = RevaluationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn me_list(
    State(st): State<ExamsState>,
    auth: Authenticated,
) -> ApiResult<Json<RevaluationList>> {
    let ctx = auth.require(Permission::ExamSelfService)?;
    let t = ctx.scope.tenant_id;
    let ids: Vec<Uuid> = my_registrations(&st, ctx)
        .await?
        .iter()
        .map(|r| r.registration_id.as_uuid())
        .collect();
    let mut items = Vec::new();
    for r in repo::list_of_registrations(&st, t, &ids).await? {
        items.push(view(&st, t, &r).await?);
    }
    Ok(Json(RevaluationList { items }))
}

/// Request a revaluation / rechecking of one of the caller's own published papers within the
/// review window (`exam.self`). The fee is raised in Finance (pay it under Student Fees).
#[utoipa::path(post, path = "/api/v1/exams/me/revaluation-requests", operation_id = "exam_revaluation_self_create", tag = "exam-revaluation",
    params(("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = RevaluationCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = RevaluationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn me_create(
    State(st): State<ExamsState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(b): Json<RevaluationCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSelfService)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let mine = my_registrations(&st, ctx).await?;
    let reason = b
        .reason
        .map(|r| r.trim().to_owned())
        .filter(|r| !r.is_empty());
    let r = repo::submit(
        &st,
        t,
        &mine,
        ExamPaperId(b.paper_id),
        b.kind,
        reason,
        &actor(ctx),
    )
    .await?;
    let v = view(&st, t, &r).await?;
    Ok(with_etag(StatusCode::CREATED, &v, Version(r.version)))
}

/// Withdraw one of the caller's own requests before its review starts (`exam.self`).
#[utoipa::path(post, path = "/api/v1/exams/me/revaluation-requests/{id}/withdraw", operation_id = "exam_revaluation_self_withdraw", tag = "exam-revaluation",
    params(("id" = String, Path, format = Uuid)),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RevaluationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn me_withdraw(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSelfService)?;
    let t = ctx.scope.tenant_id;
    let ids: Vec<Uuid> = my_registrations(&st, ctx)
        .await?
        .iter()
        .map(|r| r.registration_id.as_uuid())
        .collect();
    let reason = ReasonBody::text(body);
    let r = repo::withdraw(&st, t, id, None, Some(ids.as_slice()), reason, &actor(ctx)).await?;
    respond(&st, t, &r).await
}

// ------------------------------------------------------------------ supplementary

/// Subsequent-attempt eligibilities determined from a session (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/exam-sessions/{id}/supplementary-eligibility", operation_id = "exam_supplementary_list", tag = "exam-revaluation",
    params(("id" = String, Path, format = Uuid), EligibilityQuery), security(("session_cookie" = [])),
    responses((status = 200, body = EligibilityList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn eligibility_list(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<EligibilityQuery>,
) -> ApiResult<Json<EligibilityList>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let t = ctx.scope.tenant_id;
    session_of(&st, t, ExamSessionId(id)).await?;
    let items = repo::eligibility_of_session(&st, t, ExamSessionId(id), q.status)
        .await?
        .iter()
        .map(EligibilityView::from)
        .collect();
    Ok(Json(EligibilityList { items }))
}

/// Determine subsequent-attempt eligibility from the session's published failed lines
/// (`exam.result.manage`; idempotent bulk command).
#[utoipa::path(post, path = "/api/v1/exams/exam-sessions/{id}/supplementary-eligibility", operation_id = "exam_supplementary_determine", tag = "exam-revaluation",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = DetermineSummaryView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn eligibility_determine(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Json<DetermineSummaryView>> {
    let ctx = auth.require(Permission::ResultManage)?;
    require_idempotency_key(&headers)?;
    let s = repo::determine(&st, ctx.scope.tenant_id, ExamSessionId(id), &actor(ctx)).await?;
    Ok(Json(DetermineSummaryView {
        processed: s.processed,
        eligible: s.eligible,
        exhausted: s.exhausted,
        skipped: s.skipped,
        failed: Vec::new(),
    }))
}

/// Authorise the attempt in a supplementary / improvement session (`exam.result.manage`).
#[utoipa::path(post, path = "/api/v1/exams/supplementary-eligibility/{id}/authorise", operation_id = "exam_supplementary_authorise", tag = "exam-revaluation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the eligibility")),
    request_body = AuthoriseBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = EligibilityView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn eligibility_authorise(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<AuthoriseBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResultManage)?;
    let v = expected!(headers);
    let e = repo::authorise(
        &st,
        ctx.scope.tenant_id,
        id,
        v,
        ExamSessionId(b.supplementary_session_id),
        &actor(ctx),
    )
    .await?;
    Ok(with_etag(
        StatusCode::OK,
        &EligibilityView::from(&e),
        Version(e.version),
    ))
}

pub fn router(state: ExamsState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(list))
        .routes(routes!(get))
        .routes(routes!(verify))
        .routes(routes!(decide))
        .routes(routes!(start_review))
        .routes(routes!(complete))
        .routes(routes!(verify_outcome))
        .routes(routes!(approve_marks))
        .routes(routes!(withdraw))
        .routes(routes!(correct))
        .routes(routes!(me_list, me_create))
        .routes(routes!(me_withdraw))
        .routes(routes!(eligibility_list, eligibility_determine))
        .routes(routes!(eligibility_authorise))
        .with_state(state)
}
