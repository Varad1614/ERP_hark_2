//! Bulk collection (L3-F-013): a population of counter / bank / sponsor payments is drafted,
//! validated (every party exists in the institution and is not closed) and executed — each
//! item completes as its own payment (allocation, receipt, posting) in its own transaction, so
//! one bad item never blocks the rest; the batch ends `completed`, `partially_completed` or
//! `failed` and emits `payment_batch.completed`.

use super::common::{history, illegal, parse_amount, require_version, Changed};
use super::domain::{batch_outcome, can_transition_batch};
use super::http::open_charge_order;
use super::service::{complete, insert_payment, NewPayment};
use crate::shared::approval::{business_key, process, request_approval};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::lifecycle::StatusChange;
use crate::shared::money::fmt;
use crate::shared::publish::{audit, event};
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::time::business_today;
use crate::shared::{Actor, FinanceState};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::finance::events::payments::{
    PAYMENT_BATCH_COMPLETED, PAYMENT_BATCH_COMPLETED_SCHEMA,
};
use sutra_contracts::finance::ids::{PaymentBatchId, StudentAccountId};
use sutra_contracts::finance::status::{
    PaymentBatchStatus, PaymentChannel, PaymentStatus, StudentAccountStatus,
};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// One item of a bulk collection.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CollectionItemView {
    pub line_no: i32,
    pub student_account_id: Uuid,
    #[schema(value_type = String, example = "5000.00")]
    pub amount: String,
    pub external_ref: Option<String>,
    /// `pending | valid | invalid | completed | failed`.
    pub status: String,
    pub error: Option<String>,
    pub payment_id: Option<Uuid>,
}

/// A bulk collection batch.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CollectionView {
    pub id: PaymentBatchId,
    pub institution_id: Uuid,
    pub name: String,
    pub channel: PaymentChannel,
    pub received_on: NaiveDate,
    #[schema(value_type = String, example = "10000.00")]
    pub total_amount: String,
    pub currency: String,
    pub status: PaymentBatchStatus,
    pub process_instance_id: Option<String>,
    pub items: Vec<CollectionItemView>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CollectionList {
    pub items: Vec<CollectionView>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CollectionItemInput {
    pub student_account_id: Uuid,
    #[schema(value_type = String, example = "5000.00")]
    pub amount: String,
    pub external_ref: Option<String>,
}

/// A new bulk collection (draft).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateCollectionRequest {
    pub name: String,
    /// `cash | cheque | bank_transfer | demand_draft | sponsor`.
    pub channel: PaymentChannel,
    pub received_on: Option<NaiveDate>,
    pub items: Vec<CollectionItemInput>,
}

struct Head {
    id: Uuid,
    tenant: TenantId,
    institution: InstitutionId,
    name: String,
    channel: PaymentChannel,
    received_on: NaiveDate,
    status: PaymentBatchStatus,
    version: i64,
}

