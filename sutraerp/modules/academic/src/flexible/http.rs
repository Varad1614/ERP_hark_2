//! HTTP API of flexible and external learning (tag `flexible-learning`; contract §5.2).
//! Staff reads need `academic.read`; every write needs `flexible_learning.manage`; students
//! act on their own requests through `academic.self` under `/me/flexible-learning`.

use super::enrolment::{self, *};
use super::exception::{self, *};
use super::kit::{
    self, created, expect_version, ok, FlexibleDecision, FlexibleDecisionValue, FlexibleReason,
};
use super::opportunity::{self, *};
use super::policy::{self, *};
use super::provider::{self, *};
use crate::shared::http::actor;
use crate::shared::{me, AcademicState};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::Json;
use sutra_contracts::academic::status::{LearningOpportunityStatus, LearningProviderStatus};
use sutra_kernel::auth::Permission;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct StatusFilter {
    pub status: Option<String>,
}

fn decision(cmd: &FlexibleDecision) -> (bool, Option<String>) {
    (
        cmd.decision == FlexibleDecisionValue::Approve,
        kit::opt_text(cmd.reason.as_deref()),
    )
}

// ============================================================== providers (L3-D-044)

/// Learning providers of an institution.
#[utoipa::path(get, path = "/api/v1/academic/institutions/{id}/learning-providers", operation_id = "learning_provider_list", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), StatusFilter), security(("session_cookie" = [])),
    responses((status = 200, body = LearningProviderList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn provider_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(f): Query<StatusFilter>,
) -> ApiResult<Json<LearningProviderList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    Ok(Json(LearningProviderList {
        items: provider::list(&st, ctx.scope.tenant_id, id, f.status.as_deref()).await?,
    }))
}

/// Propose a learning provider (onboarding starts in `proposed`).
#[utoipa::path(post, path = "/api/v1/academic/institutions/{id}/learning-providers", operation_id = "learning_provider_create", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewLearningProvider, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = LearningProvider, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn provider_create(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewLearningProvider>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    require_idempotency_key(&headers)?;
    let p = provider::create(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&p, p.version))
}

/// A learning provider with its status history.
#[utoipa::path(get, path = "/api/v1/academic/learning-providers/{id}", operation_id = "learning_provider_get", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = LearningProvider, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn provider_get(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let p = provider::get(&st, ctx.scope.tenant_id, id).await?;
    Ok(ok(&p, p.version))
}

/// Submit the provider for onboarding review (`LEARNING_PROVIDER_ONBOARDING`; auto-approval activates).
#[utoipa::path(post, path = "/api/v1/academic/learning-providers/{id}/submit", operation_id = "learning_provider_submit", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the provider")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LearningProvider, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn provider_submit(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    let v = expect_version!(headers);
    let p = provider::submit(&st, ctx, id, v, &actor(ctx)).await?;
    Ok(ok(&p, p.version))
}

/// Decide the pending onboarding (approve → active, reject → proposed).
#[utoipa::path(post, path = "/api/v1/academic/learning-providers/{id}/decision", operation_id = "learning_provider_decision", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the provider")),
    request_body = FlexibleDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LearningProvider, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn provider_decision(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<FlexibleDecision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    let v = expect_version!(headers);
    let (approve, reason) = decision(&cmd);
    let p = provider::decide(&st, ctx, id, v, approve, reason, &actor(ctx)).await?;
    Ok(ok(&p, p.version))
}

