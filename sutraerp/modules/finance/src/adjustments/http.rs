//! HTTP API of adjustments (tag `finance-adjustments`).

use super::repo::{self, AdjustmentRow, NewAdjustment};
use crate::charges::kit::{
    if_match, invalid, parse_amount, require_text, DecisionBody, DecisionKind, ListQuery,
    ReasonBody,
};
use crate::shared::guard::require_institution;
use crate::shared::http::{actor, with_etag};
use crate::shared::money::fmt;
use crate::shared::scope::institution_scope;
use crate::shared::FinanceState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use serde::{Deserialize, Serialize};
use sutra_contracts::finance::ids::{AdjustmentId, ChargeId};
use sutra_contracts::finance::status::{AdjustmentKind, AdjustmentStatus};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::scope::InstitutionId;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// A financial adjustment (amounts are decimal strings, INR; `amount` is always positive and
/// its direction follows `kind`).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AdjustmentView {
    pub id: Uuid,
    pub adjustment_number: String,
    pub charge_id: Uuid,
    pub charge_number: String,
    pub student_account_id: Uuid,
    pub kind: AdjustmentKind,
    #[schema(example = "5000.00")]
    pub amount: String,
    /// Signed effect on the charge (`-5000.00` for a credit).
    pub effect: String,
    pub currency: String,
    pub reason: String,
    /// `manual | charge_revision | dispute | correction`.
    pub origin: String,
    pub status: AdjustmentStatus,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub decided_by: Option<String>,
    pub decided_at: Option<chrono::DateTime<chrono::Utc>>,
    pub posted_at: Option<chrono::DateTime<chrono::Utc>>,
    pub posting_date: Option<chrono::NaiveDate>,
    pub journal_entry_id: Option<Uuid>,
    pub reversal_entry_id: Option<Uuid>,
    pub supersedes_id: Option<Uuid>,
    pub superseded_by_id: Option<Uuid>,
    pub version: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl From<&AdjustmentRow> for AdjustmentView {
    fn from(a: &AdjustmentRow) -> Self {
        Self {
            id: a.id.as_uuid(),
            adjustment_number: a.adjustment_number.clone(),
            charge_id: a.charge_id.as_uuid(),
            charge_number: a.charge_number.clone(),
            student_account_id: a.student_account_id.as_uuid(),
            kind: a.kind,
            amount: fmt(a.amount),
            effect: fmt(super::domain::delta(a.kind, a.amount)),
            currency: a.currency.clone(),
            reason: a.reason.clone(),
            origin: a.origin.clone(),
            status: a.status,
            status_reason: a.status_reason.clone(),
            process_instance_id: a.process_instance_id.clone(),
            decided_by: a.decided_by.clone(),
            decided_at: a.decided_at,
            posted_at: a.posted_at,
            posting_date: a.posting_date,
            journal_entry_id: a.journal_entry_id,
            reversal_entry_id: a.reversal_entry_id,
            supersedes_id: a.supersedes_id,
            superseded_by_id: a.superseded_by_id,
            version: a.version,
            created_at: a.created_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AdjustmentList {
    pub items: Vec<AdjustmentView>,
}

/// New adjustment draft of a charge.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AdjustmentCreate {
    pub kind: AdjustmentKind,
    #[schema(example = "5000.00")]
    pub amount: String,
    pub reason: String,
}

/// Corrected successor of an erroneous adjustment.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AdjustmentCorrect {
    /// Defaults to the original kind.
    pub kind: Option<AdjustmentKind>,
    #[schema(example = "4500.00")]
    pub amount: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct AdjustmentQuery {
    pub status: Option<AdjustmentStatus>,
    pub kind: Option<AdjustmentKind>,
    /// ≤ 200 (default 50).
    pub limit: Option<i64>,
}

fn ok(a: &AdjustmentRow, status: StatusCode) -> Response {
    with_etag(status, &AdjustmentView::from(a), Version(a.version))
}

/// Draft a credit note, debit note, write-off, waiver or concession on a charge (`adjustment.manage`).
#[utoipa::path(post, path = "/api/v1/finance/charges/{id}/adjustments", operation_id = "finance_adjustment_create", tag = "finance-adjustments",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = AdjustmentCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = AdjustmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn adjustment_create(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<AdjustmentCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AdjustmentManage)?;
    require_idempotency_key(&headers)?;
    let amount = parse_amount("amount", &b.amount)?;
    let reason = require_text("reason", Some(&b.reason))?;
    let a = repo::create(
        &st,
        ctx.scope.tenant_id,
        ChargeId(id),
        NewAdjustment {
            kind: b.kind,
            amount,
            reason: &reason,
            origin: "manual",
            supersedes: None,
        },
        &actor(ctx),
    )
    .await?;
    Ok(ok(&a, StatusCode::CREATED))
}

/// Adjustments of a charge, oldest first (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/charges/{id}/adjustments", operation_id = "finance_adjustment_list_of_charge", tag = "finance-adjustments",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = AdjustmentList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn adjustment_list_of_charge(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<AdjustmentList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let mut conn = st
        .pool
        .acquire()
        .await
        .map_err(sutra_persistence::map_db_error)?;
    crate::shared::charge::load_charge(&mut conn, ctx.scope.tenant_id, ChargeId(id)).await?;
    drop(conn);
    let items = repo::list_of_charge(&st, ctx.scope.tenant_id, ChargeId(id))
        .await?
        .iter()
        .map(AdjustmentView::from)
        .collect();
    Ok(Json(AdjustmentList { items }))
}

