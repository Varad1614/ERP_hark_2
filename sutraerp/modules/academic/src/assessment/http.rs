//! HTTP API of internal assessment and coursework (tag `assessment`; contract §5.2).

use super::repo::{self, *};
use crate::shared::faculty::require_teaching;
use crate::shared::http::actor;
use crate::shared::AcademicState;
use crate::timetable::kit::{
    self, created, expect_version, ok, DeliveryDecision, DeliveryDecisionValue, DeliveryReason,
};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::Json;
use sutra_contracts::academic::ids::OfferingSectionId;
use sutra_contracts::academic::status::AssessmentWindowStatus;
use sutra_kernel::auth::Permission;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct WindowFilter {
    pub status: Option<AssessmentWindowStatus>,
}

async fn read_section(st: &AcademicState, auth: &Authenticated, section: Uuid) -> ApiResult<()> {
    let ctx = auth.require(Permission::AcademicRead)?;
    if !ctx.has_permission(Permission::AssessmentManage) {
        require_teaching(
            st,
            ctx,
            OfferingSectionId(section),
            Permission::AssessmentManage,
        )
        .await?;
    }
    Ok(())
}

/// Assessment blueprints of a section.
#[utoipa::path(get, path = "/api/v1/academic/sections/{id}/assessment-blueprints", operation_id = "assessment_blueprint_list", tag = "assessment",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = AssessmentBlueprintList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn blueprint_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<AssessmentBlueprintList>> {
    read_section(&st, &auth, id).await?;
    Ok(Json(AssessmentBlueprintList {
        items: repo::list_blueprints(&st, auth.0.scope.tenant_id, id).await?,
    }))
}

/// Draft an internal-assessment blueprint (weighted components + policy).
#[utoipa::path(post, path = "/api/v1/academic/sections/{id}/assessment-blueprints", operation_id = "assessment_blueprint_create", tag = "assessment",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewAssessmentBlueprint, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = AssessmentBlueprint, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn blueprint_create(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewAssessmentBlueprint>,
) -> ApiResult<Response> {
    require_idempotency_key(&headers)?;
    let b = repo::create_blueprint(&st, &auth.0, id, cmd, &actor(&auth.0)).await?;
    Ok(created(&b, b.version))
}

/// An assessment blueprint.
#[utoipa::path(get, path = "/api/v1/academic/assessment-blueprints/{id}", operation_id = "assessment_blueprint_get", tag = "assessment",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = AssessmentBlueprint, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn blueprint_get(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    auth.require(Permission::AcademicRead)?;
    let b = repo::get_blueprint(&st, auth.0.scope.tenant_id, id).await?;
    Ok(ok(&b, b.version))
}

/// Edit a draft blueprint.
#[utoipa::path(patch, path = "/api/v1/academic/assessment-blueprints/{id}", operation_id = "assessment_blueprint_update", tag = "assessment",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the blueprint")),
    request_body = UpdateAssessmentBlueprint, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AssessmentBlueprint, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn blueprint_update(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<UpdateAssessmentBlueprint>,
) -> ApiResult<Response> {
    let v = expect_version!(headers);
    let b = repo::update_blueprint(&st, &auth.0, id, v, cmd, &actor(&auth.0)).await?;
    Ok(ok(&b, b.version))
}

/// Publish a validated blueprint (`assessment.manage`; supersedes the current one).
#[utoipa::path(post, path = "/api/v1/academic/assessment-blueprints/{id}/publish", operation_id = "assessment_blueprint_publish", tag = "assessment",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the blueprint")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AssessmentBlueprint, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn blueprint_publish(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let v = expect_version!(headers);
    let b = repo::publish_blueprint(&st, &auth.0, id, v, &actor(&auth.0)).await?;
    Ok(ok(&b, b.version))
}

/// Assessment windows (coursework = kind `assignment`) of a section.
#[utoipa::path(get, path = "/api/v1/academic/sections/{id}/assessment-windows", operation_id = "assessment_window_list", tag = "assessment",
    params(("id" = String, Path, format = Uuid), WindowFilter), security(("session_cookie" = [])),
    responses((status = 200, body = AssessmentWindowList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn window_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(f): Query<WindowFilter>,
) -> ApiResult<Json<AssessmentWindowList>> {
    read_section(&st, &auth, id).await?;
    Ok(Json(AssessmentWindowList {
        items: repo::list_windows(
            &st,
            auth.0.scope.tenant_id,
            id,
            f.status.map(|s| s.as_str()),
        )
        .await?,
    }))
}

/// Create an assessment window / assignment for a component of the published blueprint.
#[utoipa::path(post, path = "/api/v1/academic/sections/{id}/assessment-windows", operation_id = "assessment_window_create", tag = "assessment",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewAssessmentWindow, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = AssessmentWindow, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn window_create(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewAssessmentWindow>,
) -> ApiResult<Response> {
    require_idempotency_key(&headers)?;
    let w = repo::create_window(&st, &auth.0, id, cmd, &actor(&auth.0)).await?;
    Ok(created(&w, w.version))
}

