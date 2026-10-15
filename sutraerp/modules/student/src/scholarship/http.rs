//! HTTP API of scholarships, eligibility policies, student eligibility, external sync and bulk
//! recalculation (tag `scholarships`; contract doc §5).

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
use sutra_contracts::student::ids::{
    ScholarshipDefinitionId, ScholarshipEligibilityId, ScholarshipPolicyId,
    ScholarshipRecalculationId, StudentId,
};
use sutra_contracts::student::status::{ScholarshipDefinitionStatus, ScholarshipEligibilityStatus};
use sutra_kernel::auth::Permission;
use sutra_kernel::scope::InstitutionId;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ScholarshipFilter {
    pub status: Option<ScholarshipDefinitionStatus>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EligibilityFilter {
    pub status: Option<ScholarshipEligibilityStatus>,
}

macro_rules! def_cmd {
    ($(#[$m:meta])* $fn:ident, $path:literal, $op:literal, $to:expr) => {
        $(#[$m])*
        #[utoipa::path(post, path = $path, operation_id = $op, tag = "scholarships",
            params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the scholarship")),
            request_body(content = Option<LifecycleReason>), security(("session_cookie" = [], "csrf_token" = [])),
            responses((status = 200, body = Scholarship, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
        pub async fn $fn(State(st): State<StudentState>, auth: Authenticated, Path(id): Path<ScholarshipDefinitionId>, headers: HeaderMap, body: Option<Json<LifecycleReason>>) -> ApiResult<Response> {
            let ctx = auth.require(Permission::ScholarshipPublish)?;
            let v = expect_version!(headers);
            let s = repo::move_definition(&st, ctx, id, v, $to, kit::reason(body), &actor(ctx)).await?;
            Ok(ok(&s, s.version))
        }
    };
}

macro_rules! el_cmd {
    ($(#[$m:meta])* $fn:ident, $path:literal, $op:literal, |$st:ident, $ctx:ident, $id:ident, $v:ident, $reason:ident, $actor:ident| $call:expr) => {
        $(#[$m])*
        #[utoipa::path(post, path = $path, operation_id = $op, tag = "scholarships",
            params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the eligibility")),
            request_body(content = Option<LifecycleReason>), security(("session_cookie" = [], "csrf_token" = [])),
            responses((status = 200, body = ScholarshipEligibility, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
        pub async fn $fn(State($st): State<StudentState>, auth: Authenticated, Path($id): Path<ScholarshipEligibilityId>, headers: HeaderMap, body: Option<Json<LifecycleReason>>) -> ApiResult<Response> {
            let $ctx = auth.require(Permission::ScholarshipManage)?;
            let $v = expect_version!(headers);
            let $reason = kit::reason(body);
            let $actor = actor($ctx);
            let e = $call.await?;
            Ok(ok(&e, e.version))
        }
    };
}

// ---- definitions ----

/// Scholarships of an institution.
#[utoipa::path(get, path = "/api/v1/student/institutions/{id}/scholarships", operation_id = "scholarship_list", tag = "scholarships",
    params(("id" = String, Path, format = Uuid), ScholarshipFilter), security(("session_cookie" = [])),
    responses((status = 200, body = ScholarshipList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn scholarship_list(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    Query(f): Query<ScholarshipFilter>,
) -> ApiResult<Json<ScholarshipList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    Ok(Json(ScholarshipList {
        items: repo::list(&st, ctx.scope.tenant_id, id, f.status).await?,
    }))
}

/// Draft a scholarship definition.
#[utoipa::path(post, path = "/api/v1/student/institutions/{id}/scholarships", operation_id = "scholarship_create", tag = "scholarships",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewScholarship, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = Scholarship, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn scholarship_create(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    Json(cmd): Json<NewScholarship>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ScholarshipManage)?;
    require_idempotency_key(&headers)?;
    let s = repo::create(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&s, s.version))
}

/// Get a scholarship (with its effective policy).
#[utoipa::path(get, path = "/api/v1/student/scholarships/{id}", operation_id = "scholarship_get", tag = "scholarships",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = Scholarship, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn scholarship_get(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<ScholarshipDefinitionId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentRead)?;
    let s = repo::get(&st, ctx.scope.tenant_id, id).await?;
    Ok(ok(&s, s.version))
}

/// Update a draft scholarship.
#[utoipa::path(patch, path = "/api/v1/student/scholarships/{id}", operation_id = "scholarship_update", tag = "scholarships",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the scholarship")),
    request_body = ScholarshipUpdate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = Scholarship, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn scholarship_update(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<ScholarshipDefinitionId>,
    headers: HeaderMap,
    Json(cmd): Json<ScholarshipUpdate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ScholarshipManage)?;
    let v = expect_version!(headers);
    let s = repo::update(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&s, s.version))
}

def_cmd!(
    /// Publish a draft scholarship (visible, not yet accepting determinations).
    scholarship_publish, "/api/v1/student/scholarships/{id}/publish", "scholarship_publish", ScholarshipDefinitionStatus::Published);
def_cmd!(
    /// Make a published (or reactivate a suspended) scholarship available; needs an effective policy.
    scholarship_activate, "/api/v1/student/scholarships/{id}/activate", "scholarship_activate", ScholarshipDefinitionStatus::Active);
def_cmd!(
    /// Suspend an active scholarship (reason required).
    scholarship_suspend, "/api/v1/student/scholarships/{id}/suspend", "scholarship_suspend", ScholarshipDefinitionStatus::Suspended);
def_cmd!(
    /// Expire an active scholarship.
    scholarship_expire, "/api/v1/student/scholarships/{id}/expire", "scholarship_expire", ScholarshipDefinitionStatus::Expired);
def_cmd!(
    /// Archive a published, suspended or expired scholarship.
    scholarship_archive, "/api/v1/student/scholarships/{id}/archive", "scholarship_archive", ScholarshipDefinitionStatus::Archived);

// ---- policies ----

/// Policy versions of a scholarship.
#[utoipa::path(get, path = "/api/v1/student/scholarships/{id}/policies", operation_id = "scholarship_policy_list", tag = "scholarships",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ScholarshipPolicyList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_list(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<ScholarshipDefinitionId>,
) -> ApiResult<Json<ScholarshipPolicyList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    repo::get(&st, ctx.scope.tenant_id, id).await?;
    Ok(Json(ScholarshipPolicyList {
        items: repo::list_policies(&st, ctx.scope.tenant_id, id).await?,
    }))
}

/// Draft a new eligibility policy version.
#[utoipa::path(post, path = "/api/v1/student/scholarships/{id}/policies", operation_id = "scholarship_policy_create", tag = "scholarships",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewScholarshipPolicy, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ScholarshipPolicy, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_create(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<ScholarshipDefinitionId>,
    headers: HeaderMap,
    Json(cmd): Json<NewScholarshipPolicy>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ScholarshipManage)?;
    require_idempotency_key(&headers)?;
    let p = repo::create_policy(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&p, p.version))
}

