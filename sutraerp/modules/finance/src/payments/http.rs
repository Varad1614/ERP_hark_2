//! HTTP API of payments, receipts, the mock gateway checkout and the gateway webhook
//! (tag `finance-payments`; contract §5.2). L3-F-007 … F-012.

use super::common::{parse_amount, require_text, require_version};
use super::service::{
    allocate_unapplied, close_unpaid, complete, insert_payment, load_payment, payment_from_row,
    reverse, AllocationLine, NewPayment, PAYMENT_COLUMNS,
};
use super::views::{
    payment_view, receipt_view, AllocationInput, PaymentList, PaymentView, ReceiptList, ReceiptView,
};
use crate::shared::approval::{business_key, process, request_approval};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::me::{my_accounts, require_own_account};
use crate::shared::money::INR;
use crate::shared::scope::{institution_scope, tenant, tenant_scope};
use crate::shared::time::business_today;
use crate::shared::{Actor, FinanceState};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::collections::BTreeMap;
use sutra_contracts::finance::ids::{ChargeId, StudentAccountId};
use sutra_contracts::finance::ports::{
    GatewayOrderRequest, GatewayPaymentOutcome, StudentAccountSummary,
};
use sutra_contracts::finance::status::{PaymentChannel, PaymentStatus, StudentAccountStatus};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

// ------------------------------------------------------------------ inputs

#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct PaymentListQuery {
    pub status: Option<PaymentStatus>,
    /// ≤ 200 (default 50).
    pub limit: Option<i64>,
    /// `nextCursor` of the previous page.
    pub cursor: Option<Uuid>,
}

/// Counter / bank / sponsor payment recorded by the finance office (L3-F-007/008).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RecordPaymentRequest {
    /// `cash | cheque | bank_transfer | demand_draft | sponsor` (gateway payments start at `/me/payments`).
    pub channel: PaymentChannel,
    #[schema(value_type = String, example = "50000.00")]
    pub amount: String,
    /// Defaults to INR (the only supported currency).
    pub currency: Option<String>,
    /// Value date (institution business date; defaults to today, never in the future).
    pub received_on: Option<NaiveDate>,
    /// Cheque / UTR / DD number.
    pub external_ref: Option<String>,
    /// Sponsor or payer name (sponsor payments).
    pub payer_name: Option<String>,
    /// Explicit allocation; omitted → open charges oldest due first, surplus kept unapplied.
    pub allocations: Option<Vec<AllocationInput>>,
}

/// Allocation of a payment's unapplied amount (omit `allocations` for oldest-due-first).
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AllocateRequest {
    pub allocations: Option<Vec<AllocationInput>>,
}

/// `{ "reason": "…" }` of a state command.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReasonRequest {
    pub reason: Option<String>,
}

/// Recovery of a pending / timed-out payment (L3-F-010).
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RecoverRequest {
    /// When the gateway still reports no outcome, expire the payment instead of leaving it pending.
    pub expire: Option<bool>,
    pub reason: Option<String>,
}

/// A student starts an online payment with the (mock) gateway.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitiatePaymentRequest {
    #[schema(value_type = String, example = "25000.00")]
    pub amount: String,
    /// Which of the caller's accounts (defaults to the only / first one).
    pub student_account_id: Option<Uuid>,
    /// Charges to pay (defaults to every open charge, oldest due first).
    pub charge_ids: Option<Vec<Uuid>>,
}

/// Acknowledgement of a gateway webhook.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WebhookAck {
    pub event_id: String,
    /// `processed | duplicate | ignored`.
    pub status: String,
    pub payment_id: Option<Uuid>,
    pub payment_status: Option<PaymentStatus>,
}

// ------------------------------------------------------------------ helpers

fn allocation_lines(
    input: Option<&[AllocationInput]>,
) -> Result<Option<Vec<AllocationLine>>, KernelError> {
    input
        .map(|lines| {
            if lines.is_empty() {
                return Err(KernelError::Validation(vec![FieldViolation::new(
                    "allocations",
                    "empty",
                    "give at least one allocation or omit the field",
                )]));
            }
            lines
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    Ok(AllocationLine {
                        charge_id: ChargeId(l.charge_id),
                        amount: parse_amount(&format!("allocations[{i}].amount"), &l.amount)?,
                    })
                })
                .collect()
        })
        .transpose()
}

