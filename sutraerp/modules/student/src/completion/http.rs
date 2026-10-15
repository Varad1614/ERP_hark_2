//! HTTP API of qualification completion, conferred qualifications, exits and exit clearance
//! (tag `completion`; contract doc §5). Creates need `Idempotency-Key` (201 + `ETag`); state
//! commands need `If-Match` of the aggregate they act on (clearance commands: the parent exit /
//! completion). Illegal transitions → 409.

use super::domain::*;
use super::kit::{self, created, expect_version, ok, LifecycleDecision, LifecycleReason};
use super::{exit, qualification};
use crate::shared::http::actor;
use crate::shared::StudentState;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::Json;
use sutra_contracts::student::ids::{
    ExitClearanceId, QualificationCompletionId, QualificationCorrectionId, StudentExitId, StudentId,
};
use sutra_contracts::student::status::{
    ConferredQualificationStatus, QualificationCompletionStatus, StudentExitStatus,
};
use sutra_kernel::auth::Permission;
use sutra_kernel::scope::InstitutionId;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CompletionFilter {
    pub status: Option<QualificationCompletionStatus>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ExitFilter {
    pub status: Option<StudentExitStatus>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ConferredFilter {
    pub status: Option<ConferredQualificationStatus>,
}

// ============================================================================================
// Qualification completions
// ============================================================================================

/// List the qualification completions of a student.
#[utoipa::path(get, path = "/api/v1/student/students/{id}/qualification-completions", operation_id = "qualification_completion_list", tag = "completion",
    params(("id" = String, Path, format = Uuid), CompletionFilter), security(("session_cookie" = [])),
    responses((status = 200, body = QualificationCompletionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn completion_list(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    Query(q): Query<CompletionFilter>,
) -> ApiResult<Json<QualificationCompletionList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    crate::shared::student::load_student(&st.pool, ctx.scope.tenant_id, id).await?;
    Ok(Json(QualificationCompletionList {
        items: qualification::list_for_student(&st, ctx.scope.tenant_id, id, q.status).await?,
    }))
}

/// Initiate qualification completion (programme completion or NEP MEME intermediate exit).
#[utoipa::path(post, path = "/api/v1/student/students/{id}/qualification-completions", operation_id = "qualification_completion_create", tag = "completion",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewQualificationCompletion, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = QualificationCompletion, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn completion_create(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    headers: HeaderMap,
    Json(cmd): Json<NewQualificationCompletion>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CompletionManage)?;
    require_idempotency_key(&headers)?;
    let c = qualification::create(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&c, c.version))
}

/// Get a qualification completion (with clearance items and deferrals).
#[utoipa::path(get, path = "/api/v1/student/qualification-completions/{id}", operation_id = "qualification_completion_get", tag = "completion",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = QualificationCompletion, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn completion_get(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<QualificationCompletionId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentRead)?;
    let c = qualification::get(&st, ctx.scope.tenant_id, id).await?;
    Ok(ok(&c, c.version))
}

/// Evaluate qualification eligibility (3B facts when present, else manual evidence).
#[utoipa::path(post, path = "/api/v1/student/qualification-completions/{id}/eligibility-evaluation", operation_id = "qualification_completion_evaluate_eligibility", tag = "completion",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the completion")),
    request_body(content = Option<EligibilityEvaluation>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = QualificationCompletion, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn completion_evaluate(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<QualificationCompletionId>,
    headers: HeaderMap,
    body: Option<Json<EligibilityEvaluation>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CompletionManage)?;
    let v = expect_version!(headers);
    let cmd = body.map(|Json(b)| b).unwrap_or_default();
    let c = qualification::evaluate_eligibility(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&c, c.version))
}

/// Record the degree / qualification audit.
#[utoipa::path(post, path = "/api/v1/student/qualification-completions/{id}/degree-audit", operation_id = "qualification_completion_degree_audit", tag = "completion",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the completion")),
    request_body = DegreeAudit, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = QualificationCompletion, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn completion_audit(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<QualificationCompletionId>,
    headers: HeaderMap,
    Json(cmd): Json<DegreeAudit>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CompletionManage)?;
    let v = expect_version!(headers);
    let c = qualification::record_audit(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&c, c.version))
}

