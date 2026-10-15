//! HTTP API of financial aid (tag `financial-aid`).

use super::repo::{self, AidAllocationView, AidReconciliationView, Mode};
use crate::charges::kit::{
    if_match, invalid, parse_amount, require_text, DecisionBody, DecisionKind,
};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::me::my_accounts;
use crate::shared::money::fmt;
use crate::shared::scope::institution_scope;
use crate::shared::FinanceState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use serde::{Deserialize, Serialize};
use sutra_contracts::finance::ids::{AidAllocationId, ChargeId, FeeHeadId};
use sutra_contracts::finance::status::AidAllocationStatus;
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::scope::InstitutionId;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// A scholarship decision Domain C handed off to Financial Aid, with its allocation (if any).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AidDecisionView {
    pub eligibility_id: Uuid,
    pub scholarship_id: Uuid,
    pub scholarship_code: String,
    pub scholarship_name: String,
    pub sponsor_type: String,
    pub benefit_type: String,
    pub benefit_value: Option<String>,
    pub currency: String,
    pub student_id: Uuid,
    pub academic_year_id: Option<Uuid>,
    /// Eligibility status in Domain C (`active | renewal` are live).
    pub status: String,
    pub live: bool,
    pub valid_from: Option<chrono::NaiveDate>,
    pub valid_until: Option<chrono::NaiveDate>,
    pub renewal_count: i32,
    pub handed_off_at: chrono::DateTime<chrono::Utc>,
    pub allocation_id: Option<Uuid>,
    pub allocation_status: Option<AidAllocationStatus>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AidDecisionList {
    pub items: Vec<AidDecisionView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AidAllocationList {
    pub items: Vec<AidAllocationView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AidReconciliationList {
    pub items: Vec<AidReconciliationView>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AidAllocationCreate {
    /// The handed-off scholarship eligibility (idempotency key of the allocation).
    pub eligibility_id: Uuid,
}

/// Disbursement: `charge_adjustment` (apply to `chargeId`) or `cash` (stipend).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AidDisburse {
    pub mode: String,
    pub charge_id: Option<Uuid>,
    /// Defaults to the computed benefit (charge) or what remains (cash).
    #[schema(example = "30000.00")]
    pub amount: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AidRevoke {
    pub reason: String,
    /// Raise the recovery charge for the utilised aid at once.
    #[serde(default)]
    pub recover: bool,
    /// Fee head of the recovery charge (defaults to the first aided charge's fee head).
    pub fee_head_id: Option<Uuid>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AidRecover {
    pub reason: String,
    pub amount: Option<String>,
    pub fee_head_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AidReconcile {
    pub scholarship_id: Option<Uuid>,
    pub academic_year_id: Option<Uuid>,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct AidQuery {
    pub status: Option<AidAllocationStatus>,
}

fn ok(a: &AidAllocationView, status: StatusCode) -> Response {
    with_etag(status, a, Version(a.version))
}

/// Scholarship decisions handed off to Financial Aid by Student Records (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/aid-decisions", operation_id = "finance_aid_decision_list", tag = "financial-aid",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = AidDecisionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn aid_decision_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<AidDecisionList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let scope = institution_scope(ctx, InstitutionId(id));
    require_institution(&st, &scope, InstitutionId(id)).await?;
    let mut items = Vec::new();
    for d in st
        .aid
        .handed_off_decisions(&scope, InstitutionId(id), None)
        .await?
    {
        let alloc =
            repo::allocation_of_decision(&st, ctx.scope.tenant_id, d.eligibility_id).await?;
        items.push(AidDecisionView {
            eligibility_id: d.eligibility_id,
            scholarship_id: d.scholarship_id,
            scholarship_code: d.scholarship_code.clone(),
            scholarship_name: d.scholarship_name.clone(),
            sponsor_type: d.sponsor_type.clone(),
            benefit_type: d.benefit_type.clone(),
            benefit_value: d.benefit_value.map(fmt),
            currency: d.currency.clone(),
            student_id: d.student_id.as_uuid(),
            academic_year_id: d.academic_year_id,
            live: d.is_live(),
            status: d.status.clone(),
            valid_from: d.valid_from,
            valid_until: d.valid_until,
            renewal_count: d.renewal_count,
            handed_off_at: d.handed_off_at,
            allocation_id: alloc.map(|a| a.0),
            allocation_status: alloc.map(|a| a.1),
        });
    }
    Ok(Json(AidDecisionList { items }))
}

/// Aid allocations of an institution (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/aid-allocations", operation_id = "finance_aid_allocation_list", tag = "financial-aid",
    params(("id" = String, Path, format = Uuid), AidQuery), security(("session_cookie" = [])),
    responses((status = 200, body = AidAllocationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn aid_allocation_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<AidQuery>,
) -> ApiResult<Json<AidAllocationList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let scope = institution_scope(ctx, InstitutionId(id));
    require_institution(&st, &scope, InstitutionId(id)).await?;
    let items = repo::list(
        &st,
        ctx.scope.tenant_id,
        Some(InstitutionId(id)),
        None,
        q.status,
    )
    .await?;
    Ok(Json(AidAllocationList { items }))
}

/// Allocate aid from a handed-off decision (idempotent per decision; approval through `finance.aid_allocation`; `aid.manage`).
#[utoipa::path(post, path = "/api/v1/finance/institutions/{id}/aid-allocations", operation_id = "finance_aid_allocation_create", tag = "financial-aid",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = AidAllocationCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = AidAllocationView, headers(("ETag" = String))), (status = 200, body = AidAllocationView, description = "Already allocated"), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn aid_allocation_create(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<AidAllocationCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AidManage)?;
    require_idempotency_key(&headers)?;
    let scope = institution_scope(ctx, InstitutionId(id));
    require_institution_writable(&st, &scope, InstitutionId(id)).await?;
    let (a, created) = repo::allocate(
        &st,
        &scope,
        InstitutionId(id),
        b.eligibility_id,
        &actor(ctx),
    )
    .await?;
    Ok(ok(
        &a,
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
    ))
}

/// One aid allocation with its disbursements (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/aid-allocations/{id}", operation_id = "finance_aid_allocation_get", tag = "financial-aid",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = AidAllocationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn aid_allocation_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let a = repo::get(&st, ctx.scope.tenant_id, AidAllocationId(id)).await?;
    Ok(ok(&a, StatusCode::OK))
}

/// Approve (allocate) or reject a proposed allocation (`finance.approve`, step-up).
#[utoipa::path(post, path = "/api/v1/finance/aid-allocations/{id}/approve", operation_id = "finance_aid_allocation_approve", tag = "financial-aid",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the allocation")),
    request_body = DecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AidAllocationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn aid_allocation_approve(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<DecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceApprove)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision_for_row(
        &st.pool,
        ctx,
        "finance.aid_allocation",
        "id",
        id,
        &[],
        "aid allocation",
    )
    .await?;
    let v = if_match!(headers);
    let a = repo::decide(
        &st,
        ctx.scope.tenant_id,
        AidAllocationId(id),
        v,
        b.decision == DecisionKind::Approve,
        b.reason,
        &actor(ctx),
    )
    .await?;
    Ok(ok(&a, StatusCode::OK))
}

/// Disburse aid: apply it to one of the student's charges (Dr aid expense / Cr receivables) or pay it in cash (Dr aid expense / Cr refunds payable; `aid.manage`).
#[utoipa::path(post, path = "/api/v1/finance/aid-allocations/{id}/disburse", operation_id = "finance_aid_allocation_disburse", tag = "financial-aid",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the allocation"), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = AidDisburse, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AidAllocationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn aid_allocation_disburse(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<AidDisburse>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AidManage)?;
    let v = if_match!(headers);
    require_idempotency_key(&headers)?;
    let mode = match (b.mode.as_str(), b.charge_id) {
        ("charge_adjustment", Some(c)) => Mode::ChargeAdjustment(ChargeId(c)),
        ("charge_adjustment", None) => {
            return Err(invalid("chargeId", "required", "the charge to apply the aid to").into())
        }
        ("cash", _) => Mode::Cash,
        _ => return Err(invalid("mode", "invalid", "charge_adjustment or cash").into()),
    };
    let amount = match b.amount.as_deref() {
        Some(raw) => Some(parse_amount("amount", raw)?),
        None => None,
    };
    let a = repo::disburse(
        &st,
        ctx.scope.tenant_id,
        AidAllocationId(id),
        v,
        mode,
        amount,
        &actor(ctx),
    )
    .await?;
    Ok(ok(&a, StatusCode::OK))
}

/// Renew an allocation from a renewed Domain C decision (`aid.manage`).
#[utoipa::path(post, path = "/api/v1/finance/aid-allocations/{id}/renew", operation_id = "finance_aid_allocation_renew", tag = "financial-aid",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the allocation")),
    request_body(content = Option<crate::charges::kit::ReasonBody>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AidAllocationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn aid_allocation_renew(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    _body: Option<Json<crate::charges::kit::ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AidManage)?;
    let v = if_match!(headers);
    let a = repo::renew(
        &st,
        ctx.scope.tenant_id,
        AidAllocationId(id),
        v,
        &actor(ctx),
    )
    .await?;
    Ok(ok(&a, StatusCode::OK))
}

/// Revoke an allocation, optionally recovering the utilised aid through a published recovery charge (`finance.approve`, step-up).
#[utoipa::path(post, path = "/api/v1/finance/aid-allocations/{id}/revoke", operation_id = "finance_aid_allocation_revoke", tag = "financial-aid",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the allocation"), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = AidRevoke, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AidAllocationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn aid_allocation_revoke(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<AidRevoke>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceApprove)?;
    let v = if_match!(headers);
    require_idempotency_key(&headers)?;
    let reason = require_text("reason", Some(&b.reason))?;
    let a = repo::revoke(
        &st,
        ctx.scope.tenant_id,
        AidAllocationId(id),
        v,
        &reason,
        b.recover,
        b.fee_head_id.map(FeeHeadId),
        &actor(ctx),
    )
    .await?;
    Ok(ok(&a, StatusCode::OK))
}

