//! HTTP API of certificates, credit mapping, credit recognition and reconciliation (tag
//! `credit-recognition`; contract §5.2). Reads need `academic.read`; certificate, mapping and
//! reconciliation writes need `flexible_learning.manage`; the recognition decision and the
//! posting need `credit_recognition.decide` (step-up). Students upload certificates for their
//! own enrolments under `/me/flexible-learning` (tag `flexible-learning`).

use super::certificate::{self, *};
use super::credit::{self, *};
use super::mapping::{self, *};
use super::reconciliation::{self, *};
use crate::flexible::kit::{
    self, created, expect_version, ok, FlexibleDecision, FlexibleDecisionValue, FlexibleReason,
};
use crate::shared::http::actor;
use crate::shared::{me, AcademicState};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::Json;
use sutra_kernel::auth::Permission;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RecognitionStatusFilter {
    pub status: Option<String>,
}

// ============================================================== certificates (L3-D-043)

/// Certificates recorded for a flexible enrolment.
#[utoipa::path(get, path = "/api/v1/academic/flexible-enrolments/{id}/certificates", operation_id = "certificate_verification_list", tag = "credit-recognition",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = CertificateVerificationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn cert_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<CertificateVerificationList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    Ok(Json(CertificateVerificationList {
        items: certificate::list_for_enrolment(&st, ctx.scope.tenant_id, id).await?,
    }))
}

/// Record certificate evidence for a registered / completed participation.
#[utoipa::path(post, path = "/api/v1/academic/flexible-enrolments/{id}/certificates", operation_id = "certificate_verification_submit", tag = "credit-recognition",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewCertificate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = CertificateVerification, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn cert_submit(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewCertificate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    require_idempotency_key(&headers)?;
    let c = certificate::submit(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&c, c.version))
}

/// Certificate verifications of an institution (verification queue).
#[utoipa::path(get, path = "/api/v1/academic/institutions/{id}/certificate-verifications", operation_id = "certificate_verification_queue", tag = "credit-recognition",
    params(("id" = String, Path, format = Uuid), RecognitionStatusFilter), security(("session_cookie" = [])),
    responses((status = 200, body = CertificateVerificationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn cert_queue(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(f): Query<RecognitionStatusFilter>,
) -> ApiResult<Json<CertificateVerificationList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    Ok(Json(CertificateVerificationList {
        items: certificate::list_for_institution(&st, ctx.scope.tenant_id, id, f.status.as_deref())
            .await?,
    }))
}

/// A certificate verification with its history.
#[utoipa::path(get, path = "/api/v1/academic/certificate-verifications/{id}", operation_id = "certificate_verification_get", tag = "credit-recognition",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = CertificateVerification, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn cert_get(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let c = certificate::get(&st, ctx.scope.tenant_id, id).await?;
    Ok(ok(&c, c.version))
}

/// Record the verification checks (`CERTIFICATE_VERIFICATION`; auto-approval records the derived outcome).
#[utoipa::path(post, path = "/api/v1/academic/certificate-verifications/{id}/verify", operation_id = "certificate_verification_verify", tag = "credit-recognition",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the verification")),
    request_body = VerifyCertificate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CertificateVerification, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn cert_verify(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<VerifyCertificate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    let v = expect_version!(headers);
    let c = certificate::verify(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&c, c.version))
}

/// Explicit verification outcome (accepted / rejected / flagged).
#[utoipa::path(post, path = "/api/v1/academic/certificate-verifications/{id}/decision", operation_id = "certificate_verification_decision", tag = "credit-recognition",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the verification")),
    request_body = CertificateDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CertificateVerification, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn cert_decision(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<CertificateDecision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "certificate verification").await?;
    let v = expect_version!(headers);
    let c = certificate::decide(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&c, c.version))
}

/// Upload a certificate for one of my own flexible enrolments.
#[utoipa::path(post, path = "/api/v1/academic/me/flexible-learning/{id}/certificates", operation_id = "flexible_self_certificate_submit", tag = "flexible-learning",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewCertificate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = CertificateVerification, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn self_cert_submit(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewCertificate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AcademicSelfService)?;
    require_idempotency_key(&headers)?;
    crate::flexible::http::own_enrolment(&st, ctx, id).await?;
    let c = certificate::submit(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&c, c.version))
}