/// Charges of `account` that accept a settlement, in allocation order (due date, number) —
/// read through the foundation `ChargeDirectory`.
pub async fn open_charge_order(
    st: &FinanceState,
    scope: &ExecutionScope,
    account: StudentAccountId,
    only: &[Uuid],
) -> Result<Vec<ChargeId>, KernelError> {
    Ok(st
        .charges
        .open_charges_of_account(scope, account)
        .await?
        .into_iter()
        .filter(|c| c.status.accepts_settlement())
        .filter(|c| only.is_empty() || only.contains(&c.id.as_uuid()))
        .map(|c| c.id)
        .collect())
}

async fn account_or_404(
    st: &FinanceState,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<StudentAccountSummary, KernelError> {
    st.charges
        .student_account(&tenant_scope(ctx), StudentAccountId(id))
        .await?
        .ok_or(KernelError::NotFound {
            entity: "student account",
        })
}

async fn view_response(
    st: &FinanceState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> ApiResult<Response> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let p = load_payment(&mut conn, tenant, id, false).await?;
    let version = p.version;
    let v = payment_view(&mut conn, p).await?;
    Ok(with_etag(status, &v, Version(version)))
}

async fn list_payments(
    st: &FinanceState,
    tenant: TenantId,
    institution: Option<Uuid>,
    account: Option<Uuid>,
    q: &PaymentListQuery,
) -> Result<PaymentList, KernelError> {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let rows = sqlx::query(&format!(
        "SELECT {PAYMENT_COLUMNS} FROM finance.payment WHERE tenant_id = $1 \
         AND ($2::uuid IS NULL OR institution_id = $2) AND ($3::uuid IS NULL OR student_account_id = $3) \
         AND ($4::text IS NULL OR status = $4) AND ($5::uuid IS NULL OR id < $5) \
         ORDER BY id DESC LIMIT $6"
    ))
    .bind(tenant.as_uuid())
    .bind(institution)
    .bind(account)
    .bind(q.status.map(|s| s.as_str()))
    .bind(q.cursor)
    .bind(limit)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        let p = payment_from_row(r).map_err(map_db_error)?;
        items.push(payment_view(&mut conn, p).await?);
    }
    let next_cursor = if items.len() as i64 == limit {
        items.last().map(|p| p.id.as_uuid())
    } else {
        None
    };
    Ok(PaymentList { items, next_cursor })
}

/// Applies a gateway outcome to a locked open payment (capture → completion, failure → failed).
async fn apply_outcome(
    st: &FinanceState,
    conn: &mut sqlx::PgConnection,
    p: &super::service::PaymentRow,
    outcome: &GatewayPaymentOutcome,
    actor: &Actor,
) -> Result<super::service::PaymentRow, KernelError> {
    let scope = ExecutionScope::institution(p.tenant_id, p.institution_id);
    match outcome {
        GatewayPaymentOutcome::Captured {
            payment_ref,
            amount,
        } => {
            if p.status == PaymentStatus::Completed {
                return Ok(p.clone());
            }
            if *amount != p.amount {
                return Err(KernelError::Conflict(format!(
                    "gateway captured {amount} but the payment is for {}",
                    p.amount
                )));
            }
            let today = business_today(st, &scope, p.institution_id).await?;
            let order =
                open_charge_order(st, &scope, p.student_account_id, &p.target_charge_ids).await?;
            complete(
                conn,
                &scope,
                p,
                None,
                &order,
                Some(payment_ref),
                today,
                actor,
            )
            .await
        }
        GatewayPaymentOutcome::Failed { code, reason } => {
            if matches!(p.status, PaymentStatus::Initiated | PaymentStatus::Pending) {
                close_unpaid(
                    conn,
                    &scope,
                    p,
                    PaymentStatus::Failed,
                    reason,
                    Some(code),
                    actor,
                )
                .await
            } else {
                Ok(p.clone())
            }
        }
        GatewayPaymentOutcome::Pending => Ok(p.clone()),
    }
}

