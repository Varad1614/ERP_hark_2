//! Unidentified / unallocated receipts (L3-F-009): money received without a known payer is
//! posted Dr bank / cash · Cr unapplied receipts at once, investigated, then identified (it
//! becomes a completed payment of the party with the whole amount unapplied + a receipt) and
//! allocated, or refunded to the remitter.

use super::common::{
    history, illegal, parse_amount, require_text, require_version, subject_ref, Changed,
};
use super::domain::can_transition_unidentified;
use super::http::{open_charge_order, AllocateRequest, ReasonRequest};
use super::service::{
    allocate_unapplied, debit_account, insert_payment, issue_receipt, load_payment, AllocationLine,
    NewPayment,
};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::lifecycle::StatusChange;
use crate::shared::money::fmt;
use crate::shared::posting;
use crate::shared::publish::{audit, event};
use crate::shared::scope::{institution_scope, tenant, tenant_scope};
use crate::shared::time::business_today;
use crate::shared::{Actor, FinanceState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use sutra_contracts::finance::events::payments::{
    PAYMENT_COMPLETED, PAYMENT_COMPLETED_SCHEMA, RECEIPT_ISSUED, RECEIPT_ISSUED_SCHEMA,
    UNIDENTIFIED_RECEIPT_RESOLVED, UNIDENTIFIED_RECEIPT_RESOLVED_SCHEMA,
};
use sutra_contracts::finance::ids::{ChargeId, StudentAccountId, UnidentifiedReceiptId};
use sutra_contracts::finance::ports::{
    source, AccountRef, PostingLine, PostingRequest, SystemAccount,
};
use sutra_contracts::finance::status::{
    JournalEntryKind, PaymentChannel, PaymentStatus, UnidentifiedReceiptStatus,
};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::money::CurrencyCode;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

const COLUMNS: &str = "id, tenant_id, institution_id, channel, amount, currency, received_on, \
    payer_name, external_ref, notes, status, student_account_id, payment_id, journal_entry_id, \
    resolution_entry_id, version, created_at";

/// An unidentified receipt.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UnidentifiedReceiptView {
    pub id: UnidentifiedReceiptId,
    pub institution_id: Uuid,
    pub channel: PaymentChannel,
    #[schema(value_type = String, example = "15000.00")]
    pub amount: String,
    pub currency: String,
    pub received_on: NaiveDate,
    pub payer_name: Option<String>,
    pub external_ref: Option<String>,
    pub notes: Option<String>,
    pub status: UnidentifiedReceiptStatus,
    pub student_account_id: Option<Uuid>,
    /// The payment created when it was identified.
    pub payment_id: Option<Uuid>,
    pub journal_entry_id: Option<Uuid>,
    pub resolution_entry_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UnidentifiedReceiptList {
    pub items: Vec<UnidentifiedReceiptView>,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct UnidentifiedListQuery {
    pub status: Option<UnidentifiedReceiptStatus>,
}

/// Money received without a known payer.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordUnidentifiedRequest {
    /// `cash | cheque | bank_transfer | demand_draft | sponsor`.
    pub channel: PaymentChannel,
    #[schema(value_type = String, example = "15000.00")]
    pub amount: String,
    pub received_on: Option<NaiveDate>,
    pub payer_name: Option<String>,
    pub external_ref: Option<String>,
    pub notes: Option<String>,
}

/// The party the money belongs to.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentifyRequest {
    pub student_account_id: Uuid,
}

#[derive(Clone, Debug)]
struct Row5b {
    id: Uuid,
    tenant: TenantId,
    institution: InstitutionId,
    channel: PaymentChannel,
    amount: Decimal,
    received_on: NaiveDate,
    payer_name: Option<String>,
    external_ref: Option<String>,
    status: UnidentifiedReceiptStatus,
    payment_id: Option<Uuid>,
    journal_entry_id: Option<Uuid>,
    version: i64,
    view: UnidentifiedReceiptView,
}