/// Recover the utilised aid of a revoked allocation (published `aid_recovery` charge; `aid.manage`).
#[utoipa::path(post, path = "/api/v1/finance/aid-allocations/{id}/recover", operation_id = "finance_aid_allocation_recover", tag = "financial-aid",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the allocation"), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = AidRecover, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AidAllocationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn aid_allocation_recover(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<AidRecover>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AidManage)?;
    let v = if_match!(headers);
    require_idempotency_key(&headers)?;
    let reason = require_text("reason", Some(&b.reason))?;
    let amount = match b.amount.as_deref() {
        Some(raw) => Some(parse_amount("amount", raw)?),
        None => None,
    };
    let a = repo::recover(
        &st,
        ctx.scope.tenant_id,
        AidAllocationId(id),
        v,
        amount,
        b.fee_head_id.map(FeeHeadId),
        &reason,
        &actor(ctx),
    )
    .await?;
    Ok(ok(&a, StatusCode::OK))
}

/// Programme reconciliation runs of an institution, newest first (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/aid-reconciliations", operation_id = "finance_aid_reconciliation_list", tag = "financial-aid",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = AidReconciliationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn aid_reconciliation_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<AidReconciliationList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let scope = institution_scope(ctx, InstitutionId(id));
    require_institution(&st, &scope, InstitutionId(id)).await?;
    let items = repo::reconciliations(&st, ctx.scope.tenant_id, InstitutionId(id)).await?;
    Ok(Json(AidReconciliationList { items }))
}