/// An assessment window.
#[utoipa::path(get, path = "/api/v1/academic/assessment-windows/{id}", operation_id = "assessment_window_get", tag = "assessment",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = AssessmentWindow, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn window_get(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    auth.require(Permission::AcademicRead)?;
    let w = repo::get_window(&st, auth.0.scope.tenant_id, id).await?;
    Ok(ok(&w, w.version))
}

macro_rules! window_cmd {
    ($(#[$m:meta])* $fn:ident, $path:literal, $op:literal, $cmd:literal) => {
        $(#[$m])*
        #[utoipa::path(post, path = $path, operation_id = $op, tag = "assessment",
            params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the window")),
            request_body(content = Option<DeliveryReason>), security(("session_cookie" = [], "csrf_token" = [])),
            responses((status = 200, body = AssessmentWindow, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
        pub async fn $fn(State(st): State<AcademicState>, auth: Authenticated, Path(id): Path<Uuid>, headers: HeaderMap, body: Option<Json<DeliveryReason>>) -> ApiResult<Response> {
            let v = expect_version!(headers);
            let w = repo::window_command(&st, &auth.0, id, v, $cmd, kit::reason(body), None, &actor(&auth.0)).await?;
            Ok(ok(&w, w.version))
        }
    };
}

window_cmd!(
    /// Open the window to students → `assessment_window.opened`.
    window_open, "/api/v1/academic/assessment-windows/{id}/open", "assessment_window_open", "open");
window_cmd!(
    /// Close the window (no new submissions) → `assessment_window.closed`.
    window_close, "/api/v1/academic/assessment-windows/{id}/close", "assessment_window_close", "close");
window_cmd!(
    /// Cancel a window without submissions (reason required).
    window_cancel, "/api/v1/academic/assessment-windows/{id}/cancel", "assessment_window_cancel", "cancel");

/// Extend the due date (`assessment.manage`, reason required; the revised deadline is kept).
#[utoipa::path(post, path = "/api/v1/academic/assessment-windows/{id}/extend", operation_id = "assessment_window_extend", tag = "assessment",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the window")),
    request_body = ExtendWindow, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AssessmentWindow, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn window_extend(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<ExtendWindow>,
) -> ApiResult<Response> {
    let v = expect_version!(headers);
    let w = repo::window_command(
        &st,
        &auth.0,
        id,
        v,
        "extend",
        Some(cmd.reason),
        Some(cmd.due_at),
        &actor(&auth.0),
    )
    .await?;
    Ok(ok(&w, w.version))
}

/// Submissions of a window (seated students without one are listed as `pending`).
#[utoipa::path(get, path = "/api/v1/academic/assessment-windows/{id}/submissions", operation_id = "assessment_submission_list", tag = "assessment",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = AssessmentSubmissionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn submission_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<AssessmentSubmissionList>> {
    auth.require(Permission::AcademicRead)?;
    let w = repo::get_window(&st, auth.0.scope.tenant_id, id).await?;
    read_section(&st, &auth, w.section_id).await?;
    Ok(Json(AssessmentSubmissionList {
        items: repo::list_submissions(&st, auth.0.scope.tenant_id, &w).await?,
    }))
}

/// Record an outcome for a student without a submission (offline assessment or absence).
#[utoipa::path(post, path = "/api/v1/academic/assessment-windows/{id}/submissions", operation_id = "assessment_submission_record", tag = "assessment",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = RecordOutcome, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = AssessmentSubmission), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn submission_record(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<RecordOutcome>,
) -> ApiResult<Response> {
    require_idempotency_key(&headers)?;
    let s = repo::record_outcome(&st, &auth.0, id, cmd, &actor(&auth.0)).await?;
    Ok(created(&s, s.version.unwrap_or(1)))
}

/// Mark a submission (a change of recorded marks needs a reason and is kept in the history).
#[utoipa::path(post, path = "/api/v1/academic/assessment-windows/{id}/submissions/{submissionId}/mark", operation_id = "assessment_submission_mark", tag = "assessment",
    params(("id" = String, Path, format = Uuid), ("submissionId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the submission")),
    request_body = MarkSubmission, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AssessmentSubmission, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn submission_mark(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path((id, sub)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    Json(cmd): Json<MarkSubmission>,
) -> ApiResult<Response> {
    let v = expect_version!(headers);
    let s = repo::mark(&st, &auth.0, id, sub, v, cmd, &actor(&auth.0)).await?;
    Ok(ok(&s, s.version.unwrap_or(1)))
}

/// Moderation cases of a window.
#[utoipa::path(get, path = "/api/v1/academic/assessment-windows/{id}/moderation", operation_id = "assessment_moderation_list", tag = "assessment",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = AssessmentModerationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn moderation_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<AssessmentModerationList>> {
    auth.require(Permission::AcademicRead)?;
    Ok(Json(AssessmentModerationList {
        items: repo::list_moderations(&st, auth.0.scope.tenant_id, id).await?,
    }))
}

/// Propose a moderated outcome for a marked submission (`ASSESSMENT_MODERATION`).
#[utoipa::path(post, path = "/api/v1/academic/assessment-windows/{id}/moderation", operation_id = "assessment_moderation_request", tag = "assessment",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewModeration, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = AssessmentModeration, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn moderation_request(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewModeration>,
) -> ApiResult<Response> {
    require_idempotency_key(&headers)?;
    let m = repo::request_moderation(&st, &auth.0, id, cmd, &actor(&auth.0)).await?;
    Ok(created(&m, m.version))
}

/// Independent moderation decision (approve → moderated outcome; reject → original marks stay).
#[utoipa::path(post, path = "/api/v1/academic/assessment-windows/{id}/moderation/{moderationId}/decision", operation_id = "assessment_moderation_decision", tag = "assessment",
    params(("id" = String, Path, format = Uuid), ("moderationId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the moderation")),
    request_body = DeliveryDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AssessmentModeration, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn moderation_decision(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path((id, mid)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    Json(cmd): Json<DeliveryDecision>,
) -> ApiResult<Response> {
    let v = expect_version!(headers);
    let m = repo::decide_moderation(
        &st,
        &auth.0,
        id,
        mid,
        v,
        cmd.decision == DeliveryDecisionValue::Approve,
        kit::opt_text(cmd.reason.as_deref()),
        &actor(&auth.0),
    )
    .await?;
    Ok(ok(&m, m.version))
}

/// Internal-assessment results of a section (all versions).
#[utoipa::path(get, path = "/api/v1/academic/sections/{id}/internal-assessment", operation_id = "internal_assessment_list", tag = "assessment",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = InternalAssessmentResultList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn result_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<InternalAssessmentResultList>> {
    read_section(&st, &auth, id).await?;
    Ok(Json(InternalAssessmentResultList {
        items: repo::list_results(&st, auth.0.scope.tenant_id, id).await?,
    }))
}

/// Assemble and finalise the internal assessment (`assessment.finalize`, `ASSESSMENT_FINALIZATION`) → `internal_assessment.finalized`.
#[utoipa::path(post, path = "/api/v1/academic/sections/{id}/internal-assessment/finalize", operation_id = "internal_assessment_finalize", tag = "assessment",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = InternalAssessmentResultList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn result_finalize(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<InternalAssessmentResultList>> {
    Ok(Json(InternalAssessmentResultList {
        items: repo::finalize(&st, &auth.0, id, &actor(&auth.0)).await?,
    }))
}