/// Get a policy version.
#[utoipa::path(get, path = "/api/v1/student/scholarship-policies/{id}", operation_id = "scholarship_policy_get", tag = "scholarships",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ScholarshipPolicy, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_get(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<ScholarshipPolicyId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentRead)?;
    let p = repo::get_policy(&st, ctx.scope.tenant_id, id).await?;
    Ok(ok(&p, p.version))
}

/// Update a draft policy version.
#[utoipa::path(patch, path = "/api/v1/student/scholarship-policies/{id}", operation_id = "scholarship_policy_update", tag = "scholarships",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the policy version")),
    request_body = ScholarshipPolicyUpdate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ScholarshipPolicy, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_update(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<ScholarshipPolicyId>,
    headers: HeaderMap,
    Json(cmd): Json<ScholarshipPolicyUpdate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ScholarshipManage)?;
    let v = expect_version!(headers);
    let p = repo::update_policy(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&p, p.version))
}

/// Submit a draft policy version for approval (`SCHOLARSHIP_POLICY`).
#[utoipa::path(post, path = "/api/v1/student/scholarship-policies/{id}/submit", operation_id = "scholarship_policy_submit", tag = "scholarships",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the policy version")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ScholarshipPolicy, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_submit(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<ScholarshipPolicyId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ScholarshipPublish)?;
    let v = expect_version!(headers);
    let p = repo::submit_policy(&st, ctx, id, v, &actor(ctx)).await?;
    Ok(ok(&p, p.version))
}

