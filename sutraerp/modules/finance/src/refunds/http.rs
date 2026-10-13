//! Refunds (L2-F03, L3-F-014 … F-019; tag `finance-refunds`). **The reference approval of
//! Batch 5:** a request moves `requested → under_review` through the WorkflowPort
//! (`REFUND_APPROVAL`); auto-approval applies the decision in the same transaction, otherwise
//! `POST /refunds/{id}/decision` (`finance.approve`, step-up) decides. Approval draws the
//! amount from the party's unapplied receipts (Dr unapplied receipts · Cr refunds payable);
//! completion pays it out (Dr refunds payable · Cr bank / gateway clearing).

use super::domain::{can_transition, draw_credit};
use crate::payments::common::{
    history, illegal, parse_amount, require_text, require_version, subject_ref, Changed,
};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::lifecycle::StatusChange;
use crate::shared::me::{my_accounts, require_own_account};
use crate::shared::money::{fmt, INR};
use crate::shared::numbering::{kind, next};
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
use std::collections::BTreeMap;
use sutra_contracts::finance::events::payments::{
    REFUND_CANCELLED, REFUND_CANCELLED_SCHEMA, REFUND_COMPLETED, REFUND_COMPLETED_SCHEMA,
    REFUND_DECIDED, REFUND_DECIDED_SCHEMA, REFUND_FAILED, REFUND_FAILED_SCHEMA, REFUND_REQUESTED,
    REFUND_REQUESTED_SCHEMA, REFUND_REVERSED, REFUND_REVERSED_SCHEMA,
};
use sutra_contracts::finance::ids::{JournalEntryId, RefundId, StudentAccountId};
use sutra_contracts::finance::ports::{
    source, AccountRef, GatewayRefundRequest, GatewayRefundStatus, PostingLine, PostingRequest,
    StudentAccountSummary, SystemAccount,
};
use sutra_contracts::finance::status::{JournalEntryKind, PaymentChannel, RefundStatus};
use sutra_kernel::auth::{AuthContext, Permission};
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

const COLUMNS: &str =
    "id, tenant_id, institution_id, student_account_id, refund_number, payment_id, \
    amount, currency, reason, method, status, status_reason, requested_via, requested_by, sources, \
    process_instance_id, decided_by, decided_at, decision_reason, approve_entry_id, pay_entry_id, \
    gateway_refund_ref, attempts, failure_reason, completed_at, closed_at, version, created_at";

// ------------------------------------------------------------------ rows and views

#[derive(Clone, Debug)]
pub struct RefundRow {
    pub id: Uuid,
    pub tenant: TenantId,
    pub institution: InstitutionId,
    pub account: StudentAccountId,
    pub number: String,
    pub payment_id: Option<Uuid>,
    pub amount: Decimal,
    pub currency: String,
    pub reason: String,
    pub method: String,
    pub status: RefundStatus,
    pub status_reason: Option<String>,
    pub requested_via: String,
    pub sources: Vec<(Uuid, Decimal)>,
    pub process_instance_id: Option<String>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decision_reason: Option<String>,
    pub approve_entry_id: Option<Uuid>,
    pub pay_entry_id: Option<Uuid>,
    pub gateway_refund_ref: Option<String>,
    pub attempts: i32,
    pub failure_reason: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

fn sources_from_json(v: &serde_json::Value) -> Vec<(Uuid, Decimal)> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| {
                    let p = s.get("paymentId")?.as_str()?.parse::<Uuid>().ok()?;
                    let amt = s.get("amount")?.as_str()?.parse::<Decimal>().ok()?;
                    Some((p, amt))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn sources_json(s: &[(Uuid, Decimal)]) -> serde_json::Value {
    serde_json::Value::Array(
        s.iter()
            .map(|(p, a)| json!({"paymentId": p.to_string(), "amount": fmt(*a)}))
            .collect(),
    )
}

fn from_row(r: &PgRow) -> Result<RefundRow, sqlx::Error> {
    let status: String = r.try_get("status")?;
    let sources: serde_json::Value = r.try_get("sources")?;
    Ok(RefundRow {
        id: r.try_get("id")?,
        tenant: TenantId(r.try_get("tenant_id")?),
        institution: InstitutionId(r.try_get("institution_id")?),
        account: StudentAccountId(r.try_get("student_account_id")?),
        number: r.try_get("refund_number")?,
        payment_id: r.try_get("payment_id")?,
        amount: r.try_get("amount")?,
        currency: r.try_get("currency")?,
        reason: r.try_get("reason")?,
        method: r.try_get("method")?,
        status: RefundStatus::parse(&status)
            .ok_or_else(|| sqlx::Error::Decode(format!("refund status `{status}`").into()))?,
        status_reason: r.try_get("status_reason")?,
        requested_via: r.try_get("requested_via")?,
        sources: sources_from_json(&sources),
        process_instance_id: r.try_get("process_instance_id")?,
        decided_by: r.try_get("decided_by")?,
        decided_at: r.try_get("decided_at")?,
        decision_reason: r.try_get("decision_reason")?,
        approve_entry_id: r.try_get("approve_entry_id")?,
        pay_entry_id: r.try_get("pay_entry_id")?,
        gateway_refund_ref: r.try_get("gateway_refund_ref")?,
        attempts: r.try_get("attempts")?,
        failure_reason: r.try_get("failure_reason")?,
        completed_at: r.try_get("completed_at")?,
        closed_at: r.try_get("closed_at")?,
        version: r.try_get("version")?,
        created_at: r.try_get("created_at")?,
    })
}

pub async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<RefundRow, KernelError> {
    let r = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM finance.refund WHERE tenant_id = $1 AND id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    ))
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: "refund" })?;
    from_row(&r).map_err(map_db_error)
}

