//! Treasury (L2-F10, L3-F-056…061): bank accounts mapped to postable asset ledger accounts
//! (request → approval through the WorkflowPort → active), gateway settlement to the bank (the
//! `payment` / `settle` posting of contract §3.3: Dr bank + bank charges · Cr gateway clearing),
//! returned / failed bank transactions (mirror entries via `shared::posting::reverse`), bank
//! statement import (JSON lines) and statement ↔ ledger reconciliation, and the cash position.
//!
//! Gateway captures are found in the journal itself (`payment` / `capture` entries with a
//! gateway clearing debit), so treasury never reads or writes 5B's tables.

use super::domain::{can_transition_account, match_score, parse_statement, StatementLine};
use crate::ledger::accounts::insert_account;
use crate::ledger::common::{
    changed, emit, history, illegal, invalid, load_history, opt_text, parse_amount_or_zero,
    parse_decision, parse_signed, require_text, require_version, HistoryView,
};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::coa::resolve;
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::lifecycle::StatusChange;
use crate::shared::money::fmt;
use crate::shared::posting;
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
use sha2::{Digest, Sha256};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::{BTreeMap, HashSet};
use sutra_contracts::finance::events::ledger::{
    BANK_ACCOUNT_ACTIVATED, BANK_ACCOUNT_ACTIVATED_SCHEMA, BANK_STATEMENT_RECONCILED,
    BANK_STATEMENT_RECONCILED_SCHEMA, BANK_TRANSACTION_FAILED, BANK_TRANSACTION_FAILED_SCHEMA,
};
use sutra_contracts::finance::ids::{
    BankAccountId, BankReconciliationId, BankStatementId, BankStatementLineId, BankTransactionId,
    JournalEntryId, LedgerAccountId,
};
use sutra_contracts::finance::ports::{
    source, AccountRef, PostingLine, PostingRequest, SystemAccount,
};
use sutra_contracts::finance::status::{
    BankAccountStatus, BankStatementStatus, BankTransactionStatus, JournalEntryKind,
    LedgerAccountType, ReconciliationStatus,
};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::money::CurrencyCode;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use uuid::Uuid;

// ================================================================== bank accounts

/// A treasury bank account.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BankAccountView {
    pub id: BankAccountId,
    pub institution_id: Uuid,
    pub name: String,
    pub bank_name: String,
    pub account_number_last4: String,
    pub ifsc: String,
    /// `collection`, `operating`, `payout` or `deposit`.
    pub purpose: String,
    pub ledger_account_id: LedgerAccountId,
    pub ledger_account_code: String,
    pub currency: String,
    pub status: BankAccountStatus,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    /// Ledger balance of the mapped account (debit normal).
    #[schema(value_type = String, example = "100000.00")]
    pub ledger_balance: String,
    pub unreconciled_lines: i64,
    pub history: Vec<HistoryView>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BankAccountList {
    pub items: Vec<BankAccountView>,
}

#[derive(Clone, Debug)]
struct AccountRow {
    id: Uuid,
    tenant: TenantId,
    institution: InstitutionId,
    name: String,
    ledger: Uuid,
    status: BankAccountStatus,
    requested_by: Option<String>,
    process_instance_id: Option<String>,
    version: i64,
}

async fn load_account(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<AccountRow, KernelError> {
    let r = sqlx::query(&format!(
        "SELECT id, tenant_id, institution_id, name, ledger_account_id, status, requested_by, \
         process_instance_id, version FROM finance.bank_account WHERE tenant_id = $1 AND id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    ))
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "bank account",
    })?;
    let st: String = r.try_get("status").map_err(map_db_error)?;
    Ok(AccountRow {
        id: r.try_get("id").map_err(map_db_error)?,
        tenant: TenantId(r.try_get("tenant_id").map_err(map_db_error)?),
        institution: InstitutionId(r.try_get("institution_id").map_err(map_db_error)?),
        name: r.try_get("name").map_err(map_db_error)?,
        ledger: r.try_get("ledger_account_id").map_err(map_db_error)?,
        status: BankAccountStatus::parse(&st)
            .ok_or_else(|| KernelError::internal_msg(format!("bank account status `{st}`")))?,
        requested_by: r.try_get("requested_by").map_err(map_db_error)?,
        process_instance_id: r.try_get("process_instance_id").map_err(map_db_error)?,
        version: r.try_get("version").map_err(map_db_error)?,
    })
}

/// Σ debit − Σ credit of a ledger account (optionally up to a date).
async fn ledger_balance(
    conn: &mut PgConnection,
    ledger: Uuid,
    as_of: Option<NaiveDate>,
) -> Result<Decimal, KernelError> {
    sqlx::query_scalar(
        "SELECT COALESCE(sum(l.debit - l.credit), 0) FROM finance.journal_line l \
         JOIN finance.journal_entry e ON e.id = l.journal_entry_id \
         WHERE l.ledger_account_id = $1 AND ($2::date IS NULL OR e.posting_date <= $2)",
    )
    .bind(ledger)
    .bind(as_of)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)
}

async fn account_view(
    conn: &mut PgConnection,
    id: Uuid,
    tenant: TenantId,
) -> Result<BankAccountView, KernelError> {
    let r = sqlx::query(
        "SELECT b.*, a.code AS ledger_code, \
         (SELECT count(*) FROM finance.bank_statement_line l JOIN finance.bank_statement s ON s.id = l.bank_statement_id \
          WHERE s.bank_account_id = b.id AND l.match_status = 'unmatched') AS unreconciled \
         FROM finance.bank_account b JOIN finance.ledger_account a ON a.id = b.ledger_account_id \
         WHERE b.tenant_id = $1 AND b.id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "bank account",
    })?;
    let st: String = r.try_get("status").map_err(map_db_error)?;
    let ledger: Uuid = r.try_get("ledger_account_id").map_err(map_db_error)?;
    let balance = ledger_balance(conn, ledger, None).await?;
    let history = load_history(conn, tenant, "bank_account", id).await?;
    Ok(BankAccountView {
        id: BankAccountId(id),
        institution_id: r.try_get("institution_id").map_err(map_db_error)?,
        name: r.try_get("name").map_err(map_db_error)?,
        bank_name: r.try_get("bank_name").map_err(map_db_error)?,
        account_number_last4: r.try_get("account_number_last4").map_err(map_db_error)?,
        ifsc: r.try_get("ifsc").map_err(map_db_error)?,
        purpose: r.try_get("purpose").map_err(map_db_error)?,
        ledger_account_id: LedgerAccountId(ledger),
        ledger_account_code: r.try_get("ledger_code").map_err(map_db_error)?,
        currency: r.try_get("currency").map_err(map_db_error)?,
        status: BankAccountStatus::parse(&st)
            .ok_or_else(|| KernelError::internal_msg(format!("bank account status `{st}`")))?,
        status_reason: r.try_get("status_reason").map_err(map_db_error)?,
        process_instance_id: r.try_get("process_instance_id").map_err(map_db_error)?,
        ledger_balance: fmt(balance),
        unreconciled_lines: r.try_get("unreconciled").map_err(map_db_error)?,
        history,
        version: r.try_get("version").map_err(map_db_error)?,
    })
}

async fn respond_account(
    st: &FinanceState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> ApiResult<Response> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v = account_view(&mut conn, id, tenant).await?;
    let ver = v.version;
    Ok(with_etag(status, &v, Version(ver)))
}

async fn set_account_status(
    conn: &mut PgConnection,
    a: &AccountRow,
    to: BankAccountStatus,
    reason: Option<&str>,
    cause: &'static str,
    process_instance: Option<&str>,
    actor: &Actor,
) -> Result<AccountRow, KernelError> {
    if !can_transition_account(a.status, to) {
        return Err(illegal("bank account", a.status.as_str(), to.as_str()));
    }
    let n = sqlx::query(
        "UPDATE finance.bank_account SET status = $3, status_reason = $4, \
         process_instance_id = COALESCE($5, process_instance_id), version = version + 1, updated_at = now() \
         WHERE tenant_id = $1 AND id = $2 AND version = $6",
    )
    .bind(a.tenant.as_uuid())
    .bind(a.id)
    .bind(to.as_str())
    .bind(reason)
    .bind(process_instance)
    .bind(a.version)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?
    .rows_affected();
    if n != 1 {
        return Err(KernelError::VersionConflict {
            expected: a.version,
            actual: a.version + 1,
        });
    }
    history(
        conn,
        a.tenant,
        a.institution,
        "bank_account",
        a.id,
        Some(a.status.as_str()),
        to.as_str(),
        &StatusChange {
            cause_event_type: cause,
            reason,
            process_instance_id: process_instance.or(a.process_instance_id.as_deref()),
        },
        actor,
    )
    .await?;
    load_account(conn, a.tenant, a.id, true).await
}