macro_rules! provider_cmd {
    ($(#[$m:meta])* $fn:ident, $path:literal, $op:literal, $to:expr) => {
        $(#[$m])*
        #[utoipa::path(post, path = $path, operation_id = $op, tag = "flexible-learning",
            params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the provider")),
            request_body(content = Option<FlexibleReason>), security(("session_cookie" = [], "csrf_token" = [])),
            responses((status = 200, body = LearningProvider, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
        pub async fn $fn(State(st): State<AcademicState>, auth: Authenticated, Path(id): Path<Uuid>, headers: HeaderMap, body: Option<Json<FlexibleReason>>) -> ApiResult<Response> {
            let ctx = auth.require(Permission::FlexibleLearningManage)?;
            let v = expect_version!(headers);
            let p = provider::command(&st, ctx, id, v, $to, kit::reason(body), &actor(ctx)).await?;
            Ok(ok(&p, p.version))
        }
    };
}

provider_cmd!(
    /// Suspend an active provider (reason required; no new enrolments).
    provider_suspend, "/api/v1/academic/learning-providers/{id}/suspend", "learning_provider_suspend", LearningProviderStatus::Suspended);
provider_cmd!(
    /// Reactivate a suspended provider (accreditation re-checked).
    provider_reactivate, "/api/v1/academic/learning-providers/{id}/reactivate", "learning_provider_reactivate", LearningProviderStatus::Active);
provider_cmd!(
    /// Offboard the provider (reason required; blocked while live opportunities use it).
    provider_offboard, "/api/v1/academic/learning-providers/{id}/offboard", "learning_provider_offboard", LearningProviderStatus::Offboarded);

// ============================================================== opportunities (L3-D-040…042, 047)

/// Learning opportunities of an institution (`?status=`, `?academicTermId=`).
#[utoipa::path(get, path = "/api/v1/academic/institutions/{id}/learning-opportunities", operation_id = "learning_opportunity_list", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), OpportunityFilter), security(("session_cookie" = [])),
    responses((status = 200, body = LearningOpportunityList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn opportunity_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(f): Query<OpportunityFilter>,
) -> ApiResult<Json<LearningOpportunityList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    Ok(Json(LearningOpportunityList {
        items: opportunity::list(&st, ctx.scope.tenant_id, id, &f).await?,
    }))
}

/// Establish a learning opportunity (draft) against authoritative references.
#[utoipa::path(post, path = "/api/v1/academic/institutions/{id}/learning-opportunities", operation_id = "learning_opportunity_create", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewLearningOpportunity, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = LearningOpportunity, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn opportunity_create(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewLearningOpportunity>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    require_idempotency_key(&headers)?;
    let o = opportunity::create(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&o, o.version))
}

/// Archive the D06 records of an academic session (term) after its closure (L3-D-047).
#[utoipa::path(post, path = "/api/v1/academic/institutions/{id}/learning-opportunities/archive-session", operation_id = "learning_opportunity_session_archive", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid)), request_body = SessionArchival, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SessionArchivalResult), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn opportunity_session_archive(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(cmd): Json<SessionArchival>,
) -> ApiResult<Json<SessionArchivalResult>> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    Ok(Json(
        opportunity::archive_session(&st, ctx, id, cmd, &actor(ctx)).await?,
    ))
}

/// A learning opportunity with seat counts and status history.
#[utoipa::path(get, path = "/api/v1/academic/learning-opportunities/{id}", operation_id = "learning_opportunity_get", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = LearningOpportunity, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn opportunity_get(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let o = opportunity::get(&st, ctx.scope.tenant_id, id).await?;
    Ok(ok(&o, o.version))
}

/// Submit for activation (`LEARNING_OPPORTUNITY_APPROVAL`; auto-approval activates).
#[utoipa::path(post, path = "/api/v1/academic/learning-opportunities/{id}/submit", operation_id = "learning_opportunity_submit", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the opportunity")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LearningOpportunity, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn opportunity_submit(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    let v = expect_version!(headers);
    let o = opportunity::submit(&st, ctx, id, v, &actor(ctx)).await?;
    Ok(ok(&o, o.version))
}

/// Decide the pending activation (approve → active, reject → draft).
#[utoipa::path(post, path = "/api/v1/academic/learning-opportunities/{id}/decision", operation_id = "learning_opportunity_decision", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the opportunity")),
    request_body = FlexibleDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LearningOpportunity, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn opportunity_decision(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<FlexibleDecision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    let v = expect_version!(headers);
    let (approve, reason) = decision(&cmd);
    let o = opportunity::decide(&st, ctx, id, v, approve, reason, &actor(ctx)).await?;
    Ok(ok(&o, o.version))
}