/// Adjustments of an institution, newest first (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/adjustments", operation_id = "finance_adjustment_list", tag = "finance-adjustments",
    params(("id" = String, Path, format = Uuid), AdjustmentQuery), security(("session_cookie" = [])),
    responses((status = 200, body = AdjustmentList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn adjustment_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<AdjustmentQuery>,
) -> ApiResult<Json<AdjustmentList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let scope = institution_scope(ctx, InstitutionId(id));
    require_institution(&st, &scope, InstitutionId(id)).await?;
    let limit = ListQuery {
        status: None,
        limit: q.limit,
    }
    .limit();
    let items = repo::list_of_institution(
        &st,
        ctx.scope.tenant_id,
        InstitutionId(id),
        q.status,
        q.kind,
        limit,
    )
    .await?
    .iter()
    .map(AdjustmentView::from)
    .collect();
    Ok(Json(AdjustmentList { items }))
}

/// One adjustment (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/adjustments/{id}", operation_id = "finance_adjustment_get", tag = "finance-adjustments",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = AdjustmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn adjustment_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let a = repo::get(&st, ctx.scope.tenant_id, AdjustmentId(id)).await?;
    Ok(ok(&a, StatusCode::OK))
}

/// Submit a draft for approval (WorkflowPort `finance.adjustment_approval`); auto-approval posts it at once (`adjustment.manage`).
#[utoipa::path(post, path = "/api/v1/finance/adjustments/{id}/submit", operation_id = "finance_adjustment_submit", tag = "finance-adjustments",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the adjustment"), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body(content = Option<ReasonBody>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AdjustmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn adjustment_submit(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    _body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AdjustmentManage)?;
    let v = if_match!(headers);
    require_idempotency_key(&headers)?;
    let a = repo::submit(&st, ctx.scope.tenant_id, AdjustmentId(id), v, &actor(ctx)).await?;
    Ok(ok(&a, StatusCode::OK))
}

/// Approve (post) or reject a pending adjustment (`finance.approve`, step-up).
#[utoipa::path(post, path = "/api/v1/finance/adjustments/{id}/decision", operation_id = "finance_adjustment_decide", tag = "finance-adjustments",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the adjustment"), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = DecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AdjustmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn adjustment_decide(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<DecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceApprove)?;
    let v = if_match!(headers);
    require_idempotency_key(&headers)?;
    let a = repo::decide(
        &st,
        ctx.scope.tenant_id,
        AdjustmentId(id),
        v,
        b.decision == DecisionKind::Approve,
        b.reason.as_deref(),
        &actor(ctx),
    )
    .await?;
    Ok(ok(&a, StatusCode::OK))
}

/// Reverse a posted adjustment: mirror journal entry and the opposite charge effect (`finance.approve`, step-up; reason required).
#[utoipa::path(post, path = "/api/v1/finance/adjustments/{id}/reverse", operation_id = "finance_adjustment_reverse", tag = "finance-adjustments",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the adjustment"), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AdjustmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn adjustment_reverse(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceApprove)?;
    let v = if_match!(headers);
    require_idempotency_key(&headers)?;
    let reason = require_text("reason", b.reason.as_deref())?;
    let a = repo::reverse(
        &st,
        ctx.scope.tenant_id,
        AdjustmentId(id),
        v,
        &reason,
        &actor(ctx),
    )
    .await?;
    Ok(ok(&a, StatusCode::OK))
}

/// Correct an erroneous adjustment: the original is superseded (reversed in the ledger when it was posted) and a corrected successor draft is returned (`adjustment.manage`; `finance.approve` when the original was posted).
#[utoipa::path(post, path = "/api/v1/finance/adjustments/{id}/correct", operation_id = "finance_adjustment_correct", tag = "finance-adjustments",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the original adjustment"), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = AdjustmentCorrect, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = AdjustmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn adjustment_correct(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<AdjustmentCorrect>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AdjustmentManage)?;
    let v = if_match!(headers);
    require_idempotency_key(&headers)?;
    let original = repo::get(&st, ctx.scope.tenant_id, AdjustmentId(id)).await?;
    if original.status == AdjustmentStatus::Posted {
        auth.require(Permission::FinanceApprove)?;
    }
    let amount = parse_amount("amount", &b.amount)?;
    let reason = require_text("reason", Some(&b.reason))?;
    // A different kind is allowed (e.g. a credit note that should have been a write-off).
    if amount == original.amount && b.kind.is_none_or(|k| k == original.kind) {
        return Err(invalid(
            "amount",
            "unchanged",
            "the correction must change the amount or the kind",
        )
        .into());
    }
    let a = repo::correct(
        &st,
        ctx.scope.tenant_id,
        AdjustmentId(id),
        v,
        b.kind,
        amount,
        &reason,
        &actor(ctx),
    )
    .await?;
    Ok(ok(&a, StatusCode::CREATED))
}

pub fn router(state: FinanceState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(adjustment_create, adjustment_list_of_charge))
        .routes(routes!(adjustment_list))
        .routes(routes!(adjustment_get))
        .routes(routes!(adjustment_submit))
        .routes(routes!(adjustment_decide))
        .routes(routes!(adjustment_reverse))
        .routes(routes!(adjustment_correct))
        .with_state(state)
}
