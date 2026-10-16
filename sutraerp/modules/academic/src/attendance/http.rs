//! HTTP API of teaching sessions, attendance and syllabus coverage (tag `attendance`; contract §5.2).

use super::repo::{self, *};
use super::syllabus::{
    self, NewSyllabusCoverage, ReplaceTopics, SyllabusCoverage, SyllabusCoverageList,
};
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
use sutra_contracts::academic::status::{
    ChangeRequestStatus, PolicyVersionStatus, TeachingSessionStatus,
};
use sutra_kernel::auth::Permission;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SessionFilter {
    pub status: Option<TeachingSessionStatus>,
    /// At most 200 (default 50).
    pub limit: Option<i64>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PolicyFilter {
    pub status: Option<PolicyVersionStatus>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CorrectionFilter {
    /// `requested` lists the corrections awaiting a decision.
    pub status: Option<ChangeRequestStatus>,
}

/// Staff read of a section (`academic.read`; faculty only on sections they teach).
async fn read_section(st: &AcademicState, auth: &Authenticated, section: Uuid) -> ApiResult<()> {
    let ctx = auth.require(Permission::AcademicRead)?;
    if !ctx.has_permission(Permission::AttendanceManage) {
        require_teaching(
            st,
            ctx,
            OfferingSectionId(section),
            Permission::AttendanceManage,
        )
        .await?;
    }
    Ok(())
}

/// Teaching sessions of a section (newest first).
#[utoipa::path(get, path = "/api/v1/academic/sections/{id}/sessions", operation_id = "teaching_session_list", tag = "attendance",
    params(("id" = String, Path, format = Uuid), SessionFilter), security(("session_cookie" = [])),
    responses((status = 200, body = TeachingSessionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn session_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(f): Query<SessionFilter>,
) -> ApiResult<Json<TeachingSessionList>> {
    read_section(&st, &auth, id).await?;
    Ok(Json(TeachingSessionList {
        items: repo::list_for_section(
            &st,
            auth.0.scope.tenant_id,
            id,
            f.status.map(|s| s.as_str()),
            kit::limit(f.limit),
        )
        .await?,
    }))
}

/// Schedule a teaching session (from an operational timetable slot, or ad hoc times).
#[utoipa::path(post, path = "/api/v1/academic/sections/{id}/sessions", operation_id = "teaching_session_create", tag = "attendance",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewTeachingSession, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = TeachingSession, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn session_create(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewTeachingSession>,
) -> ApiResult<Response> {
    require_idempotency_key(&headers)?;
    let s = repo::create_session(&st, &auth.0, id, cmd, &actor(&auth.0)).await?;
    Ok(created(&s, s.version))
}

/// A session with its attendance lines.
#[utoipa::path(get, path = "/api/v1/academic/sessions/{id}", operation_id = "teaching_session_get", tag = "attendance",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = TeachingSession, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn session_get(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    auth.require(Permission::AcademicRead)?;
    let s = repo::get(&st, auth.0.scope.tenant_id, id).await?;
    read_section(&st, &auth, s.section_id).await?;
    Ok(ok(&s, s.version))
}

macro_rules! session_cmd {
    ($(#[$m:meta])* $fn:ident, $path:literal, $op:literal, $cmd:expr) => {
        $(#[$m])*
        #[utoipa::path(post, path = $path, operation_id = $op, tag = "attendance",
            params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the session")),
            request_body(content = Option<SessionProgress>), security(("session_cookie" = [], "csrf_token" = [])),
            responses((status = 200, body = TeachingSession, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
        pub async fn $fn(State(st): State<AcademicState>, auth: Authenticated, Path(id): Path<Uuid>, headers: HeaderMap, body: Option<Json<SessionProgress>>) -> ApiResult<Response> {
            let v = expect_version!(headers);
            let s = repo::session_command(&st, &auth.0, id, v, $cmd, body.map(|Json(b)| b).unwrap_or_default(), &actor(&auth.0)).await?;
            Ok(ok(&s, s.version))
        }
    };
}

session_cmd!(
    /// Start a scheduled session (faculty on their section, or staff).
    session_start, "/api/v1/academic/sessions/{id}/start", "teaching_session_start", SessionCommand::Start);
session_cmd!(
    /// Pause (interrupt) a running session.
    session_pause, "/api/v1/academic/sessions/{id}/pause", "teaching_session_pause", SessionCommand::Pause);
session_cmd!(
    /// Resume a paused session.
    session_resume, "/api/v1/academic/sessions/{id}/resume", "teaching_session_resume", SessionCommand::Resume);
session_cmd!(
    /// Complete a running session (optional topic / notes of instructional progress).
    session_complete, "/api/v1/academic/sessions/{id}/complete", "teaching_session_complete", SessionCommand::Complete);
session_cmd!(
    /// Cancel a scheduled session (reason required).
    session_cancel, "/api/v1/academic/sessions/{id}/cancel", "teaching_session_cancel", SessionCommand::Cancel);
session_cmd!(
    /// Abandon a started session — exceptional outcome, administrators only (reason required).
    session_abandon, "/api/v1/academic/sessions/{id}/abandon", "teaching_session_abandon", SessionCommand::Abandon);
session_cmd!(
    /// Lock a session with a terminal outcome (completed sessions need finalised attendance).
    session_lock, "/api/v1/academic/sessions/{id}/lock", "teaching_session_lock", SessionCommand::Lock);

/// Attendance lines of a session.
#[utoipa::path(get, path = "/api/v1/academic/sessions/{id}/attendance", operation_id = "attendance_list", tag = "attendance",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = TeachingSession, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn attendance_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    session_get(State(st), auth, Path(id)).await
}

/// Record marks for seated students (one canonical record per student; re-marking updates it).
#[utoipa::path(post, path = "/api/v1/academic/sessions/{id}/attendance", operation_id = "attendance_capture", tag = "attendance",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the session")),
    request_body = CaptureAttendance, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TeachingSession, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn attendance_capture(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<CaptureAttendance>,
) -> ApiResult<Response> {
    let v = expect_version!(headers);
    let s = repo::capture(&st, &auth.0, id, v, cmd, &actor(&auth.0)).await?;
    Ok(ok(&s, s.version))
}

/// Submit the complete attendance register for finalisation.
#[utoipa::path(post, path = "/api/v1/academic/sessions/{id}/submit-attendance", operation_id = "attendance_submit", tag = "attendance",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the session")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TeachingSession, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn attendance_submit(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let v = expect_version!(headers);
    let s = repo::submit_attendance(&st, &auth.0, id, v, &actor(&auth.0)).await?;
    Ok(ok(&s, s.version))
}

/// Finalise the attendance (immutable afterwards; evaluates thresholds → `attendance.threshold_breached`).
#[utoipa::path(post, path = "/api/v1/academic/sessions/{id}/finalise-attendance", operation_id = "attendance_finalise", tag = "attendance",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the session")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TeachingSession, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn attendance_finalise(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let v = expect_version!(headers);
    let s = repo::finalise_attendance(&st, &auth.0, id, v, &actor(&auth.0)).await?;
    Ok(ok(&s, s.version))
}

/// Corrections requested for a session.
#[utoipa::path(get, path = "/api/v1/academic/sessions/{id}/attendance-corrections", operation_id = "attendance_correction_list", tag = "attendance",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = AttendanceCorrectionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn correction_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<AttendanceCorrectionList>> {
    auth.require(Permission::AcademicRead)?;
    Ok(Json(AttendanceCorrectionList {
        items: repo::list_corrections(&st, auth.0.scope.tenant_id, id).await?,
    }))
}

/// Request a governed correction of a finalised record (`ATTENDANCE_CORRECTION`).
#[utoipa::path(post, path = "/api/v1/academic/sessions/{id}/attendance-corrections", operation_id = "attendance_correction_request", tag = "attendance",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewAttendanceCorrection, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = AttendanceCorrection, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn correction_request(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewAttendanceCorrection>,
) -> ApiResult<Response> {
    require_idempotency_key(&headers)?;
    let c = repo::request_correction(&st, &auth.0, id, cmd, &actor(&auth.0)).await?;
    Ok(created(&c, c.version))
}