// ------------------------------------------------------------------ staff: lists, record, get

/// Payments of an institution, newest first (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/payments", operation_id = "finance_payment_list", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid), PaymentListQuery), security(("session_cookie" = [])),
    responses((status = 200, body = PaymentList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn payment_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<PaymentListQuery>,
) -> ApiResult<Json<PaymentList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    require_institution(
        &st,
        &institution_scope(ctx, InstitutionId(id)),
        InstitutionId(id),
    )
    .await?;
    Ok(Json(
        list_payments(&st, tenant(ctx), Some(id), None, &q).await?,
    ))
}

/// Payments of one party account (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/student-accounts/{id}/payments", operation_id = "finance_payment_account_list", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid), PaymentListQuery), security(("session_cookie" = [])),
    responses((status = 200, body = PaymentList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn payment_account_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<PaymentListQuery>,
) -> ApiResult<Json<PaymentList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    account_or_404(&st, ctx, id).await?;
    Ok(Json(
        list_payments(&st, tenant(ctx), None, Some(id), &q).await?,
    ))
}

/// Records a counter / bank / sponsor payment: allocation, receipt and posting in one step
/// (`payment.manage`, `Idempotency-Key`).
#[utoipa::path(post, path = "/api/v1/finance/student-accounts/{id}/payments", operation_id = "finance_payment_record", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)), request_body = RecordPaymentRequest, security(("session_cookie" = [])),
    responses((status = 201, body = PaymentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn payment_record(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<RecordPaymentRequest>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PaymentManage)?;
    let key = require_idempotency_key(&headers)?;
    let account = account_or_404(&st, ctx, id).await?;
    let scope = institution_scope(ctx, account.institution_id);
    require_institution_writable(&st, &scope, account.institution_id).await?;
    if req.channel == PaymentChannel::Gateway {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "channel",
            "gateway_not_recordable",
            "gateway payments are started by the payer and confirmed by the gateway",
        )])
        .into());
    }
    if let Some(c) = req.currency.as_deref() {
        crate::shared::money::require_inr("currency", c)?;
    }
    let amount = parse_amount("amount", &req.amount)?;
    let lines = allocation_lines(req.allocations.as_deref())?;
    if account.status == StudentAccountStatus::Closed {
        return Err(KernelError::Conflict("the party account is closed".into()).into());
    }
    let today = business_today(&st, &scope, account.institution_id).await?;
    let received_on = req.received_on.unwrap_or(today);
    if received_on > today {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "receivedOn",
            "future_date",
            "cannot be after today",
        )])
        .into());
    }
    let order = open_charge_order(&st, &scope, account.id, &[]).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = insert_payment(
        &mut tx,
        &NewPayment {
            tenant: tenant(ctx),
            institution: account.institution_id,
            account: account.id,
            channel: req.channel,
            payer_kind: if req.channel == PaymentChannel::Sponsor {
                "sponsor"
            } else {
                "student"
            },
            payer_name: req
                .payer_name
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
            external_ref: req
                .external_ref
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
            amount,
            status: PaymentStatus::Initiated,
            targets: &[],
            gateway: None,
            order_ref: None,
            checkout_url: None,
            idempotency_key: Some(&key),
            received_on,
            unidentified_receipt_id: None,
            journal_entry_id: None,
            unapplied: Decimal::ZERO,
            batch_id: None,
        },
        &a,
    )
    .await?;
    let done = complete(
        &mut tx,
        &scope,
        &p,
        lines.as_deref(),
        &order,
        None,
        received_on,
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    view_response(&st, tenant(ctx), done.id.as_uuid(), StatusCode::CREATED).await
}

/// One payment with allocations and receipt (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/payments/{id}", operation_id = "finance_payment_get", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = PaymentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn payment_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceRead)?;
    view_response(&st, tenant(ctx), id, StatusCode::OK).await
}