fn from_row(r: &PgRow) -> Result<Row5b, sqlx::Error> {
    let channel: String = r.try_get("channel")?;
    let status: String = r.try_get("status")?;
    let channel = PaymentChannel::parse(&channel).unwrap_or(PaymentChannel::BankTransfer);
    let status = UnidentifiedReceiptStatus::parse(&status)
        .unwrap_or(UnidentifiedReceiptStatus::Unidentified);
    let amount: Decimal = r.try_get("amount")?;
    let id: Uuid = r.try_get("id")?;
    let institution: Uuid = r.try_get("institution_id")?;
    let view = UnidentifiedReceiptView {
        id: UnidentifiedReceiptId(id),
        institution_id: institution,
        channel,
        amount: fmt(amount),
        currency: r.try_get("currency")?,
        received_on: r.try_get("received_on")?,
        payer_name: r.try_get("payer_name")?,
        external_ref: r.try_get("external_ref")?,
        notes: r.try_get("notes")?,
        status,
        student_account_id: r.try_get("student_account_id")?,
        payment_id: r.try_get("payment_id")?,
        journal_entry_id: r.try_get("journal_entry_id")?,
        resolution_entry_id: r.try_get("resolution_entry_id")?,
        created_at: r.try_get("created_at")?,
        version: r.try_get("version")?,
    };
    Ok(Row5b {
        id,
        tenant: TenantId(r.try_get("tenant_id")?),
        institution: InstitutionId(institution),
        channel,
        amount,
        received_on: view.received_on,
        payer_name: view.payer_name.clone(),
        external_ref: view.external_ref.clone(),
        status,
        payment_id: view.payment_id,
        journal_entry_id: view.journal_entry_id,
        version: view.version,
        view,
    })
}

async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<Row5b, KernelError> {
    let r = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM finance.unidentified_receipt WHERE tenant_id = $1 AND id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    ))
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "unidentified receipt",
    })?;
    from_row(&r).map_err(map_db_error)
}

async fn set_status(
    conn: &mut PgConnection,
    u: &Row5b,
    to: UnidentifiedReceiptStatus,
    reason: Option<&str>,
    cause: &'static str,
    actor: &Actor,
) -> Result<(), KernelError> {
    if !can_transition_unidentified(u.status, to) {
        return Err(illegal(
            "unidentified receipt",
            u.status.as_str(),
            to.as_str(),
        ));
    }
    sqlx::query(
        "UPDATE finance.unidentified_receipt SET status = $3, version = version + 1, updated_at = now() \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(u.tenant.as_uuid())
    .bind(u.id)
    .bind(to.as_str())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        u.tenant,
        u.institution,
        "unidentified_receipt",
        u.id,
        Some(u.status.as_str()),
        to.as_str(),
        &StatusChange {
            cause_event_type: cause,
            reason,
            process_instance_id: None,
        },
        actor,
    )
    .await
}

async fn respond(
    st: &FinanceState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> ApiResult<Response> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let u = load(&mut conn, tenant, id, false).await?;
    Ok(with_etag(status, &u.view, Version(u.version)))
}

async fn resolved_event(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    u: &Row5b,
    to: UnidentifiedReceiptStatus,
    account: Option<StudentAccountId>,
    journal: Option<Uuid>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let mut payload = Changed::new(u.id, u.institution, to.as_str(), u.version + 1)
        .from(u.status.as_str())
        .amount(u.amount, "INR")
        .journal(journal);
    if let Some(a) = account {
        let subject = subject_ref(conn, u.tenant, a).await?;
        payload = payload.account(a, subject);
    }
    event(
        conn,
        scope,
        UNIDENTIFIED_RECEIPT_RESOLVED,
        UNIDENTIFIED_RECEIPT_RESOLVED_SCHEMA,
        u.id.to_string(),
        &payload.v,
        actor,
    )
    .await
}

/// Unidentified receipts of an institution (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/unidentified-receipts", operation_id = "finance_unidentified_list", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid), UnidentifiedListQuery), security(("session_cookie" = [])),
    responses((status = 200, body = UnidentifiedReceiptList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn unidentified_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<UnidentifiedListQuery>,
) -> ApiResult<Json<UnidentifiedReceiptList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    require_institution(
        &st,
        &institution_scope(ctx, InstitutionId(id)),
        InstitutionId(id),
    )
    .await?;
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM finance.unidentified_receipt WHERE tenant_id = $1 AND institution_id = $2 \
         AND ($3::text IS NULL OR status = $3) ORDER BY created_at DESC, id DESC LIMIT 200"
    ))
    .bind(tenant(ctx).as_uuid())
    .bind(id)
    .bind(q.status.map(|s| s.as_str()))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(|r| from_row(r).map(|u| u.view))
        .collect::<Result<_, _>>()
        .map_err(map_db_error)?;
    Ok(Json(UnidentifiedReceiptList { items }))
}