/// Attendance corrections of an institution: the HOD / administrator decision queue
/// (`attendance.correct`).
#[utoipa::path(get, path = "/api/v1/academic/institutions/{id}/attendance-corrections", operation_id = "attendance_correction_queue", tag = "attendance",
    params(("id" = String, Path, format = Uuid), CorrectionFilter), security(("session_cookie" = [])),
    responses((status = 200, body = AttendanceCorrectionQueue), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn correction_queue(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(f): Query<CorrectionFilter>,
) -> ApiResult<Json<AttendanceCorrectionQueue>> {
    // Reading the queue is not a decision: no step-up here, the decision itself requires it.
    if !auth.0.has_permission(Permission::AttendanceCorrect) {
        return Err(sutra_kernel::error::KernelError::Forbidden(
            "deciding attendance corrections needs attendance.correct".into(),
        )
        .into());
    }
    Ok(Json(AttendanceCorrectionQueue {
        items: repo::list_institution_corrections(
            &st,
            auth.0.scope.tenant_id,
            id,
            f.status.map(|s| s.as_str()),
        )
        .await?,
    }))
}

/// An attendance correction.
#[utoipa::path(get, path = "/api/v1/academic/attendance-corrections/{id}", operation_id = "attendance_correction_get", tag = "attendance",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = AttendanceCorrection, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn correction_get(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    auth.require(Permission::AcademicRead)?;
    let c = repo::get_correction(&st, auth.0.scope.tenant_id, id).await?;
    Ok(ok(&c, c.version))
}