/// One status history entry (historical reconstruction, L3-F-019).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HistoryView {
    pub from_status: Option<String>,
    pub to_status: String,
    pub reason: Option<String>,
    pub actor: Option<String>,
    pub process_instance_id: Option<String>,
    pub at: DateTime<Utc>,
}

/// A refund (contract §5.3).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RefundView {
    pub id: RefundId,
    pub refund_number: String,
    pub institution_id: Uuid,
    pub student_account_id: StudentAccountId,
    pub account_name: Option<String>,
    pub payment_id: Option<Uuid>,
    #[schema(value_type = String, example = "5000.00")]
    pub amount: String,
    pub currency: String,
    pub reason: String,
    /// `original` (back to the gateway instrument) or `bank_transfer`.
    pub method: String,
    pub status: RefundStatus,
    pub status_reason: Option<String>,
    /// `self` (student) or `staff`.
    pub requested_via: String,
    pub process_instance_id: Option<String>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decision_reason: Option<String>,
    /// Approval entry (Dr unapplied receipts · Cr refunds payable).
    pub journal_entry_id: Option<Uuid>,
    /// Payout entry (Dr refunds payable · Cr bank / gateway clearing).
    pub pay_entry_id: Option<Uuid>,
    pub gateway_refund_ref: Option<String>,
    pub attempts: i32,
    pub failure_reason: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub history: Vec<HistoryView>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RefundList {
    pub items: Vec<RefundView>,
}