/// My credit recognitions.
#[utoipa::path(get, path = "/api/v1/academic/me/flexible-learning/recognitions", operation_id = "flexible_self_recognitions", tag = "flexible-learning",
    security(("session_cookie" = [])),
    responses((status = 200, body = CreditRecognitionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn self_recognitions(
    State(st): State<AcademicState>,
    auth: Authenticated,
) -> ApiResult<Json<CreditRecognitionList>> {
    let ctx = auth.require(Permission::AcademicSelfService)?;
    let s = me::me(&st, ctx).await?;
    Ok(Json(CreditRecognitionList {
        items: credit::list_for_student(&st, ctx.scope.tenant_id, s.id.as_uuid()).await?,
    }))
}

// ============================================================== credit mapping rules (L3-D-046)

/// Credit mapping rule versions of an institution (`?opportunityId=`, `?status=`).
#[utoipa::path(get, path = "/api/v1/academic/institutions/{id}/credit-mapping-rules", operation_id = "credit_mapping_list", tag = "credit-recognition",
    params(("id" = String, Path, format = Uuid), MappingFilter), security(("session_cookie" = [])),
    responses((status = 200, body = CreditMappingRuleList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn mapping_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(f): Query<MappingFilter>,
) -> ApiResult<Json<CreditMappingRuleList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    Ok(Json(CreditMappingRuleList {
        items: mapping::list(&st, ctx.scope.tenant_id, id, &f).await?,
    }))
}

/// Draft a new mapping version for an opportunity.
#[utoipa::path(post, path = "/api/v1/academic/institutions/{id}/credit-mapping-rules", operation_id = "credit_mapping_create", tag = "credit-recognition",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewCreditMappingRule, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = CreditMappingRule, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn mapping_create(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewCreditMappingRule>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    require_idempotency_key(&headers)?;
    let r = mapping::create(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&r, r.version))
}

/// A mapping rule version with its history.
#[utoipa::path(get, path = "/api/v1/academic/credit-mapping-rules/{id}", operation_id = "credit_mapping_get", tag = "credit-recognition",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = CreditMappingRule, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn mapping_get(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let r = mapping::get(&st, ctx.scope.tenant_id, id).await?;
    Ok(ok(&r, r.version))
}

/// Submit for approval (`CREDIT_MAPPING_RULE`; auto-approval publishes it).
#[utoipa::path(post, path = "/api/v1/academic/credit-mapping-rules/{id}/submit", operation_id = "credit_mapping_submit", tag = "credit-recognition",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the rule")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CreditMappingRule, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn mapping_submit(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    let v = expect_version!(headers);
    let r = mapping::submit(&st, ctx, id, v, &actor(ctx)).await?;
    Ok(ok(&r, r.version))
}

/// Decide the pending mapping approval.
#[utoipa::path(post, path = "/api/v1/academic/credit-mapping-rules/{id}/decision", operation_id = "credit_mapping_decision", tag = "credit-recognition",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the rule")),
    request_body = FlexibleDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CreditMappingRule, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn mapping_decision(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<FlexibleDecision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "credit mapping rule").await?;
    let v = expect_version!(headers);
    let approve = cmd.decision == FlexibleDecisionValue::Approve;
    let r = mapping::decide(
        &st,
        ctx,
        id,
        v,
        approve,
        kit::opt_text(cmd.reason.as_deref()),
        &actor(ctx),
    )
    .await?;
    Ok(ok(&r, r.version))
}

/// Retire a mapping version.
#[utoipa::path(post, path = "/api/v1/academic/credit-mapping-rules/{id}/retire", operation_id = "credit_mapping_retire", tag = "credit-recognition",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the rule")),
    request_body(content = Option<FlexibleReason>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CreditMappingRule, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn mapping_retire(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<FlexibleReason>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    let v = expect_version!(headers);
    let r = mapping::retire(&st, ctx, id, v, kit::reason(body), &actor(ctx)).await?;
    Ok(ok(&r, r.version))
}

// ============================================================== credit recognition (L3-D-039)

/// Credit recognitions of an institution.
#[utoipa::path(get, path = "/api/v1/academic/institutions/{id}/credit-recognitions", operation_id = "credit_recognition_list", tag = "credit-recognition",
    params(("id" = String, Path, format = Uuid), RecognitionStatusFilter), security(("session_cookie" = [])),
    responses((status = 200, body = CreditRecognitionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn recognition_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(f): Query<RecognitionStatusFilter>,
) -> ApiResult<Json<CreditRecognitionList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    Ok(Json(CreditRecognitionList {
        items: credit::list(&st, ctx.scope.tenant_id, id, f.status.as_deref()).await?,
    }))
}

/// Open the credit recognition of a completed participation (a repeat returns the live one).
#[utoipa::path(post, path = "/api/v1/academic/flexible-enrolments/{id}/credit-recognitions", operation_id = "credit_recognition_request", tag = "credit-recognition",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body(content = Option<NewCreditRecognition>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = CreditRecognition, headers(("ETag" = String))), (status = 200, body = CreditRecognition), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn recognition_request(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<NewCreditRecognition>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    require_idempotency_key(&headers)?;
    let cmd = body.map(|Json(b)| b).unwrap_or_default();
    let (r, new) = credit::request(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(if new {
        created(&r, r.version)
    } else {
        ok(&r, r.version)
    })
}

/// A credit recognition with postings and history.
#[utoipa::path(get, path = "/api/v1/academic/credit-recognitions/{id}", operation_id = "credit_recognition_get", tag = "credit-recognition",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = CreditRecognition, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn recognition_get(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let r = credit::get(&st, ctx.scope.tenant_id, id).await?;
    Ok(ok(&r, r.version))
}