/// Decide a pending correction (approve → corrected record + history; reject → finalised fact stays).
#[utoipa::path(post, path = "/api/v1/academic/attendance-corrections/{id}/decision", operation_id = "attendance_correction_decision", tag = "attendance",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the correction")),
    request_body = DeliveryDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AttendanceCorrection, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn correction_decision(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<DeliveryDecision>,
) -> ApiResult<Response> {
    let v = expect_version!(headers);
    let c = repo::decide_correction(
        &st,
        &auth.0,
        id,
        v,
        cmd.decision == DeliveryDecisionValue::Approve,
        kit::opt_text(cmd.reason.as_deref()),
        &actor(&auth.0),
    )
    .await?;
    Ok(ok(&c, c.version))
}

/// Withdraw a pending correction.
#[utoipa::path(post, path = "/api/v1/academic/attendance-corrections/{id}/withdraw", operation_id = "attendance_correction_withdraw", tag = "attendance",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the correction")),
    request_body(content = Option<DeliveryReason>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AttendanceCorrection, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn correction_withdraw(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<DeliveryReason>>,
) -> ApiResult<Response> {
    let v = expect_version!(headers);
    let c =
        repo::withdraw_correction(&st, &auth.0, id, v, kit::reason(body), &actor(&auth.0)).await?;
    Ok(ok(&c, c.version))
}

/// Attendance policy versions of an institution.
#[utoipa::path(get, path = "/api/v1/academic/institutions/{id}/attendance-policies", operation_id = "attendance_policy_list", tag = "attendance",
    params(("id" = String, Path, format = Uuid), PolicyFilter), security(("session_cookie" = [])),
    responses((status = 200, body = AttendancePolicyList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(f): Query<PolicyFilter>,
) -> ApiResult<Json<AttendancePolicyList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    Ok(Json(AttendancePolicyList {
        items: repo::list_policies(&st, ctx.scope.tenant_id, id, f.status.map(|s| s.as_str()))
            .await?,
    }))
}

/// Draft an attendance policy (threshold rules).
#[utoipa::path(post, path = "/api/v1/academic/institutions/{id}/attendance-policies", operation_id = "attendance_policy_create", tag = "attendance",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewAttendancePolicy, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = AttendancePolicy, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_create(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewAttendancePolicy>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AttendanceManage)?;
    require_idempotency_key(&headers)?;
    let p = repo::create_policy(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&p, p.version))
}

/// An attendance policy version.
#[utoipa::path(get, path = "/api/v1/academic/attendance-policies/{id}", operation_id = "attendance_policy_get", tag = "attendance",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = AttendancePolicy, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_get(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let p = repo::get_policy(&st, ctx.scope.tenant_id, id).await?;
    Ok(ok(&p, p.version))
}