/// `requested → approved → active` in the caller's transaction (`bank_account.activated`).
async fn activate(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    a: &AccountRow,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let approved = set_account_status(
        conn,
        a,
        BankAccountStatus::Approved,
        reason,
        BANK_ACCOUNT_ACTIVATED,
        None,
        actor,
    )
    .await?;
    let active = set_account_status(
        conn,
        &approved,
        BankAccountStatus::Active,
        reason,
        BANK_ACCOUNT_ACTIVATED,
        None,
        actor,
    )
    .await?;
    sqlx::query(
        "UPDATE finance.bank_account SET decided_by = $2, decided_at = now() WHERE id = $1",
    )
    .bind(a.id)
    .bind(actor.id.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    emit(
        conn,
        scope,
        Some((BANK_ACCOUNT_ACTIVATED, BANK_ACCOUNT_ACTIVATED_SCHEMA)),
        &changed(
            a.id,
            a.institution,
            Some(a.status.as_str()),
            active.status.as_str(),
            active.version,
            None,
            Some(&a.name),
            active.process_instance_id.as_deref(),
            None,
        ),
        "finance.bank_account.activate",
        "bank_account",
        json!({"name": a.name, "reason": reason}),
        actor,
    )
    .await
}

/// A bank account request.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BankAccountCreate {
    pub name: String,
    pub bank_name: String,
    /// Last four characters of the account number.
    pub account_number_last4: String,
    pub ifsc: String,
    /// `collection` (default), `operating`, `payout`, `deposit`.
    pub purpose: Option<String>,
    /// An existing postable asset account; by default the seeded main bank account (first bank
    /// account) or a new `12xx` account.
    pub ledger_account_id: Option<Uuid>,
}

fn valid_ifsc(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 11
        && b[..4].iter().all(u8::is_ascii_uppercase)
        && b[4] == b'0'
        && b[5..]
            .iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// Bank accounts of an institution (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/bank-accounts", operation_id = "finance_bank_account_list", tag = "finance-treasury",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = BankAccountList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn bank_account_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<BankAccountList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM finance.bank_account WHERE tenant_id = $1 AND institution_id = $2 ORDER BY created_at",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(ids.len());
    for b in ids {
        items.push(account_view(&mut conn, b, t).await?);
    }
    Ok(Json(BankAccountList { items }))
}

/// Requests a bank account (`treasury.manage`, `Idempotency-Key`; L3-F-056); goes through
/// `BANK_ACCOUNT_APPROVAL` (auto-approved in Batch 5 → `active`).
#[utoipa::path(post, path = "/api/v1/finance/institutions/{id}/bank-accounts", operation_id = "finance_bank_account_create", tag = "finance-treasury",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)), request_body = BankAccountCreate, security(("session_cookie" = [])),
    responses((status = 201, body = BankAccountView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn bank_account_create(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<BankAccountCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TreasuryManage)?;
    require_idempotency_key(&headers)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let name = require_text("name", Some(&body.name))?;
    let bank_name = require_text("bankName", Some(&body.bank_name))?;
    let last4 = body.account_number_last4.trim().to_uppercase();
    if last4.len() != 4 || !last4.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(invalid(
            "accountNumberLast4",
            "invalid",
            "the last four characters of the account number",
        )
        .into());
    }
    let ifsc = body.ifsc.trim().to_uppercase();
    if !valid_ifsc(&ifsc) {
        return Err(invalid("ifsc", "invalid", "an IFSC such as HDFC0001234").into());
    }
    let purpose = body.purpose.as_deref().unwrap_or("collection").to_owned();
    if !["collection", "operating", "payout", "deposit"].contains(&purpose.as_str()) {
        return Err(invalid(
            "purpose",
            "invalid",
            "collection, operating, payout or deposit",
        )
        .into());
    }
    let t = tenant(ctx);
    let a = actor(ctx);
    let bid = Uuid::now_v7();
    let outcome = request_approval(
        &st,
        &scope,
        process::BANK_ACCOUNT_APPROVAL,
        business_key("bank_account", bid, 1),
        BTreeMap::from([("name".to_owned(), json!(name))]),
    )
    .await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 56))")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let ledger = match body.ledger_account_id {
        Some(l) => {
            let acc = resolve(&mut tx, t, inst, AccountRef::Id(LedgerAccountId(l)))
                .await
                .map_err(|e| match e {
                    KernelError::NotFound { .. } => {
                        invalid("ledgerAccountId", "not_found", "unknown ledger account")
                    }
                    other => other,
                })?;
            if acc.account_type != LedgerAccountType::Asset {
                return Err(invalid(
                    "ledgerAccountId",
                    "not_asset",
                    "a bank account maps to an asset account",
                )
                .into());
            }
            l
        }
        None => {
            let main = resolve(
                &mut tx,
                t,
                inst,
                AccountRef::System(SystemAccount::BankMain),
            )
            .await?;
            let mapped: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM finance.bank_account WHERE ledger_account_id = $1)",
            )
            .bind(main.id.as_uuid())
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
            if mapped {
                let used: Vec<String> = sqlx::query_scalar(
                    "SELECT code FROM finance.ledger_account WHERE tenant_id = $1 AND institution_id = $2 \
                     AND code ~ '^12[0-4][0-9]$'",
                )
                .bind(t.as_uuid())
                .bind(id)
                .fetch_all(&mut *tx)
                .await
                .map_err(map_db_error)?;
                let code = (1201..1250)
                    .map(|n: i32| n.to_string())
                    .find(|c| !used.contains(c))
                    .ok_or_else(|| {
                        KernelError::Conflict("no free bank ledger code 1201-1249 left".into())
                    })?;
                insert_account(
                    &mut tx,
                    t,
                    inst,
                    &code,
                    &format!("Bank — {name}"),
                    LedgerAccountType::Asset,
                    None,
                    true,
                )
                .await?
            } else {
                main.id.as_uuid()
            }
        }
    };
    let taken: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM finance.bank_account WHERE ledger_account_id = $1)",
    )
    .bind(ledger)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if taken {
        return Err(KernelError::Conflict(
            "that ledger account already belongs to another bank account".into(),
        )
        .into());
    }
    sqlx::query(
        "INSERT INTO finance.bank_account (id, tenant_id, institution_id, name, bank_name, account_number_last4, \
         ifsc, purpose, ledger_account_id, requested_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(bid)
    .bind(t.as_uuid())
    .bind(id)
    .bind(&name)
    .bind(&bank_name)
    .bind(&last4)
    .bind(&ifsc)
    .bind(&purpose)
    .bind(ledger)
    .bind(a.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        inst,
        "bank_account",
        bid,
        None,
        "requested",
        &StatusChange {
            cause_event_type: "sutraerp.finance.bank_account.requested",
            reason: None,
            process_instance_id: Some(outcome.process_instance_id()),
        },
        &a,
    )
    .await?;
    sqlx::query("UPDATE finance.bank_account SET process_instance_id = $2 WHERE id = $1")
        .bind(bid)
        .bind(outcome.process_instance_id())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    emit(
        &mut tx,
        &scope,
        None,
        &changed(bid, inst, None, "requested", 1, None, Some(&name), Some(outcome.process_instance_id()), None),
        "finance.bank_account.request",
        "bank_account",
        json!({"name": name, "bankName": bank_name, "ifsc": ifsc, "last4": last4, "purpose": purpose}),
        &a,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let row = load_account(&mut tx, t, bid, true).await?;
        let system = Actor::system(a.correlation_id.clone());
        activate(&mut tx, &scope, &row, Some("auto-approved"), &system).await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    respond_account(&st, t, bid, StatusCode::CREATED).await
}

/// One bank account with its ledger balance (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/bank-accounts/{id}", operation_id = "finance_bank_account_get", tag = "finance-treasury",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = BankAccountView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn bank_account_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceRead)?;
    respond_account(&st, tenant(ctx), id, StatusCode::OK).await
}