/// Resolve one institutional clearance item of a completion (finance is refreshed, not resolved).
#[utoipa::path(post, path = "/api/v1/student/qualification-completions/{id}/clearances/{clearanceId}/resolve", operation_id = "qualification_completion_clearance_resolve", tag = "completion",
    params(("id" = String, Path, format = Uuid), ("clearanceId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the completion")),
    request_body = ResolveClearance, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = QualificationCompletion, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn completion_clearance_resolve(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path((id, clearance_id)): Path<(QualificationCompletionId, ExitClearanceId)>,
    headers: HeaderMap,
    Json(cmd): Json<ResolveClearance>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CompletionManage)?;
    let v = expect_version!(headers);
    let c = qualification::resolve_clearance(&st, ctx, id, clearance_id, v, Some(cmd), &actor(ctx))
        .await?;
    Ok(ok(&c, c.version))
}

/// Re-read the finance clearance item from Finance.
#[utoipa::path(post, path = "/api/v1/student/qualification-completions/{id}/clearances/{clearanceId}/refresh", operation_id = "qualification_completion_clearance_refresh", tag = "completion",
    params(("id" = String, Path, format = Uuid), ("clearanceId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the completion")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = QualificationCompletion, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn completion_clearance_refresh(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path((id, clearance_id)): Path<(QualificationCompletionId, ExitClearanceId)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CompletionManage)?;
    let v = expect_version!(headers);
    let c =
        qualification::resolve_clearance(&st, ctx, id, clearance_id, v, None, &actor(ctx)).await?;
    Ok(ok(&c, c.version))
}

/// Approval decision on a pending completion (`AwaitDecision`; reject → degree audit).
#[utoipa::path(post, path = "/api/v1/student/qualification-completions/{id}/decision", operation_id = "qualification_completion_decision", tag = "completion",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the completion")),
    request_body = LifecycleDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = QualificationCompletion, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn completion_decision(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<QualificationCompletionId>,
    headers: HeaderMap,
    Json(cmd): Json<LifecycleDecision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::QualificationConfer)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "qualification completion").await?;
    let v = expect_version!(headers);
    let c = qualification::decide(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&c, c.version))
}

/// Confer the approved qualification (step-up).
#[utoipa::path(post, path = "/api/v1/student/qualification-completions/{id}/award", operation_id = "qualification_completion_award", tag = "completion",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the completion")),
    request_body(content = Option<AwardQualification>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = QualificationCompletion, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn completion_award(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<QualificationCompletionId>,
    headers: HeaderMap,
    body: Option<Json<AwardQualification>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::QualificationConfer)?;
    let v = expect_version!(headers);
    let c = qualification::award(
        &st,
        ctx,
        id,
        v,
        body.map(|Json(b)| b).unwrap_or_default(),
        &actor(ctx),
    )
    .await?;
    Ok(ok(&c, c.version))
}

/// Finalise the academic record (step-up).
#[utoipa::path(post, path = "/api/v1/student/qualification-completions/{id}/finalise", operation_id = "qualification_completion_finalise", tag = "completion",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the completion")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = QualificationCompletion, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn completion_finalise(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<QualificationCompletionId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::QualificationConfer)?;
    let v = expect_version!(headers);
    let c = qualification::finalise(&st, ctx, id, v, &actor(ctx)).await?;
    Ok(ok(&c, c.version))
}

/// Complete the student exit of a finalised completion (step-up): the context becomes `qualification_completed`.
#[utoipa::path(post, path = "/api/v1/student/qualification-completions/{id}/complete-exit", operation_id = "qualification_completion_complete_exit", tag = "completion",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the completion")),
    request_body(content = Option<CompleteExit>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = QualificationCompletion, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn completion_complete_exit(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<QualificationCompletionId>,
    headers: HeaderMap,
    body: Option<Json<CompleteExit>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExitDecide)?;
    let v = expect_version!(headers);
    let c = qualification::complete_exit(
        &st,
        ctx,
        id,
        v,
        body.map(|Json(b)| b).unwrap_or_default(),
        &actor(ctx),
    )
    .await?;
    Ok(ok(&c, c.version))
}