macro_rules! opportunity_cmd {
    ($(#[$m:meta])* $fn:ident, $path:literal, $op:literal, $to:expr) => {
        $(#[$m])*
        #[utoipa::path(post, path = $path, operation_id = $op, tag = "flexible-learning",
            params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the opportunity")),
            request_body(content = Option<FlexibleReason>), security(("session_cookie" = [], "csrf_token" = [])),
            responses((status = 200, body = LearningOpportunity, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
        pub async fn $fn(State(st): State<AcademicState>, auth: Authenticated, Path(id): Path<Uuid>, headers: HeaderMap, body: Option<Json<FlexibleReason>>) -> ApiResult<Response> {
            let ctx = auth.require(Permission::FlexibleLearningManage)?;
            let v = expect_version!(headers);
            let o = opportunity::command(&st, ctx, id, v, $to, kit::reason(body), &actor(ctx)).await?;
            Ok(ok(&o, o.version))
        }
    };
}

opportunity_cmd!(
    /// Suspend an active opportunity (reason required; existing enrolments are kept).
    opportunity_suspend, "/api/v1/academic/learning-opportunities/{id}/suspend", "learning_opportunity_suspend", LearningOpportunityStatus::Suspended);
opportunity_cmd!(
    /// Reactivate a suspended opportunity (readiness re-checked).
    opportunity_reactivate, "/api/v1/academic/learning-opportunities/{id}/reactivate", "learning_opportunity_reactivate", LearningOpportunityStatus::Active);
opportunity_cmd!(
    /// Archive the opportunity (refused while enrolment / recognition / exception / reconciliation obligations remain).
    opportunity_archive, "/api/v1/academic/learning-opportunities/{id}/archive", "learning_opportunity_archive", LearningOpportunityStatus::Archived);

/// Change capacity, reserved seats, waitlist or window (below allocations → `overAllocated`).
#[utoipa::path(post, path = "/api/v1/academic/learning-opportunities/{id}/capacity", operation_id = "learning_opportunity_capacity", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the opportunity")),
    request_body = OpportunityCapacity, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LearningOpportunity, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn opportunity_capacity(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<OpportunityCapacity>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    let v = expect_version!(headers);
    let o = opportunity::set_capacity(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&o, o.version))
}

macro_rules! seat_cmd {
    ($(#[$m:meta])* $fn:ident, $path:literal, $op:literal, $call:path) => {
        $(#[$m])*
        #[utoipa::path(post, path = $path, operation_id = $op, tag = "flexible-learning",
            params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the opportunity")),
            security(("session_cookie" = [], "csrf_token" = [])),
            responses((status = 200, body = AllocationRun), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
        pub async fn $fn(State(st): State<AcademicState>, auth: Authenticated, Path(id): Path<Uuid>, headers: HeaderMap) -> ApiResult<Response> {
            let ctx = auth.require(Permission::FlexibleLearningManage)?;
            let v = expect_version!(headers);
            let run = $call(&st, ctx, id, v, &actor(ctx)).await?;
            let version = run.opportunity.as_ref().map_or(0, |o| o.version);
            Ok(ok(&run, version))
        }
    };
}

seat_cmd!(
    /// Allocate seats to eligible demand by priority (rest waitlisted or unallocated).
    opportunity_allocate, "/api/v1/academic/learning-opportunities/{id}/allocate", "learning_opportunity_allocate", enrolment::run_allocation);
seat_cmd!(
    /// Promote the next eligible waitlist entries into free seats (expired entries recorded).
    opportunity_promote, "/api/v1/academic/learning-opportunities/{id}/promote-waitlist", "learning_opportunity_waitlist_promote", enrolment::promote_waitlist);
seat_cmd!(
    /// Resolve allocation conflicts (release over-capacity allocations back to the waitlist).
    opportunity_resolve_conflicts, "/api/v1/academic/learning-opportunities/{id}/resolve-conflicts", "learning_opportunity_conflict_resolve", enrolment::resolve_conflicts);

/// Flexible enrolments (demand) of an opportunity.
#[utoipa::path(get, path = "/api/v1/academic/learning-opportunities/{id}/enrolments", operation_id = "flexible_enrolment_list", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), EnrolmentFilter), security(("session_cookie" = [])),
    responses((status = 200, body = FlexibleEnrolmentList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn enrolment_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(f): Query<EnrolmentFilter>,
) -> ApiResult<Json<FlexibleEnrolmentList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    Ok(Json(FlexibleEnrolmentList {
        items: enrolment::list_for_opportunity(
            &st,
            ctx.scope.tenant_id,
            id,
            f.status.map(|s| s.as_str()),
        )
        .await?,
    }))
}

/// Record a student's request on their behalf (a repeated `sourceKey` returns the existing one).
#[utoipa::path(post, path = "/api/v1/academic/learning-opportunities/{id}/enrolments", operation_id = "flexible_enrolment_create", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewFlexibleEnrolment, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = FlexibleEnrolment, headers(("ETag" = String))), (status = 200, body = FlexibleEnrolment), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn enrolment_create(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewFlexibleEnrolment>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    require_idempotency_key(&headers)?;
    let (e, new) = enrolment::request(
        &st,
        ctx,
        id,
        cmd.student_id,
        cmd.priority,
        cmd.source_key,
        &actor(ctx),
    )
    .await?;
    Ok(if new {
        created(&e, e.version)
    } else {
        ok(&e, e.version)
    })
}

// ============================================================== flexible enrolments (L3-D-034…038)

/// A flexible enrolment with allocations, registrations, completion and history.
#[utoipa::path(get, path = "/api/v1/academic/flexible-enrolments/{id}", operation_id = "flexible_enrolment_get", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = FlexibleEnrolment, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn enrolment_get(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let e = enrolment::get(&st, ctx.scope.tenant_id, id).await?;
    Ok(ok(&e, e.version))
}

/// Academic decision on a request that needs human review (approve → eligible, reject → rejected).
#[utoipa::path(post, path = "/api/v1/academic/flexible-enrolments/{id}/decision", operation_id = "flexible_enrolment_decision", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the enrolment")),
    request_body = FlexibleDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FlexibleEnrolment, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn enrolment_decision(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<FlexibleDecision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "flexible enrolment").await?;
    let v = expect_version!(headers);
    let (approve, reason) = decision(&cmd);
    let e = enrolment::decide(&st, ctx, id, v, approve, reason, &actor(ctx)).await?;
    Ok(ok(&e, e.version))
}

/// Authorised removal (reason required; a held seat is released, history kept).
#[utoipa::path(post, path = "/api/v1/academic/flexible-enrolments/{id}/withdraw", operation_id = "flexible_enrolment_withdraw", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the enrolment")),
    request_body(content = Option<FlexibleReason>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FlexibleEnrolment, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn enrolment_withdraw(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<FlexibleReason>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    let v = expect_version!(headers);
    let e = enrolment::withdraw(&st, ctx, id, v, kit::reason(body), false, &actor(ctx)).await?;
    Ok(ok(&e, e.version))
}

/// Registration hand-off (open elective: registered; external: provider registration requested).
#[utoipa::path(post, path = "/api/v1/academic/flexible-enrolments/{id}/register", operation_id = "flexible_enrolment_register", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the enrolment")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FlexibleEnrolment, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn enrolment_register(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    let v = expect_version!(headers);
    let e = enrolment::register(&st, ctx, id, v, &actor(ctx)).await?;
    Ok(ok(&e, e.version))
}

/// Record the provider's registration acknowledgement (accepted / rejected / recovery_required).
#[utoipa::path(post, path = "/api/v1/academic/flexible-enrolments/{id}/registration-outcome", operation_id = "flexible_enrolment_registration_outcome", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the enrolment")),
    request_body = RegistrationOutcome, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FlexibleEnrolment, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn enrolment_registration_outcome(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<RegistrationOutcome>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    let v = expect_version!(headers);
    let e = enrolment::registration_outcome(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&e, e.version))
}

/// Record learning progress (registered → in_progress; hours).
#[utoipa::path(post, path = "/api/v1/academic/flexible-enrolments/{id}/progress", operation_id = "flexible_enrolment_progress", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the enrolment")),
    request_body = LearningProgress, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FlexibleEnrolment, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn enrolment_progress(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<LearningProgress>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    let v = expect_version!(headers);
    let e = enrolment::progress(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&e, e.version))
}

/// Record the institutional learning completion (validated; no automatic credit).
#[utoipa::path(post, path = "/api/v1/academic/flexible-enrolments/{id}/completion", operation_id = "flexible_enrolment_complete", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the enrolment")),
    request_body = NewLearningCompletion, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FlexibleEnrolment, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn enrolment_complete(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewLearningCompletion>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    let v = expect_version!(headers);
    let e = enrolment::complete(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&e, e.version))
}