/// `approve | reject` decision.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionRequest {
    /// `approve` or `reject`.
    pub decision: String,
    pub reason: Option<String>,
}

/// Decides a requested bank account (`finance.approve` + step-up, `If-Match`): approve →
/// `active`, reject (reason required) → `closed`.
#[utoipa::path(post, path = "/api/v1/finance/bank-accounts/{id}/decision", operation_id = "finance_bank_account_decide", tag = "finance-treasury",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = DecisionRequest, security(("session_cookie" = [])),
    responses((status = 200, body = BankAccountView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn bank_account_decide(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<DecisionRequest>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceApprove)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "bank account").await?;
    let v = match if_match(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let approve = parse_decision(&body.decision)?;
    let reason = opt_text(body.reason.as_deref());
    if !approve && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row = load_account(&mut tx, t, id, true).await?;
    require_version(v, row.version)?;
    let scope = institution_scope(ctx, row.institution);
    require_institution_writable(&st, &scope, row.institution).await?;
    if row.status != BankAccountStatus::Requested {
        return Err(illegal("bank account", row.status.as_str(), "decided").into());
    }
    if row.requested_by.is_some() && row.requested_by == a.id {
        return Err(KernelError::Forbidden(
            "maker-checker: the requester of a bank account cannot approve it".into(),
        )
        .into());
    }
    if approve {
        activate(&mut tx, &scope, &row, reason.as_deref(), &a).await?;
    } else {
        let after = set_account_status(
            &mut tx,
            &row,
            BankAccountStatus::Closed,
            reason.as_deref(),
            "sutraerp.finance.bank_account.rejected",
            None,
            &a,
        )
        .await?;
        emit(
            &mut tx,
            &scope,
            None,
            &changed(
                id,
                row.institution,
                Some("requested"),
                after.status.as_str(),
                after.version,
                None,
                reason.as_deref(),
                None,
                None,
            ),
            "finance.bank_account.reject",
            "bank_account",
            json!({"reason": reason}),
            &a,
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    respond_account(&st, t, id, StatusCode::OK).await
}

/// Optional / required reason on a state command.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReasonBody {
    pub reason: Option<String>,
}

async fn account_command(
    st: &FinanceState,
    ctx: &AuthContext,
    headers: &HeaderMap,
    id: Uuid,
    to: BankAccountStatus,
    reason: Option<String>,
) -> ApiResult<Response> {
    let v = match if_match(headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row = load_account(&mut tx, t, id, true).await?;
    require_version(v, row.version)?;
    let scope = institution_scope(ctx, row.institution);
    require_institution_writable(st, &scope, row.institution).await?;
    if to == BankAccountStatus::Closed {
        let bal = ledger_balance(&mut tx, row.ledger, None).await?;
        if !bal.is_zero() {
            return Err(KernelError::Conflict(format!(
                "bank account {} still holds {} on the ledger; transfer it out first",
                row.name,
                fmt(bal)
            ))
            .into());
        }
    }
    let after = set_account_status(
        &mut tx,
        &row,
        to,
        reason.as_deref(),
        "sutraerp.finance.bank_account.changed",
        None,
        &a,
    )
    .await?;
    emit(
        &mut tx,
        &scope,
        None,
        &changed(
            id,
            row.institution,
            Some(row.status.as_str()),
            to.as_str(),
            after.version,
            None,
            reason.as_deref(),
            None,
            None,
        ),
        &format!(
            "finance.bank_account.{}",
            match to {
                BankAccountStatus::Suspended => "suspend",
                BankAccountStatus::Closed => "close",
                _ => "resume",
            }
        ),
        "bank_account",
        json!({"from": row.status, "to": to, "reason": reason}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond_account(st, t, id, StatusCode::OK).await
}

/// Suspends an active bank account (`treasury.manage`, `If-Match`, reason required).
#[utoipa::path(post, path = "/api/v1/finance/bank-accounts/{id}/suspend", operation_id = "finance_bank_account_suspend", tag = "finance-treasury",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = ReasonBody, security(("session_cookie" = [])),
    responses((status = 200, body = BankAccountView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn bank_account_suspend(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TreasuryManage)?;
    let reason = require_text("reason", body.as_ref().and_then(|b| b.0.reason.as_deref()))?;
    account_command(
        &st,
        ctx,
        &headers,
        id,
        BankAccountStatus::Suspended,
        Some(reason),
    )
    .await
}

/// Resumes a suspended bank account (`treasury.manage`, `If-Match`).
#[utoipa::path(post, path = "/api/v1/finance/bank-accounts/{id}/resume", operation_id = "finance_bank_account_resume", tag = "finance-treasury",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = ReasonBody, security(("session_cookie" = [])),
    responses((status = 200, body = BankAccountView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn bank_account_resume(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TreasuryManage)?;
    let reason = body.and_then(|b| opt_text(b.0.reason.as_deref()));
    account_command(&st, ctx, &headers, id, BankAccountStatus::Active, reason).await
}

/// Closes a bank account with a zero ledger balance (`treasury.manage`, `If-Match`).
#[utoipa::path(post, path = "/api/v1/finance/bank-accounts/{id}/close", operation_id = "finance_bank_account_close", tag = "finance-treasury",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = ReasonBody, security(("session_cookie" = [])),
    responses((status = 200, body = BankAccountView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn bank_account_close(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TreasuryManage)?;
    let reason = body.and_then(|b| opt_text(b.0.reason.as_deref()));
    account_command(&st, ctx, &headers, id, BankAccountStatus::Closed, reason).await
}

async fn require_active(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> Result<AccountRow, KernelError> {
    let row = load_account(conn, tenant, id, false).await?;
    if row.status != BankAccountStatus::Active {
        return Err(KernelError::Conflict(format!(
            "bank account {} is {}",
            row.name, row.status
        )));
    }
    Ok(row)
}

// ================================================================== bank transactions (settlement)

/// One settled gateway payment.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SettlementItemView {
    pub payment_id: Uuid,
    #[schema(value_type = String)]
    pub gross: String,
    #[schema(value_type = String)]
    pub fee: String,
    #[schema(value_type = String)]
    pub net: String,
    pub journal_entry_id: Uuid,
    pub reversal_entry_id: Option<Uuid>,
}

/// A bank transaction (gateway settlement credited to the bank).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BankTransactionView {
    pub id: BankTransactionId,
    pub institution_id: Uuid,
    pub bank_account_id: BankAccountId,
    pub kind: String,
    pub direction: String,
    /// Net amount credited to the bank.
    #[schema(value_type = String, example = "99000.00")]
    pub amount: String,
    #[schema(value_type = String, example = "1000.00")]
    pub fee_amount: String,
    pub currency: String,
    pub value_date: NaiveDate,
    pub reference: String,
    pub status: BankTransactionStatus,
    pub status_reason: Option<String>,
    pub items: Vec<SettlementItemView>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BankTransactionList {
    pub items: Vec<BankTransactionView>,
}

fn txn_from_row(r: &PgRow) -> Result<BankTransactionView, KernelError> {
    let st: String = r.try_get("status").map_err(map_db_error)?;
    let items: serde_json::Value = r.try_get("items").map_err(map_db_error)?;
    Ok(BankTransactionView {
        id: BankTransactionId(r.try_get("id").map_err(map_db_error)?),
        institution_id: r.try_get("institution_id").map_err(map_db_error)?,
        bank_account_id: BankAccountId(r.try_get("bank_account_id").map_err(map_db_error)?),
        kind: r.try_get("kind").map_err(map_db_error)?,
        direction: r.try_get("direction").map_err(map_db_error)?,
        amount: fmt(r.try_get("amount").map_err(map_db_error)?),
        fee_amount: fmt(r.try_get("fee_amount").map_err(map_db_error)?),
        currency: r.try_get("currency").map_err(map_db_error)?,
        value_date: r.try_get("value_date").map_err(map_db_error)?,
        reference: r.try_get("reference").map_err(map_db_error)?,
        status: BankTransactionStatus::parse(&st)
            .ok_or_else(|| KernelError::internal_msg(format!("bank transaction status `{st}`")))?,
        status_reason: r.try_get("status_reason").map_err(map_db_error)?,
        items: serde_json::from_value(items).unwrap_or_default(),
        created_at: r.try_get("created_at").map_err(map_db_error)?,
        version: r.try_get("version").map_err(map_db_error)?,
    })
}

async fn load_txn(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<BankTransactionView, KernelError> {
    let r = sqlx::query(&format!(
        "SELECT * FROM finance.bank_transaction WHERE tenant_id = $1 AND id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    ))
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "bank transaction",
    })?;
    txn_from_row(&r)
}

/// One payment of a gateway settlement.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SettlementItemInput {
    /// A completed gateway payment (5B) whose capture sits on gateway clearing.
    pub payment_id: Uuid,
    /// Gateway fee deducted from this payment (default `0.00`).
    #[schema(value_type = Option<String>, example = "20.00")]
    pub fee: Option<String>,
}

/// A gateway settlement credited to the bank account.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SettlementInput {
    /// Gateway settlement / UTR reference (unique per bank account).
    pub reference: String,
    pub value_date: NaiveDate,
    pub items: Vec<SettlementItemInput>,
}

/// Records a gateway settlement to the bank (`treasury.manage`, `Idempotency-Key`; contract §3.3
/// "Gateway settlement to bank", L3-F-057): per payment `payment` / `settle` — Dr bank (net) +
/// Dr bank charges (fee) · Cr gateway clearing (gross). A payment is settled at most once.
#[utoipa::path(post, path = "/api/v1/finance/bank-accounts/{id}/settlements", operation_id = "finance_bank_settlement_create", tag = "finance-treasury",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)), request_body = SettlementInput, security(("session_cookie" = [])),
    responses((status = 201, body = BankTransactionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn bank_settlement_create(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<SettlementInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TreasuryManage)?;
    require_idempotency_key(&headers)?;
    let reference = require_text("reference", Some(&body.reference))?;
    if body.items.is_empty() || body.items.len() > 500 {
        return Err(invalid("items", "count", "1-500 payments per settlement").into());
    }
    let mut seen = HashSet::new();
    let mut fees = Vec::with_capacity(body.items.len());
    for (i, it) in body.items.iter().enumerate() {
        if !seen.insert(it.payment_id) {
            return Err(invalid(
                &format!("items[{i}].paymentId"),
                "duplicate",
                "listed twice",
            )
            .into());
        }
        fees.push(parse_amount_or_zero(
            &format!("items[{i}].fee"),
            it.fee.as_deref(),
        )?);
    }
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let acc = require_active(&mut tx, t, id).await?;
    let scope = institution_scope(ctx, acc.institution);
    require_institution_writable(&st, &scope, acc.institution).await?;
    let dup: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM finance.bank_transaction WHERE tenant_id = $1 AND bank_account_id = $2 \
         AND kind = 'gateway_settlement' AND reference = $3)",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&reference)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if dup {
        return Err(
            KernelError::Conflict(format!("settlement {reference} was already recorded")).into(),
        );
    }
    let clearing = resolve(
        &mut tx,
        t,
        acc.institution,
        AccountRef::System(SystemAccount::GatewayClearing),
    )
    .await?;
    let txn_id = Uuid::now_v7();
    let (mut total_net, mut total_fee) = (Decimal::ZERO, Decimal::ZERO);
    let mut items = Vec::with_capacity(body.items.len());
    let mut amounts = Vec::with_capacity(body.items.len());
    for (i, (it, fee)) in body.items.iter().zip(fees).enumerate() {
        let field = format!("items[{i}].paymentId");
        // The gateway capture of the payment (posted by 5B), not reversed.
        let gross: Option<Decimal> = sqlx::query_scalar(
            "SELECT sum(l.debit) FROM finance.journal_entry e JOIN finance.journal_line l ON l.journal_entry_id = e.id \
             WHERE e.tenant_id = $1 AND e.institution_id = $2 AND e.source_type = 'payment' AND e.source_id = $3 \
             AND e.posting_key = 'capture' AND l.ledger_account_id = $4 \
             AND NOT EXISTS (SELECT 1 FROM finance.journal_entry r WHERE r.reverses_entry_id = e.id)",
        )
        .bind(t.as_uuid())
        .bind(acc.institution.as_uuid())
        .bind(it.payment_id)
        .bind(clearing.id.as_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        let Some(gross) = gross.filter(|g| *g > Decimal::ZERO) else {
            return Err(invalid(
                &field,
                "not_captured",
                "no unreversed gateway capture for this payment",
            )
            .into());
        };
        if fee >= gross {
            return Err(invalid(
                &format!("items[{i}].fee"),
                "too_high",
                "the fee must be below the payment amount",
            )
            .into());
        }
        let settled: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM finance.bank_transaction_item WHERE tenant_id = $1 AND payment_id = $2)",
        )
        .bind(t.as_uuid())
        .bind(it.payment_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if settled {
            return Err(KernelError::Conflict(format!(
                "payment {} was already settled to the bank",
                it.payment_id
            ))
            .into());
        }
        let net = gross - fee;
        let mut lines = vec![
            PostingLine::debit(AccountRef::Id(LedgerAccountId(acc.ledger)), net)
                .memo(reference.clone()),
        ];
        if fee > Decimal::ZERO {
            lines.push(
                PostingLine::debit(AccountRef::System(SystemAccount::BankCharges), fee)
                    .memo("gateway fee"),
            );
        }
        lines.push(PostingLine::credit(AccountRef::Id(clearing.id), gross));
        let entry = posting::post(
            &mut tx,
            t,
            &PostingRequest {
                institution_id: acc.institution,
                posting_date: body.value_date,
                kind: JournalEntryKind::Standard,
                source_type: source::PAYMENT.into(),
                source_id: it.payment_id,
                posting_key: format!("settle:{txn_id}"),
                description: format!("Gateway settlement {reference} to {}", acc.name),
                currency: CurrencyCode::INR,
                lines,
            },
            &a,
        )
        .await?;
        if entry.already_posted {
            return Err(KernelError::Conflict(format!(
                "payment {} was already settled",
                it.payment_id
            ))
            .into());
        }
        items.push(SettlementItemView {
            payment_id: it.payment_id,
            gross: fmt(gross),
            fee: fmt(fee),
            net: fmt(net),
            journal_entry_id: entry.journal_entry_id.as_uuid(),
            reversal_entry_id: None,
        });
        amounts.push((gross, fee));
        total_net += net;
        total_fee += fee;
    }
    sqlx::query(
        "INSERT INTO finance.bank_transaction (id, tenant_id, institution_id, bank_account_id, kind, direction, \
         amount, fee_amount, value_date, reference, status, items, created_by) \
         VALUES ($1, $2, $3, $4, 'gateway_settlement', 'credit', $5, $6, $7, $8, 'executed', $9, $10)",
    )
    .bind(txn_id)
    .bind(t.as_uuid())
    .bind(acc.institution.as_uuid())
    .bind(id)
    .bind(total_net)
    .bind(total_fee)
    .bind(body.value_date)
    .bind(&reference)
    .bind(serde_json::to_value(&items).map_err(KernelError::internal)?)
    .bind(a.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    for (it, (gross, fee)) in items.iter().zip(amounts.iter()) {
        sqlx::query(
            "INSERT INTO finance.bank_transaction_item (tenant_id, bank_transaction_id, payment_id, gross_amount, \
             fee_amount, journal_entry_id) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(t.as_uuid())
        .bind(txn_id)
        .bind(it.payment_id)
        .bind(gross)
        .bind(fee)
        .bind(it.journal_entry_id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    history(
        &mut tx,
        t,
        acc.institution,
        "bank_transaction",
        txn_id,
        None,
        "executed",
        &StatusChange::default(),
        &a,
    )
    .await?;
    emit(
        &mut tx,
        &scope,
        None,
        &changed(txn_id, acc.institution, None, "executed", 1, Some(total_net), Some(&reference), None, None),
        "finance.bank_transaction.settle",
        "bank_transaction",
        json!({"reference": reference, "net": fmt(total_net), "fee": fmt(total_fee), "payments": items.len()}),
        &a,
    )
    .await?;
    let v = load_txn(&mut tx, t, txn_id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &v, Version(v.version)))
}

/// Bank transactions of an institution, newest first (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/bank-transactions", operation_id = "finance_bank_transaction_list", tag = "finance-treasury",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = BankTransactionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn bank_transaction_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<BankTransactionList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let rows = sqlx::query(
        "SELECT * FROM finance.bank_transaction WHERE tenant_id = $1 AND institution_id = $2 ORDER BY created_at DESC, id DESC LIMIT 200",
    )
    .bind(tenant(ctx).as_uuid())
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(txn_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(BankTransactionList { items }))
}

/// A returned / failed bank transaction.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReturnInput {
    /// `returned` (default) or `failed`.
    pub outcome: Option<BankTransactionStatus>,
    pub reason: String,
    /// Defaults to today (business date).
    pub posting_date: Option<NaiveDate>,
}

/// Marks an executed settlement returned / failed by the bank (`treasury.manage`, `If-Match`,
/// `Idempotency-Key`; L3-F-061): every settle entry is reversed (`shared::posting::reverse`) so
/// the amounts are back on gateway clearing, the payments can be settled again, and
/// `bank_transaction.failed` raises a to-do for the finance office.
#[utoipa::path(post, path = "/api/v1/finance/bank-transactions/{id}/return", operation_id = "finance_bank_transaction_return", tag = "finance-treasury",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header), ("Idempotency-Key" = String, Header)), request_body = ReturnInput, security(("session_cookie" = [])),
    responses((status = 200, body = BankTransactionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn bank_transaction_return(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ReturnInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TreasuryManage)?;
    require_idempotency_key(&headers)?;
    let v = match if_match(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let reason = require_text("reason", Some(&body.reason))?;
    let outcome = body.outcome.unwrap_or(BankTransactionStatus::Returned);
    if !matches!(
        outcome,
        BankTransactionStatus::Returned | BankTransactionStatus::Failed
    ) {
        return Err(invalid("outcome", "invalid", "returned or failed").into());
    }
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let txn = load_txn(&mut tx, t, id, true).await?;
    require_version(v, txn.version)?;
    let inst = InstitutionId(txn.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    if txn.status != BankTransactionStatus::Executed {
        return Err(illegal("bank transaction", txn.status.as_str(), outcome.as_str()).into());
    }
    let matched: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM finance.bank_statement_line WHERE matched_transaction_id = $1)",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if matched {
        return Err(KernelError::Conflict(
            "the transaction is matched to a bank statement line; unmatch it first".into(),
        )
        .into());
    }
    let date = match body.posting_date {
        Some(d) => d,
        None => business_today(&st, &scope, inst).await?,
    };
    let mut items = txn.items.clone();
    for it in &mut items {
        let r = posting::reverse(
            &mut tx,
            t,
            JournalEntryId(it.journal_entry_id),
            date,
            &reason,
            &a,
        )
        .await?;
        it.reversal_entry_id = Some(r.journal_entry_id.as_uuid());
    }
    sqlx::query("DELETE FROM finance.bank_transaction_item WHERE bank_transaction_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    sqlx::query(
        "UPDATE finance.bank_transaction SET status = $2, status_reason = $3, items = $4, version = version + 1, \
         updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(outcome.as_str())
    .bind(&reason)
    .bind(serde_json::to_value(&items).map_err(KernelError::internal)?)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        inst,
        "bank_transaction",
        id,
        Some(txn.status.as_str()),
        outcome.as_str(),
        &StatusChange {
            cause_event_type: BANK_TRANSACTION_FAILED,
            reason: Some(&reason),
            process_instance_id: None,
        },
        &a,
    )
    .await?;
    let amount: Decimal = txn
        .amount
        .parse()
        .map_err(|_| KernelError::internal_msg("amount"))?;
    emit(
        &mut tx,
        &scope,
        Some((BANK_TRANSACTION_FAILED, BANK_TRANSACTION_FAILED_SCHEMA)),
        &changed(
            id,
            inst,
            Some(txn.status.as_str()),
            outcome.as_str(),
            txn.version + 1,
            Some(amount),
            Some(&format!("{}: {reason}", txn.reference)),
            None,
            None,
        ),
        "finance.bank_transaction.return",
        "bank_transaction",
        json!({"reference": txn.reference, "outcome": outcome, "reason": reason}),
        &a,
    )
    .await?;
    let out = load_txn(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

// ================================================================== statements

/// One imported statement line.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StatementLineView {
    pub id: BankStatementLineId,
    pub line_no: i32,
    pub value_date: NaiveDate,
    /// `credit` (into the account) or `debit` (out of it).
    pub direction: String,
    #[schema(value_type = String, example = "99000.00")]
    pub amount: String,
    pub reference: Option<String>,
    pub description: Option<String>,
    /// `unmatched`, `matched` or `ignored`.
    pub match_status: String,
    pub matched_journal_line_id: Option<Uuid>,
    pub matched_entry_number: Option<String>,
    pub matched_transaction_id: Option<Uuid>,
    pub note: Option<String>,
}

/// An imported bank statement.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StatementView {
    pub id: BankStatementId,
    pub bank_account_id: BankAccountId,
    pub statement_ref: String,
    pub period_from: NaiveDate,
    pub period_to: NaiveDate,
    #[schema(value_type = Option<String>)]
    pub opening_balance: Option<String>,
    #[schema(value_type = Option<String>)]
    pub closing_balance: Option<String>,
    #[schema(value_type = String)]
    pub total_credits: String,
    #[schema(value_type = String)]
    pub total_debits: String,
    pub line_count: i32,
    pub status: BankStatementStatus,
    pub reconciled_at: Option<DateTime<Utc>>,
    pub lines: Vec<StatementLineView>,
    pub reconciliations: Vec<BankReconciliationView>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StatementList {
    pub items: Vec<StatementView>,
}

/// One reconciliation run of a statement.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BankReconciliationView {
    pub id: BankReconciliationId,
    pub status: ReconciliationStatus,
    pub matched_lines: i32,
    pub unmatched_lines: i32,
    pub ignored_lines: i32,
    #[schema(value_type = Option<String>)]
    pub statement_closing: Option<String>,
    #[schema(value_type = String)]
    pub ledger_balance: String,
    pub performed_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

async fn statement_view(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    with_lines: bool,
) -> Result<StatementView, KernelError> {
    let r = sqlx::query("SELECT * FROM finance.bank_statement WHERE tenant_id = $1 AND id = $2")
        .bind(tenant.as_uuid())
        .bind(id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "bank statement",
        })?;
    let st: String = r.try_get("status").map_err(map_db_error)?;
    let lines = if with_lines {
        sqlx::query(
            "SELECT s.*, e.entry_number FROM finance.bank_statement_line s \
             LEFT JOIN finance.journal_line l ON l.id = s.matched_journal_line_id \
             LEFT JOIN finance.journal_entry e ON e.id = l.journal_entry_id \
             WHERE s.bank_statement_id = $1 ORDER BY s.line_no",
        )
        .bind(id)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?
        .iter()
        .map(|l| -> Result<StatementLineView, sqlx::Error> {
            Ok(StatementLineView {
                id: BankStatementLineId(l.try_get("id")?),
                line_no: l.try_get("line_no")?,
                value_date: l.try_get("value_date")?,
                direction: l.try_get("direction")?,
                amount: fmt(l.try_get("amount")?),
                reference: l.try_get("reference")?,
                description: l.try_get("description")?,
                match_status: l.try_get("match_status")?,
                matched_journal_line_id: l.try_get("matched_journal_line_id")?,
                matched_entry_number: l.try_get("entry_number")?,
                matched_transaction_id: l.try_get("matched_transaction_id")?,
                note: l.try_get("note")?,
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?
    } else {
        Vec::new()
    };
    let reconciliations = sqlx::query(
        "SELECT * FROM finance.bank_reconciliation WHERE bank_statement_id = $1 ORDER BY created_at DESC, id DESC",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(|b| -> Result<BankReconciliationView, sqlx::Error> {
        let s: String = b.try_get("status")?;
        Ok(BankReconciliationView {
            id: BankReconciliationId(b.try_get("id")?),
            status: ReconciliationStatus::parse(&s).unwrap_or(ReconciliationStatus::Exceptions),
            matched_lines: b.try_get("matched_lines")?,
            unmatched_lines: b.try_get("unmatched_lines")?,
            ignored_lines: b.try_get("ignored_lines")?,
            statement_closing: b.try_get::<Option<Decimal>, _>("statement_closing")?.map(fmt),
            ledger_balance: fmt(b.try_get("ledger_balance")?),
            performed_by: b.try_get("performed_by")?,
            created_at: b.try_get("created_at")?,
        })
    })
    .collect::<Result<Vec<_>, _>>()
    .map_err(map_db_error)?;
    Ok(StatementView {
        id: BankStatementId(id),
        bank_account_id: BankAccountId(r.try_get("bank_account_id").map_err(map_db_error)?),
        statement_ref: r.try_get("statement_ref").map_err(map_db_error)?,
        period_from: r.try_get("period_from").map_err(map_db_error)?,
        period_to: r.try_get("period_to").map_err(map_db_error)?,
        opening_balance: r
            .try_get::<Option<Decimal>, _>("opening_balance")
            .map_err(map_db_error)?
            .map(fmt),
        closing_balance: r
            .try_get::<Option<Decimal>, _>("closing_balance")
            .map_err(map_db_error)?
            .map(fmt),
        total_credits: fmt(r.try_get("total_credits").map_err(map_db_error)?),
        total_debits: fmt(r.try_get("total_debits").map_err(map_db_error)?),
        line_count: r.try_get("line_count").map_err(map_db_error)?,
        status: BankStatementStatus::parse(&st)
            .ok_or_else(|| KernelError::internal_msg(format!("statement status `{st}`")))?,
        reconciled_at: r.try_get("reconciled_at").map_err(map_db_error)?,
        lines,
        reconciliations,
        created_at: r.try_get("created_at").map_err(map_db_error)?,
        version: r.try_get("version").map_err(map_db_error)?,
    })
}

/// A statement to import.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StatementImport {
    pub statement_ref: String,
    pub period_from: NaiveDate,
    pub period_to: NaiveDate,
    #[schema(value_type = Option<String>, example = "0.00")]
    pub opening_balance: Option<String>,
    #[schema(value_type = Option<String>, example = "99000.00")]
    pub closing_balance: Option<String>,
    /// JSON lines, one object per statement line:
    /// `{"valueDate":"2026-07-03","direction":"credit","amount":"99000.00","reference":"UTR123","description":"…"}`.
    pub content: String,
}

/// Imports a bank statement from JSON lines (`treasury.manage`, `Idempotency-Key`; L3-F-058).
/// The same content or statement reference is never imported twice; when both balances are
/// given they must agree with the lines.
#[utoipa::path(post, path = "/api/v1/finance/bank-accounts/{id}/statements", operation_id = "finance_statement_import", tag = "finance-treasury",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)), request_body = StatementImport, security(("session_cookie" = [])),
    responses((status = 201, body = StatementView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn statement_import(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<StatementImport>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TreasuryManage)?;
    require_idempotency_key(&headers)?;
    let sref = require_text("statementRef", Some(&body.statement_ref))?;
    if body.period_to < body.period_from {
        return Err(invalid("periodTo", "invalid_range", "must not be before periodFrom").into());
    }
    let lines: Vec<StatementLine> =
        parse_statement(&body.content, body.period_from, body.period_to)?;
    let opening = body
        .opening_balance
        .as_deref()
        .map(|s| parse_signed("openingBalance", s))
        .transpose()?;
    let closing = body
        .closing_balance
        .as_deref()
        .map(|s| parse_signed("closingBalance", s))
        .transpose()?;
    let credits: Decimal = lines.iter().filter(|l| l.credit).map(|l| l.amount).sum();
    let debits: Decimal = lines.iter().filter(|l| !l.credit).map(|l| l.amount).sum();
    if let (Some(o), Some(c)) = (opening, closing) {
        if o + credits - debits != c {
            return Err(invalid(
                "closingBalance",
                "does_not_agree",
                format!(
                    "opening {} + credits {} − debits {} ≠ closing {}",
                    fmt(o),
                    fmt(credits),
                    fmt(debits),
                    fmt(c)
                ),
            )
            .into());
        }
    }
    let sha = {
        let mut h = Sha256::new();
        h.update(body.content.trim().as_bytes());
        h.finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let acc = require_active(&mut tx, t, id).await?;
    let scope = institution_scope(ctx, acc.institution);
    require_institution_writable(&st, &scope, acc.institution).await?;
    let dup: Option<String> = sqlx::query_scalar(
        "SELECT statement_ref FROM finance.bank_statement WHERE bank_account_id = $1 AND (statement_ref = $2 OR content_sha256 = $3)",
    )
    .bind(id)
    .bind(&sref)
    .bind(&sha)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if let Some(d) = dup {
        return Err(
            KernelError::Conflict(format!("this statement was already imported as {d}")).into(),
        );
    }
    let sid = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO finance.bank_statement (id, tenant_id, institution_id, bank_account_id, statement_ref, period_from, \
         period_to, opening_balance, closing_balance, total_credits, total_debits, line_count, content_sha256, imported_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
    )
    .bind(sid)
    .bind(t.as_uuid())
    .bind(acc.institution.as_uuid())
    .bind(id)
    .bind(&sref)
    .bind(body.period_from)
    .bind(body.period_to)
    .bind(opening)
    .bind(closing)
    .bind(credits)
    .bind(debits)
    .bind(i32::try_from(lines.len()).unwrap_or(i32::MAX))
    .bind(&sha)
    .bind(a.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    for (i, l) in lines.iter().enumerate() {
        sqlx::query(
            "INSERT INTO finance.bank_statement_line (tenant_id, bank_statement_id, line_no, value_date, direction, \
             amount, reference, description) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(t.as_uuid())
        .bind(sid)
        .bind(i32::try_from(i + 1).unwrap_or(i32::MAX))
        .bind(l.value_date)
        .bind(if l.credit { "credit" } else { "debit" })
        .bind(l.amount)
        .bind(l.reference.as_deref())
        .bind(l.description.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    history(
        &mut tx,
        t,
        acc.institution,
        "bank_statement",
        sid,
        None,
        "imported",
        &StatusChange::default(),
        &a,
    )
    .await?;
    emit(
        &mut tx,
        &scope,
        None,
        &changed(sid, acc.institution, None, "imported", 1, None, Some(&sref), None, None),
        "finance.bank_statement.import",
        "bank_statement",
        json!({"statementRef": sref, "lines": lines.len(), "credits": fmt(credits), "debits": fmt(debits)}),
        &a,
    )
    .await?;
    let v = statement_view(&mut tx, t, sid, true).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &v, Version(v.version)))
}

/// Statements of a bank account, newest first (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/bank-accounts/{id}/statements", operation_id = "finance_statement_list", tag = "finance-treasury",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = StatementList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn statement_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<StatementList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load_account(&mut conn, t, id, false).await?;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM finance.bank_statement WHERE tenant_id = $1 AND bank_account_id = $2 ORDER BY period_to DESC, created_at DESC",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(ids.len());
    for s in ids {
        items.push(statement_view(&mut conn, t, s, false).await?);
    }
    Ok(Json(StatementList { items }))
}

/// One statement with its lines and reconciliation runs (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/bank-statements/{id}", operation_id = "finance_statement_get", tag = "finance-treasury",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = StatementView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn statement_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v = statement_view(&mut conn, tenant(ctx), id, true).await?;
    Ok(with_etag(StatusCode::OK, &v, Version(v.version)))
}

/// A manual match of one statement line.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ManualMatch {
    pub line_id: Uuid,
    /// A journal line on the bank account's ledger account …
    pub journal_line_id: Option<Uuid>,
    /// … or an executed bank transaction of the account.
    pub bank_transaction_id: Option<Uuid>,
}

/// A statement line explained outside the ledger (e.g. a bank-side reversal pair).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct IgnoreLine {
    pub line_id: Uuid,
    pub note: String,
}

/// A reconciliation run.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileInput {
    /// Auto-match the remaining lines (default `true`).
    pub auto_match: Option<bool>,
    #[serde(default)]
    pub matches: Vec<ManualMatch>,
    #[serde(default)]
    pub ignore: Vec<IgnoreLine>,
}

/// `(line id, entry id, debit, credit, posting date, entry number, description, memo)`.
type LedgerLineCandidate = (
    Uuid,
    Uuid,
    Decimal,
    Decimal,
    NaiveDate,
    String,
    String,
    Option<String>,
);

struct Candidate {
    kind_txn: bool,
    id: Uuid,
    amount: Decimal,
    date: NaiveDate,
    reference: String,
}

/// Reconciles a statement with the bank ledger (`treasury.manage`, `If-Match`; L3-F-058):
/// manual matches / ignores first, then auto-matching by direction + amount, preferring a
/// reference match and the closest date (±5 days). All lines explained → `reconciled`
/// (`bank_statement.reconciled`), otherwise `reconciling` with the exceptions left `unmatched`.
#[utoipa::path(post, path = "/api/v1/finance/bank-statements/{id}/reconcile", operation_id = "finance_statement_reconcile", tag = "finance-treasury",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = ReconcileInput, security(("session_cookie" = [])),
    responses((status = 200, body = StatementView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn statement_reconcile(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Option<Json<ReconcileInput>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TreasuryManage)?;
    let v = match if_match(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let body = body.map(|b| b.0).unwrap_or_default();
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let (acc_id, version, status_raw, closing, period_to): (Uuid, i64, String, Option<Decimal>, NaiveDate) = sqlx::query_as(
        "SELECT bank_account_id, version, status, closing_balance, period_to FROM finance.bank_statement \
         WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "bank statement",
    })?;
    require_version(v, version)?;
    let status = BankStatementStatus::parse(&status_raw)
        .ok_or_else(|| KernelError::internal_msg("statement status"))?;
    if status == BankStatementStatus::Reconciled {
        return Err(illegal("bank statement", "reconciled", "reconciling").into());
    }
    let acc = load_account(&mut tx, t, acc_id, false).await?;
    let scope = institution_scope(ctx, acc.institution);
    require_institution_writable(&st, &scope, acc.institution).await?;
    let lines: Vec<(Uuid, bool, Decimal, NaiveDate, Option<String>, String)> = sqlx::query_as(
        "SELECT id, direction = 'credit', amount, value_date, reference, match_status FROM finance.bank_statement_line \
         WHERE bank_statement_id = $1 ORDER BY line_no FOR UPDATE",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let line_of = |lid: Uuid| lines.iter().find(|l| l.0 == lid);
    for (i, ig) in body.ignore.iter().enumerate() {
        let note = require_text(&format!("ignore[{i}].note"), Some(&ig.note))?;
        if line_of(ig.line_id).is_none() {
            return Err(invalid(
                &format!("ignore[{i}].lineId"),
                "not_found",
                "not a line of this statement",
            )
            .into());
        }
        sqlx::query(
            "UPDATE finance.bank_statement_line SET match_status = 'ignored', note = $2, matched_by = $3, \
             matched_journal_line_id = NULL, matched_transaction_id = NULL WHERE id = $1",
        )
        .bind(ig.line_id)
        .bind(note)
        .bind(a.id.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    for (i, m) in body.matches.iter().enumerate() {
        let field = format!("matches[{i}]");
        let Some(line) = line_of(m.line_id) else {
            return Err(invalid(
                &format!("{field}.lineId"),
                "not_found",
                "not a line of this statement",
            )
            .into());
        };
        match (m.journal_line_id, m.bank_transaction_id) {
            (Some(jl), None) => {
                let amt: Option<Decimal> = sqlx::query_scalar(
                    "SELECT CASE WHEN $3 THEN debit ELSE credit END FROM finance.journal_line \
                     WHERE tenant_id = $1 AND id = $2 AND ledger_account_id = $4",
                )
                .bind(t.as_uuid())
                .bind(jl)
                .bind(line.1)
                .bind(acc.ledger)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_db_error)?;
                if amt != Some(line.2) {
                    return Err(invalid(&format!("{field}.journalLineId"), "mismatch",
                        "the ledger line is not on this bank account, on the other side, or of another amount").into());
                }
            }
            (None, Some(bt)) => {
                let ok: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM finance.bank_transaction WHERE tenant_id = $1 AND id = $2 AND bank_account_id = $3 \
                     AND status = 'executed' AND amount = $4 AND (direction = 'credit') = $5)",
                )
                .bind(t.as_uuid())
                .bind(bt)
                .bind(acc_id)
                .bind(line.2)
                .bind(line.1)
                .fetch_one(&mut *tx)
                .await
                .map_err(map_db_error)?;
                if !ok {
                    return Err(invalid(&format!("{field}.bankTransactionId"), "mismatch",
                        "not an executed transaction of this account with this amount and direction").into());
                }
            }
            _ => {
                return Err(invalid(
                    &field,
                    "one_target",
                    "give either journalLineId or bankTransactionId",
                )
                .into());
            }
        }
        sqlx::query(
            "UPDATE finance.bank_statement_line SET match_status = 'matched', matched_journal_line_id = $2, \
             matched_transaction_id = $3, matched_by = $4, note = NULL WHERE id = $1",
        )
        .bind(m.line_id)
        .bind(m.journal_line_id)
        .bind(m.bank_transaction_id)
        .bind(a.id.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    if body.auto_match.unwrap_or(true) {
        // Candidates not yet explaining any statement line.
        let txns: Vec<(Uuid, bool, Decimal, NaiveDate, String, serde_json::Value)> = sqlx::query_as(
            "SELECT t.id, t.direction = 'credit', t.amount, t.value_date, t.reference, t.items FROM finance.bank_transaction t \
             WHERE t.tenant_id = $1 AND t.bank_account_id = $2 AND t.status = 'executed' \
             AND NOT EXISTS (SELECT 1 FROM finance.bank_statement_line s WHERE s.matched_transaction_id = t.id)",
        )
        .bind(t.as_uuid())
        .bind(acc_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_db_error)?;
        // Ledger lines of entries that belong to a bank transaction are explained by it.
        let mut txn_entries: HashSet<Uuid> = HashSet::new();
        let all_items: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT items FROM finance.bank_transaction WHERE tenant_id = $1 AND bank_account_id = $2 AND status = 'executed'",
        )
        .bind(t.as_uuid())
        .bind(acc_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_db_error)?;
        for items in &all_items {
            if let Ok(v) = serde_json::from_value::<Vec<SettlementItemView>>(items.clone()) {
                txn_entries.extend(v.iter().map(|i| i.journal_entry_id));
            }
        }
        let jls: Vec<LedgerLineCandidate> = sqlx::query_as(
            "SELECT l.id, e.id, l.debit, l.credit, e.posting_date, e.entry_number, e.description, l.memo \
             FROM finance.journal_line l JOIN finance.journal_entry e ON e.id = l.journal_entry_id \
             WHERE l.tenant_id = $1 AND l.ledger_account_id = $2 \
             AND NOT EXISTS (SELECT 1 FROM finance.bank_statement_line s WHERE s.matched_journal_line_id = l.id)",
        )
        .bind(t.as_uuid())
        .bind(acc.ledger)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_db_error)?;
        let mut credit_cands: Vec<Candidate> = Vec::new();
        let mut debit_cands: Vec<Candidate> = Vec::new();
        for (tid, credit, amount, date, reference, _) in &txns {
            let c = Candidate {
                kind_txn: true,
                id: *tid,
                amount: *amount,
                date: *date,
                reference: reference.clone(),
            };
            if *credit {
                credit_cands.push(c)
            } else {
                debit_cands.push(c)
            }
        }
        for (lid, eid, dr, cr, date, number, desc, memo) in &jls {
            if txn_entries.contains(eid) {
                continue;
            }
            let reference = format!("{number} {desc} {}", memo.clone().unwrap_or_default());
            if *dr > Decimal::ZERO {
                credit_cands.push(Candidate {
                    kind_txn: false,
                    id: *lid,
                    amount: *dr,
                    date: *date,
                    reference,
                });
            } else {
                debit_cands.push(Candidate {
                    kind_txn: false,
                    id: *lid,
                    amount: *cr,
                    date: *date,
                    reference,
                });
            }
        }
        let current: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT id, match_status FROM finance.bank_statement_line WHERE bank_statement_id = $1",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_db_error)?;
        let manual: HashSet<Uuid> = body
            .matches
            .iter()
            .filter_map(|m| m.journal_line_id.or(m.bank_transaction_id))
            .collect();
        for (lid, credit, amount, date, reference, _) in &lines {
            if current.iter().any(|(c, s)| c == lid && s != "unmatched") {
                continue;
            }
            let pool = if *credit {
                &mut credit_cands
            } else {
                &mut debit_cands
            };
            let best = pool
                .iter()
                .enumerate()
                .filter(|(_, c)| !manual.contains(&c.id))
                .filter_map(|(i, c)| {
                    match_score(
                        *amount,
                        *date,
                        reference.as_deref(),
                        c.amount,
                        c.date,
                        &c.reference,
                    )
                    .map(|s| (i, s))
                })
                .max_by_key(|(_, s)| *s);
            if let Some((i, _)) = best {
                let c = pool.remove(i);
                sqlx::query(
                    "UPDATE finance.bank_statement_line SET match_status = 'matched', matched_journal_line_id = $2, \
                     matched_transaction_id = $3, matched_by = 'system:auto-match', note = NULL WHERE id = $1",
                )
                .bind(lid)
                .bind(if c.kind_txn { None } else { Some(c.id) })
                .bind(if c.kind_txn { Some(c.id) } else { None })
                .execute(&mut *tx)
                .await
                .map_err(map_db_error)?;
            }
        }
    }
    let (matched, unmatched, ignored): (i64, i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE match_status = 'matched'), count(*) FILTER (WHERE match_status = 'unmatched'), \
         count(*) FILTER (WHERE match_status = 'ignored') FROM finance.bank_statement_line WHERE bank_statement_id = $1",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let balance = ledger_balance(&mut tx, acc.ledger, Some(period_to)).await?;
    let rstatus = if unmatched == 0 {
        ReconciliationStatus::Matched
    } else {
        ReconciliationStatus::Exceptions
    };
    let to = if unmatched == 0 {
        BankStatementStatus::Reconciled
    } else {
        BankStatementStatus::Reconciling
    };
    let i32_of = |n: i64| i32::try_from(n).unwrap_or(i32::MAX);
    sqlx::query(
        "INSERT INTO finance.bank_reconciliation (tenant_id, institution_id, bank_statement_id, status, matched_lines, \
         unmatched_lines, ignored_lines, statement_closing, ledger_balance, performed_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(t.as_uuid())
    .bind(acc.institution.as_uuid())
    .bind(id)
    .bind(rstatus.as_str())
    .bind(i32_of(matched))
    .bind(i32_of(unmatched))
    .bind(i32_of(ignored))
    .bind(closing)
    .bind(balance)
    .bind(a.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    sqlx::query(
        "UPDATE finance.bank_statement SET status = $2, version = version + 1, updated_at = now(), \
         reconciled_at = CASE WHEN $2 = 'reconciled' THEN now() ELSE reconciled_at END WHERE id = $1",
    )
    .bind(id)
    .bind(to.as_str())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if to.as_str() != status.as_str() {
        history(
            &mut tx,
            t,
            acc.institution,
            "bank_statement",
            id,
            Some(status.as_str()),
            to.as_str(),
            &StatusChange::default(),
            &a,
        )
        .await?;
    }
    emit(
        &mut tx,
        &scope,
        (to == BankStatementStatus::Reconciled).then_some((BANK_STATEMENT_RECONCILED, BANK_STATEMENT_RECONCILED_SCHEMA)),
        &changed(id, acc.institution, Some(status.as_str()), to.as_str(), version + 1, None,
            Some(&format!("{matched} matched, {unmatched} unmatched, {ignored} ignored")), None, None),
        "finance.bank_statement.reconcile",
        "bank_statement",
        json!({"matched": matched, "unmatched": unmatched, "ignored": ignored, "ledgerBalance": fmt(balance)}),
        &a,
    )
    .await?;
    let out = statement_view(&mut tx, t, id, true).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

// ================================================================== cash position

/// One row of the cash position.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CashPositionRow {
    /// `bank`, `cash` or `in_transit` (gateway clearing).
    pub kind: String,
    pub bank_account_id: Option<BankAccountId>,
    pub name: String,
    pub ledger_account_code: String,
    pub status: Option<BankAccountStatus>,
    #[schema(value_type = String, example = "99000.00")]
    pub balance: String,
    pub unreconciled_lines: i64,
}

/// Cash and liquidity position (L3-F-059).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CashPositionView {
    pub as_of: NaiveDate,
    pub rows: Vec<CashPositionRow>,
    #[schema(value_type = String, example = "99000.00")]
    pub available: String,
    #[schema(value_type = String, example = "0.00")]
    pub in_transit: String,
    #[schema(value_type = String, example = "99000.00")]
    pub total: String,
    pub currency: String,
}

/// Cash position from the ledger: every bank account, cash on hand and gateway clearing in
/// transit (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/cash-position", operation_id = "finance_cash_position", tag = "finance-treasury",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = CashPositionView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn cash_position(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<CashPositionView>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let t = tenant(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut rows = Vec::new();
    let (mut available, mut transit) = (Decimal::ZERO, Decimal::ZERO);
    let banks: Vec<(Uuid, String, Uuid, String, String)> = sqlx::query_as(
        "SELECT b.id, b.name, b.ledger_account_id, a.code, b.status FROM finance.bank_account b \
         JOIN finance.ledger_account a ON a.id = b.ledger_account_id WHERE b.tenant_id = $1 AND b.institution_id = $2 \
         ORDER BY a.code",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let mut bank_ledgers = HashSet::new();
    for (bid, name, ledger, code, status) in banks {
        bank_ledgers.insert(ledger);
        let bal = ledger_balance(&mut tx, ledger, Some(today)).await?;
        let unrec: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM finance.bank_statement_line l JOIN finance.bank_statement s ON s.id = l.bank_statement_id \
             WHERE s.bank_account_id = $1 AND l.match_status = 'unmatched'",
        )
        .bind(bid)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        available += bal;
        rows.push(CashPositionRow {
            kind: "bank".into(),
            bank_account_id: Some(BankAccountId(bid)),
            name,
            ledger_account_code: code,
            status: BankAccountStatus::parse(&status),
            balance: fmt(bal),
            unreconciled_lines: unrec,
        });
    }
    for (role, kind) in [
        (SystemAccount::BankMain, "bank"),
        (SystemAccount::CashOnHand, "cash"),
        (SystemAccount::GatewayClearing, "in_transit"),
    ] {
        let acc = resolve(&mut tx, t, inst, AccountRef::System(role)).await?;
        if bank_ledgers.contains(&acc.id.as_uuid()) {
            continue;
        }
        let bal = ledger_balance(&mut tx, acc.id.as_uuid(), Some(today)).await?;
        if kind == "in_transit" {
            transit += bal;
        } else {
            available += bal;
        }
        rows.push(CashPositionRow {
            kind: kind.into(),
            bank_account_id: None,
            name: acc.name,
            ledger_account_code: acc.code,
            status: None,
            balance: fmt(bal),
            unreconciled_lines: 0,
        });
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(CashPositionView {
        as_of: today,
        rows,
        available: fmt(available),
        in_transit: fmt(transit),
        total: fmt(available + transit),
        currency: "INR".into(),
    }))
}