async fn view(conn: &mut PgConnection, r: RefundRow) -> Result<RefundView, KernelError> {
    let history = sqlx::query(
        "SELECT from_status, to_status, reason, actor, process_instance_id, created_at FROM finance.status_history \
         WHERE tenant_id = $1 AND aggregate_type = 'refund' AND aggregate_id = $2 ORDER BY created_at, id",
    )
    .bind(r.tenant.as_uuid())
    .bind(r.id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(|h| -> Result<HistoryView, sqlx::Error> {
        Ok(HistoryView {
            from_status: h.try_get("from_status")?,
            to_status: h.try_get("to_status")?,
            reason: h.try_get("reason")?,
            actor: h.try_get("actor")?,
            process_instance_id: h.try_get("process_instance_id")?,
            at: h.try_get("created_at")?,
        })
    })
    .collect::<Result<Vec<_>, _>>()
    .map_err(map_db_error)?;
    let account_name: Option<String> = sqlx::query_scalar(
        "SELECT display_name FROM finance.student_account WHERE tenant_id = $1 AND id = $2",
    )
    .bind(r.tenant.as_uuid())
    .bind(r.account.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(RefundView {
        id: RefundId(r.id),
        refund_number: r.number,
        institution_id: r.institution.as_uuid(),
        student_account_id: r.account,
        account_name,
        payment_id: r.payment_id,
        amount: fmt(r.amount),
        currency: r.currency,
        reason: r.reason,
        method: r.method,
        status: r.status,
        status_reason: r.status_reason,
        requested_via: r.requested_via,
        process_instance_id: r.process_instance_id,
        decided_by: r.decided_by,
        decided_at: r.decided_at,
        decision_reason: r.decision_reason,
        journal_entry_id: r.approve_entry_id,
        pay_entry_id: r.pay_entry_id,
        gateway_refund_ref: r.gateway_refund_ref,
        attempts: r.attempts,
        failure_reason: r.failure_reason,
        completed_at: r.completed_at,
        closed_at: r.closed_at,
        history,
        created_at: r.created_at,
        version: r.version,
    })
}

async fn respond(
    st: &FinanceState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> ApiResult<Response> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let r = load(&mut conn, tenant, id, false).await?;
    let version = r.version;
    let v = view(&mut conn, r).await?;
    Ok(with_etag(status, &v, Version(version)))
}

// ------------------------------------------------------------------ transactional core

async fn set_status(
    conn: &mut PgConnection,
    r: &RefundRow,
    to: RefundStatus,
    reason: Option<&str>,
    cause: &'static str,
    process_instance: Option<&str>,
    actor: &Actor,
) -> Result<RefundRow, KernelError> {
    if !can_transition(r.status, to) {
        return Err(illegal("refund", r.status.as_str(), to.as_str()));
    }
    let n = sqlx::query(
        "UPDATE finance.refund SET status = $3, status_reason = $4, \
         process_instance_id = COALESCE($5, process_instance_id), version = version + 1, updated_at = now(), \
         closed_at = CASE WHEN $3 = 'closed' THEN now() ELSE closed_at END, \
         completed_at = CASE WHEN $3 = 'completed' THEN now() ELSE completed_at END \
         WHERE tenant_id = $1 AND id = $2 AND version = $6",
    )
    .bind(r.tenant.as_uuid())
    .bind(r.id)
    .bind(to.as_str())
    .bind(reason)
    .bind(process_instance)
    .bind(r.version)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?
    .rows_affected();
    if n != 1 {
        return Err(KernelError::VersionConflict {
            expected: r.version,
            actual: r.version + 1,
        });
    }
    history(
        conn,
        r.tenant,
        r.institution,
        "refund",
        r.id,
        Some(r.status.as_str()),
        to.as_str(),
        &StatusChange {
            cause_event_type: cause,
            reason,
            process_instance_id: process_instance.or(r.process_instance_id.as_deref()),
        },
        actor,
    )
    .await?;
    load(conn, r.tenant, r.id, false).await
}

#[allow(clippy::too_many_arguments)]
async fn emit(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    before: &RefundRow,
    after: &RefundRow,
    ev: &'static str,
    schema: &'static str,
    reason: Option<&str>,
    journal: Option<Uuid>,
    action: &str,
    actor: &Actor,
) -> Result<(), KernelError> {
    let subject = subject_ref(conn, after.tenant, after.account).await?;
    let payload = Changed::new(
        after.id,
        after.institution,
        after.status.as_str(),
        after.version,
    )
    .from(before.status.as_str())
    .account(after.account, subject)
    .amount(after.amount, &after.currency)
    .reason(reason.or(Some(&after.number)))
    .process(after.process_instance_id.as_deref())
    .journal(journal);
    event(
        conn,
        scope,
        ev,
        schema,
        after.id.to_string(),
        &payload.v,
        actor,
    )
    .await?;
    audit(
        conn,
        scope,
        action,
        "refund",
        after.id.to_string(),
        json!({"refundNumber": after.number, "from": before.status.as_str(), "to": after.status.as_str(),
               "amount": fmt(after.amount), "reason": reason, "journalEntryId": journal}),
        actor,
    )
    .await
}

fn post_req(
    r: &RefundRow,
    key: &str,
    date: NaiveDate,
    debit: SystemAccount,
    credit: SystemAccount,
    what: &str,
) -> PostingRequest {
    PostingRequest {
        institution_id: r.institution,
        posting_date: date,
        kind: JournalEntryKind::Standard,
        source_type: source::REFUND.into(),
        source_id: r.id,
        posting_key: key.into(),
        description: format!("Refund {} {what}", r.number),
        currency: CurrencyCode::INR,
        lines: vec![
            PostingLine::debit(AccountRef::System(debit), r.amount).for_charge(r.account, None),
            PostingLine::credit(AccountRef::System(credit), r.amount).for_charge(r.account, None),
        ],
    }
}

/// Approves an `under_review` refund in the caller's transaction: draws the unapplied credit,
/// posts Dr unapplied receipts · Cr refunds payable, emits `refund.decided`.
async fn approve_in_tx(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    r: &RefundRow,
    reason: Option<&str>,
    date: NaiveDate,
    actor: &Actor,
) -> Result<RefundRow, KernelError> {
    let pots: Vec<(Uuid, Decimal)> = sqlx::query_as(
        "SELECT id, unapplied_amount FROM finance.payment WHERE tenant_id = $1 AND student_account_id = $2 \
         AND status = 'completed' AND unapplied_amount > 0 AND ($3::uuid IS NULL OR id = $3) \
         ORDER BY completed_at, id FOR UPDATE",
    )
    .bind(r.tenant.as_uuid())
    .bind(r.account.as_uuid())
    .bind(r.payment_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let drawn = draw_credit(&pots, r.amount).ok_or_else(|| {
        KernelError::Conflict(format!(
            "the account's unapplied credit does not cover the refund of {}",
            fmt(r.amount)
        ))
    })?;
    for (p, a) in &drawn {
        sqlx::query("UPDATE finance.payment SET unapplied_amount = unapplied_amount - $2, version = version + 1, updated_at = now() WHERE id = $1")
            .bind(p)
            .bind(a)
            .execute(&mut *conn)
            .await
            .map_err(map_db_error)?;
    }
    let entry = posting::post(
        conn,
        r.tenant,
        &post_req(
            r,
            "approve",
            date,
            SystemAccount::UnappliedReceipts,
            SystemAccount::RefundsPayable,
            "approved",
        ),
        actor,
    )
    .await?;
    let after = set_status(
        conn,
        r,
        RefundStatus::Approved,
        reason,
        REFUND_DECIDED,
        None,
        actor,
    )
    .await?;
    sqlx::query(
        "UPDATE finance.refund SET sources = $2, approve_entry_id = $3, decided_by = $4, decided_at = now(), \
         decision_reason = $5 WHERE id = $1",
    )
    .bind(r.id)
    .bind(sources_json(&drawn))
    .bind(entry.journal_entry_id.as_uuid())
    .bind(actor.id.as_deref())
    .bind(reason)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let after = RefundRow {
        approve_entry_id: Some(entry.journal_entry_id.as_uuid()),
        ..after
    };
    emit(
        conn,
        scope,
        r,
        &after,
        REFUND_DECIDED,
        REFUND_DECIDED_SCHEMA,
        reason,
        Some(entry.journal_entry_id.as_uuid()),
        "finance.refund.approve",
        actor,
    )
    .await?;
    load(conn, r.tenant, r.id, false).await
}

/// Gives the drawn credit back to its payments and reverses `entries` (cancel / reverse).
async fn release_credit(
    conn: &mut PgConnection,
    r: &RefundRow,
    entries: &[Option<Uuid>],
    date: NaiveDate,
    reason: &str,
    actor: &Actor,
) -> Result<Option<Uuid>, KernelError> {
    let mut last = None;
    for e in entries.iter().flatten() {
        last = Some(
            posting::reverse(conn, r.tenant, JournalEntryId(*e), date, reason, actor)
                .await?
                .journal_entry_id
                .as_uuid(),
        );
    }
    if r.approve_entry_id.is_some() {
        for (p, a) in &r.sources {
            sqlx::query(
                "UPDATE finance.payment SET unapplied_amount = unapplied_amount + $2, version = version + 1, updated_at = now() \
                 WHERE id = $1 AND status = 'completed'",
            )
            .bind(p)
            .bind(a)
            .execute(&mut *conn)
            .await
            .map_err(map_db_error)?;
        }
    }
    Ok(last)
}

/// Available unapplied credit of an account (minus refunds awaiting a decision).
async fn available_credit(
    conn: &mut PgConnection,
    tenant: TenantId,
    account: StudentAccountId,
    payment: Option<Uuid>,
) -> Result<Decimal, KernelError> {
    let (credit, reserved): (Decimal, Decimal) = sqlx::query_as(
        "SELECT (SELECT COALESCE(sum(unapplied_amount), 0) FROM finance.payment WHERE tenant_id = $1 \
                 AND student_account_id = $2 AND status = 'completed' AND ($3::uuid IS NULL OR id = $3)), \
                (SELECT COALESCE(sum(amount), 0) FROM finance.refund WHERE tenant_id = $1 \
                 AND student_account_id = $2 AND status IN ('requested', 'under_review') \
                 AND ($3::uuid IS NULL OR payment_id = $3 OR payment_id IS NULL))",
    )
    .bind(tenant.as_uuid())
    .bind(account.as_uuid())
    .bind(payment)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok((credit - reserved).max(Decimal::ZERO))
}

// ------------------------------------------------------------------ request (shared by staff + self)

/// A refund request.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RefundRequestBody {
    #[schema(value_type = String, example = "5000.00")]
    pub amount: String,
    pub reason: String,
    /// Refund the unapplied credit of this payment (gateway payments refund to the original instrument).
    pub payment_id: Option<Uuid>,
    /// Which of the caller's accounts (self-service; defaults to the first).
    pub student_account_id: Option<Uuid>,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct RefundListQuery {
    pub status: Option<RefundStatus>,
}

async fn request_common(
    st: &FinanceState,
    ctx: &AuthContext,
    account: StudentAccountSummary,
    body: &RefundRequestBody,
    via: &str,
) -> ApiResult<Response> {
    let scope = institution_scope(ctx, account.institution_id);
    require_institution_writable(st, &scope, account.institution_id).await?;
    let amount = parse_amount("amount", &body.amount)?;
    let reason = require_text("reason", Some(&body.reason))?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let mut method = "bank_transfer";
    if let Some(pid) = body.payment_id {
        let p = crate::payments::service::load_payment(&mut conn, t, pid, false)
            .await
            .map_err(|_| {
                KernelError::Validation(vec![FieldViolation::new(
                    "paymentId",
                    "not_found",
                    "unknown payment",
                )])
            })?;
        if p.student_account_id != account.id {
            return Err(KernelError::Validation(vec![FieldViolation::new(
                "paymentId",
                "other_account",
                "the payment belongs to another account",
            )])
            .into());
        }
        if p.channel == PaymentChannel::Gateway && p.gateway_payment_ref.is_some() {
            method = "original";
        }
    }
    let available = available_credit(&mut conn, t, account.id, body.payment_id).await?;
    drop(conn);
    if amount > available {
        return Err(KernelError::Conflict(format!(
            "only {} of unapplied credit is available for a refund",
            fmt(available)
        ))
        .into());
    }
    let id = Uuid::now_v7();
    let mut vars = BTreeMap::new();
    vars.insert("amount".to_owned(), json!(fmt(amount)));
    vars.insert("studentAccountId".to_owned(), json!(account.id.to_string()));
    let outcome = request_approval(
        st,
        &scope,
        process::REFUND_APPROVAL,
        business_key("refund", id, 1),
        vars,
    )
    .await?;
    let today = business_today(st, &scope, account.institution_id).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let number = next(&mut tx, t, account.institution_id, kind::REFUND).await?;
    sqlx::query(
        "INSERT INTO finance.refund (id, tenant_id, institution_id, student_account_id, refund_number, payment_id, \
         amount, reason, method, status, requested_via, requested_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'requested', $10, $11)",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(account.institution_id.as_uuid())
    .bind(account.id.as_uuid())
    .bind(&number)
    .bind(body.payment_id)
    .bind(amount)
    .bind(&reason)
    .bind(method)
    .bind(via)
    .bind(a.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        account.institution_id,
        "refund",
        id,
        None,
        "requested",
        &StatusChange::default(),
        &a,
    )
    .await?;
    let requested = load(&mut tx, t, id, true).await?;
    let pid = outcome.process_instance_id().to_owned();
    let review = set_status(
        &mut tx,
        &requested,
        RefundStatus::UnderReview,
        None,
        REFUND_REQUESTED,
        Some(&pid),
        &a,
    )
    .await?;
    emit(
        &mut tx,
        &scope,
        &requested,
        &review,
        REFUND_REQUESTED,
        REFUND_REQUESTED_SCHEMA,
        Some(&reason),
        None,
        "finance.refund.request",
        &a,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let system = Actor::system(a.correlation_id.clone());
        approve_in_tx(
            &mut tx,
            &scope,
            &review,
            Some("auto-approved"),
            today,
            &system,
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    respond(st, t, id, StatusCode::CREATED).await
}

// ------------------------------------------------------------------ handlers

/// Refunds of an institution (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/refunds", operation_id = "finance_refund_list", tag = "finance-refunds",
    params(("id" = String, Path, format = Uuid), RefundListQuery), security(("session_cookie" = [])),
    responses((status = 200, body = RefundList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn refund_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<RefundListQuery>,
) -> ApiResult<Json<RefundList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    require_institution(
        &st,
        &institution_scope(ctx, InstitutionId(id)),
        InstitutionId(id),
    )
    .await?;
    Ok(Json(
        list(&st, tenant(ctx), Some(id), None, q.status).await?,
    ))
}

async fn list(
    st: &FinanceState,
    t: TenantId,
    inst: Option<Uuid>,
    accounts: Option<Vec<Uuid>>,
    status: Option<RefundStatus>,
) -> Result<RefundList, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM finance.refund WHERE tenant_id = $1 AND ($2::uuid IS NULL OR institution_id = $2) \
         AND ($3::uuid[] IS NULL OR student_account_id = ANY($3)) AND ($4::text IS NULL OR status = $4) \
         ORDER BY created_at DESC, id DESC LIMIT 200"
    ))
    .bind(t.as_uuid())
    .bind(inst)
    .bind(accounts)
    .bind(status.map(|s| s.as_str()))
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        let row = from_row(r).map_err(map_db_error)?;
        items.push(view(&mut conn, row).await?);
    }
    Ok(RefundList { items })
}

/// Refunds of one party account (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/student-accounts/{id}/refunds", operation_id = "finance_refund_account_list", tag = "finance-refunds",
    params(("id" = String, Path, format = Uuid), RefundListQuery), security(("session_cookie" = [])),
    responses((status = 200, body = RefundList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn refund_account_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<RefundListQuery>,
) -> ApiResult<Json<RefundList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    Ok(Json(
        list(&st, tenant(ctx), None, Some(vec![id]), q.status).await?,
    ))
}

/// Requests a refund on behalf of a party (`refund.manage`, `Idempotency-Key`); goes through
/// the refund approval (auto-approved in Batch 5).
#[utoipa::path(post, path = "/api/v1/finance/student-accounts/{id}/refunds", operation_id = "finance_refund_request", tag = "finance-refunds",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)), request_body = RefundRequestBody, security(("session_cookie" = [])),
    responses((status = 201, body = RefundView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn refund_request(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<RefundRequestBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::RefundManage)?;
    require_idempotency_key(&headers)?;
    let account = st
        .charges
        .student_account(&tenant_scope(ctx), StudentAccountId(id))
        .await?
        .ok_or(KernelError::NotFound {
            entity: "student account",
        })?;
    request_common(&st, ctx, account, &body, "staff").await
}

/// One refund with its full history (`finance.read`; L3-F-019 reconstruction).
#[utoipa::path(get, path = "/api/v1/finance/refunds/{id}", operation_id = "finance_refund_get", tag = "finance-refunds",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = RefundView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn refund_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceRead)?;
    respond(&st, tenant(ctx), id, StatusCode::OK).await
}

/// `approve | reject` decision of a refund under review.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionRequest {
    /// `approve` or `reject`.
    pub decision: String,
    pub reason: Option<String>,
}

/// Decides a refund under review (`finance.approve` + step-up, `If-Match`, `Idempotency-Key`).
#[utoipa::path(post, path = "/api/v1/finance/refunds/{id}/decision", operation_id = "finance_refund_decide", tag = "finance-refunds",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header), ("Idempotency-Key" = String, Header)), request_body = DecisionRequest, security(("session_cookie" = [])),
    responses((status = 200, body = RefundView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn refund_decide(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<DecisionRequest>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceApprove)?;
    require_idempotency_key(&headers)?;
    let v = match if_match(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let approve = match body.decision.as_str() {
        "approve" => true,
        "reject" => false,
        _ => {
            return Err(KernelError::Validation(vec![FieldViolation::new(
                "decision",
                "invalid",
                "approve or reject",
            )])
            .into())
        }
    };
    let reason = body
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if !approve && reason.is_none() {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "reason",
            "required",
            "a rejection needs a reason",
        )])
        .into());
    }
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let r0 = load(&mut conn, t, id, false).await?;
    drop(conn);
    let scope = institution_scope(ctx, r0.institution);
    let today = business_today(&st, &scope, r0.institution).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load(&mut tx, t, id, true).await?;
    require_version(v, r.version)?;
    if r.status != RefundStatus::UnderReview {
        return Err(illegal(
            "refund",
            r.status.as_str(),
            if approve { "approved" } else { "rejected" },
        )
        .into());
    }
    if approve {
        approve_in_tx(&mut tx, &scope, &r, reason, today, &a).await?;
    } else {
        let after = set_status(
            &mut tx,
            &r,
            RefundStatus::Rejected,
            reason,
            REFUND_DECIDED,
            None,
            &a,
        )
        .await?;
        sqlx::query("UPDATE finance.refund SET decided_by = $2, decided_at = now(), decision_reason = $3 WHERE id = $1")
            .bind(id)
            .bind(a.id.as_deref())
            .bind(reason)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        emit(
            &mut tx,
            &scope,
            &r,
            &after,
            REFUND_DECIDED,
            REFUND_DECIDED_SCHEMA,
            reason,
            None,
            "finance.refund.reject",
            &a,
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, id, StatusCode::OK).await
}