// ============================================================== policies (L3-D-045)

/// Flexible-learning policy versions of an institution.
#[utoipa::path(get, path = "/api/v1/academic/institutions/{id}/flexible-learning-policies", operation_id = "flexible_policy_list", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), StatusFilter), security(("session_cookie" = [])),
    responses((status = 200, body = FlexibleLearningPolicyList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(f): Query<StatusFilter>,
) -> ApiResult<Json<FlexibleLearningPolicyList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    Ok(Json(FlexibleLearningPolicyList {
        items: policy::list(&st, ctx.scope.tenant_id, id, f.status.as_deref()).await?,
    }))
}

/// Draft a new policy version (rules + effective period).
#[utoipa::path(post, path = "/api/v1/academic/institutions/{id}/flexible-learning-policies", operation_id = "flexible_policy_create", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewFlexibleLearningPolicy, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = FlexibleLearningPolicy, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_create(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewFlexibleLearningPolicy>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    require_idempotency_key(&headers)?;
    let p = policy::create(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&p, p.version))
}

/// A policy version with its history.
#[utoipa::path(get, path = "/api/v1/academic/flexible-learning-policies/{id}", operation_id = "flexible_policy_get", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = FlexibleLearningPolicy, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_get(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let p = policy::get(&st, ctx.scope.tenant_id, id).await?;
    Ok(ok(&p, p.version))
}