async fn head(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<Head, KernelError> {
    let r = sqlx::query(&format!(
        "SELECT id, tenant_id, institution_id, name, channel, received_on, status, version \
         FROM finance.payment_batch WHERE tenant_id = $1 AND id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    ))
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "collection",
    })?;
    let map = || -> Result<Head, sqlx::Error> {
        let channel: String = r.try_get("channel")?;
        let status: String = r.try_get("status")?;
        Ok(Head {
            id: r.try_get("id")?,
            tenant: TenantId(r.try_get("tenant_id")?),
            institution: InstitutionId(r.try_get("institution_id")?),
            name: r.try_get("name")?,
            channel: PaymentChannel::parse(&channel).unwrap_or(PaymentChannel::Cash),
            received_on: r.try_get("received_on")?,
            status: PaymentBatchStatus::parse(&status).unwrap_or(PaymentBatchStatus::Draft),
            version: r.try_get("version")?,
        })
    };
    map().map_err(map_db_error)
}

async fn view(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> Result<CollectionView, KernelError> {
    let r = sqlx::query(
        "SELECT id, institution_id, name, channel, received_on, total_amount, currency, status, \
         process_instance_id, created_at, version FROM finance.payment_batch WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "collection",
    })?;
    let items = sqlx::query(
        "SELECT line_no, student_account_id, amount, external_ref, status, error, payment_id \
         FROM finance.payment_batch_item WHERE tenant_id = $1 AND batch_id = $2 ORDER BY line_no",
    )
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(|i| -> Result<CollectionItemView, sqlx::Error> {
        let amount: Decimal = i.try_get("amount")?;
        Ok(CollectionItemView {
            line_no: i.try_get("line_no")?,
            student_account_id: i.try_get("student_account_id")?,
            amount: fmt(amount),
            external_ref: i.try_get("external_ref")?,
            status: i.try_get("status")?,
            error: i.try_get("error")?,
            payment_id: i.try_get("payment_id")?,
        })
    })
    .collect::<Result<Vec<_>, _>>()
    .map_err(map_db_error)?;
    let map = || -> Result<CollectionView, sqlx::Error> {
        let channel: String = r.try_get("channel")?;
        let status: String = r.try_get("status")?;
        let total: Decimal = r.try_get("total_amount")?;
        Ok(CollectionView {
            id: PaymentBatchId(r.try_get("id")?),
            institution_id: r.try_get("institution_id")?,
            name: r.try_get("name")?,
            channel: PaymentChannel::parse(&channel).unwrap_or(PaymentChannel::Cash),
            received_on: r.try_get("received_on")?,
            total_amount: fmt(total),
            currency: r.try_get("currency")?,
            status: PaymentBatchStatus::parse(&status).unwrap_or(PaymentBatchStatus::Draft),
            process_instance_id: r.try_get("process_instance_id")?,
            items: Vec::new(),
            created_at: r.try_get("created_at")?,
            version: r.try_get("version")?,
        })
    };
    let mut v = map().map_err(map_db_error)?;
    v.items = items;
    Ok(v)
}

async fn respond(
    st: &FinanceState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> ApiResult<Response> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v = view(&mut conn, tenant, id).await?;
    let version = v.version;
    Ok(with_etag(status, &v, Version(version)))
}

async fn move_batch(
    conn: &mut PgConnection,
    h: &Head,
    to: PaymentBatchStatus,
    extra_total: Option<Decimal>,
    process_instance: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    if !can_transition_batch(h.status, to) {
        return Err(illegal("collection", h.status.as_str(), to.as_str()));
    }
    sqlx::query(
        "UPDATE finance.payment_batch SET status = $3, total_amount = COALESCE($4, total_amount), \
         process_instance_id = COALESCE($5, process_instance_id), version = version + 1, updated_at = now() \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(h.tenant.as_uuid())
    .bind(h.id)
    .bind(to.as_str())
    .bind(extra_total)
    .bind(process_instance)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        h.tenant,
        h.institution,
        "payment_batch",
        h.id,
        Some(h.status.as_str()),
        to.as_str(),
        &StatusChange {
            cause_event_type: "",
            reason: None,
            process_instance_id: process_instance,
        },
        actor,
    )
    .await
}

/// Bulk collections of an institution (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/collections", operation_id = "finance_collection_list", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = CollectionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn collection_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<CollectionList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    require_institution(
        &st,
        &institution_scope(ctx, InstitutionId(id)),
        InstitutionId(id),
    )
    .await?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM finance.payment_batch WHERE tenant_id = $1 AND institution_id = $2 ORDER BY created_at DESC, id DESC LIMIT 100",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(ids.len());
    for b in ids {
        items.push(view(&mut conn, t, b).await?);
    }
    Ok(Json(CollectionList { items }))
}

