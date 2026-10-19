//! HTTP API of academic monitoring (tag `monitoring`; contract §5.2). Every route needs
//! `monitoring.manage` except reads (`academic.read`).

use super::repo::{self, *};
use crate::shared::http::actor;
use crate::shared::AcademicState;
use crate::timetable::kit::{
    self, created, expect_version, ok, DeliveryDecision, DeliveryDecisionValue, DeliveryReason,
};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::Json;
use sutra_contracts::academic::status::MonitoringCaseStatus;
use sutra_kernel::auth::Permission;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CaseFilter {
    pub status: Option<MonitoringCaseStatus>,
}

/// Monitoring cases of an institution.
#[utoipa::path(get, path = "/api/v1/academic/institutions/{id}/monitoring-cases", operation_id = "monitoring_case_list", tag = "monitoring",
    params(("id" = String, Path, format = Uuid), CaseFilter), security(("session_cookie" = [])),
    responses((status = 200, body = MonitoringCaseList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(f): Query<CaseFilter>,
) -> ApiResult<Json<MonitoringCaseList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    Ok(Json(MonitoringCaseList {
        items: repo::list(&st, ctx.scope.tenant_id, id, f.status.map(|s| s.as_str())).await?,
    }))
}

/// Open a monitoring case over a section, offering or student (a repeated `sourceKey` returns the existing case).
#[utoipa::path(post, path = "/api/v1/academic/institutions/{id}/monitoring-cases", operation_id = "monitoring_case_open", tag = "monitoring",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewMonitoringCase, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = MonitoringCase, headers(("ETag" = String))), (status = 200, body = MonitoringCase), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_open(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewMonitoringCase>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MonitoringManage)?;
    require_idempotency_key(&headers)?;
    let (c, new) = repo::open(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(if new {
        created(&c, c.version)
    } else {
        ok(&c, c.version)
    })
}

/// A monitoring case with its assessments, exceptions, interventions and decisions.
#[utoipa::path(get, path = "/api/v1/academic/monitoring-cases/{id}", operation_id = "monitoring_case_get", tag = "monitoring",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = MonitoringCase, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_get(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let c = repo::get(&st, ctx.scope.tenant_id, id).await?;
    Ok(ok(&c, c.version))
}

/// Record a risk assessment (level derived from finalised attendance under the D05 rule, else reviewer-set).
#[utoipa::path(post, path = "/api/v1/academic/monitoring-cases/{id}/risk-assessments", operation_id = "monitoring_risk_assess", tag = "monitoring",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewRiskAssessment, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = MonitoringCase, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn risk_assess(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewRiskAssessment>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MonitoringManage)?;
    require_idempotency_key(&headers)?;
    let c = repo::assess_risk(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&c, c.version))
}

/// Publish the monitoring outcome of this cycle (immutable snapshot of the latest assessment).
#[utoipa::path(post, path = "/api/v1/academic/monitoring-cases/{id}/publish-outcome", operation_id = "monitoring_outcome_publish", tag = "monitoring",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body = PublishOutcome, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MonitoringCase, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn outcome_publish(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<PublishOutcome>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MonitoringManage)?;
    let v = expect_version!(headers);
    let c = repo::publish_outcome(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&c, c.version))
}

/// Submit the case to governance review (`MONITORING_DECISION`; an approved decision may require an intervention).
#[utoipa::path(post, path = "/api/v1/academic/monitoring-cases/{id}/submit-review", operation_id = "monitoring_review_submit", tag = "monitoring",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body = GovernanceReview, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MonitoringCase, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn review_submit(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<GovernanceReview>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MonitoringManage)?;
    let v = expect_version!(headers);
    let c = repo::submit_review(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&c, c.version))
}

/// Decide the pending governance, certification or reopening decision (`AwaitDecision`).
#[utoipa::path(post, path = "/api/v1/academic/monitoring-cases/{id}/decision", operation_id = "monitoring_case_decision", tag = "monitoring",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body = DeliveryDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MonitoringCase, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_decision(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<DeliveryDecision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MonitoringManage)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "monitoring case").await?;
    let v = expect_version!(headers);
    let c = repo::decide(
        &st,
        ctx,
        id,
        v,
        cmd.decision == DeliveryDecisionValue::Approve,
        kit::opt_text(cmd.reason.as_deref()),
        &actor(ctx),
    )
    .await?;
    Ok(ok(&c, c.version))
}

/// Request certification of monitoring completion (`MONITORING_CLOSURE`; blocked by open exceptions/interventions).
#[utoipa::path(post, path = "/api/v1/academic/monitoring-cases/{id}/certify", operation_id = "monitoring_case_certify", tag = "monitoring",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body(content = Option<DeliveryReason>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MonitoringCase, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_certify(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<DeliveryReason>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MonitoringManage)?;
    let v = expect_version!(headers);
    let c = repo::certify(&st, ctx, id, v, kit::reason(body), &actor(ctx)).await?;
    Ok(ok(&c, c.version))
}

/// Close a certified case (final immutable snapshot).
#[utoipa::path(post, path = "/api/v1/academic/monitoring-cases/{id}/close", operation_id = "monitoring_case_close", tag = "monitoring",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body(content = Option<DeliveryReason>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MonitoringCase, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_close(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<DeliveryReason>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MonitoringManage)?;
    let v = expect_version!(headers);
    let c = repo::close(&st, ctx, id, v, kit::reason(body), &actor(ctx)).await?;
    Ok(ok(&c, c.version))
}