/// Approval decision on a policy version in review (`AwaitDecision`; reject → draft).
#[utoipa::path(post, path = "/api/v1/student/scholarship-policies/{id}/decision", operation_id = "scholarship_policy_decision", tag = "scholarships",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the policy version")),
    request_body = LifecycleDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ScholarshipPolicy, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_decision(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<ScholarshipPolicyId>,
    headers: HeaderMap,
    Json(cmd): Json<LifecycleDecision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ScholarshipPublish)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "scholarship policy").await?;
    let v = expect_version!(headers);
    let p = repo::decide_policy(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&p, p.version))
}

/// Retire the effective policy version.
#[utoipa::path(post, path = "/api/v1/student/scholarship-policies/{id}/retire", operation_id = "scholarship_policy_retire", tag = "scholarships",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the policy version")),
    request_body(content = Option<LifecycleReason>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ScholarshipPolicy, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn policy_retire(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<ScholarshipPolicyId>,
    headers: HeaderMap,
    body: Option<Json<LifecycleReason>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ScholarshipPublish)?;
    let v = expect_version!(headers);
    let p = repo::retire_policy(&st, ctx, id, v, kit::reason(body), &actor(ctx)).await?;
    Ok(ok(&p, p.version))
}

// ---- eligibility ----

/// Eligibilities determined for one scholarship.
#[utoipa::path(get, path = "/api/v1/student/scholarships/{id}/eligibilities", operation_id = "scholarship_eligibility_list_scholarship", tag = "scholarships",
    params(("id" = String, Path, format = Uuid), EligibilityFilter), security(("session_cookie" = [])),
    responses((status = 200, body = ScholarshipEligibilityList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn eligibility_list_scholarship(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<ScholarshipDefinitionId>,
    Query(f): Query<EligibilityFilter>,
) -> ApiResult<Json<ScholarshipEligibilityList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    repo::get(&st, ctx.scope.tenant_id, id).await?;
    Ok(Json(ScholarshipEligibilityList {
        items: repo::list_eligibilities(
            &st,
            ctx.scope.tenant_id,
            "scholarship_id",
            id.as_uuid(),
            f.status,
        )
        .await?,
    }))
}

/// Scholarship eligibilities of a student.
#[utoipa::path(get, path = "/api/v1/student/students/{id}/scholarship-eligibilities", operation_id = "scholarship_eligibility_list", tag = "scholarships",
    params(("id" = String, Path, format = Uuid), EligibilityFilter), security(("session_cookie" = [])),
    responses((status = 200, body = ScholarshipEligibilityList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn eligibility_list(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    Query(f): Query<EligibilityFilter>,
) -> ApiResult<Json<ScholarshipEligibilityList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    crate::shared::student::load_student(&st.pool, ctx.scope.tenant_id, id).await?;
    Ok(Json(ScholarshipEligibilityList {
        items: repo::list_eligibilities(
            &st,
            ctx.scope.tenant_id,
            "student_id",
            id.as_uuid(),
            f.status,
        )
        .await?,
    }))
}

/// Determine a student's eligibility for a scholarship (L3-C-064).
#[utoipa::path(post, path = "/api/v1/student/students/{id}/scholarship-eligibilities", operation_id = "scholarship_eligibility_determine", tag = "scholarships",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewScholarshipEligibility, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ScholarshipEligibility, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn eligibility_determine(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    headers: HeaderMap,
    Json(cmd): Json<NewScholarshipEligibility>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ScholarshipManage)?;
    require_idempotency_key(&headers)?;
    let e = repo::determine(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&e, e.version))
}

/// Get an eligibility with its assessment history.
#[utoipa::path(get, path = "/api/v1/student/scholarship-eligibilities/{id}", operation_id = "scholarship_eligibility_get", tag = "scholarships",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ScholarshipEligibility, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn eligibility_get(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<ScholarshipEligibilityId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentRead)?;
    let e = repo::get_eligibility(&st, ctx.scope.tenant_id, id).await?;
    Ok(ok(&e, e.version))
}

el_cmd!(
    /// Generate the recommendation of an eligible determination (`SCHOLARSHIP_RECOMMENDATION`).
    eligibility_recommend, "/api/v1/student/scholarship-eligibilities/{id}/recommend", "scholarship_eligibility_recommend",
    |st, ctx, id, v, _reason, a| repo::recommend(&st, ctx, id, v, &a));
