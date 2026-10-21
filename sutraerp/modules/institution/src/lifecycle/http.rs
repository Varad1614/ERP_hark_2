//! HTTP handlers of the institution lifecycle, profile and timezone (tag `institution`).

use super::common::{expect_version, CommandErrors, ReadErrors};
use super::domain::{
    ActivationRequest, ChangeTimezone, CommandBody, DecideActivation, ProfileRevision,
    ReviseProfile,
};
use super::repo::{self, LifecycleCommand};
use crate::domain::Institution;
use crate::http::invalidate;
use crate::shared::http::{actor, with_etag};
use crate::shared::scope::institution_scope;
use crate::shared::InstitutionState;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use sutra_kernel::auth::Permission;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::InstitutionId;
use sutra_web::{ApiResult, Authenticated};

fn ok(r: Result<Institution, KernelError>) -> ApiResult<Response> {
    let inst = r?;
    Ok(with_etag(StatusCode::OK, &inst, inst.version))
}

/// Activation request cycles of an institution.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct ActivationRequestList {
    pub items: Vec<ActivationRequest>,
}

/// Applied profile revisions of an institution.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct ProfileRevisionList {
    pub items: Vec<ProfileRevision>,
}

/// Mark the institution configured (`draft | returned | rejected → configured`).
///
/// Validates the mandatory master information (L2-B01 §7) and the timezone; 422 lists the
/// missing members. Requires `institution.manage`.
#[utoipa::path(
    post,
    path = "/api/v1/institution/institutions/{id}/configure",
    operation_id = "institution_configure",
    tag = "institution",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = Institution, headers(("ETag" = String))), CommandErrors)
)]
pub async fn configure(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionManage)?;
    let expected = expect_version!(headers);
    let scope = institution_scope(ctx, id);
    let r = repo::configure(&st.pool, &scope, id, expected, &actor(ctx)).await;
    invalidate(&st, &scope, id).await;
    ok(r)
}

/// Submit the activation request (`configured → pending_approval`).
///
/// Refused with 409 while a required activation-readiness item is unmet (the detail lists the
/// blocking keys). The approval step runs through the WorkflowPort; the institution stays
/// `pending_approval` until the Institution Activation Authority decides (`/decision`,
/// `institution.activation.decide` + MFA step-up). Submitting never activates. Requires
/// `institution.manage`.
#[utoipa::path(
    post,
    path = "/api/v1/institution/institutions/{id}/submit",
    operation_id = "institution_submit",
    tag = "institution",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = Institution, headers(("ETag" = String))), CommandErrors)
)]
pub async fn submit(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionManage)?;
    let expected = expect_version!(headers);
    let scope = institution_scope(ctx, id);
    let r = repo::submit(&st, &scope, id, expected, &actor(ctx)).await;
    invalidate(&st, &scope, id).await;
    ok(r)
}

/// Decide the pending activation request: `approve` (→ active, re-validated against the current
/// readiness), `return` (→ returned) or `reject` (→ rejected); `return`/`reject` need a reason.
///
/// Institution Activation Authority: `institution.activation.decide` (MFA step-up).
#[utoipa::path(
    post,
    path = "/api/v1/institution/institutions/{id}/decision",
    operation_id = "institution_decide_activation",
    tag = "institution",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = DecideActivation,
    responses((status = 200, body = Institution, headers(("ETag" = String))), CommandErrors)
)]
pub async fn decide(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    Json(cmd): Json<DecideActivation>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionActivationDecide)?;
    let expected = expect_version!(headers);
    let scope = institution_scope(ctx, id);
    let r = repo::decide(&st.pool, &scope, id, expected, &cmd, &actor(ctx)).await;
    invalidate(&st, &scope, id).await;
    ok(r)
}

async fn lifecycle(
    st: InstitutionState,
    auth: Authenticated,
    id: InstitutionId,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
    command: LifecycleCommand,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionLifecycle)?;
    let expected = expect_version!(headers);
    let reason = body.map(|b| b.0).unwrap_or_default().required_reason()?;
    let scope = institution_scope(ctx, id);
    let r =
        repo::lifecycle_command(&st.pool, &scope, id, expected, command, reason, &actor(ctx)).await;
    invalidate(&st, &scope, id).await;
    ok(r)
}