/// Finalisation decision for results awaiting one (`AwaitDecision`).
#[utoipa::path(post, path = "/api/v1/academic/sections/{id}/internal-assessment/decision", operation_id = "internal_assessment_decision", tag = "assessment",
    params(("id" = String, Path, format = Uuid)), request_body = DeliveryDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = InternalAssessmentResultList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn result_decision(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(cmd): Json<DeliveryDecision>,
) -> ApiResult<Json<InternalAssessmentResultList>> {
    let items = repo::decide_finalization(
        &st,
        &auth.0,
        id,
        cmd.decision == DeliveryDecisionValue::Approve,
        kit::opt_text(cmd.reason.as_deref()),
        &actor(&auth.0),
    )
    .await?;
    Ok(Json(InternalAssessmentResultList { items }))
}

/// Corrections of a finalised result.
#[utoipa::path(get, path = "/api/v1/academic/internal-assessment-results/{id}/corrections", operation_id = "internal_assessment_correction_list", tag = "assessment",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = AssessmentCorrectionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn correction_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<AssessmentCorrectionList>> {
    auth.require(Permission::AcademicRead)?;
    Ok(Json(AssessmentCorrectionList {
        items: repo::list_corrections(&st, auth.0.scope.tenant_id, id).await?,
    }))
}