el_cmd!(
    /// Withdraw a generated recommendation (back to eligible; reason required).
    eligibility_withdraw_recommendation, "/api/v1/student/scholarship-eligibilities/{id}/withdraw-recommendation", "scholarship_eligibility_withdraw_recommendation",
    |st, ctx, id, v, reason, a| repo::withdraw_recommendation(&st, ctx, id, v, reason, &a));
el_cmd!(
    /// Hand the recommendation off to Financial Aid (event only); the eligibility becomes active.
    eligibility_hand_off, "/api/v1/student/scholarship-eligibilities/{id}/hand-off", "scholarship_eligibility_hand_off",
    |st, ctx, id, v, _reason, a| repo::hand_off(&st, ctx, id, v, &a));
el_cmd!(
    /// Expire an eligibility.
    eligibility_expire, "/api/v1/student/scholarship-eligibilities/{id}/expire", "scholarship_eligibility_expire",
    |st, ctx, id, v, reason, a| repo::expire(&st, ctx, id, v, reason, &a));
el_cmd!(
    /// Archive an expired / not-eligible record.
    eligibility_archive, "/api/v1/student/scholarship-eligibilities/{id}/archive", "scholarship_eligibility_archive",
    |st, ctx, id, v, reason, a| repo::archive(&st, ctx, id, v, reason, &a));

/// Decision on a pending recommendation (`AwaitDecision`).
#[utoipa::path(post, path = "/api/v1/student/scholarship-eligibilities/{id}/recommendation-decision", operation_id = "scholarship_eligibility_recommendation_decision", tag = "scholarships",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the eligibility")),
    request_body = LifecycleDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ScholarshipEligibility, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn eligibility_recommendation_decision(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<ScholarshipEligibilityId>,
    headers: HeaderMap,
    Json(cmd): Json<LifecycleDecision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ScholarshipManage)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "scholarship recommendation").await?;
    let v = expect_version!(headers);
    let e = repo::recommendation_decision(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&e, e.version))
}

/// Resolve a conditional eligibility (fulfilled / failed / expired; L3-C-065).
#[utoipa::path(post, path = "/api/v1/student/scholarship-eligibilities/{id}/resolve-condition", operation_id = "scholarship_eligibility_resolve_condition", tag = "scholarships",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the eligibility")),
    request_body = ResolveCondition, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ScholarshipEligibility, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn eligibility_resolve_condition(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<ScholarshipEligibilityId>,
    headers: HeaderMap,
    Json(cmd): Json<ResolveCondition>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ScholarshipManage)?;
    let v = expect_version!(headers);
    let e = repo::resolve_condition(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&e, e.version))
}

/// Reassess an active eligibility against the current policy (L3-C-066).
#[utoipa::path(post, path = "/api/v1/student/scholarship-eligibilities/{id}/reassess", operation_id = "scholarship_eligibility_reassess", tag = "scholarships",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the eligibility")),
    request_body(content = Option<Reevaluation>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ScholarshipEligibility, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn eligibility_reassess(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<ScholarshipEligibilityId>,
    headers: HeaderMap,
    body: Option<Json<Reevaluation>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ScholarshipManage)?;
    let v = expect_version!(headers);
    let e = repo::reevaluate(
        &st,
        ctx,
        id,
        v,
        body.map(|Json(b)| b).unwrap_or_default(),
        false,
        &actor(ctx),
    )
    .await?;
    Ok(ok(&e, e.version))
}

/// Renewal evaluation of an active eligibility (L3-C-067).
#[utoipa::path(post, path = "/api/v1/student/scholarship-eligibilities/{id}/renew", operation_id = "scholarship_eligibility_renew", tag = "scholarships",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the eligibility")),
    request_body(content = Option<Reevaluation>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ScholarshipEligibility, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn eligibility_renew(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<ScholarshipEligibilityId>,
    headers: HeaderMap,
    body: Option<Json<Reevaluation>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ScholarshipManage)?;
    let v = expect_version!(headers);
    let e = repo::reevaluate(
        &st,
        ctx,
        id,
        v,
        body.map(|Json(b)| b).unwrap_or_default(),
        true,
        &actor(ctx),
    )
    .await?;
    Ok(ok(&e, e.version))
}