/// Submit for publication (`FLEXIBLE_LEARNING_POLICY`; auto-approval makes it effective).
#[utoipa::path(post, path = "/api/v1/academic/flexible-learning-policies/{id}/submit", operation_id = "flexible_policy_submit", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the policy version")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FlexibleLearningPolicy, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_submit(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    let v = expect_version!(headers);
    let p = policy::submit(&st, ctx, id, v, &actor(ctx)).await?;
    Ok(ok(&p, p.version))
}

/// Decide the pending publication.
#[utoipa::path(post, path = "/api/v1/academic/flexible-learning-policies/{id}/decision", operation_id = "flexible_policy_decision", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the policy version")),
    request_body = FlexibleDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FlexibleLearningPolicy, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_decision(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<FlexibleDecision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "flexible learning policy").await?;
    let v = expect_version!(headers);
    let (approve, reason) = decision(&cmd);
    let p = policy::decide(&st, ctx, id, v, approve, reason, &actor(ctx)).await?;
    Ok(ok(&p, p.version))
}

/// Retire a policy version (history kept).
#[utoipa::path(post, path = "/api/v1/academic/flexible-learning-policies/{id}/retire", operation_id = "flexible_policy_retire", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the policy version")),
    request_body(content = Option<FlexibleReason>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FlexibleLearningPolicy, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_retire(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<FlexibleReason>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    let v = expect_version!(headers);
    let p = policy::retire(&st, ctx, id, v, kit::reason(body), &actor(ctx)).await?;
    Ok(ok(&p, p.version))
}

// ============================================================== exception cases (L3-D-050)

/// Flexible-learning exception cases of an institution.
#[utoipa::path(get, path = "/api/v1/academic/institutions/{id}/flexible-exceptions", operation_id = "flexible_exception_list", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), StatusFilter), security(("session_cookie" = [])),
    responses((status = 200, body = FlexibleExceptionCaseList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(f): Query<StatusFilter>,
) -> ApiResult<Json<FlexibleExceptionCaseList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    Ok(Json(FlexibleExceptionCaseList {
        items: exception::list(&st, ctx.scope.tenant_id, id, f.status.as_deref()).await?,
    }))
}

/// Raise an exception case (a repeated `sourceKey` returns the existing case).
#[utoipa::path(post, path = "/api/v1/academic/institutions/{id}/flexible-exceptions", operation_id = "flexible_exception_raise", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewFlexibleExceptionCase, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = FlexibleExceptionCase, headers(("ETag" = String))), (status = 200, body = FlexibleExceptionCase), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_raise(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewFlexibleExceptionCase>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    require_idempotency_key(&headers)?;
    let (c, new) = exception::raise(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(if new {
        created(&c, c.version)
    } else {
        ok(&c, c.version)
    })
}

/// An exception case with its history.
#[utoipa::path(get, path = "/api/v1/academic/flexible-exceptions/{id}", operation_id = "flexible_exception_get", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = FlexibleExceptionCase, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_get(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let c = exception::get(&st, ctx.scope.tenant_id, id).await?;
    Ok(ok(&c, c.version))
}