/// Submit a draft policy for approval (`ATTENDANCE_POLICY`; approval supersedes the effective one).
#[utoipa::path(post, path = "/api/v1/academic/attendance-policies/{id}/submit", operation_id = "attendance_policy_submit", tag = "attendance",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the policy")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AttendancePolicy, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_submit(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AttendanceManage)?;
    let v = expect_version!(headers);
    let p = repo::submit_policy(&st, ctx, id, v, &actor(ctx)).await?;
    Ok(ok(&p, p.version))
}

/// Approval decision on a policy in review.
#[utoipa::path(post, path = "/api/v1/academic/attendance-policies/{id}/decision", operation_id = "attendance_policy_decision", tag = "attendance",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the policy")),
    request_body = DeliveryDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AttendancePolicy, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_decision(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<DeliveryDecision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AttendanceManage)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "attendance policy").await?;
    let v = expect_version!(headers);
    let p = repo::decide_policy(
        &st,
        ctx,
        id,
        v,
        cmd.decision == DeliveryDecisionValue::Approve,
        kit::opt_text(cmd.reason.as_deref()),
        &actor(ctx),
    )
    .await?;
    Ok(ok(&p, p.version))
}

/// Retire a policy version.
#[utoipa::path(post, path = "/api/v1/academic/attendance-policies/{id}/retire", operation_id = "attendance_policy_retire", tag = "attendance",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the policy")),
    request_body(content = Option<DeliveryReason>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AttendancePolicy, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_retire(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<DeliveryReason>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AttendanceManage)?;
    let v = expect_version!(headers);
    let p = repo::retire_policy(&st, ctx, id, v, kit::reason(body), &actor(ctx)).await?;
    Ok(ok(&p, p.version))
}

/// Per-student attendance of a section under the effective policy.
#[utoipa::path(get, path = "/api/v1/academic/sections/{id}/attendance-summary", operation_id = "attendance_summary", tag = "attendance",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = AttendanceSummary), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn attendance_summary(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<AttendanceSummary>> {
    read_section(&st, &auth, id).await?;
    Ok(Json(repo::summary(&st, &auth.0, id).await?))
}

/// Re-evaluate thresholds of a section now (raises `attendance.threshold_breached` for new breaches).
#[utoipa::path(post, path = "/api/v1/academic/sections/{id}/attendance-summary/evaluate", operation_id = "attendance_evaluate", tag = "attendance",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AttendanceSummary), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn attendance_evaluate(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<AttendanceSummary>> {
    Ok(Json(
        repo::evaluate(&st, &auth.0, id, &actor(&auth.0)).await?,
    ))
}

/// The signed-in student's attendance per seated section.
#[utoipa::path(get, path = "/api/v1/academic/me/attendance", operation_id = "attendance_self_student", tag = "attendance",
    security(("session_cookie" = [])),
    responses((status = 200, body = MyAttendance), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn me_attendance(
    State(st): State<AcademicState>,
    auth: Authenticated,
) -> ApiResult<Json<MyAttendance>> {
    Ok(Json(repo::my_attendance(&st, &auth.0).await?))
}

/// The signed-in faculty member's sections with their recent sessions.
#[utoipa::path(get, path = "/api/v1/academic/my/attendance", operation_id = "attendance_self_faculty", tag = "attendance",
    security(("session_cookie" = [])),
    responses((status = 200, body = FacultyAttendance), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_attendance(
    State(st): State<AcademicState>,
    auth: Authenticated,
) -> ApiResult<Json<FacultyAttendance>> {
    Ok(Json(repo::faculty_attendance(&st, &auth.0).await?))
}

/// Syllabus coverage records of a section (newest first).
#[utoipa::path(get, path = "/api/v1/academic/sections/{id}/syllabus-coverage", operation_id = "syllabus_coverage_list", tag = "attendance",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = SyllabusCoverageList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn coverage_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<SyllabusCoverageList>> {
    read_section(&st, &auth, id).await?;
    Ok(Json(SyllabusCoverageList {
        items: syllabus::list(&st, auth.0.scope.tenant_id, id).await?,
    }))
}

/// Start a coverage record (topics given, or copied from the latest published record).
#[utoipa::path(post, path = "/api/v1/academic/sections/{id}/syllabus-coverage", operation_id = "syllabus_coverage_create", tag = "attendance",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body(content = Option<NewSyllabusCoverage>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = SyllabusCoverage, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn coverage_create(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<NewSyllabusCoverage>>,
) -> ApiResult<Response> {
    require_idempotency_key(&headers)?;
    let c = syllabus::create(
        &st,
        &auth.0,
        id,
        body.map(|Json(b)| b).unwrap_or_default(),
        &actor(&auth.0),
    )
    .await?;
    Ok(created(&c, c.version))
}