// ---- external sync + recalculation ----

/// External ecosystem synchronisations of a scholarship.
#[utoipa::path(get, path = "/api/v1/student/scholarships/{id}/external-syncs", operation_id = "scholarship_external_sync_list", tag = "scholarships",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ScholarshipExternalSyncList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn sync_list(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<ScholarshipDefinitionId>,
) -> ApiResult<Json<ScholarshipExternalSyncList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    repo::get(&st, ctx.scope.tenant_id, id).await?;
    Ok(Json(ScholarshipExternalSyncList {
        items: repo::list_syncs(&st, ctx.scope.tenant_id, id).await?,
    }))
}

/// Synchronise with the external scholarship ecosystem (port; no-op adapter in Batch 3).
#[utoipa::path(post, path = "/api/v1/student/scholarships/{id}/external-syncs", operation_id = "scholarship_external_sync_run", tag = "scholarships",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ScholarshipExternalSync), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn sync_run(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<ScholarshipDefinitionId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ScholarshipManage)?;
    require_idempotency_key(&headers)?;
    let s = repo::external_sync(&st, ctx, id, &actor(ctx)).await?;
    Ok(created(&s, 1))
}

/// Bulk recalculation runs of an institution.
#[utoipa::path(get, path = "/api/v1/student/institutions/{id}/scholarship-recalculations", operation_id = "scholarship_recalculation_list", tag = "scholarships",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ScholarshipRecalculationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn recalculation_list(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
) -> ApiResult<Json<ScholarshipRecalculationList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    Ok(Json(ScholarshipRecalculationList {
        items: repo::list_recalculations(&st, ctx.scope.tenant_id, id).await?,
    }))
}

/// Recalculate current eligibilities against the effective policies (L3-C-070).
#[utoipa::path(post, path = "/api/v1/student/institutions/{id}/scholarship-recalculations", operation_id = "scholarship_recalculation_run", tag = "scholarships",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body(content = Option<NewScholarshipRecalculation>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ScholarshipRecalculation, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn recalculation_run(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    body: Option<Json<NewScholarshipRecalculation>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ScholarshipManage)?;
    require_idempotency_key(&headers)?;
    let r = repo::recalculate(
        &st,
        ctx,
        id,
        body.map(|Json(b)| b).unwrap_or_default(),
        &actor(ctx),
    )
    .await?;
    Ok(created(&r, r.version))
}

/// Get a recalculation run.
#[utoipa::path(get, path = "/api/v1/student/scholarship-recalculations/{id}", operation_id = "scholarship_recalculation_get", tag = "scholarships",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ScholarshipRecalculation, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn recalculation_get(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<ScholarshipRecalculationId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentRead)?;
    let r = repo::get_recalculation(&st, ctx.scope.tenant_id, id).await?;
    Ok(ok(&r, r.version))
}

/// All scholarship routes.
pub fn router(state: StudentState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(scholarship_list, scholarship_create))
        .routes(routes!(scholarship_get, scholarship_update))
        .routes(routes!(scholarship_publish))
        .routes(routes!(scholarship_activate))
        .routes(routes!(scholarship_suspend))
        .routes(routes!(scholarship_expire))
        .routes(routes!(scholarship_archive))
        .routes(routes!(policy_list, policy_create))
        .routes(routes!(policy_get, policy_update))
        .routes(routes!(policy_submit))
        .routes(routes!(policy_decision))
        .routes(routes!(policy_retire))
        .routes(routes!(eligibility_list_scholarship))
        .routes(routes!(eligibility_list, eligibility_determine))
        .routes(routes!(eligibility_get))
        .routes(routes!(eligibility_recommend))
        .routes(routes!(eligibility_recommendation_decision))
        .routes(routes!(eligibility_withdraw_recommendation))
        .routes(routes!(eligibility_hand_off))
        .routes(routes!(eligibility_resolve_condition))
        .routes(routes!(eligibility_reassess))
        .routes(routes!(eligibility_renew))
        .routes(routes!(eligibility_expire))
        .routes(routes!(eligibility_archive))
        .routes(routes!(sync_list, sync_run))
        .routes(routes!(recalculation_list, recalculation_run))
        .routes(routes!(recalculation_get))
        .with_state(state)
}