/// Optional processing note.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProcessRequest {
    /// Bank / gateway reference recorded with a manual completion.
    pub reference: Option<String>,
    pub reason: Option<String>,
}

/// Pays out an approved (or retries a failed) refund (L3-F-015/017): gateway refunds go back to
/// the original instrument (processed → completed, failed → failed, pending → processing); bank
/// transfers stay `processing` until confirmed with `/complete` (`refund.manage`, `If-Match`,
/// `Idempotency-Key`).
async fn process_common(
    st: &FinanceState,
    ctx: &AuthContext,
    headers: &HeaderMap,
    id: Uuid,
    retry: bool,
) -> ApiResult<Response> {
    require_idempotency_key(headers)?;
    let v = match if_match(headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let r0 = load(&mut conn, t, id, false).await?;
    require_version(v, r0.version)?;
    let expected = if retry {
        RefundStatus::Failed
    } else {
        RefundStatus::Approved
    };
    if r0.status != expected {
        return Err(illegal("refund", r0.status.as_str(), "processing").into());
    }
    let payment_ref = match (r0.method.as_str(), r0.payment_id) {
        ("original", Some(pid)) => {
            crate::payments::service::load_payment(&mut conn, t, pid, false)
                .await?
                .gateway_payment_ref
        }
        _ => None,
    };
    drop(conn);
    let scope = institution_scope(ctx, r0.institution);
    require_institution_writable(st, &scope, r0.institution).await?;
    let attempt = r0.attempts + 1;
    let gateway = match payment_ref {
        Some(pref) => Some(
            st.gateway
                .refund(
                    &scope,
                    GatewayRefundRequest {
                        payment_ref: pref,
                        amount: r0.amount,
                        currency: INR,
                        idempotency_key: format!("refund:{id}:{attempt}"),
                    },
                )
                .await?,
        ),
        None => None,
    };
    let today = business_today(st, &scope, r0.institution).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load(&mut tx, t, id, true).await?;
    require_version(v, r.version)?;
    let processing = set_status(&mut tx, &r, RefundStatus::Processing, None, "", None, &a).await?;
    sqlx::query("UPDATE finance.refund SET attempts = attempts + 1, gateway_refund_ref = COALESCE($2, gateway_refund_ref) WHERE id = $1")
        .bind(id)
        .bind(gateway.as_ref().map(|g| g.refund_ref.clone()))
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let processing = RefundRow {
        attempts: attempt,
        ..processing
    };
    audit(
        &mut tx,
        &scope,
        if retry {
            "finance.refund.retry"
        } else {
            "finance.refund.process"
        },
        "refund",
        id.to_string(),
        json!({"attempt": attempt, "method": r.method}),
        &a,
    )
    .await?;
    match gateway.map(|g| g.status) {
        Some(GatewayRefundStatus::Processed) => {
            complete_in_tx(&mut tx, &scope, &processing, None, today, &a).await?;
        }
        Some(GatewayRefundStatus::Failed) => {
            let after = set_status(
                &mut tx,
                &processing,
                RefundStatus::Failed,
                Some("gateway refund failed"),
                REFUND_FAILED,
                None,
                &a,
            )
            .await?;
            sqlx::query("UPDATE finance.refund SET failure_reason = 'gateway refund failed (mock)' WHERE id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(map_db_error)?;
            emit(
                &mut tx,
                &scope,
                &processing,
                &after,
                REFUND_FAILED,
                REFUND_FAILED_SCHEMA,
                Some("gateway refund failed"),
                None,
                "finance.refund.fail",
                &a,
            )
            .await?;
        }
        Some(GatewayRefundStatus::Pending) | None => {}
    }
    tx.commit().await.map_err(map_db_error)?;
    respond(st, t, id, StatusCode::OK).await
}

async fn complete_in_tx(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    r: &RefundRow,
    reference: Option<&str>,
    date: NaiveDate,
    actor: &Actor,
) -> Result<RefundRow, KernelError> {
    let credit = if r.method == "original" {
        SystemAccount::GatewayClearing
    } else {
        SystemAccount::BankMain
    };
    let entry = posting::post(
        conn,
        r.tenant,
        &post_req(
            r,
            "pay",
            date,
            SystemAccount::RefundsPayable,
            credit,
            "paid",
        ),
        actor,
    )
    .await?;
    let after = set_status(
        conn,
        r,
        RefundStatus::Completed,
        reference,
        REFUND_COMPLETED,
        None,
        actor,
    )
    .await?;
    sqlx::query("UPDATE finance.refund SET pay_entry_id = $2, failure_reason = NULL, gateway_refund_ref = COALESCE(gateway_refund_ref, $3) WHERE id = $1")
        .bind(r.id)
        .bind(entry.journal_entry_id.as_uuid())
        .bind(reference)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    emit(
        conn,
        scope,
        r,
        &after,
        REFUND_COMPLETED,
        REFUND_COMPLETED_SCHEMA,
        reference,
        Some(entry.journal_entry_id.as_uuid()),
        "finance.refund.complete",
        actor,
    )
    .await?;
    load(conn, r.tenant, r.id, false).await
}

/// Pays out an approved refund (L3-F-015).
#[utoipa::path(post, path = "/api/v1/finance/refunds/{id}/process", operation_id = "finance_refund_process", tag = "finance-refunds",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header), ("Idempotency-Key" = String, Header)), security(("session_cookie" = [])),
    responses((status = 200, body = RefundView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn refund_process(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::RefundManage)?;
    process_common(&st, ctx, &headers, id, false).await
}

/// Retries a failed refund (L3-F-017).
#[utoipa::path(post, path = "/api/v1/finance/refunds/{id}/retry", operation_id = "finance_refund_retry", tag = "finance-refunds",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header), ("Idempotency-Key" = String, Header)), security(("session_cookie" = [])),
    responses((status = 200, body = RefundView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn refund_retry(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::RefundManage)?;
    process_common(&st, ctx, &headers, id, true).await
}

/// Confirms the payout of a processing refund, or resolves a failed one manually (paid by other
/// means) (`refund.manage`, `If-Match`, `Idempotency-Key`).
#[utoipa::path(post, path = "/api/v1/finance/refunds/{id}/complete", operation_id = "finance_refund_complete", tag = "finance-refunds",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header), ("Idempotency-Key" = String, Header)), request_body = ProcessRequest, security(("session_cookie" = [])),
    responses((status = 200, body = RefundView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn refund_complete(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ProcessRequest>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::RefundManage)?;
    require_idempotency_key(&headers)?;
    let v = match if_match(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let r0 = load(&mut conn, t, id, false).await?;
    drop(conn);
    let scope = institution_scope(ctx, r0.institution);
    let today = business_today(&st, &scope, r0.institution).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load(&mut tx, t, id, true).await?;
    require_version(v, r.version)?;
    let reference = body
        .reference
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    complete_in_tx(&mut tx, &scope, &r, reference, today, &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, id, StatusCode::OK).await
}

/// Cancels / withdraws a refund before payout (L3-F-016/017): an approved one gives its credit
/// back (approval entry reversed) (`refund.manage`, `If-Match`).
#[utoipa::path(post, path = "/api/v1/finance/refunds/{id}/cancel", operation_id = "finance_refund_cancel", tag = "finance-refunds",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = ProcessRequest, security(("session_cookie" = [])),
    responses((status = 200, body = RefundView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn refund_cancel(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ProcessRequest>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::RefundManage)?;
    let v = match if_match(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let reason = require_text("reason", body.reason.as_deref())?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let r0 = load(&mut conn, t, id, false).await?;
    drop(conn);
    let scope = institution_scope(ctx, r0.institution);
    let today = business_today(&st, &scope, r0.institution).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load(&mut tx, t, id, true).await?;
    require_version(v, r.version)?;
    if !can_transition(r.status, RefundStatus::Cancelled) {
        return Err(illegal("refund", r.status.as_str(), "cancelled").into());
    }
    let reversal = release_credit(&mut tx, &r, &[r.approve_entry_id], today, &reason, &a).await?;
    let after = set_status(
        &mut tx,
        &r,
        RefundStatus::Cancelled,
        Some(&reason),
        REFUND_CANCELLED,
        None,
        &a,
    )
    .await?;
    emit(
        &mut tx,
        &scope,
        &r,
        &after,
        REFUND_CANCELLED,
        REFUND_CANCELLED_SCHEMA,
        Some(&reason),
        reversal,
        "finance.refund.cancel",
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, id, StatusCode::OK).await
}

/// Reverses a completed refund (money recovered / returned; L3-F-018): payout and approval
/// entries reversed, the credit goes back to the party (`finance.approve` + step-up, `If-Match`).
#[utoipa::path(post, path = "/api/v1/finance/refunds/{id}/reverse", operation_id = "finance_refund_reverse", tag = "finance-refunds",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = ProcessRequest, security(("session_cookie" = [])),
    responses((status = 200, body = RefundView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn refund_reverse(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ProcessRequest>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceApprove)?;
    let v = match if_match(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let reason = require_text("reason", body.reason.as_deref())?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let r0 = load(&mut conn, t, id, false).await?;
    drop(conn);
    require_version(v, r0.version)?;
    let scope = institution_scope(ctx, r0.institution);
    let mut vars = BTreeMap::new();
    vars.insert("refundNumber".to_owned(), json!(r0.number));
    let outcome = request_approval(
        &st,
        &scope,
        process::REFUND_REVERSAL,
        business_key("refund", id, r0.version),
        vars,
    )
    .await?;
    let today = business_today(&st, &scope, r0.institution).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load(&mut tx, t, id, true).await?;
    require_version(v, r.version)?;
    if !can_transition(r.status, RefundStatus::Reversed) {
        return Err(illegal("refund", r.status.as_str(), "reversed").into());
    }
    let reversal = release_credit(
        &mut tx,
        &r,
        &[r.pay_entry_id, r.approve_entry_id],
        today,
        &reason,
        &a,
    )
    .await?;
    let after = set_status(
        &mut tx,
        &r,
        RefundStatus::Reversed,
        Some(&reason),
        REFUND_REVERSED,
        Some(outcome.process_instance_id()),
        &a,
    )
    .await?;
    emit(
        &mut tx,
        &scope,
        &r,
        &after,
        REFUND_REVERSED,
        REFUND_REVERSED_SCHEMA,
        Some(&reason),
        reversal,
        "finance.refund.reverse",
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, id, StatusCode::OK).await
}

/// Closes a finished refund (L3-F-019; history stays reconstructable) (`refund.manage`, `If-Match`).
#[utoipa::path(post, path = "/api/v1/finance/refunds/{id}/close", operation_id = "finance_refund_close", tag = "finance-refunds",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), security(("session_cookie" = [])),
    responses((status = 200, body = RefundView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn refund_close(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::RefundManage)?;
    let v = match if_match(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load(&mut tx, t, id, true).await?;
    require_version(v, r.version)?;
    set_status(&mut tx, &r, RefundStatus::Closed, None, "", None, &a).await?;
    audit(
        &mut tx,
        &institution_scope(ctx, r.institution),
        "finance.refund.close",
        "refund",
        id.to_string(),
        json!({"refundNumber": r.number, "from": r.status.as_str()}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, id, StatusCode::OK).await
}

/// The caller's own refunds (`finance.self`).
#[utoipa::path(get, path = "/api/v1/finance/me/refunds", operation_id = "finance_refund_self_list", tag = "finance-refunds",
    security(("session_cookie" = [])),
    responses((status = 200, body = RefundList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_refund_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
) -> ApiResult<Json<RefundList>> {
    let ctx = &auth.0;
    let ids: Vec<Uuid> = my_accounts(&st, ctx)
        .await?
        .iter()
        .map(|a| a.id.as_uuid())
        .collect();
    Ok(Json(list(&st, tenant(ctx), None, Some(ids), None).await?))
}

/// The caller requests a refund of their own unapplied credit (`finance.self`, `Idempotency-Key`).
#[utoipa::path(post, path = "/api/v1/finance/me/refunds", operation_id = "finance_refund_self_request", tag = "finance-refunds",
    params(("Idempotency-Key" = String, Header)), request_body = RefundRequestBody, security(("session_cookie" = [])),
    responses((status = 201, body = RefundView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_refund_request(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(body): Json<RefundRequestBody>,
) -> ApiResult<Response> {
    let ctx = &auth.0;
    require_idempotency_key(&headers)?;
    let account = match body.student_account_id {
        Some(id) => require_own_account(&st, ctx, StudentAccountId(id)).await?,
        None => my_accounts(&st, ctx)
            .await?
            .into_iter()
            .next()
            .ok_or(KernelError::NotFound {
                entity: "student account",
            })?,
    };
    request_common(&st, ctx, account, &body, "self").await
}

pub fn router(state: FinanceState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(refund_list))
        .routes(routes!(refund_account_list, refund_request))
        .routes(routes!(refund_get))
        .routes(routes!(refund_decide))
        .routes(routes!(refund_process))
        .routes(routes!(refund_retry))
        .routes(routes!(refund_complete))
        .routes(routes!(refund_cancel))
        .routes(routes!(refund_reverse))
        .routes(routes!(refund_close))
        .routes(routes!(my_refund_list, my_refund_request))
        .with_state(state)
}