/// Suspend an active institution (`active → suspended`); the reason is mandatory.
///
/// While suspended the institution and all its downstream structure are read-only.
/// Requires `institution.lifecycle` (MFA step-up).
#[utoipa::path(
    post,
    path = "/api/v1/institution/institutions/{id}/suspend",
    operation_id = "institution_suspend",
    tag = "institution",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = CommandBody,
    responses((status = 200, body = Institution, headers(("ETag" = String))), CommandErrors)
)]
pub async fn suspend(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
) -> ApiResult<Response> {
    lifecycle(st, auth, id, headers, body, LifecycleCommand::Suspend).await
}

/// Restore a suspended institution (`suspended → active`); the resolution reason is mandatory.
///
/// Requires `institution.lifecycle` (MFA step-up).
#[utoipa::path(
    post,
    path = "/api/v1/institution/institutions/{id}/reactivate",
    operation_id = "institution_reactivate",
    tag = "institution",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = CommandBody,
    responses((status = 200, body = Institution, headers(("ETag" = String))), CommandErrors)
)]
pub async fn reactivate(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
) -> ApiResult<Response> {
    lifecycle(st, auth, id, headers, body, LifecycleCommand::Reactivate).await
}

/// Archive an institution (`active | suspended → archived`, terminal and read-only).
///
/// Identity, history and policy versions are preserved; nothing cascades. Requires
/// `institution.lifecycle` (MFA step-up).
#[utoipa::path(
    post,
    path = "/api/v1/institution/institutions/{id}/archive",
    operation_id = "institution_archive",
    tag = "institution",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = CommandBody,
    responses((status = 200, body = Institution, headers(("ETag" = String))), CommandErrors)
)]
pub async fn archive(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
) -> ApiResult<Response> {
    lifecycle(st, auth, id, headers, body, LifecycleCommand::Archive).await
}

/// Revise the institution profile / master information (L3-B-004).
///
/// Absent members stay unchanged; an empty string or list clears a member. Previous values are
/// kept in the revision history. Not allowed while pending approval, suspended or archived.
/// Requires `institution.manage`.
#[utoipa::path(
    put,
    path = "/api/v1/institution/institutions/{id}/profile",
    operation_id = "institution_revise_profile",
    tag = "institution",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = ReviseProfile,
    responses((status = 200, body = Institution, headers(("ETag" = String))), CommandErrors)
)]
pub async fn revise_profile(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    Json(cmd): Json<ReviseProfile>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionManage)?;
    let expected = expect_version!(headers);
    let scope = institution_scope(ctx, id);
    let r = repo::revise_profile(&st.pool, &scope, id, expected, &cmd, &actor(ctx)).await;
    invalidate(&st, &scope, id).await;
    ok(r)
}

/// Applied profile revisions, newest first (`institution.read`; `403` for student-only
/// sessions — revisions carry the full master information, Audit C3).
#[utoipa::path(
    get,
    path = "/api/v1/institution/institutions/{id}/profile/revisions",
    operation_id = "institution_list_profile_revisions",
    tag = "institution",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = ProfileRevisionList), ReadErrors)
)]
pub async fn list_profile_revisions(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
) -> ApiResult<Json<ProfileRevisionList>> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    if ctx.is_student_only() {
        return Err(KernelError::Forbidden(
            "profile revisions are not available to student sessions".into(),
        )
        .into());
    }
    let scope = institution_scope(ctx, id);
    let items = repo::list_profile_revisions(&st.pool, &scope, id).await?;
    Ok(Json(ProfileRevisionList { items }))
}

/// Change the business timezone (validated IANA zone). Every business-local date of the
/// institution (effective dates of policies, governance, positions) is read in this zone.
///
/// Requires `institution.manage`.
#[utoipa::path(
    put,
    path = "/api/v1/institution/institutions/{id}/timezone",
    operation_id = "institution_change_timezone",
    tag = "institution",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = ChangeTimezone,
    responses((status = 200, body = Institution, headers(("ETag" = String))), CommandErrors)
)]
pub async fn change_timezone(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    Json(cmd): Json<ChangeTimezone>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionManage)?;
    let expected = expect_version!(headers);
    let scope = institution_scope(ctx, id);
    let r = repo::change_timezone(&st.pool, &scope, id, expected, &cmd.timezone, &actor(ctx)).await;
    invalidate(&st, &scope, id).await;
    ok(r)
}

/// Activation request cycles of an institution, newest first (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/institutions/{id}/activation-requests",
    operation_id = "institution_list_activation_requests",
    tag = "institution",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = ActivationRequestList), ReadErrors)
)]
pub async fn list_activation_requests(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
) -> ApiResult<Json<ActivationRequestList>> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let scope = institution_scope(ctx, id);
    let items = repo::list_activation_requests(&st.pool, &scope, id).await?;
    Ok(Json(ActivationRequestList { items }))
}