/// Defer completion pending remediation.
#[utoipa::path(post, path = "/api/v1/student/qualification-completions/{id}/defer", operation_id = "qualification_completion_defer", tag = "completion",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the completion")),
    request_body = DeferCompletion, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = QualificationCompletion, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn completion_defer(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<QualificationCompletionId>,
    headers: HeaderMap,
    Json(cmd): Json<DeferCompletion>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CompletionManage)?;
    let v = expect_version!(headers);
    let c = qualification::defer(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&c, c.version))
}

/// Resume a deferred completion (re-enters eligibility evaluation).
#[utoipa::path(post, path = "/api/v1/student/qualification-completions/{id}/resume", operation_id = "qualification_completion_resume", tag = "completion",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the completion")),
    request_body = ResumeCompletion, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = QualificationCompletion, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn completion_resume(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<QualificationCompletionId>,
    headers: HeaderMap,
    Json(cmd): Json<ResumeCompletion>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CompletionManage)?;
    let v = expect_version!(headers);
    let c = qualification::resume(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&c, c.version))
}

/// Cancel a completion before approval (reason required).
#[utoipa::path(post, path = "/api/v1/student/qualification-completions/{id}/cancel", operation_id = "qualification_completion_cancel", tag = "completion",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the completion")),
    request_body = LifecycleReason, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = QualificationCompletion, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn completion_cancel(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<QualificationCompletionId>,
    headers: HeaderMap,
    body: Option<Json<LifecycleReason>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CompletionManage)?;
    let v = expect_version!(headers);
    let c = qualification::cancel(&st, ctx, id, v, kit::reason(body), &actor(ctx)).await?;
    Ok(ok(&c, c.version))
}

// ============================================================================================
// Conferred qualifications
// ============================================================================================

/// List the conferred qualifications of a student (every version, newest first).
#[utoipa::path(get, path = "/api/v1/student/students/{id}/conferred-qualifications", operation_id = "conferred_qualification_list", tag = "completion",
    params(("id" = String, Path, format = Uuid), ConferredFilter), security(("session_cookie" = [])),
    responses((status = 200, body = ConferredQualificationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn conferred_list(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    Query(q): Query<ConferredFilter>,
) -> ApiResult<Json<ConferredQualificationList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    crate::shared::student::load_student(&st.pool, ctx.scope.tenant_id, id).await?;
    Ok(Json(ConferredQualificationList {
        items: qualification::list_conferred(&st, ctx.scope.tenant_id, id, q.status).await?,
    }))
}

/// Get a conferred qualification with its correction / revocation cases.
#[utoipa::path(get, path = "/api/v1/student/conferred-qualifications/{id}", operation_id = "conferred_qualification_get", tag = "completion",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ConferredQualification, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn conferred_get(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentRead)?;
    let q = qualification::get_conferred(&st, ctx.scope.tenant_id, id).await?;
    Ok(ok(&q, q.version))
}

/// Request a governed correction (step-up); auto-approval applies it as a new version.
#[utoipa::path(post, path = "/api/v1/student/conferred-qualifications/{id}/corrections", operation_id = "conferred_qualification_correct", tag = "completion",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the conferred qualification")),
    request_body = NewQualificationCorrection, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ConferredQualification, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn conferred_correct(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewQualificationCorrection>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::QualificationConfer)?;
    let v = expect_version!(headers);
    let q = qualification::request_correction(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&q, q.version))
}

/// Request revocation of a conferred qualification (step-up).
#[utoipa::path(post, path = "/api/v1/student/conferred-qualifications/{id}/revoke", operation_id = "conferred_qualification_revoke", tag = "completion",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the conferred qualification")),
    request_body = RevokeQualification, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ConferredQualification, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn conferred_revoke(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<RevokeQualification>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::QualificationRevoke)?;
    let v = expect_version!(headers);
    let q = qualification::request_revocation(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&q, q.version))
}