/// A coverage record with its topics.
#[utoipa::path(get, path = "/api/v1/academic/syllabus-coverage/{id}", operation_id = "syllabus_coverage_get", tag = "attendance",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = SyllabusCoverage, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn coverage_get(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    auth.require(Permission::AcademicRead)?;
    let c = syllabus::get(&st, auth.0.scope.tenant_id, id).await?;
    Ok(ok(&c, c.version))
}

/// Replace the topics of a draft/submitted record (a submitted record returns to draft).
#[utoipa::path(put, path = "/api/v1/academic/syllabus-coverage/{id}/topics", operation_id = "syllabus_coverage_topics", tag = "attendance",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the record")),
    request_body = ReplaceTopics, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SyllabusCoverage, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn coverage_topics(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<ReplaceTopics>,
) -> ApiResult<Response> {
    let v = expect_version!(headers);
    let c = syllabus::replace_topics(&st, &auth.0, id, v, cmd, &actor(&auth.0)).await?;
    Ok(ok(&c, c.version))
}

/// Submit a coverage record for publication.
#[utoipa::path(post, path = "/api/v1/academic/syllabus-coverage/{id}/submit", operation_id = "syllabus_coverage_submit", tag = "attendance",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the record")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SyllabusCoverage, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn coverage_submit(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let v = expect_version!(headers);
    let c = syllabus::submit(&st, &auth.0, id, v, &actor(&auth.0)).await?;
    Ok(ok(&c, c.version))
}

/// Publish a submitted coverage record (`attendance.manage`; immutable afterwards).
#[utoipa::path(post, path = "/api/v1/academic/syllabus-coverage/{id}/publish", operation_id = "syllabus_coverage_publish", tag = "attendance",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the record")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SyllabusCoverage, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn coverage_publish(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let v = expect_version!(headers);
    let c = syllabus::publish(&st, &auth.0, id, v, &actor(&auth.0)).await?;
    Ok(ok(&c, c.version))
}

/// All attendance routes.
pub fn router(state: AcademicState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(session_list, session_create))
        .routes(routes!(session_get))
        .routes(routes!(session_start))
        .routes(routes!(session_pause))
        .routes(routes!(session_resume))
        .routes(routes!(session_complete))
        .routes(routes!(session_cancel))
        .routes(routes!(session_abandon))
        .routes(routes!(session_lock))
        .routes(routes!(attendance_list, attendance_capture))
        .routes(routes!(attendance_submit))
        .routes(routes!(attendance_finalise))
        .routes(routes!(correction_list, correction_request))
        .routes(routes!(correction_queue))
        .routes(routes!(correction_get))
        .routes(routes!(correction_decision))
        .routes(routes!(correction_withdraw))
        .routes(routes!(policy_list, policy_create))
        .routes(routes!(policy_get))
        .routes(routes!(policy_submit))
        .routes(routes!(policy_decision))
        .routes(routes!(policy_retire))
        .routes(routes!(attendance_summary))
        .routes(routes!(attendance_evaluate))
        .routes(routes!(me_attendance))
        .routes(routes!(my_attendance))
        .routes(routes!(coverage_list, coverage_create))
        .routes(routes!(coverage_get))
        .routes(routes!(coverage_topics))
        .routes(routes!(coverage_submit))
        .routes(routes!(coverage_publish))
        .with_state(state)
}
