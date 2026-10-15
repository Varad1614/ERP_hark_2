//! HTTP API of the student → alumni transition (tag `alumni`; contract doc §5).

use super::domain::*;
use super::repo;
use crate::completion::kit::{
    self, created, expect_version, ok, LifecycleDecision, LifecycleReason,
};
use crate::shared::http::actor;
use crate::shared::StudentState;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::Json;
use sutra_contracts::student::ids::{AlumniTransitionId, StudentId};
use sutra_contracts::student::status::AlumniTransitionStatus;
use sutra_kernel::auth::Permission;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AlumniFilter {
    pub status: Option<AlumniTransitionStatus>,
}

macro_rules! cmd {
    ($(#[$m:meta])* $fn:ident, $path:literal, $op:literal, $call:ident) => {
        $(#[$m])*
        #[utoipa::path(post, path = $path, operation_id = $op, tag = "alumni",
            params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the transition")),
            security(("session_cookie" = [], "csrf_token" = [])),
            responses((status = 200, body = AlumniTransition, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
        pub async fn $fn(State(st): State<StudentState>, auth: Authenticated, Path(id): Path<AlumniTransitionId>, headers: HeaderMap) -> ApiResult<Response> {
            let ctx = auth.require(Permission::AlumniManage)?;
            let v = expect_version!(headers);
            let t = repo::$call(&st, ctx, id, v, &actor(ctx)).await?;
            Ok(ok(&t, t.version))
        }
    };
}

/// Alumni transitions of a student.
#[utoipa::path(get, path = "/api/v1/student/students/{id}/alumni-transitions", operation_id = "alumni_transition_list", tag = "alumni",
    params(("id" = String, Path, format = Uuid), AlumniFilter), security(("session_cookie" = [])),
    responses((status = 200, body = AlumniTransitionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn transition_list(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    Query(f): Query<AlumniFilter>,
) -> ApiResult<Json<AlumniTransitionList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    crate::shared::student::load_student(&st.pool, ctx.scope.tenant_id, id).await?;
    Ok(Json(AlumniTransitionList {
        items: repo::list(&st, ctx.scope.tenant_id, id, f.status).await?,
    }))
}

/// Initiate the transition for a completed or eligible-exit student context.
#[utoipa::path(post, path = "/api/v1/student/students/{id}/alumni-transitions", operation_id = "alumni_transition_initiate", tag = "alumni",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body(content = Option<NewAlumniTransition>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = AlumniTransition, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn transition_initiate(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    headers: HeaderMap,
    body: Option<Json<NewAlumniTransition>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AlumniManage)?;
    require_idempotency_key(&headers)?;
    let t = repo::initiate(
        &st,
        ctx,
        id,
        body.map(|Json(b)| b).unwrap_or_default(),
        &actor(ctx),
    )
    .await?;
    Ok(created(&t, t.version))
}

/// Get an alumni transition (with its correction history).
#[utoipa::path(get, path = "/api/v1/student/alumni-transitions/{id}", operation_id = "alumni_transition_get", tag = "alumni",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = AlumniTransition, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn transition_get(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AlumniTransitionId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentRead)?;
    let t = repo::get(&st, ctx.scope.tenant_id, id).await?;
    Ok(ok(&t, t.version))
}

cmd!(
    /// Validate transition eligibility (completion / exit facts); failures are recorded, the state is kept.
    transition_validate_eligibility, "/api/v1/student/alumni-transitions/{id}/validate-eligibility", "alumni_transition_validate_eligibility", validate_eligibility);
cmd!(
    /// Execute the approved transition (student relationship closed, alumni relationship activated).
    transition_execute, "/api/v1/student/alumni-transitions/{id}/execute", "alumni_transition_execute", execute);
cmd!(
    /// Complete the transition (hand-off to Alumni engagement; persona archived when no live context remains).
    transition_complete, "/api/v1/student/alumni-transitions/{id}/complete", "alumni_transition_complete", complete);

/// Validate the institutional transition policy and request approval (`ALUMNI_TRANSITION`).
#[utoipa::path(post, path = "/api/v1/student/alumni-transitions/{id}/validate-policy", operation_id = "alumni_transition_validate_policy", tag = "alumni",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the transition")),
    request_body = ValidateAlumniPolicy, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AlumniTransition, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn transition_validate_policy(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AlumniTransitionId>,
    headers: HeaderMap,
    Json(cmd): Json<ValidateAlumniPolicy>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AlumniManage)?;
    let v = expect_version!(headers);
    let t = repo::validate_policy(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&t, t.version))
}

/// Approval decision (`AwaitDecision`; reject → rejected).
#[utoipa::path(post, path = "/api/v1/student/alumni-transitions/{id}/decision", operation_id = "alumni_transition_decision", tag = "alumni",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the transition")),
    request_body = LifecycleDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AlumniTransition, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn transition_decision(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AlumniTransitionId>,
    headers: HeaderMap,
    Json(cmd): Json<LifecycleDecision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AlumniManage)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "alumni transition").await?;
    let v = expect_version!(headers);
    let t = repo::decide(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&t, t.version))
}

/// Governed correction of an executed transition (L3-C-073).
#[utoipa::path(post, path = "/api/v1/student/alumni-transitions/{id}/corrections", operation_id = "alumni_transition_correct", tag = "alumni",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the transition")),
    request_body = CorrectAlumniTransition, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AlumniTransition, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn transition_correct(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AlumniTransitionId>,
    headers: HeaderMap,
    Json(cmd): Json<CorrectAlumniTransition>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AlumniManage)?;
    let v = expect_version!(headers);
    let t = repo::correct(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&t, t.version))
}

/// Archive a completed transition (L3-C-074).
#[utoipa::path(post, path = "/api/v1/student/alumni-transitions/{id}/archive", operation_id = "alumni_transition_archive", tag = "alumni",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the transition")),
    request_body(content = Option<LifecycleReason>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AlumniTransition, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn transition_archive(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AlumniTransitionId>,
    headers: HeaderMap,
    body: Option<Json<LifecycleReason>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AlumniManage)?;
    let v = expect_version!(headers);
    let t = repo::archive(&st, ctx, id, v, kit::reason(body), &actor(ctx)).await?;
    Ok(ok(&t, t.version))
}

/// All alumni routes.
pub fn router(state: StudentState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(transition_list, transition_initiate))
        .routes(routes!(transition_get))
        .routes(routes!(transition_validate_eligibility))
        .routes(routes!(transition_validate_policy))
        .routes(routes!(transition_decision))
        .routes(routes!(transition_execute))
        .routes(routes!(transition_complete))
        .routes(routes!(transition_correct))
        .routes(routes!(transition_archive))
        .with_state(state)
}