/// Decide a pending correction / revocation case (`apply`, `no_change`, `reject`).
#[utoipa::path(post, path = "/api/v1/student/conferred-qualifications/{id}/corrections/{correctionId}/decision", operation_id = "conferred_qualification_decision", tag = "completion",
    params(("id" = String, Path, format = Uuid), ("correctionId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the conferred qualification")),
    request_body = CorrectionDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ConferredQualification, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn conferred_decision(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path((id, correction_id)): Path<(Uuid, QualificationCorrectionId)>,
    headers: HeaderMap,
    Json(cmd): Json<CorrectionDecision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::QualificationConfer)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &correction_id, "qualification correction").await?;
    let v = expect_version!(headers);
    let q = qualification::decide_case(&st, ctx, id, correction_id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&q, q.version))
}

// ============================================================================================
// Exits + clearance
// ============================================================================================

/// List the exits of a student.
#[utoipa::path(get, path = "/api/v1/student/students/{id}/exits", operation_id = "exit_list", tag = "completion",
    params(("id" = String, Path, format = Uuid), ExitFilter), security(("session_cookie" = [])),
    responses((status = 200, body = StudentExitList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exit_list(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    Query(q): Query<ExitFilter>,
) -> ApiResult<Json<StudentExitList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    crate::shared::student::load_student(&st.pool, ctx.scope.tenant_id, id).await?;
    Ok(Json(StudentExitList {
        items: exit::list_for_student(&st, ctx.scope.tenant_id, id, q.status).await?,
    }))
}

/// Initiate a withdrawal / transfer-out / discontinuation / termination / administrative / deceased exit.
#[utoipa::path(post, path = "/api/v1/student/students/{id}/exits", operation_id = "exit_create", tag = "completion",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewStudentExit, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = StudentExit, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exit_create(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    headers: HeaderMap,
    Json(cmd): Json<NewStudentExit>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CompletionManage)?;
    require_idempotency_key(&headers)?;
    let e = exit::create(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&e, e.version))
}

/// Exits of an institution (Admin → Students → Exits).
#[utoipa::path(get, path = "/api/v1/student/institutions/{id}/exits", operation_id = "exit_list_institution", tag = "completion",
    params(("id" = String, Path, format = Uuid), ExitFilter), security(("session_cookie" = [])),
    responses((status = 200, body = StudentExitList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exit_list_institution(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    Query(q): Query<ExitFilter>,
) -> ApiResult<Json<StudentExitList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    Ok(Json(StudentExitList {
        items: exit::list_for_institution(&st, ctx.scope.tenant_id, id, q.status).await?,
    }))
}

/// Get an exit with its clearance items.
#[utoipa::path(get, path = "/api/v1/student/exits/{id}", operation_id = "exit_get", tag = "completion",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = StudentExit, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exit_get(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentExitId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentRead)?;
    let e = exit::get(&st, ctx.scope.tenant_id, id).await?;
    Ok(ok(&e, e.version))
}

/// The clearance items of an exit.
#[utoipa::path(get, path = "/api/v1/student/exits/{id}/clearances", operation_id = "exit_clearance_list", tag = "completion",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExitClearanceList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exit_clearance_list(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentExitId>,
) -> ApiResult<Json<ExitClearanceList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    let e = exit::get(&st, ctx.scope.tenant_id, id).await?;
    Ok(Json(ExitClearanceList {
        items: e.clearances,
    }))
}

/// Add a clearance item to an exit in clearance.
#[utoipa::path(post, path = "/api/v1/student/exits/{id}/clearances", operation_id = "exit_clearance_add", tag = "completion",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the exit")),
    request_body = NewExitClearance, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = StudentExit, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exit_clearance_add(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentExitId>,
    headers: HeaderMap,
    Json(cmd): Json<NewExitClearance>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CompletionManage)?;
    let v = expect_version!(headers);
    let e = exit::add_clearance(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&e, e.version))
}

/// Resolve one clearance item (cleared / waived / blocked); all cleared → approval.
#[utoipa::path(post, path = "/api/v1/student/exits/{id}/clearances/{clearanceId}/resolve", operation_id = "exit_clearance_resolve", tag = "completion",
    params(("id" = String, Path, format = Uuid), ("clearanceId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the exit")),
    request_body = ResolveClearance, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = StudentExit, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exit_clearance_resolve(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path((id, clearance_id)): Path<(StudentExitId, ExitClearanceId)>,
    headers: HeaderMap,
    Json(cmd): Json<ResolveClearance>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CompletionManage)?;
    let v = expect_version!(headers);
    let e = exit::resolve_clearance(&st, ctx, id, clearance_id, v, cmd, false, &actor(ctx)).await?;
    Ok(ok(&e, e.version))
}