/// Records money received without a known payer: Dr bank / cash · Cr unapplied receipts
/// (`payment.manage`, `Idempotency-Key`).
#[utoipa::path(post, path = "/api/v1/finance/institutions/{id}/unidentified-receipts", operation_id = "finance_unidentified_record", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)), request_body = RecordUnidentifiedRequest, security(("session_cookie" = [])),
    responses((status = 201, body = UnidentifiedReceiptView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn unidentified_record(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<RecordUnidentifiedRequest>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PaymentManage)?;
    require_idempotency_key(&headers)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    if req.channel == PaymentChannel::Gateway {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "channel",
            "gateway_not_recordable",
            "gateway receipts always carry their order",
        )])
        .into());
    }
    let amount = parse_amount("amount", &req.amount)?;
    let today = business_today(&st, &scope, inst).await?;
    let received_on = req.received_on.unwrap_or(today).min(today);
    let a = actor(ctx);
    let t = tenant(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let uid: Uuid = sqlx::query_scalar(
        "INSERT INTO finance.unidentified_receipt (tenant_id, institution_id, channel, amount, received_on, \
         payer_name, external_ref, notes, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(req.channel.as_str())
    .bind(amount)
    .bind(received_on)
    .bind(req.payer_name.as_deref().map(str::trim).filter(|s| !s.is_empty()))
    .bind(req.external_ref.as_deref().map(str::trim).filter(|s| !s.is_empty()))
    .bind(req.notes.as_deref().map(str::trim).filter(|s| !s.is_empty()))
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let entry = posting::post(
        &mut tx,
        t,
        &PostingRequest {
            institution_id: inst,
            posting_date: received_on,
            kind: JournalEntryKind::Standard,
            source_type: source::UNIDENTIFIED_RECEIPT.into(),
            source_id: uid,
            posting_key: "receive".into(),
            description: "Unidentified receipt".into(),
            currency: CurrencyCode::INR,
            lines: vec![
                PostingLine::debit(AccountRef::System(debit_account(req.channel)), amount),
                PostingLine::credit(AccountRef::System(SystemAccount::UnappliedReceipts), amount)
                    .memo("unidentified receipt"),
            ],
        },
        &a,
    )
    .await?;
    sqlx::query("UPDATE finance.unidentified_receipt SET journal_entry_id = $2 WHERE id = $1")
        .bind(uid)
        .bind(entry.journal_entry_id.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        inst,
        "unidentified_receipt",
        uid,
        None,
        "unidentified",
        &StatusChange::default(),
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "finance.unidentified_receipt.record",
        "unidentified_receipt",
        uid.to_string(),
        json!({"amount": fmt(amount), "journalEntryId": entry.journal_entry_id}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, uid, StatusCode::CREATED).await
}

/// One unidentified receipt (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/unidentified-receipts/{id}", operation_id = "finance_unidentified_get", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = UnidentifiedReceiptView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn unidentified_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceRead)?;
    respond(&st, tenant(ctx), id, StatusCode::OK).await
}

/// Marks an unidentified receipt as under investigation (`payment.manage`, `If-Match`).
#[utoipa::path(post, path = "/api/v1/finance/unidentified-receipts/{id}/investigate", operation_id = "finance_unidentified_investigate", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = ReasonRequest, security(("session_cookie" = [])),
    responses((status = 200, body = UnidentifiedReceiptView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn unidentified_investigate(
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
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let u = load(&mut tx, t, id, true).await?;
    require_version(v, u.version)?;
    set_status(
        &mut tx,
        &u,
        UnidentifiedReceiptStatus::UnderInvestigation,
        req.reason.as_deref(),
        "",
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &institution_scope(ctx, u.institution),
        "finance.unidentified_receipt.investigate",
        "unidentified_receipt",
        id.to_string(),
        json!({"reason": req.reason}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, id, StatusCode::OK).await
}

/// Identifies the payer: the receipt becomes a completed payment of that party with the whole
/// amount unapplied, and a receipt is issued (`payment.manage`, `If-Match`).
#[utoipa::path(post, path = "/api/v1/finance/unidentified-receipts/{id}/identify", operation_id = "finance_unidentified_identify", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = IdentifyRequest, security(("session_cookie" = [])),
    responses((status = 200, body = UnidentifiedReceiptView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn unidentified_identify(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<IdentifyRequest>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PaymentManage)?;
    let v = match if_match(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let t = tenant(ctx);
    let account = st
        .charges
        .student_account(&tenant_scope(ctx), StudentAccountId(req.student_account_id))
        .await?
        .ok_or_else(|| {
            KernelError::Validation(vec![FieldViolation::new(
                "studentAccountId",
                "not_found",
                "unknown party account",
            )])
        })?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let u = load(&mut tx, t, id, true).await?;
    require_version(v, u.version)?;
    if account.institution_id != u.institution {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "studentAccountId",
            "other_institution",
            "the party account belongs to another institution",
        )])
        .into());
    }
    if !can_transition_unidentified(u.status, UnidentifiedReceiptStatus::Identified) {
        return Err(illegal("unidentified receipt", u.status.as_str(), "identified").into());
    }
    let scope = institution_scope(ctx, u.institution);
    let p = insert_payment(
        &mut tx,
        &NewPayment {
            tenant: t,
            institution: u.institution,
            account: account.id,
            channel: u.channel,
            payer_kind: if u.channel == PaymentChannel::Sponsor {
                "sponsor"
            } else {
                "student"
            },
            payer_name: u.payer_name.as_deref(),
            external_ref: u.external_ref.as_deref(),
            amount: u.amount,
            status: PaymentStatus::Completed,
            targets: &[],
            gateway: None,
            order_ref: None,
            checkout_url: None,
            idempotency_key: None,
            received_on: u.received_on,
            unidentified_receipt_id: Some(u.id),
            journal_entry_id: u.journal_entry_id,
            unapplied: u.amount,
            batch_id: None,
        },
        &a,
    )
    .await?;
    sqlx::query("UPDATE finance.payment SET completed_at = now() WHERE id = $1")
        .bind(p.id.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let (receipt_id, receipt_number) = issue_receipt(&mut tx, &p).await?;
    set_status(
        &mut tx,
        &u,
        UnidentifiedReceiptStatus::Identified,
        None,
        PAYMENT_COMPLETED,
        &a,
    )
    .await?;
    sqlx::query(
        "UPDATE finance.unidentified_receipt SET student_account_id = $2, payment_id = $3 WHERE id = $1",
    )
    .bind(u.id)
    .bind(account.id.as_uuid())
    .bind(p.id.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let subject = subject_ref(&mut tx, t, account.id).await?;
    let payload = Changed::new(p.id.as_uuid(), u.institution, "completed", p.version)
        .account(account.id, subject.clone())
        .amount(u.amount, "INR")
        .reason(Some(&p.payment_number))
        .journal(u.journal_entry_id);
    event(
        &mut tx,
        &scope,
        PAYMENT_COMPLETED,
        PAYMENT_COMPLETED_SCHEMA,
        p.id.to_string(),
        &payload.v,
        &a,
    )
    .await?;
    let rp = Changed::new(receipt_id, u.institution, "issued", 1)
        .account(account.id, subject)
        .amount(u.amount, "INR")
        .reason(Some(&receipt_number));
    event(
        &mut tx,
        &scope,
        RECEIPT_ISSUED,
        RECEIPT_ISSUED_SCHEMA,
        receipt_id.to_string(),
        &rp.v,
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "finance.unidentified_receipt.identify",
        "unidentified_receipt",
        id.to_string(),
        json!({"studentAccountId": account.id, "paymentId": p.id, "receiptNumber": receipt_number}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, id, StatusCode::OK).await
}

/// Allocates an identified receipt to the party's open charges (Dr unapplied · Cr receivables;
/// `payment.manage`, `If-Match`, `Idempotency-Key`). Fully allocated → `allocated`.
#[utoipa::path(post, path = "/api/v1/finance/unidentified-receipts/{id}/allocate", operation_id = "finance_unidentified_allocate", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header), ("Idempotency-Key" = String, Header)), request_body = AllocateRequest, security(("session_cookie" = [])),
    responses((status = 200, body = UnidentifiedReceiptView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn unidentified_allocate(
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
    let lines: Option<Vec<AllocationLine>> = req
        .allocations
        .as_ref()
        .map(|ls| {
            ls.iter()
                .enumerate()
                .map(|(i, l)| {
                    Ok(AllocationLine {
                        charge_id: ChargeId(l.charge_id),
                        amount: parse_amount(&format!("allocations[{i}].amount"), &l.amount)?,
                    })
                })
                .collect::<Result<Vec<_>, KernelError>>()
        })
        .transpose()?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let u0 = load(&mut conn, t, id, false).await?;
    drop(conn);
    require_version(v, u0.version)?;
    let Some(pid) = u0.payment_id else {
        return Err(KernelError::Conflict("identify the payer before allocating".into()).into());
    };
    if u0.status != UnidentifiedReceiptStatus::Identified {
        return Err(illegal("unidentified receipt", u0.status.as_str(), "allocated").into());
    }
    let scope = institution_scope(ctx, u0.institution);
    let account = StudentAccountId(u0.view.student_account_id.unwrap_or_default());
    let order = open_charge_order(&st, &scope, account, &[]).await?;
    let today = business_today(&st, &scope, u0.institution).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let u = load(&mut tx, t, id, true).await?;
    require_version(v, u.version)?;
    let p = load_payment(&mut tx, t, pid, true).await?;
    let p = allocate_unapplied(&mut tx, &scope, &p, lines.as_deref(), &order, today, &a).await?;
    if p.unapplied_amount == Decimal::ZERO {
        set_status(
            &mut tx,
            &u,
            UnidentifiedReceiptStatus::Allocated,
            None,
            UNIDENTIFIED_RECEIPT_RESOLVED,
            &a,
        )
        .await?;
        resolved_event(
            &mut tx,
            &scope,
            &u,
            UnidentifiedReceiptStatus::Allocated,
            Some(account),
            None,
            &a,
        )
        .await?;
    } else {
        sqlx::query("UPDATE finance.unidentified_receipt SET version = version + 1, updated_at = now() WHERE id = $1")
            .bind(u.id)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
    }
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, id, StatusCode::OK).await
}

/// Returns an unidentified receipt to the remitter: Dr unapplied receipts · Cr bank / cash
/// (`finance.approve` — money leaves; `If-Match`, `Idempotency-Key`).
#[utoipa::path(post, path = "/api/v1/finance/unidentified-receipts/{id}/refund", operation_id = "finance_unidentified_refund", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header), ("Idempotency-Key" = String, Header)), request_body = ReasonRequest, security(("session_cookie" = [])),
    responses((status = 200, body = UnidentifiedReceiptView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn unidentified_refund(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<ReasonRequest>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceApprove)?;
    require_idempotency_key(&headers)?;
    let v = match if_match(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let reason = require_text("reason", req.reason.as_deref())?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let u0 = load(&mut conn, t, id, false).await?;
    drop(conn);
    let scope = institution_scope(ctx, u0.institution);
    let today = business_today(&st, &scope, u0.institution).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let u = load(&mut tx, t, id, true).await?;
    require_version(v, u.version)?;
    if !can_transition_unidentified(u.status, UnidentifiedReceiptStatus::Refunded) {
        return Err(illegal("unidentified receipt", u.status.as_str(), "refunded").into());
    }
    let entry = posting::post(
        &mut tx,
        t,
        &PostingRequest {
            institution_id: u.institution,
            posting_date: today,
            kind: JournalEntryKind::Standard,
            source_type: source::UNIDENTIFIED_RECEIPT.into(),
            source_id: u.id,
            posting_key: "refund".into(),
            description: format!("Unidentified receipt returned: {reason}"),
            currency: CurrencyCode::INR,
            lines: vec![
                PostingLine::debit(
                    AccountRef::System(SystemAccount::UnappliedReceipts),
                    u.amount,
                ),
                PostingLine::credit(AccountRef::System(debit_account(u.channel)), u.amount),
            ],
        },
        &a,
    )
    .await?;
    set_status(
        &mut tx,
        &u,
        UnidentifiedReceiptStatus::Refunded,
        Some(&reason),
        UNIDENTIFIED_RECEIPT_RESOLVED,
        &a,
    )
    .await?;
    sqlx::query("UPDATE finance.unidentified_receipt SET resolution_entry_id = $2 WHERE id = $1")
        .bind(u.id)
        .bind(entry.journal_entry_id.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    resolved_event(
        &mut tx,
        &scope,
        &u,
        UnidentifiedReceiptStatus::Refunded,
        None,
        Some(entry.journal_entry_id.as_uuid()),
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "finance.unidentified_receipt.refund",
        "unidentified_receipt",
        id.to_string(),
        json!({"reason": reason, "journalEntryId": entry.journal_entry_id}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, id, StatusCode::OK).await
}

pub fn router(state: FinanceState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(unidentified_list, unidentified_record))
        .routes(routes!(unidentified_get))
        .routes(routes!(unidentified_investigate))
        .routes(routes!(unidentified_identify))
        .routes(routes!(unidentified_allocate))
        .routes(routes!(unidentified_refund))
        .with_state(state)
}