/// Governed recognition decision (approve — optionally partial — or reject).
#[utoipa::path(post, path = "/api/v1/academic/credit-recognitions/{id}/decision", operation_id = "credit_recognition_decision", tag = "credit-recognition",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the recognition")),
    request_body = CreditRecognitionDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CreditRecognition, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn recognition_decision(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<CreditRecognitionDecision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CreditRecognitionDecide)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "credit recognition").await?;
    let v = expect_version!(headers);
    let r = credit::decide(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&r, r.version))
}

/// Post approved credits into the Domain C ledger through `CreditPostingPort` (idempotent).
#[utoipa::path(post, path = "/api/v1/academic/credit-recognitions/{id}/post", operation_id = "credit_recognition_post", tag = "credit-recognition",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the recognition")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CreditRecognition, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn recognition_post(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CreditRecognitionDecide)?;
    let v = expect_version!(headers);
    let r = credit::post(&st, ctx, id, v, &actor(ctx)).await?;
    Ok(ok(&r, r.version))
}

// ============================================================== reconciliation (L3-D-049)

/// Reconciliation runs of an institution.
#[utoipa::path(get, path = "/api/v1/academic/institutions/{id}/reconciliation-runs", operation_id = "reconciliation_list", tag = "credit-recognition",
    params(("id" = String, Path, format = Uuid), RecognitionStatusFilter), security(("session_cookie" = [])),
    responses((status = 200, body = ReconciliationRunList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn reconciliation_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(f): Query<RecognitionStatusFilter>,
) -> ApiResult<Json<ReconciliationRunList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    Ok(Json(ReconciliationRunList {
        items: reconciliation::list(&st, ctx.scope.tenant_id, id, f.status.as_deref()).await?,
    }))
}

/// Compare provider records with the institutional records of an opportunity.
#[utoipa::path(post, path = "/api/v1/academic/institutions/{id}/reconciliation-runs", operation_id = "reconciliation_run", tag = "credit-recognition",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewReconciliationRun, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ReconciliationRun, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn reconciliation_run(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewReconciliationRun>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    require_idempotency_key(&headers)?;
    let r = reconciliation::run(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&r, r.version))
}

/// A reconciliation run with both evidence sets and its discrepancies.
#[utoipa::path(get, path = "/api/v1/academic/reconciliation-runs/{id}", operation_id = "reconciliation_get", tag = "credit-recognition",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ReconciliationRun, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn reconciliation_get(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let r = reconciliation::get(&st, ctx.scope.tenant_id, id).await?;
    Ok(ok(&r, r.version))
}

/// Resolve the discrepancies (uphold the institutional record, or escalate to an exception case).
#[utoipa::path(post, path = "/api/v1/academic/reconciliation-runs/{id}/resolve", operation_id = "reconciliation_resolve", tag = "credit-recognition",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the run")),
    request_body = ReconciliationResolution, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ReconciliationRun, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn reconciliation_resolve(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<ReconciliationResolution>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FlexibleLearningManage)?;
    let v = expect_version!(headers);
    let r = reconciliation::resolve(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&r, r.version))
}

/// All certificate / mapping / recognition / reconciliation routes.
pub fn router(state: AcademicState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(cert_list, cert_submit))
        .routes(routes!(cert_queue))
        .routes(routes!(cert_get))
        .routes(routes!(cert_verify))
        .routes(routes!(cert_decision))
        .routes(routes!(self_cert_submit))
        .routes(routes!(self_recognitions))
        .routes(routes!(mapping_list, mapping_create))
        .routes(routes!(mapping_get))
        .routes(routes!(mapping_submit))
        .routes(routes!(mapping_decision))
        .routes(routes!(mapping_retire))
        .routes(routes!(recognition_list))
        .routes(routes!(recognition_request))
        .routes(routes!(recognition_get))
        .routes(routes!(recognition_decision))
        .routes(routes!(recognition_post))
        .routes(routes!(reconciliation_list, reconciliation_run))
        .routes(routes!(reconciliation_get))
        .routes(routes!(reconciliation_resolve))
        .with_state(state)
}