/// Re-read the finance clearance item from Finance.
#[utoipa::path(post, path = "/api/v1/student/exits/{id}/clearances/{clearanceId}/refresh", operation_id = "exit_clearance_refresh", tag = "completion",
    params(("id" = String, Path, format = Uuid), ("clearanceId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the exit")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = StudentExit, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exit_clearance_refresh(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path((id, clearance_id)): Path<(StudentExitId, ExitClearanceId)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CompletionManage)?;
    let v = expect_version!(headers);
    let cmd = ResolveClearance {
        status: ClearanceResolution::Cleared,
        note: None,
    };
    let e = exit::resolve_clearance(&st, ctx, id, clearance_id, v, cmd, true, &actor(ctx)).await?;
    Ok(ok(&e, e.version))
}

/// Approval decision on a pending exit (`AwaitDecision`; step-up).
#[utoipa::path(post, path = "/api/v1/student/exits/{id}/decision", operation_id = "exit_decision", tag = "completion",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the exit")),
    request_body = LifecycleDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = StudentExit, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exit_decision(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentExitId>,
    headers: HeaderMap,
    Json(cmd): Json<LifecycleDecision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExitDecide)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "student exit").await?;
    let v = expect_version!(headers);
    let e = exit::decide(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&e, e.version))
}

/// Complete an approved exit (step-up): closes the StudentContext.
#[utoipa::path(post, path = "/api/v1/student/exits/{id}/complete", operation_id = "exit_complete", tag = "completion",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the exit")),
    request_body(content = Option<CompleteExit>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = StudentExit, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exit_complete(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentExitId>,
    headers: HeaderMap,
    body: Option<Json<CompleteExit>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExitDecide)?;
    let v = expect_version!(headers);
    let e = exit::complete(
        &st,
        ctx,
        id,
        v,
        body.map(|Json(b)| b).unwrap_or_default(),
        &actor(ctx),
    )
    .await?;
    Ok(ok(&e, e.version))
}

/// Cancel an exit that has not completed.
#[utoipa::path(post, path = "/api/v1/student/exits/{id}/cancel", operation_id = "exit_cancel", tag = "completion",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the exit")),
    request_body(content = Option<LifecycleReason>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = StudentExit, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exit_cancel(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentExitId>,
    headers: HeaderMap,
    body: Option<Json<LifecycleReason>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CompletionManage)?;
    let v = expect_version!(headers);
    let e = exit::cancel(&st, ctx, id, v, kit::reason(body), &actor(ctx)).await?;
    Ok(ok(&e, e.version))
}

/// All completion / exit routes.
pub fn router(state: StudentState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(completion_list, completion_create))
        .routes(routes!(completion_get))
        .routes(routes!(completion_evaluate))
        .routes(routes!(completion_audit))
        .routes(routes!(completion_clearance_resolve))
        .routes(routes!(completion_clearance_refresh))
        .routes(routes!(completion_decision))
        .routes(routes!(completion_award))
        .routes(routes!(completion_finalise))
        .routes(routes!(completion_complete_exit))
        .routes(routes!(completion_defer))
        .routes(routes!(completion_resume))
        .routes(routes!(completion_cancel))
        .routes(routes!(conferred_list))
        .routes(routes!(conferred_get))
        .routes(routes!(conferred_correct))
        .routes(routes!(conferred_revoke))
        .routes(routes!(conferred_decision))
        .routes(routes!(exit_list, exit_create))
        .routes(routes!(exit_list_institution))
        .routes(routes!(exit_get))
        .routes(routes!(exit_clearance_list, exit_clearance_add))
        .routes(routes!(exit_clearance_resolve))
        .routes(routes!(exit_clearance_refresh))
        .routes(routes!(exit_decision))
        .routes(routes!(exit_complete))
        .routes(routes!(exit_cancel))
        .with_state(state)
}