/// Request a governed correction (`ASSESSMENT_CORRECTION`; approval re-finalises a new result version → `internal_assessment.corrected`).
#[utoipa::path(post, path = "/api/v1/academic/internal-assessment-results/{id}/corrections", operation_id = "internal_assessment_correction_request", tag = "assessment",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewAssessmentCorrection, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = AssessmentCorrection, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn correction_request(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewAssessmentCorrection>,
) -> ApiResult<Response> {
    require_idempotency_key(&headers)?;
    let c = repo::request_correction(&st, &auth.0, id, cmd, &actor(&auth.0)).await?;
    Ok(created(&c, c.version))
}

/// Decide a pending correction.
#[utoipa::path(post, path = "/api/v1/academic/internal-assessment-results/{id}/corrections/{correctionId}/decision", operation_id = "internal_assessment_correction_decision", tag = "assessment",
    params(("id" = String, Path, format = Uuid), ("correctionId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the correction")),
    request_body = DeliveryDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AssessmentCorrection, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn correction_decision(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path((id, cid)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    Json(cmd): Json<DeliveryDecision>,
) -> ApiResult<Response> {
    let v = expect_version!(headers);
    let c = repo::decide_correction(
        &st,
        &auth.0,
        id,
        cid,
        v,
        cmd.decision == DeliveryDecisionValue::Approve,
        kit::opt_text(cmd.reason.as_deref()),
        &actor(&auth.0),
    )
    .await?;
    Ok(ok(&c, c.version))
}

/// Archive a finalised result (historical; no pending correction).
#[utoipa::path(post, path = "/api/v1/academic/internal-assessment-results/{id}/archive", operation_id = "internal_assessment_archive", tag = "assessment",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the result")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = InternalAssessmentResult, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn result_archive(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let v = expect_version!(headers);
    let r = repo::archive_result(&st, &auth.0, id, v, &actor(&auth.0)).await?;
    Ok(ok(&r, r.version))
}

/// The signed-in student's coursework and assessments with their own submission.
#[utoipa::path(get, path = "/api/v1/academic/me/assignments", operation_id = "assessment_self_list", tag = "assessment",
    security(("session_cookie" = [])),
    responses((status = 200, body = MyAssignments), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn me_assignments(
    State(st): State<AcademicState>,
    auth: Authenticated,
) -> ApiResult<Json<MyAssignments>> {
    Ok(Json(repo::my_assignments(&st, &auth.0).await?))
}

/// Submit an open assignment (once; `late` after the due date while the window is extended).
#[utoipa::path(post, path = "/api/v1/academic/me/assignments/{id}/submission", operation_id = "assessment_self_submit", tag = "assessment",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewSubmission, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = MyAssignment), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn me_submit(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewSubmission>,
) -> ApiResult<Response> {
    require_idempotency_key(&headers)?;
    let a = repo::submit_own(&st, &auth.0, id, cmd, &actor(&auth.0)).await?;
    Ok(created(&a, 1))
}

/// All assessment routes.
pub fn router(state: AcademicState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(blueprint_list, blueprint_create))
        .routes(routes!(blueprint_get, blueprint_update))
        .routes(routes!(blueprint_publish))
        .routes(routes!(window_list, window_create))
        .routes(routes!(window_get))
        .routes(routes!(window_open))
        .routes(routes!(window_close))
        .routes(routes!(window_cancel))
        .routes(routes!(window_extend))
        .routes(routes!(submission_list, submission_record))
        .routes(routes!(submission_mark))
        .routes(routes!(moderation_list, moderation_request))
        .routes(routes!(moderation_decision))
        .routes(routes!(result_list))
        .routes(routes!(result_finalize))
        .routes(routes!(result_decision))
        .routes(routes!(correction_list, correction_request))
        .routes(routes!(correction_decision))
        .routes(routes!(result_archive))
        .routes(routes!(me_assignments))
        .routes(routes!(me_submit))
        .with_state(state)
}