/// Request reopening of a closed case (`MONITORING_DECISION`; approval starts a new case, the closed one is kept).
#[utoipa::path(post, path = "/api/v1/academic/monitoring-cases/{id}/reopen", operation_id = "monitoring_case_reopen", tag = "monitoring",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body = ReopenCase, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MonitoringCase, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_reopen(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<ReopenCase>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MonitoringManage)?;
    let v = expect_version!(headers);
    let c = repo::reopen(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&c, c.version))
}

/// Raise a governance exception on the case (owner and independent reviewer required).
#[utoipa::path(post, path = "/api/v1/academic/monitoring-cases/{id}/exceptions", operation_id = "monitoring_exception_raise", tag = "monitoring",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewMonitoringException, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = MonitoringCase, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_raise(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewMonitoringException>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MonitoringManage)?;
    require_idempotency_key(&headers)?;
    let c = repo::raise_exception(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&c, c.version))
}

macro_rules! step_cmd {
    ($(#[$m:meta])* $fn:ident, $path:literal, $op:literal, $param:literal, $call:ident, $to:literal) => {
        $(#[$m])*
        #[utoipa::path(post, path = $path, operation_id = $op, tag = "monitoring",
            params(("id" = String, Path, format = Uuid), ($param = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the exception / intervention")),
            request_body(content = Option<MonitoringStep>), security(("session_cookie" = [], "csrf_token" = [])),
            responses((status = 200, body = MonitoringCase, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
        pub async fn $fn(State(st): State<AcademicState>, auth: Authenticated, Path((id, child)): Path<(Uuid, Uuid)>, headers: HeaderMap, body: Option<Json<MonitoringStep>>) -> ApiResult<Response> {
            let ctx = auth.require(Permission::MonitoringManage)?;
            let v = expect_version!(headers);
            let c = repo::$call(&st, ctx, id, child, v, $to, body.map(|Json(b)| b).unwrap_or_default(), &actor(ctx)).await?;
            Ok(ok(&c, c.version))
        }
    };
}

step_cmd!(
    /// Record the authorised resolution plan (`resolutionPlan`).
    exception_plan, "/api/v1/academic/monitoring-cases/{id}/exceptions/{exceptionId}/plan", "monitoring_exception_plan", "exceptionId", exception_step, "planned");
step_cmd!(
    /// Record the completed corrective action (`resolutionNotes`).
    exception_resolve, "/api/v1/academic/monitoring-cases/{id}/exceptions/{exceptionId}/resolve", "monitoring_exception_resolve", "exceptionId", exception_step, "resolved");
step_cmd!(
    /// Independent validation of the resolution → closed (not by the owner).
    exception_validate, "/api/v1/academic/monitoring-cases/{id}/exceptions/{exceptionId}/validate", "monitoring_exception_validate", "exceptionId", exception_step, "closed");
step_cmd!(
    /// Escalate the open exception (`reason`).
    exception_escalate, "/api/v1/academic/monitoring-cases/{id}/exceptions/{exceptionId}/escalate", "monitoring_exception_escalate", "exceptionId", exception_step, "escalated");
step_cmd!(
    /// Withdraw the exception (`reason`).
    exception_withdraw, "/api/v1/academic/monitoring-cases/{id}/exceptions/{exceptionId}/withdraw", "monitoring_exception_withdraw", "exceptionId", exception_step, "withdrawn");
step_cmd!(
    /// Assign the accountable intervention owner (`ownerSubject`, `dueOn`).
    intervention_assign, "/api/v1/academic/monitoring-cases/{id}/interventions/{interventionId}/assign", "monitoring_intervention_assign", "interventionId", intervention_step, "assign");
step_cmd!(
    /// Trigger the intervention (owner required) → `monitoring_intervention.triggered`.
    intervention_trigger, "/api/v1/academic/monitoring-cases/{id}/interventions/{interventionId}/trigger", "monitoring_intervention_trigger", "interventionId", intervention_step, "trigger");
step_cmd!(
    /// The owner begins the corrective action.
    intervention_start, "/api/v1/academic/monitoring-cases/{id}/interventions/{interventionId}/start", "monitoring_intervention_start", "interventionId", intervention_step, "start");
step_cmd!(
    /// Complete the intervention (`outcome`).
    intervention_complete, "/api/v1/academic/monitoring-cases/{id}/interventions/{interventionId}/complete", "monitoring_intervention_complete", "interventionId", intervention_step, "complete");
step_cmd!(
    /// Cancel the intervention (`reason`).
    intervention_cancel, "/api/v1/academic/monitoring-cases/{id}/interventions/{interventionId}/cancel", "monitoring_intervention_cancel", "interventionId", intervention_step, "cancel");

/// All monitoring routes.
pub fn router(state: AcademicState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(case_list, case_open))
        .routes(routes!(case_get))
        .routes(routes!(risk_assess))
        .routes(routes!(outcome_publish))
        .routes(routes!(review_submit))
        .routes(routes!(case_decision))
        .routes(routes!(case_certify))
        .routes(routes!(case_close))
        .routes(routes!(case_reopen))
        .routes(routes!(exception_raise))
        .routes(routes!(exception_plan))
        .routes(routes!(exception_resolve))
        .routes(routes!(exception_validate))
        .routes(routes!(exception_escalate))
        .routes(routes!(exception_withdraw))
        .routes(routes!(intervention_assign))
        .routes(routes!(intervention_trigger))
        .routes(routes!(intervention_start))
        .routes(routes!(intervention_complete))
        .routes(routes!(intervention_cancel))
        .with_state(state)
}