/// Propose the corrective plan (`FLEXIBLE_EXCEPTION`; overrides need a justification).
#[utoipa::path(post, path = "/api/v1/academic/flexible-exceptions/{id}/plan", operation_id = "flexible_exception_plan", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body = ExceptionPlan, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FlexibleExceptionCase, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_plan(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<ExceptionPlan>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    let v = expect_version!(headers);
    let c = exception::plan(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&c, c.version))
}

/// Decide the pending corrective plan.
#[utoipa::path(post, path = "/api/v1/academic/flexible-exceptions/{id}/decision", operation_id = "flexible_exception_decision", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body = FlexibleDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FlexibleExceptionCase, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_decision(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<FlexibleDecision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "flexible learning exception").await?;
    let v = expect_version!(headers);
    let (approve, reason) = decision(&cmd);
    let c = exception::decide(&st, ctx, id, v, approve, reason, &actor(ctx)).await?;
    Ok(ok(&c, c.version))
}

/// Record the corrective action and its resolution evidence.
#[utoipa::path(post, path = "/api/v1/academic/flexible-exceptions/{id}/resolve", operation_id = "flexible_exception_resolve", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body = ExceptionResolution, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FlexibleExceptionCase, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_resolve(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<ExceptionResolution>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    let v = expect_version!(headers);
    let c = exception::resolve(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&c, c.version))
}

/// The correction failed: back to the approved plan (reason required).
#[utoipa::path(post, path = "/api/v1/academic/flexible-exceptions/{id}/reopen-correction", operation_id = "flexible_exception_reopen_correction", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body(content = Option<FlexibleReason>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FlexibleExceptionCase, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_reopen_correction(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<FlexibleReason>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    let v = expect_version!(headers);
    let c = exception::reopen_correction(&st, ctx, id, v, kit::reason(body), &actor(ctx)).await?;
    Ok(ok(&c, c.version))
}

/// Place or lift an investigation / regulatory hold.
#[utoipa::path(post, path = "/api/v1/academic/flexible-exceptions/{id}/hold", operation_id = "flexible_exception_hold", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body = ExceptionHold, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FlexibleExceptionCase, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_hold(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<ExceptionHold>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    let v = expect_version!(headers);
    let c = exception::hold(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&c, c.version))
}

/// Close a resolved case (evidence present, no hold).
#[utoipa::path(post, path = "/api/v1/academic/flexible-exceptions/{id}/close", operation_id = "flexible_exception_close", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body(content = Option<FlexibleReason>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FlexibleExceptionCase, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_close(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<FlexibleReason>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    let v = expect_version!(headers);
    let c = exception::close(&st, ctx, id, v, kit::reason(body), &actor(ctx)).await?;
    Ok(ok(&c, c.version))
}

// ============================================================== student self-service

/// The caller's flexible-learning view: open opportunities of their institutions and their own enrolments.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyFlexibleLearning {
    pub opportunities: Vec<LearningOpportunity>,
    pub enrolments: Vec<FlexibleEnrolment>,
}