/// Drafts a bulk collection (`payment.manage`, `Idempotency-Key`).
#[utoipa::path(post, path = "/api/v1/finance/institutions/{id}/collections", operation_id = "finance_collection_create", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)), request_body = CreateCollectionRequest, security(("session_cookie" = [])),
    responses((status = 201, body = CollectionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn collection_create(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<CreateCollectionRequest>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PaymentManage)?;
    require_idempotency_key(&headers)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let name = req.name.trim();
    if name.is_empty() || name.len() > 200 {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "name",
            "required",
            "1-200 characters",
        )])
        .into());
    }
    if req.channel == PaymentChannel::Gateway {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "channel",
            "gateway_not_recordable",
            "bulk collections are offline channels",
        )])
        .into());
    }
    if req.items.is_empty() || req.items.len() > 500 {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "items",
            "size",
            "1-500 items",
        )])
        .into());
    }
    let mut amounts = Vec::with_capacity(req.items.len());
    for (i, it) in req.items.iter().enumerate() {
        amounts.push(parse_amount(&format!("items[{i}].amount"), &it.amount)?);
    }
    let total: Decimal = amounts.iter().copied().sum();
    let today = business_today(&st, &scope, inst).await?;
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let bid: Uuid = sqlx::query_scalar(
        "INSERT INTO finance.payment_batch (tenant_id, institution_id, name, channel, received_on, total_amount, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(name)
    .bind(req.channel.as_str())
    .bind(req.received_on.unwrap_or(today).min(today))
    .bind(total)
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    for (i, (it, amount)) in req.items.iter().zip(amounts).enumerate() {
        sqlx::query(
            "INSERT INTO finance.payment_batch_item (tenant_id, batch_id, line_no, student_account_id, amount, external_ref) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(t.as_uuid())
        .bind(bid)
        .bind(i32::try_from(i + 1).unwrap_or(i32::MAX))
        .bind(it.student_account_id)
        .bind(amount)
        .bind(it.external_ref.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    history(
        &mut tx,
        t,
        inst,
        "payment_batch",
        bid,
        None,
        "draft",
        &StatusChange::default(),
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "finance.payment_batch.create",
        "payment_batch",
        bid.to_string(),
        json!({"name": name, "items": req.items.len(), "total": fmt(total)}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, bid, StatusCode::CREATED).await
}

/// One bulk collection with its items (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/collections/{id}", operation_id = "finance_collection_get", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = CollectionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn collection_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceRead)?;
    respond(&st, tenant(ctx), id, StatusCode::OK).await
}

/// Validates every item (party exists in the institution and is not closed): `validated` when
/// at least one item is valid, `failed` otherwise (`payment.manage`, `If-Match`).
#[utoipa::path(post, path = "/api/v1/finance/collections/{id}/validate", operation_id = "finance_collection_validate", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), security(("session_cookie" = [])),
    responses((status = 200, body = CollectionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn collection_validate(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PaymentManage)?;
    let v = match if_match(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let h = head(&mut tx, t, id, true).await?;
    require_version(v, h.version)?;
    if h.status != PaymentBatchStatus::Draft {
        return Err(illegal("collection", h.status.as_str(), "validated").into());
    }
    let scope = institution_scope(ctx, h.institution);
    let items: Vec<(i32, Uuid)> = sqlx::query_as(
        "SELECT line_no, student_account_id FROM finance.payment_batch_item WHERE batch_id = $1 ORDER BY line_no",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let mut valid = 0usize;
    for (line, account) in items {
        let acc = st
            .charges
            .student_account(&scope, StudentAccountId(account))
            .await?;
        let error = match acc {
            None => Some("unknown party account"),
            Some(a) if a.institution_id != h.institution => {
                Some("party account of another institution")
            }
            Some(a) if a.status == StudentAccountStatus::Closed => Some("party account is closed"),
            Some(_) => None,
        };
        if error.is_none() {
            valid += 1;
        }
        sqlx::query("UPDATE finance.payment_batch_item SET status = $3, error = $4 WHERE batch_id = $1 AND line_no = $2")
            .bind(id)
            .bind(line)
            .bind(if error.is_some() { "invalid" } else { "valid" })
            .bind(error)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
    }
    let to = if valid > 0 {
        PaymentBatchStatus::Validated
    } else {
        PaymentBatchStatus::Failed
    };
    move_batch(&mut tx, &h, to, None, None, &a).await?;
    audit(
        &mut tx,
        &scope,
        "finance.payment_batch.validate",
        "payment_batch",
        id.to_string(),
        json!({"valid": valid, "status": to.as_str()}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, id, StatusCode::OK).await
}

/// Executes a validated collection: each valid item completes as its own payment (own
/// transaction); the batch ends completed / partially_completed / failed (`payment.manage`,
/// `If-Match`, `Idempotency-Key`).
#[utoipa::path(post, path = "/api/v1/finance/collections/{id}/execute", operation_id = "finance_collection_execute", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header), ("Idempotency-Key" = String, Header)), security(("session_cookie" = [])),
    responses((status = 200, body = CollectionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn collection_execute(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PaymentManage)?;
    require_idempotency_key(&headers)?;
    let v = match if_match(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let h0 = head(&mut conn, t, id, false).await?;
    drop(conn);
    require_version(v, h0.version)?;
    let scope = institution_scope(ctx, h0.institution);
    require_institution_writable(&st, &scope, h0.institution).await?;
    let mut vars = BTreeMap::new();
    vars.insert("collection".to_owned(), json!(h0.name));
    let approval = request_approval(
        &st,
        &scope,
        process::PAYMENT_BATCH_APPROVAL,
        business_key("payment_batch", id, h0.version),
        vars,
    )
    .await?;
    // Claim the batch (validated → executing) in its own transaction.
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let h = head(&mut tx, t, id, true).await?;
    require_version(v, h.version)?;
    move_batch(
        &mut tx,
        &h,
        PaymentBatchStatus::Executing,
        None,
        Some(approval.process_instance_id()),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let items: Vec<(i32, Uuid, Decimal, Option<String>)> = sqlx::query_as(
        "SELECT line_no, student_account_id, amount, external_ref FROM finance.payment_batch_item \
         WHERE batch_id = $1 AND status = 'valid' ORDER BY line_no",
    )
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let (mut ok, mut failed) = (0usize, 0usize);
    let mut collected = Decimal::ZERO;
    for (line, account, amount, ext) in items {
        let result = execute_item(&st, &scope, &h, account, amount, ext.as_deref(), &a).await;
        let (status, error, payment) = match result {
            Ok(pid) => {
                ok += 1;
                collected += amount;
                ("completed", None, Some(pid))
            }
            Err(e) => {
                failed += 1;
                ("failed", Some(e.to_string()), None)
            }
        };
        sqlx::query("UPDATE finance.payment_batch_item SET status = $3, error = $4, payment_id = $5 WHERE batch_id = $1 AND line_no = $2")
            .bind(id)
            .bind(line)
            .bind(status)
            .bind(error)
            .bind(payment)
            .execute(&st.pool)
            .await
            .map_err(map_db_error)?;
    }
    let outcome = batch_outcome(ok, failed);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let h = head(&mut tx, t, id, true).await?;
    move_batch(&mut tx, &h, outcome, None, None, &a).await?;
    let payload = Changed::new(id, h.institution, outcome.as_str(), h.version + 1)
        .from(h.status.as_str())
        .amount(collected, "INR")
        .reason(Some(&format!("{ok} collected, {failed} failed")))
        .process(Some(approval.process_instance_id()));
    event(
        &mut tx,
        &scope,
        PAYMENT_BATCH_COMPLETED,
        PAYMENT_BATCH_COMPLETED_SCHEMA,
        id.to_string(),
        &payload.v,
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "finance.payment_batch.execute",
        "payment_batch",
        id.to_string(),
        json!({"completed": ok, "failed": failed, "collected": fmt(collected)}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, id, StatusCode::OK).await
}

async fn execute_item(
    st: &FinanceState,
    scope: &ExecutionScope,
    h: &Head,
    account: Uuid,
    amount: Decimal,
    external_ref: Option<&str>,
    actor: &Actor,
) -> Result<Uuid, KernelError> {
    let order = open_charge_order(st, scope, StudentAccountId(account), &[]).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = insert_payment(
        &mut tx,
        &NewPayment {
            tenant: h.tenant,
            institution: h.institution,
            account: StudentAccountId(account),
            channel: h.channel,
            payer_kind: if h.channel == PaymentChannel::Sponsor {
                "sponsor"
            } else {
                "student"
            },
            payer_name: None,
            external_ref,
            amount,
            status: PaymentStatus::Initiated,
            targets: &[],
            gateway: None,
            order_ref: None,
            checkout_url: None,
            idempotency_key: None,
            received_on: h.received_on,
            unidentified_receipt_id: None,
            journal_entry_id: None,
            unapplied: Decimal::ZERO,
            batch_id: Some(h.id),
        },
        actor,
    )
    .await?;
    let done = complete(&mut tx, scope, &p, None, &order, None, h.received_on, actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(done.id.as_uuid())
}

pub fn router(state: FinanceState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(collection_list, collection_create))
        .routes(routes!(collection_get))
        .routes(routes!(collection_validate))
        .routes(routes!(collection_execute))
        .with_state(state)
}