/// Reconcile aid funding, utilisation and the ledger for a programme (new run each time; `aid.manage`).
#[utoipa::path(post, path = "/api/v1/finance/institutions/{id}/aid-reconciliations", operation_id = "finance_aid_reconciliation_run", tag = "financial-aid",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body(content = Option<AidReconcile>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = AidReconciliationView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn aid_reconciliation_run(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<AidReconcile>>,
) -> ApiResult<(StatusCode, Json<AidReconciliationView>)> {
    let ctx = auth.require(Permission::AidManage)?;
    require_idempotency_key(&headers)?;
    let scope = institution_scope(ctx, InstitutionId(id));
    require_institution(&st, &scope, InstitutionId(id)).await?;
    let b = body.map(|b| b.0).unwrap_or_default();
    let r = repo::reconcile(
        &st,
        &scope,
        InstitutionId(id),
        b.scholarship_id,
        b.academic_year_id,
        &actor(ctx),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(r)))
}

/// The caller's own aid allocations (`finance.self`).
#[utoipa::path(get, path = "/api/v1/finance/me/aid", operation_id = "finance_aid_self_list", tag = "financial-aid",
    security(("session_cookie" = [])),
    responses((status = 200, body = AidAllocationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn aid_self_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
) -> ApiResult<Json<AidAllocationList>> {
    let ctx = &auth.0;
    let accounts: Vec<Uuid> = my_accounts(&st, ctx)
        .await?
        .iter()
        .map(|a| a.id.as_uuid())
        .collect();
    if accounts.is_empty() {
        return Ok(Json(AidAllocationList { items: Vec::new() }));
    }
    let items = repo::list(&st, ctx.scope.tenant_id, None, Some(&accounts), None)
        .await?
        .into_iter()
        .filter(|a| {
            !matches!(
                a.status,
                AidAllocationStatus::Proposed | AidAllocationStatus::Approved
            )
        })
        .collect();
    Ok(Json(AidAllocationList { items }))
}

pub fn router(state: FinanceState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(aid_decision_list))
        .routes(routes!(aid_allocation_list, aid_allocation_create))
        .routes(routes!(aid_allocation_get))
        .routes(routes!(aid_allocation_approve))
        .routes(routes!(aid_allocation_disburse))
        .routes(routes!(aid_allocation_renew))
        .routes(routes!(aid_allocation_revoke))
        .routes(routes!(aid_allocation_recover))
        .routes(routes!(aid_reconciliation_list, aid_reconciliation_run))
        .routes(routes!(aid_self_list))
        .with_state(state)
}