/// My flexible learning (open opportunities + my requests and their progress).
#[utoipa::path(get, path = "/api/v1/academic/me/flexible-learning", operation_id = "flexible_self_overview", tag = "flexible-learning",
    security(("session_cookie" = [])),
    responses((status = 200, body = MyFlexibleLearning), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn self_overview(
    State(st): State<AcademicState>,
    auth: Authenticated,
) -> ApiResult<Json<MyFlexibleLearning>> {
    let ctx = auth.require(Permission::AcademicSelfService)?;
    let student = me::me(&st, ctx).await?;
    let tenant = ctx.scope.tenant_id;
    let all = opportunity::active_in_tenant(&st, tenant).await?;
    let mut allowed = std::collections::HashMap::new();
    let mut opportunities = Vec::new();
    for o in all {
        let ok = match allowed.get(&o.institution_id) {
            Some(v) => *v,
            None => {
                let v = st
                    .students
                    .live_context(
                        &ExecutionScope::tenant(tenant),
                        student.id,
                        InstitutionId(o.institution_id),
                    )
                    .await?
                    .is_some();
                allowed.insert(o.institution_id, v);
                v
            }
        };
        if ok {
            opportunities.push(o);
        }
    }
    let enrolments = enrolment::list_for_student(&st, tenant, student.id.as_uuid()).await?;
    Ok(Json(MyFlexibleLearning {
        opportunities,
        enrolments,
    }))
}

/// Request a flexible-learning opportunity for myself.
#[utoipa::path(post, path = "/api/v1/academic/me/flexible-learning/requests", operation_id = "flexible_self_request", tag = "flexible-learning",
    params(("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = SelfFlexibleRequest, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = FlexibleEnrolment, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn self_request(
    State(st): State<AcademicState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(cmd): Json<SelfFlexibleRequest>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AcademicSelfService)?;
    require_idempotency_key(&headers)?;
    let student = me::me(&st, ctx).await?;
    let (e, new) = enrolment::request(
        &st,
        ctx,
        cmd.opportunity_id,
        student.id.as_uuid(),
        None,
        None,
        &actor(ctx),
    )
    .await?;
    Ok(if new {
        created(&e, e.version)
    } else {
        ok(&e, e.version)
    })
}

/// The caller's own enrolment (404 for anyone else's).
pub(crate) async fn own_enrolment(
    st: &AcademicState,
    ctx: &sutra_kernel::auth::AuthContext,
    id: Uuid,
) -> Result<FlexibleEnrolment, KernelError> {
    let student = me::me(st, ctx).await?;
    let e = enrolment::get(st, ctx.scope.tenant_id, id).await?;
    if e.student_id != student.id.as_uuid() {
        return Err(KernelError::NotFound {
            entity: "flexible enrolment",
        });
    }
    Ok(e)
}

/// Withdraw my request / participation (allowed up to the policy's withdrawal status).
#[utoipa::path(post, path = "/api/v1/academic/me/flexible-learning/{id}/withdraw", operation_id = "flexible_self_withdraw", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the enrolment")),
    request_body(content = Option<FlexibleReason>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FlexibleEnrolment, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn self_withdraw(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<FlexibleReason>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AcademicSelfService)?;
    let v = expect_version!(headers);
    own_enrolment(&st, ctx, id).await?;
    let e = enrolment::withdraw(&st, ctx, id, v, kit::reason(body), true, &actor(ctx)).await?;
    Ok(ok(&e, e.version))
}

/// All flexible-learning routes.
pub fn router(state: AcademicState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(provider_list, provider_create))
        .routes(routes!(provider_get))
        .routes(routes!(provider_submit))
        .routes(routes!(provider_decision))
        .routes(routes!(provider_suspend))
        .routes(routes!(provider_reactivate))
        .routes(routes!(provider_offboard))
        .routes(routes!(opportunity_list, opportunity_create))
        .routes(routes!(opportunity_session_archive))
        .routes(routes!(opportunity_get))
        .routes(routes!(opportunity_submit))
        .routes(routes!(opportunity_decision))
        .routes(routes!(opportunity_suspend))
        .routes(routes!(opportunity_reactivate))
        .routes(routes!(opportunity_archive))
        .routes(routes!(opportunity_capacity))
        .routes(routes!(opportunity_allocate))
        .routes(routes!(opportunity_promote))
        .routes(routes!(opportunity_resolve_conflicts))
        .routes(routes!(enrolment_list, enrolment_create))
        .routes(routes!(enrolment_get))
        .routes(routes!(enrolment_decision))
        .routes(routes!(enrolment_withdraw))
        .routes(routes!(enrolment_register))
        .routes(routes!(enrolment_registration_outcome))
        .routes(routes!(enrolment_progress))
        .routes(routes!(enrolment_complete))
        .routes(routes!(policy_list, policy_create))
        .routes(routes!(policy_get))
        .routes(routes!(policy_submit))
        .routes(routes!(policy_decision))
        .routes(routes!(policy_retire))
        .routes(routes!(exception_list, exception_raise))
        .routes(routes!(exception_get))
        .routes(routes!(exception_plan))
        .routes(routes!(exception_decision))
        .routes(routes!(exception_resolve))
        .routes(routes!(exception_reopen_correction))
        .routes(routes!(exception_hold))
        .routes(routes!(exception_close))
        .routes(routes!(self_overview))
        .routes(routes!(self_request))
        .routes(routes!(self_withdraw))
        .with_state(state)
}