/// Allocates the unapplied part of a completed payment (`payment.manage`, `If-Match`,
/// `Idempotency-Key`).
#[utoipa::path(post, path = "/api/v1/finance/payments/{id}/allocate", operation_id = "finance_payment_allocate", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header), ("Idempotency-Key" = String, Header)), request_body = AllocateRequest, security(("session_cookie" = [])),
    responses((status = 200, body = PaymentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn payment_allocate(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<AllocateRequest>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PaymentManage)?;
    require_idempotency_key(&headers)?;
    let v = match if_match(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let lines = allocation_lines(req.allocations.as_deref())?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let current = load_payment(&mut conn, t, id, false).await?;
    drop(conn);
    let scope = institution_scope(ctx, current.institution_id);
    require_institution_writable(&st, &scope, current.institution_id).await?;
    let order = open_charge_order(&st, &scope, current.student_account_id, &[]).await?;
    let today = business_today(&st, &scope, current.institution_id).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = load_payment(&mut tx, t, id, true).await?;
    require_version(v, p.version)?;
    allocate_unapplied(&mut tx, &scope, &p, lines.as_deref(), &order, today, &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    view_response(&st, t, id, StatusCode::OK).await
}

/// Cancels a payment that has not completed (L3-F-012; `payment.manage`, `If-Match`).
#[utoipa::path(post, path = "/api/v1/finance/payments/{id}/cancel", operation_id = "finance_payment_cancel", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = ReasonRequest, security(("session_cookie" = [])),
    responses((status = 200, body = PaymentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn payment_cancel(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<ReasonRequest>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PaymentManage)?;
    let v = match if_match(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let reason = req
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("cancelled by the finance office")
        .to_owned();
    cancel_common(&st, ctx, id, v, &reason, None).await
}

async fn cancel_common(
    st: &FinanceState,
    ctx: &AuthContext,
    id: Uuid,
    v: Version,
    reason: &str,
    own: Option<&[StudentAccountSummary]>,
) -> ApiResult<Response> {
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = load_payment(&mut tx, t, id, true).await?;
    if let Some(mine) = own {
        if !mine.iter().any(|m| m.id == p.student_account_id) {
            return Err(KernelError::NotFound { entity: "payment" }.into());
        }
    }
    require_version(v, p.version)?;
    let scope = institution_scope(ctx, p.institution_id);
    close_unpaid(
        &mut tx,
        &scope,
        &p,
        PaymentStatus::Cancelled,
        reason,
        None,
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    view_response(st, t, id, StatusCode::OK).await
}

/// Safe recovery of a pending / timed-out payment (L3-F-010): asks the gateway for the outcome
/// and completes, fails or (with `expire`) expires it (`payment.manage`, `If-Match`).
#[utoipa::path(post, path = "/api/v1/finance/payments/{id}/recover", operation_id = "finance_payment_recover", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = RecoverRequest, security(("session_cookie" = [])),
    responses((status = 200, body = PaymentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn payment_recover(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<RecoverRequest>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PaymentManage)?;
    let v = match if_match(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let current = load_payment(&mut conn, t, id, false).await?;
    drop(conn);
    require_version(v, current.version)?;
    if !matches!(
        current.status,
        PaymentStatus::Initiated | PaymentStatus::Pending
    ) {
        return Err(KernelError::Conflict(format!(
            "only an open payment can be recovered (payment is {})",
            current.status
        ))
        .into());
    }
    let scope = institution_scope(ctx, current.institution_id);
    let outcome = match current.gateway_order_ref.as_deref() {
        Some(order) => st.gateway.fetch_outcome(&scope, order).await?,
        None => GatewayPaymentOutcome::Pending,
    };
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = load_payment(&mut tx, t, id, true).await?;
    require_version(v, p.version)?;
    let p = apply_outcome(&st, &mut tx, &p, &outcome, &a).await?;
    if outcome == GatewayPaymentOutcome::Pending && req.expire.unwrap_or(false) {
        let reason = req
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("no gateway outcome before the timeout");
        close_unpaid(
            &mut tx,
            &scope,
            &p,
            PaymentStatus::Expired,
            reason,
            Some("TIMEOUT"),
            &a,
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    view_response(&st, t, id, StatusCode::OK).await
}

async fn reverse_common(
    st: &FinanceState,
    ctx: &AuthContext,
    headers: &HeaderMap,
    id: Uuid,
    req: &ReasonRequest,
    to: PaymentStatus,
) -> ApiResult<Response> {
    let v = match if_match(headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let reason = require_text("reason", req.reason.as_deref())?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let current = load_payment(&mut conn, t, id, false).await?;
    drop(conn);
    require_version(v, current.version)?;
    let scope = institution_scope(ctx, current.institution_id);
    require_institution_writable(st, &scope, current.institution_id).await?;
    // The reversal is the governed decision itself (`finance.approve`, step-up); the process
    // instance records it for the workflow runtime.
    let mut vars = BTreeMap::new();
    vars.insert(
        "paymentNumber".to_owned(),
        serde_json::json!(current.payment_number),
    );
    vars.insert("kind".to_owned(), serde_json::json!(to.as_str()));
    let outcome = request_approval(
        st,
        &scope,
        process::PAYMENT_REVERSAL,
        business_key("payment", id, current.version),
        vars,
    )
    .await?;
    let today = business_today(st, &scope, current.institution_id).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = load_payment(&mut tx, t, id, true).await?;
    require_version(v, p.version)?;
    reverse(
        &mut tx,
        &scope,
        &p,
        to,
        &reason,
        Some(outcome.process_instance_id()),
        today,
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    view_response(st, t, id, StatusCode::OK).await
}

/// Reverses a completed payment (L3-F-011; `finance.approve` + step-up, `If-Match`).
#[utoipa::path(post, path = "/api/v1/finance/payments/{id}/reverse", operation_id = "finance_payment_reverse", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = ReasonRequest, security(("session_cookie" = [])),
    responses((status = 200, body = PaymentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn payment_reverse(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<ReasonRequest>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceApprove)?;
    reverse_common(&st, ctx, &headers, id, &req, PaymentStatus::Reversed).await
}

/// Records a gateway chargeback of a completed payment (L3-F-011; `finance.approve`, `If-Match`).
#[utoipa::path(post, path = "/api/v1/finance/payments/{id}/chargeback", operation_id = "finance_payment_chargeback", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = ReasonRequest, security(("session_cookie" = [])),
    responses((status = 200, body = PaymentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn payment_chargeback(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<ReasonRequest>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceApprove)?;
    reverse_common(&st, ctx, &headers, id, &req, PaymentStatus::ChargedBack).await
}

/// Receipt of a payment (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/payments/{id}/receipt", operation_id = "finance_receipt_of_payment", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ReceiptView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn receipt_of_payment(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ReceiptView>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(Json(
        receipt_view(&mut conn, tenant(ctx), None, Some(id)).await?,
    ))
}

/// One receipt (`finance.read`; students read their own through `/me/receipts`).
#[utoipa::path(get, path = "/api/v1/finance/receipts/{id}", operation_id = "finance_receipt_get", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ReceiptView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn receipt_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ReceiptView>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(Json(
        receipt_view(&mut conn, tenant(ctx), Some(id), None).await?,
    ))
}

// ------------------------------------------------------------------ student self-service

/// The caller's own payments, newest first (`finance.self`).
#[utoipa::path(get, path = "/api/v1/finance/me/payments", operation_id = "finance_payment_self_list", tag = "finance-payments",
    security(("session_cookie" = [])),
    responses((status = 200, body = PaymentList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_payment_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
) -> ApiResult<Json<PaymentList>> {
    let ctx = &auth.0;
    let mine = my_accounts(&st, ctx).await?;
    let mut items = Vec::new();
    let q = PaymentListQuery {
        status: None,
        limit: Some(100),
        cursor: None,
    };
    for a in mine {
        items.extend(
            list_payments(&st, tenant(ctx), None, Some(a.id.as_uuid()), &q)
                .await?
                .items,
        );
    }
    Ok(Json(PaymentList {
        items,
        next_cursor: None,
    }))
}

/// Starts an online payment with the (mock) gateway: creates the gateway order and a pending
/// payment (`finance.self`, `Idempotency-Key` — a retry returns the same payment / order).
#[utoipa::path(post, path = "/api/v1/finance/me/payments", operation_id = "finance_payment_self_initiate", tag = "finance-payments",
    params(("Idempotency-Key" = String, Header)), request_body = InitiatePaymentRequest, security(("session_cookie" = [])),
    responses((status = 201, body = PaymentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_payment_initiate(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(req): Json<InitiatePaymentRequest>,
) -> ApiResult<Response> {
    let ctx = &auth.0;
    let key = require_idempotency_key(&headers)?;
    let mine = my_accounts(&st, ctx).await?;
    let account = match req.student_account_id {
        Some(id) => require_own_account(&st, ctx, StudentAccountId(id)).await?,
        None => mine.into_iter().next().ok_or(KernelError::NotFound {
            entity: "student account",
        })?,
    };
    let scope = institution_scope(ctx, account.institution_id);
    require_institution_writable(&st, &scope, account.institution_id).await?;
    let amount = parse_amount("amount", &req.amount)?;
    let targets = req.charge_ids.clone().unwrap_or_default();
    let order = open_charge_order(&st, &scope, account.id, &targets).await?;
    if !targets.is_empty() && order.len() != targets.len() {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "chargeIds",
            "not_payable",
            "every charge must be one of your open charges",
        )])
        .into());
    }
    let mut payable = Decimal::ZERO;
    for c in st
        .charges
        .open_charges_of_account(&scope, account.id)
        .await?
    {
        if order.contains(&c.id) {
            payable += c.outstanding_amount;
        }
    }
    if amount > payable {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "amount",
            "exceeds_outstanding",
            "cannot exceed what is outstanding on the selected charges",
        )])
        .into());
    }
    let t = tenant(ctx);
    let scoped_key = format!("{}:{}:{key}", t.as_uuid(), account.id);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM finance.payment WHERE tenant_id = $1 AND student_account_id = $2 AND idempotency_key = $3",
    )
    .bind(t.as_uuid())
    .bind(account.id.as_uuid())
    .bind(&key)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    drop(conn);
    if let Some(id) = existing {
        return view_response(&st, t, id, StatusCode::OK).await;
    }
    let gw = st
        .gateway
        .create_order(
            &scope,
            GatewayOrderRequest {
                institution_id: account.institution_id,
                amount,
                currency: INR,
                receipt_ref: scoped_key.clone(),
                idempotency_key: scoped_key,
            },
        )
        .await?;
    let today = business_today(&st, &scope, account.institution_id).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = insert_payment(
        &mut tx,
        &NewPayment {
            tenant: t,
            institution: account.institution_id,
            account: account.id,
            channel: PaymentChannel::Gateway,
            payer_kind: "student",
            payer_name: None,
            external_ref: None,
            amount,
            status: PaymentStatus::Pending,
            targets: &targets,
            gateway: Some(gw.gateway),
            order_ref: Some(&gw.order_ref),
            checkout_url: gw.checkout_url.as_deref(),
            idempotency_key: Some(&key),
            received_on: today,
            unidentified_receipt_id: None,
            journal_entry_id: None,
            unapplied: Decimal::ZERO,
            batch_id: None,
        },
        &a,
    )
    .await?;
    let subject = super::common::subject_ref(&mut tx, t, account.id).await?;
    let payload =
        super::common::Changed::new(p.id.as_uuid(), account.institution_id, "pending", p.version)
            .account(account.id, subject)
            .amount(amount, &p.currency)
            .reason(Some(&p.payment_number));
    crate::shared::publish::event(
        &mut tx,
        &scope,
        sutra_contracts::finance::events::payments::PAYMENT_INITIATED,
        sutra_contracts::finance::events::payments::PAYMENT_INITIATED_SCHEMA,
        p.id.to_string(),
        &payload.v,
        &a,
    )
    .await?;
    crate::shared::publish::audit(
        &mut tx,
        &scope,
        "finance.payment.initiate",
        "payment",
        p.id.to_string(),
        serde_json::json!({"paymentNumber": p.payment_number, "amount": crate::shared::money::fmt(amount), "orderRef": gw.order_ref}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    view_response(&st, t, p.id.as_uuid(), StatusCode::CREATED).await
}

/// Return from the (mock) checkout: asks the gateway for the outcome and completes / fails the
/// caller's own payment (`finance.self`, `If-Match`). Still pending → unchanged.
#[utoipa::path(post, path = "/api/v1/finance/me/payments/{id}/confirm", operation_id = "finance_payment_self_confirm", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), security(("session_cookie" = [])),
    responses((status = 200, body = PaymentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_payment_confirm(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = &auth.0;
    let v = match if_match(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let mine = my_accounts(&st, ctx).await?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let current = load_payment(&mut conn, t, id, false).await?;
    drop(conn);
    if !mine.iter().any(|m| m.id == current.student_account_id) {
        return Err(KernelError::NotFound { entity: "payment" }.into());
    }
    require_version(v, current.version)?;
    if current.status == PaymentStatus::Completed {
        return view_response(&st, t, id, StatusCode::OK).await;
    }
    let scope = institution_scope(ctx, current.institution_id);
    let outcome = match current.gateway_order_ref.as_deref() {
        Some(order) => st.gateway.fetch_outcome(&scope, order).await?,
        None => GatewayPaymentOutcome::Pending,
    };
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = load_payment(&mut tx, t, id, true).await?;
    apply_outcome(&st, &mut tx, &p, &outcome, &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    view_response(&st, t, id, StatusCode::OK).await
}

/// Abandons the caller's own open online payment (L3-F-012; `finance.self`, `If-Match`).
#[utoipa::path(post, path = "/api/v1/finance/me/payments/{id}/cancel", operation_id = "finance_payment_self_cancel", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = ReasonRequest, security(("session_cookie" = [])),
    responses((status = 200, body = PaymentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_payment_cancel(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<ReasonRequest>,
) -> ApiResult<Response> {
    let ctx = &auth.0;
    let v = match if_match(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let mine = my_accounts(&st, ctx).await?;
    let reason = req
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("cancelled by the payer")
        .to_owned();
    cancel_common(&st, ctx, id, v, &reason, Some(&mine)).await
}

/// The caller's own receipts (`finance.self`).
#[utoipa::path(get, path = "/api/v1/finance/me/receipts", operation_id = "finance_receipt_self_list", tag = "finance-payments",
    security(("session_cookie" = [])),
    responses((status = 200, body = ReceiptList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_receipt_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
) -> ApiResult<Json<ReceiptList>> {
    let ctx = &auth.0;
    let mine = my_accounts(&st, ctx).await?;
    let t = tenant(ctx);
    let ids: Vec<Uuid> = mine.iter().map(|a| a.id.as_uuid()).collect();
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let receipt_ids: Vec<Uuid> = sqlx::query(
        "SELECT id FROM finance.receipt WHERE tenant_id = $1 AND student_account_id = ANY($2) ORDER BY issued_at DESC, id DESC LIMIT 200",
    )
    .bind(t.as_uuid())
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(|r| r.try_get("id"))
    .collect::<Result<_, _>>()
    .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(receipt_ids.len());
    for r in receipt_ids {
        items.push(receipt_view(&mut conn, t, Some(r), None).await?);
    }
    Ok(Json(ReceiptList { items }))
}

// ------------------------------------------------------------------ gateway webhook

/// Gateway webhook (Razorpay-shaped): **unauthenticated**, verified with the HMAC-SHA256
/// `X-Razorpay-Signature` over the raw body (401 otherwise), idempotent on the gateway event id.
#[utoipa::path(post, path = "/api/v1/finance/gateway/webhook", operation_id = "finance_gateway_webhook", tag = "finance-payments",
    params(("X-Razorpay-Signature" = String, Header)), request_body(content = String, content_type = "application/json"),
    responses((status = 200, body = WebhookAck), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn gateway_webhook(
    State(st): State<FinanceState>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Json<WebhookAck>> {
    // Part 1 S3 (DEF-X-003): the gateway is not a session; the order reference is looked up
    // across tenants (system context) and the outcome is then applied in the payment's tenant.
    sutra_kernel::db_context::system(gateway_webhook_signed(st, headers, body)).await
}

async fn gateway_webhook_signed(
    st: FinanceState,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Json<WebhookAck>> {
    // INT: SEC-9D-003 — feature off: 503 before any signature handling.
    if st.gateway.name() == crate::payments::gateway::DISABLED_GATEWAY {
        return Err(KernelError::Unavailable("online payments are disabled".into()).into());
    }
    let signature = headers
        .get("x-razorpay-signature")
        .and_then(|v| v.to_str().ok())
        .ok_or(KernelError::Unauthenticated)?;
    let hook = st.gateway.verify_webhook(&body, signature)?;
    let gateway = st.gateway.name();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let found = sqlx::query(&format!(
        "SELECT {PAYMENT_COLUMNS} FROM finance.payment WHERE gateway = $1 AND gateway_order_ref = $2 FOR UPDATE"
    ))
    .bind(gateway)
    .bind(&hook.order_ref)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .map(|r| payment_from_row(&r))
    .transpose()
    .map_err(map_db_error)?;
    let outcome_name = match &hook.outcome {
        GatewayPaymentOutcome::Captured { .. } => "captured",
        GatewayPaymentOutcome::Failed { .. } => "failed",
        GatewayPaymentOutcome::Pending => "pending",
    };
    let inserted: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO finance.gateway_webhook_event (gateway, event_id, event, order_ref, tenant_id, payment_id, outcome) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (gateway, event_id) DO NOTHING RETURNING id",
    )
    .bind(gateway)
    .bind(&hook.event_id)
    .bind(&hook.event)
    .bind(&hook.order_ref)
    .bind(found.as_ref().map(|p| p.tenant_id.as_uuid()))
    .bind(found.as_ref().map(|p| p.id.as_uuid()))
    .bind(if found.is_some() { outcome_name } else { "unknown_order" })
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let Some(p) = found else {
        tx.commit().await.map_err(map_db_error)?;
        return Ok(Json(WebhookAck {
            event_id: hook.event_id,
            status: "ignored".into(),
            payment_id: None,
            payment_status: None,
        }));
    };
    if inserted.is_none() {
        tx.rollback().await.map_err(map_db_error)?;
        return Ok(Json(WebhookAck {
            event_id: hook.event_id,
            status: "duplicate".into(),
            payment_id: Some(p.id.as_uuid()),
            payment_status: Some(p.status),
        }));
    }
    // Part 1 S3 (DEF-X-003): from here on the transaction is bound to the payment's tenant.
    sutra_persistence::pool::apply_context(
        &mut tx,
        Some(sutra_kernel::db_context::DbContext::Scoped(
            ExecutionScope::tenant(p.tenant_id),
        )),
        true,
    )
    .await
    .map_err(map_db_error)?;
    let a = Actor::system(Some(format!("gateway:{}", hook.event_id)));
    let done = apply_outcome(&st, &mut tx, &p, &hook.outcome, &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(WebhookAck {
        event_id: hook.event_id,
        status: "processed".into(),
        payment_id: Some(done.id.as_uuid()),
        payment_status: Some(done.status),
    }))
}

pub fn router(state: FinanceState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(payment_list))
        .routes(routes!(payment_account_list, payment_record))
        .routes(routes!(payment_get))
        .routes(routes!(payment_allocate))
        .routes(routes!(payment_cancel))
        .routes(routes!(payment_recover))
        .routes(routes!(payment_reverse))
        .routes(routes!(payment_chargeback))
        .routes(routes!(receipt_of_payment))
        .routes(routes!(receipt_get))
        .routes(routes!(my_payment_list, my_payment_initiate))
        .routes(routes!(my_payment_confirm))
        .routes(routes!(my_payment_cancel))
        .routes(routes!(my_receipt_list))
        .routes(routes!(gateway_webhook))
        .with_state(state)
}
