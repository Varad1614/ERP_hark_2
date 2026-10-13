//! Journal entries (read side of the append-only ledger, L3-F-025/032), manual journals
//! (request → approval → posting / rejection, L3-F-026; reversal, L3-F-028), the trial balance
//! (L3-F-030) and the receivables source ↔ ledger reconciliation (L3-F-029).
//!
//! Every ledger write goes through `shared::posting::{post, reverse}` in the same transaction as
//! the manual journal's state change — there is no second write path.

use super::common::{
    changed, emit, history, illegal, invalid, load_history, opt_text, parse_amount_or_zero,
    parse_decision, require_text, require_version, HistoryView,
};
use super::trial::{compute, TrialBalanceView};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::coa::resolve;
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::lifecycle::StatusChange;
use crate::shared::money::fmt;
use crate::shared::numbering::{kind, next};
use crate::shared::posting;
use crate::shared::scope::{institution_scope, tenant};
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
use sutra_contracts::finance::events::ledger::{
    LEDGER_RECONCILIATION_COMPLETED, LEDGER_RECONCILIATION_COMPLETED_SCHEMA, MANUAL_JOURNAL_POSTED,
    MANUAL_JOURNAL_POSTED_SCHEMA, MANUAL_JOURNAL_SUBMITTED, MANUAL_JOURNAL_SUBMITTED_SCHEMA,
};
use sutra_contracts::finance::ids::{
    JournalEntryId, LedgerAccountId, LedgerReconciliationId, ManualJournalId, StudentAccountId,
};
use sutra_contracts::finance::ports::{source, AccountRef, PostingLine, PostingRequest};
use sutra_contracts::finance::status::{
    JournalEntryKind, ManualJournalStatus, ReconciliationStatus,
};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::money::CurrencyCode;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

// ================================================================== journal entries (read)

/// One line of a journal entry.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct JournalLineView {
    pub id: Uuid,
    pub line_no: i32,
    pub ledger_account_id: LedgerAccountId,
    pub account_code: String,
    pub account_name: String,
    #[schema(value_type = String, example = "1250.00")]
    pub debit: String,
    #[schema(value_type = String, example = "0.00")]
    pub credit: String,
    pub student_account_id: Option<Uuid>,
    pub charge_id: Option<Uuid>,
    pub memo: Option<String>,
}

/// A journal entry (contract §5.3).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct JournalEntryView {
    pub id: JournalEntryId,
    pub institution_id: Uuid,
    pub entry_number: String,
    pub posting_date: NaiveDate,
    pub kind: JournalEntryKind,
    pub source_type: String,
    pub source_id: Uuid,
    pub posting_key: String,
    pub description: String,
    pub currency: String,
    #[schema(value_type = String, example = "1250.00")]
    pub total_debit: String,
    #[schema(value_type = String, example = "1250.00")]
    pub total_credit: String,
    pub financial_period_id: Option<Uuid>,
    pub reverses_entry_id: Option<Uuid>,
    /// The entry that reverses this one, if any.
    pub reversed_by_entry_id: Option<Uuid>,
    pub posted_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub lines: Vec<JournalLineView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct JournalEntryList {
    pub items: Vec<JournalEntryView>,
    /// Pass as `cursor` for the next (older) page.
    pub next_cursor: Option<Uuid>,
}

const ENTRY_COLUMNS: &str = "e.id, e.institution_id, e.entry_number, e.posting_date, e.kind, \
    e.source_type, e.source_id, e.posting_key, e.description, e.currency, e.total_debit, \
    e.total_credit, e.financial_period_id, e.reverses_entry_id, e.posted_by, e.created_at, \
    (SELECT r.id FROM finance.journal_entry r WHERE r.reverses_entry_id = e.id) AS reversed_by";

async fn entry_view(
    conn: &mut PgConnection,
    tenant: TenantId,
    r: &PgRow,
) -> Result<JournalEntryView, KernelError> {
    let id: Uuid = r.try_get("id").map_err(map_db_error)?;
    let kind: String = r.try_get("kind").map_err(map_db_error)?;
    let lines = sqlx::query(
        "SELECT l.id, l.line_no, l.ledger_account_id, a.code, a.name, l.debit, l.credit, \
         l.student_account_id, l.charge_id, l.memo FROM finance.journal_line l \
         JOIN finance.ledger_account a ON a.id = l.ledger_account_id \
         WHERE l.tenant_id = $1 AND l.journal_entry_id = $2 ORDER BY l.line_no",
    )
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(|l| -> Result<JournalLineView, sqlx::Error> {
        Ok(JournalLineView {
            id: l.try_get("id")?,
            line_no: l.try_get("line_no")?,
            ledger_account_id: LedgerAccountId(l.try_get("ledger_account_id")?),
            account_code: l.try_get("code")?,
            account_name: l.try_get("name")?,
            debit: fmt(l.try_get("debit")?),
            credit: fmt(l.try_get("credit")?),
            student_account_id: l.try_get("student_account_id")?,
            charge_id: l.try_get("charge_id")?,
            memo: l.try_get("memo")?,
        })
    })
    .collect::<Result<Vec<_>, _>>()
    .map_err(map_db_error)?;
    Ok(JournalEntryView {
        id: JournalEntryId(id),
        institution_id: r.try_get("institution_id").map_err(map_db_error)?,
        entry_number: r.try_get("entry_number").map_err(map_db_error)?,
        posting_date: r.try_get("posting_date").map_err(map_db_error)?,
        kind: JournalEntryKind::parse(&kind)
            .ok_or_else(|| KernelError::internal_msg(format!("journal kind `{kind}`")))?,
        source_type: r.try_get("source_type").map_err(map_db_error)?,
        source_id: r.try_get("source_id").map_err(map_db_error)?,
        posting_key: r.try_get("posting_key").map_err(map_db_error)?,
        description: r.try_get("description").map_err(map_db_error)?,
        currency: r.try_get("currency").map_err(map_db_error)?,
        total_debit: fmt(r.try_get("total_debit").map_err(map_db_error)?),
        total_credit: fmt(r.try_get("total_credit").map_err(map_db_error)?),
        financial_period_id: r.try_get("financial_period_id").map_err(map_db_error)?,
        reverses_entry_id: r.try_get("reverses_entry_id").map_err(map_db_error)?,
        reversed_by_entry_id: r.try_get("reversed_by").map_err(map_db_error)?,
        posted_by: r.try_get("posted_by").map_err(map_db_error)?,
        created_at: r.try_get("created_at").map_err(map_db_error)?,
        lines,
    })
}

/// One journal entry of the tenant (404 otherwise).
pub async fn load_entry(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> Result<JournalEntryView, KernelError> {
    let r = sqlx::query(&format!(
        "SELECT {ENTRY_COLUMNS} FROM finance.journal_entry e WHERE e.tenant_id = $1 AND e.id = $2"
    ))
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "journal entry",
    })?;
    entry_view(conn, tenant, &r).await
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct JournalEntryQuery {
    /// `charge`, `payment`, `manual_journal`, …
    pub source_type: Option<String>,
    pub source_id: Option<Uuid>,
    /// Only entries with a line on this account.
    pub ledger_account_id: Option<Uuid>,
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub financial_period_id: Option<Uuid>,
    /// ≤ 200, default 50.
    pub limit: Option<i64>,
    pub cursor: Option<Uuid>,
}

/// Journal entries of an institution, newest first (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/journal-entries", operation_id = "finance_journal_list", tag = "finance-ledger",
    params(("id" = String, Path, format = Uuid), JournalEntryQuery), security(("session_cookie" = [])),
    responses((status = 200, body = JournalEntryList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn journal_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<JournalEntryQuery>,
) -> ApiResult<Json<JournalEntryList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let t = tenant(ctx);
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let rows = sqlx::query(&format!(
        "SELECT {ENTRY_COLUMNS} FROM finance.journal_entry e WHERE e.tenant_id = $1 AND e.institution_id = $2 \
         AND ($3::text IS NULL OR e.source_type = $3) AND ($4::uuid IS NULL OR e.source_id = $4) \
         AND ($5::uuid IS NULL OR EXISTS (SELECT 1 FROM finance.journal_line l WHERE l.journal_entry_id = e.id AND l.ledger_account_id = $5)) \
         AND ($6::date IS NULL OR e.posting_date >= $6) AND ($7::date IS NULL OR e.posting_date <= $7) \
         AND ($8::uuid IS NULL OR e.financial_period_id = $8) AND ($9::uuid IS NULL OR e.id < $9) \
         ORDER BY e.id DESC LIMIT $10"
    ))
    .bind(t.as_uuid())
    .bind(id)
    .bind(q.source_type.as_deref())
    .bind(q.source_id)
    .bind(q.ledger_account_id)
    .bind(q.from)
    .bind(q.to)
    .bind(q.financial_period_id)
    .bind(q.cursor)
    .bind(limit + 1)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let more = rows.len() as i64 > limit;
    let mut items = Vec::with_capacity(rows.len());
    for r in rows.iter().take(limit as usize) {
        items.push(entry_view(&mut conn, t, r).await?);
    }
    let next_cursor = if more {
        items.last().map(|e| e.id.as_uuid())
    } else {
        None
    };
    Ok(Json(JournalEntryList { items, next_cursor }))
}

/// One journal entry with its lines (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/journal-entries/{id}", operation_id = "finance_journal_get", tag = "finance-ledger",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = JournalEntryView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn journal_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<JournalEntryView>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(Json(load_entry(&mut conn, tenant(ctx), id).await?))
}

/// Journal reversal request.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReverseRequest {
    pub reason: String,
    /// Defaults to today (business date); must fall into an open period.
    pub posting_date: Option<NaiveDate>,
}

/// Reverses a posted **manual journal** entry with its mirror entry through
/// `shared::posting::reverse` (`finance.approve` + step-up, `Idempotency-Key`; L3-F-028). Entries
/// of business sources (charges, payments, refunds, …) are reversed by their owning command.
#[utoipa::path(post, path = "/api/v1/finance/journal-entries/{id}/reverse", operation_id = "finance_journal_reverse", tag = "finance-ledger",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)), request_body = ReverseRequest, security(("session_cookie" = [])),
    responses((status = 201, body = JournalEntryView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn journal_reverse(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ReverseRequest>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceApprove)?;
    require_idempotency_key(&headers)?;
    let reason = require_text("reason", Some(&body.reason))?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let e = load_entry(&mut conn, t, id).await?;
    drop(conn);
    if e.source_type != source::MANUAL_JOURNAL || e.kind == JournalEntryKind::Reversal {
        return Err(KernelError::Conflict(format!(
            "only posted manual journal entries are reversed here; {} entries are reversed by their own {} command",
            e.source_type, e.source_type
        ))
        .into());
    }
    let inst = InstitutionId(e.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let date = match body.posting_date {
        Some(d) => d,
        None => business_today(&st, &scope, inst).await?,
    };
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mj = load_journal(&mut tx, t, e.source_id, true).await?;
    if mj.status != ManualJournalStatus::Posted {
        return Err(illegal(
            "manual journal",
            mj.status.as_str(),
            ManualJournalStatus::Reversed.as_str(),
        )
        .into());
    }
    let r = posting::reverse(&mut tx, t, JournalEntryId(id), date, &reason, &a).await?;
    let after = set_status(
        &mut tx,
        &mj,
        ManualJournalStatus::Reversed,
        Some(&reason),
        "sutraerp.finance.journal_entry.reversed",
        None,
        &a,
    )
    .await?;
    sqlx::query("UPDATE finance.manual_journal SET reversal_entry_id = $2 WHERE id = $1")
        .bind(mj.id)
        .bind(r.journal_entry_id.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    emit(
        &mut tx,
        &scope,
        None,
        &changed(
            mj.id,
            mj.institution,
            Some(mj.status.as_str()),
            after.status.as_str(),
            after.version,
            Some(mj.total),
            Some(&reason),
            None,
            Some(r.journal_entry_id.as_uuid()),
        ),
        "finance.manual_journal.reverse",
        "manual_journal",
        json!({"journalNumber": mj.number, "reversalEntryId": r.journal_entry_id, "reason": reason}),
        &a,
    )
    .await?;
    let out = load_entry(&mut tx, t, r.journal_entry_id.as_uuid()).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &out, Version(1)))
}

// ================================================================== manual journals

const MJ_COLUMNS: &str =
    "id, tenant_id, institution_id, journal_number, posting_date, entry_kind, \
    description, currency, total_amount, status, status_reason, process_instance_id, requested_by, \
    submitted_at, decided_by, decided_at, decision_reason, journal_entry_id, reversal_entry_id, \
    version, created_at";

#[derive(Clone, Debug)]
pub struct JournalRow {
    pub id: Uuid,
    pub tenant: TenantId,
    pub institution: InstitutionId,
    pub number: String,
    pub posting_date: NaiveDate,
    pub entry_kind: JournalEntryKind,
    pub description: String,
    pub total: Decimal,
    pub status: ManualJournalStatus,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub requested_by: Option<String>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decision_reason: Option<String>,
    pub journal_entry_id: Option<Uuid>,
    pub reversal_entry_id: Option<Uuid>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

fn mj_from_row(r: &PgRow) -> Result<JournalRow, sqlx::Error> {
    let status: String = r.try_get("status")?;
    let k: String = r.try_get("entry_kind")?;
    Ok(JournalRow {
        id: r.try_get("id")?,
        tenant: TenantId(r.try_get("tenant_id")?),
        institution: InstitutionId(r.try_get("institution_id")?),
        number: r.try_get("journal_number")?,
        posting_date: r.try_get("posting_date")?,
        entry_kind: JournalEntryKind::parse(&k)
            .ok_or_else(|| sqlx::Error::Decode(format!("entry kind `{k}`").into()))?,
        description: r.try_get("description")?,
        total: r.try_get("total_amount")?,
        status: ManualJournalStatus::parse(&status)
            .ok_or_else(|| sqlx::Error::Decode(format!("journal status `{status}`").into()))?,
        status_reason: r.try_get("status_reason")?,
        process_instance_id: r.try_get("process_instance_id")?,
        requested_by: r.try_get("requested_by")?,
        submitted_at: r.try_get("submitted_at")?,
        decided_by: r.try_get("decided_by")?,
        decided_at: r.try_get("decided_at")?,
        decision_reason: r.try_get("decision_reason")?,
        journal_entry_id: r.try_get("journal_entry_id")?,
        reversal_entry_id: r.try_get("reversal_entry_id")?,
        version: r.try_get("version")?,
        created_at: r.try_get("created_at")?,
    })
}

pub async fn load_journal(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<JournalRow, KernelError> {
    let r = sqlx::query(&format!(
        "SELECT {MJ_COLUMNS} FROM finance.manual_journal WHERE tenant_id = $1 AND id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    ))
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "manual journal",
    })?;
    mj_from_row(&r).map_err(map_db_error)
}

/// One line of a manual journal.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ManualJournalLineView {
    pub line_no: i32,
    pub ledger_account_id: LedgerAccountId,
    pub account_code: String,
    pub account_name: String,
    #[schema(value_type = String, example = "500.00")]
    pub debit: String,
    #[schema(value_type = String, example = "0.00")]
    pub credit: String,
    pub student_account_id: Option<Uuid>,
    pub memo: Option<String>,
}

/// A manual journal request.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ManualJournalView {
    pub id: ManualJournalId,
    pub institution_id: Uuid,
    pub journal_number: String,
    pub posting_date: NaiveDate,
    /// `manual`, `adjusting` or `opening`.
    pub entry_kind: JournalEntryKind,
    pub description: String,
    pub currency: String,
    #[schema(value_type = String, example = "500.00")]
    pub total_amount: String,
    pub status: ManualJournalStatus,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub requested_by: Option<String>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decision_reason: Option<String>,
    pub journal_entry_id: Option<Uuid>,
    pub reversal_entry_id: Option<Uuid>,
    pub lines: Vec<ManualJournalLineView>,
    pub history: Vec<HistoryView>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManualJournalList {
    pub items: Vec<ManualJournalView>,
}

async fn journal_view(
    conn: &mut PgConnection,
    r: JournalRow,
) -> Result<ManualJournalView, KernelError> {
    let lines = sqlx::query(
        "SELECT l.line_no, l.ledger_account_id, a.code, a.name, l.debit, l.credit, l.student_account_id, l.memo \
         FROM finance.manual_journal_line l JOIN finance.ledger_account a ON a.id = l.ledger_account_id \
         WHERE l.tenant_id = $1 AND l.manual_journal_id = $2 ORDER BY l.line_no",
    )
    .bind(r.tenant.as_uuid())
    .bind(r.id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(|l| -> Result<ManualJournalLineView, sqlx::Error> {
        Ok(ManualJournalLineView {
            line_no: l.try_get("line_no")?,
            ledger_account_id: LedgerAccountId(l.try_get("ledger_account_id")?),
            account_code: l.try_get("code")?,
            account_name: l.try_get("name")?,
            debit: fmt(l.try_get("debit")?),
            credit: fmt(l.try_get("credit")?),
            student_account_id: l.try_get("student_account_id")?,
            memo: l.try_get("memo")?,
        })
    })
    .collect::<Result<Vec<_>, _>>()
    .map_err(map_db_error)?;
    let history = load_history(conn, r.tenant, "manual_journal", r.id).await?;
    Ok(ManualJournalView {
        id: ManualJournalId(r.id),
        institution_id: r.institution.as_uuid(),
        journal_number: r.number,
        posting_date: r.posting_date,
        entry_kind: r.entry_kind,
        description: r.description,
        currency: "INR".into(),
        total_amount: fmt(r.total),
        status: r.status,
        status_reason: r.status_reason,
        process_instance_id: r.process_instance_id,
        requested_by: r.requested_by,
        submitted_at: r.submitted_at,
        decided_by: r.decided_by,
        decided_at: r.decided_at,
        decision_reason: r.decision_reason,
        journal_entry_id: r.journal_entry_id,
        reversal_entry_id: r.reversal_entry_id,
        lines,
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
    let r = load_journal(&mut conn, tenant, id, false).await?;
    let version = r.version;
    let v = journal_view(&mut conn, r).await?;
    Ok(with_etag(status, &v, Version(version)))
}

/// Legal manual journal transitions.
pub fn can_transition(from: ManualJournalStatus, to: ManualJournalStatus) -> bool {
    use ManualJournalStatus as S;
    matches!(
        (from, to),
        (S::Draft, S::PendingApproval)
            | (S::PendingApproval, S::Approved)
            | (S::PendingApproval, S::Rejected)
            | (S::PendingApproval, S::Draft)
            | (S::Approved, S::Posted)
            | (S::Posted, S::Reversed)
            | (S::Rejected, S::Draft)
    )
}

async fn set_status(
    conn: &mut PgConnection,
    r: &JournalRow,
    to: ManualJournalStatus,
    reason: Option<&str>,
    cause: &'static str,
    process_instance: Option<&str>,
    actor: &Actor,
) -> Result<JournalRow, KernelError> {
    if !can_transition(r.status, to) {
        return Err(illegal("manual journal", r.status.as_str(), to.as_str()));
    }
    let n = sqlx::query(
        "UPDATE finance.manual_journal SET status = $3, status_reason = $4, \
         process_instance_id = COALESCE($5, process_instance_id), version = version + 1, updated_at = now() \
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
        "manual_journal",
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
    load_journal(conn, r.tenant, r.id, true).await
}

/// One requested line.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ManualJournalLineInput {
    pub ledger_account_id: Uuid,
    #[schema(value_type = Option<String>, example = "500.00")]
    pub debit: Option<String>,
    #[schema(value_type = Option<String>, example = "0.00")]
    pub credit: Option<String>,
    /// Receivable sub-ledger dimension (party account of the institution).
    pub student_account_id: Option<Uuid>,
    pub memo: Option<String>,
}

/// A manual journal draft (create / replace).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ManualJournalInput {
    pub posting_date: NaiveDate,
    /// `manual` (default), `adjusting` or `opening`.
    pub entry_kind: Option<JournalEntryKind>,
    pub description: String,
    pub lines: Vec<ManualJournalLineInput>,
}

struct ValidLine {
    account: Uuid,
    debit: Decimal,
    credit: Decimal,
    party: Option<Uuid>,
    memo: Option<String>,
}

/// Pure validation: ≥ 2 lines, each exactly one positive side, Σ debit = Σ credit. Returns the
/// total.
pub fn balanced_total(lines: &[(Decimal, Decimal)]) -> Result<Decimal, KernelError> {
    if lines.len() < 2 {
        return Err(invalid(
            "lines",
            "too_few",
            "a journal needs at least two lines",
        ));
    }
    let (mut d, mut c) = (Decimal::ZERO, Decimal::ZERO);
    for (i, (dr, cr)) in lines.iter().enumerate() {
        if (*dr > Decimal::ZERO) == (*cr > Decimal::ZERO) {
            return Err(invalid(
                &format!("lines[{i}]"),
                "one_side",
                "each line has either a debit or a credit",
            ));
        }
        d += dr;
        c += cr;
    }
    if d != c {
        return Err(KernelError::Conflict(format!(
            "the journal is unbalanced: debits {} ≠ credits {}",
            fmt(d),
            fmt(c)
        )));
    }
    Ok(d)
}

async fn validate_input(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    body: &ManualJournalInput,
) -> Result<(String, JournalEntryKind, Vec<ValidLine>, Decimal), KernelError> {
    let description = require_text("description", Some(&body.description))?;
    let kind = body.entry_kind.unwrap_or(JournalEntryKind::Manual);
    if !matches!(
        kind,
        JournalEntryKind::Manual | JournalEntryKind::Adjusting | JournalEntryKind::Opening
    ) {
        return Err(invalid(
            "entryKind",
            "invalid",
            "manual, adjusting or opening",
        ));
    }
    if body.lines.len() > 200 {
        return Err(invalid("lines", "too_many", "at most 200 lines"));
    }
    let mut lines = Vec::with_capacity(body.lines.len());
    for (i, l) in body.lines.iter().enumerate() {
        let debit = parse_amount_or_zero(&format!("lines[{i}].debit"), l.debit.as_deref())?;
        let credit = parse_amount_or_zero(&format!("lines[{i}].credit"), l.credit.as_deref())?;
        resolve(
            conn,
            tenant,
            institution,
            AccountRef::Id(LedgerAccountId(l.ledger_account_id)),
        )
        .await
        .map_err(|e| match e {
            KernelError::NotFound { .. } => invalid(
                &format!("lines[{i}].ledgerAccountId"),
                "not_found",
                "unknown ledger account of this institution",
            ),
            other => other,
        })?;
        if let Some(p) = l.student_account_id {
            let ok: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM finance.student_account WHERE tenant_id = $1 AND institution_id = $2 AND id = $3)",
            )
            .bind(tenant.as_uuid())
            .bind(institution.as_uuid())
            .bind(p)
            .fetch_one(&mut *conn)
            .await
            .map_err(map_db_error)?;
            if !ok {
                return Err(invalid(
                    &format!("lines[{i}].studentAccountId"),
                    "not_found",
                    "unknown party account of this institution",
                ));
            }
        }
        lines.push(ValidLine {
            account: l.ledger_account_id,
            debit,
            credit,
            party: l.student_account_id,
            memo: opt_text(l.memo.as_deref()),
        });
    }
    let total = balanced_total(
        &lines
            .iter()
            .map(|l| (l.debit, l.credit))
            .collect::<Vec<_>>(),
    )?;
    Ok((description, kind, lines, total))
}

async fn write_lines(
    conn: &mut PgConnection,
    tenant: TenantId,
    journal: Uuid,
    lines: &[ValidLine],
) -> Result<(), KernelError> {
    sqlx::query("DELETE FROM finance.manual_journal_line WHERE manual_journal_id = $1")
        .bind(journal)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    for (i, l) in lines.iter().enumerate() {
        sqlx::query(
            "INSERT INTO finance.manual_journal_line (tenant_id, manual_journal_id, line_no, \
             ledger_account_id, debit, credit, student_account_id, memo) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(tenant.as_uuid())
        .bind(journal)
        .bind(i32::try_from(i + 1).unwrap_or(i32::MAX))
        .bind(l.account)
        .bind(l.debit)
        .bind(l.credit)
        .bind(l.party)
        .bind(l.memo.as_deref())
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct ManualJournalQuery {
    pub status: Option<ManualJournalStatus>,
}

/// Manual journals of an institution (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/manual-journals", operation_id = "finance_journal_manual_list", tag = "finance-ledger",
    params(("id" = String, Path, format = Uuid), ManualJournalQuery), security(("session_cookie" = [])),
    responses((status = 200, body = ManualJournalList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn manual_journal_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ManualJournalQuery>,
) -> ApiResult<Json<ManualJournalList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let rows = sqlx::query(&format!(
        "SELECT {MJ_COLUMNS} FROM finance.manual_journal WHERE tenant_id = $1 AND institution_id = $2 \
         AND ($3::text IS NULL OR status = $3) ORDER BY created_at DESC, id DESC LIMIT 200"
    ))
    .bind(t.as_uuid())
    .bind(id)
    .bind(q.status.map(|s| s.as_str()))
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        let row = mj_from_row(r).map_err(map_db_error)?;
        items.push(journal_view(&mut conn, row).await?);
    }
    Ok(Json(ManualJournalList { items }))
}

/// Drafts a manual journal (`ledger.manage`, `Idempotency-Key`). Lines must balance.
#[utoipa::path(post, path = "/api/v1/finance/institutions/{id}/manual-journals", operation_id = "finance_journal_manual_create", tag = "finance-ledger",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)), request_body = ManualJournalInput, security(("session_cookie" = [])),
    responses((status = 201, body = ManualJournalView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn manual_journal_create(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ManualJournalInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LedgerManage)?;
    require_idempotency_key(&headers)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let (description, kind, lines, total) = validate_input(&mut tx, t, inst, &body).await?;
    let number = next(&mut tx, t, inst, kind::MANUAL_JOURNAL).await?;
    let jid = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO finance.manual_journal (id, tenant_id, institution_id, journal_number, posting_date, \
         entry_kind, description, total_amount, requested_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(jid)
    .bind(t.as_uuid())
    .bind(id)
    .bind(&number)
    .bind(body.posting_date)
    .bind(kind.as_str())
    .bind(&description)
    .bind(total)
    .bind(a.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    write_lines(&mut tx, t, jid, &lines).await?;
    history(
        &mut tx,
        t,
        inst,
        "manual_journal",
        jid,
        None,
        "draft",
        &StatusChange::default(),
        &a,
    )
    .await?;
    emit(
        &mut tx,
        &scope,
        None,
        &changed(jid, inst, None, "draft", 1, Some(total), None, None, None),
        "finance.manual_journal.create",
        "manual_journal",
        json!({"journalNumber": number, "postingDate": body.posting_date, "amount": fmt(total)}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, jid, StatusCode::CREATED).await
}

/// One manual journal with lines and history (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/manual-journals/{id}", operation_id = "finance_journal_manual_get", tag = "finance-ledger",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ManualJournalView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn manual_journal_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceRead)?;
    respond(&st, tenant(ctx), id, StatusCode::OK).await
}

/// Replaces a draft (or rejected → draft again) journal (`ledger.manage`, `If-Match`).
#[utoipa::path(put, path = "/api/v1/finance/manual-journals/{id}", operation_id = "finance_journal_manual_update", tag = "finance-ledger",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = ManualJournalInput, security(("session_cookie" = [])),
    responses((status = 200, body = ManualJournalView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn manual_journal_update(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ManualJournalInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LedgerManage)?;
    let v = match if_match(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut r = load_journal(&mut tx, t, id, true).await?;
    require_version(v, r.version)?;
    let scope = institution_scope(ctx, r.institution);
    require_institution_writable(&st, &scope, r.institution).await?;
    if r.status == ManualJournalStatus::Rejected {
        r = set_status(
            &mut tx,
            &r,
            ManualJournalStatus::Draft,
            Some("revised after rejection"),
            "sutraerp.finance.manual_journal.revised",
            None,
            &a,
        )
        .await?;
    }
    if r.status != ManualJournalStatus::Draft {
        return Err(KernelError::Conflict(format!(
            "manual journal {} is {}; only drafts can be edited",
            r.number, r.status
        ))
        .into());
    }
    let (description, kind, lines, total) =
        validate_input(&mut tx, t, r.institution, &body).await?;
    sqlx::query(
        "UPDATE finance.manual_journal SET posting_date = $2, entry_kind = $3, description = $4, \
         total_amount = $5, version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(body.posting_date)
    .bind(kind.as_str())
    .bind(&description)
    .bind(total)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    write_lines(&mut tx, t, id, &lines).await?;
    emit(
        &mut tx,
        &scope,
        None,
        &changed(
            id,
            r.institution,
            None,
            "draft",
            r.version + 1,
            Some(total),
            None,
            None,
            None,
        ),
        "finance.manual_journal.update",
        "manual_journal",
        json!({"journalNumber": r.number, "amount": fmt(total)}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, id, StatusCode::OK).await
}

/// Approves a pending journal and posts it in the caller's transaction (`shared::posting::post`,
/// source `manual_journal` / key `post`): `pending_approval → approved → posted`.
async fn approve_and_post(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    r: &JournalRow,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<JournalRow, KernelError> {
    let lines = sqlx::query(
        "SELECT ledger_account_id, debit, credit, student_account_id, memo FROM finance.manual_journal_line \
         WHERE manual_journal_id = $1 ORDER BY line_no",
    )
    .bind(r.id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(|l| -> Result<PostingLine, sqlx::Error> {
        let acc = AccountRef::Id(LedgerAccountId(l.try_get("ledger_account_id")?));
        let debit: Decimal = l.try_get("debit")?;
        let credit: Decimal = l.try_get("credit")?;
        let mut p = if debit > Decimal::ZERO {
            PostingLine::debit(acc, debit)
        } else {
            PostingLine::credit(acc, credit)
        };
        if let Some(party) = l.try_get::<Option<Uuid>, _>("student_account_id")? {
            p = p.for_charge(StudentAccountId(party), None);
        }
        if let Some(m) = l.try_get::<Option<String>, _>("memo")? {
            p = p.memo(m);
        }
        Ok(p)
    })
    .collect::<Result<Vec<_>, _>>()
    .map_err(map_db_error)?;
    let approved = set_status(
        conn,
        r,
        ManualJournalStatus::Approved,
        reason,
        MANUAL_JOURNAL_POSTED,
        None,
        actor,
    )
    .await?;
    let entry = posting::post(
        conn,
        r.tenant,
        &PostingRequest {
            institution_id: r.institution,
            posting_date: r.posting_date,
            kind: r.entry_kind,
            source_type: source::MANUAL_JOURNAL.into(),
            source_id: r.id,
            posting_key: "post".into(),
            description: format!("Manual journal {}: {}", r.number, r.description),
            currency: CurrencyCode::INR,
            lines,
        },
        actor,
    )
    .await?;
    let posted = set_status(
        conn,
        &approved,
        ManualJournalStatus::Posted,
        reason,
        MANUAL_JOURNAL_POSTED,
        None,
        actor,
    )
    .await?;
    sqlx::query(
        "UPDATE finance.manual_journal SET journal_entry_id = $2, decided_by = $3, decided_at = now(), \
         decision_reason = $4 WHERE id = $1",
    )
    .bind(r.id)
    .bind(entry.journal_entry_id.as_uuid())
    .bind(actor.id.as_deref())
    .bind(reason)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    emit(
        conn,
        scope,
        Some((MANUAL_JOURNAL_POSTED, MANUAL_JOURNAL_POSTED_SCHEMA)),
        &changed(
            r.id,
            r.institution,
            Some(r.status.as_str()),
            posted.status.as_str(),
            posted.version,
            Some(r.total),
            reason.or(Some(&r.number)),
            posted.process_instance_id.as_deref(),
            Some(entry.journal_entry_id.as_uuid()),
        ),
        "finance.manual_journal.post",
        "manual_journal",
        json!({"journalNumber": r.number, "entryNumber": entry.entry_number,
               "journalEntryId": entry.journal_entry_id, "amount": fmt(r.total)}),
        actor,
    )
    .await?;
    load_journal(conn, r.tenant, r.id, false).await
}

/// Optional reason on a state command.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReasonBody {
    pub reason: Option<String>,
}

/// Submits a draft for approval (`ledger.manage`, `If-Match`, `Idempotency-Key`) through the
/// WorkflowPort (`MANUAL_JOURNAL_APPROVAL`); auto-approval posts it in the same transaction.
#[utoipa::path(post, path = "/api/v1/finance/manual-journals/{id}/submit", operation_id = "finance_journal_manual_submit", tag = "finance-ledger",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header), ("Idempotency-Key" = String, Header)), request_body = ReasonBody, security(("session_cookie" = [])),
    responses((status = 200, body = ManualJournalView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn manual_journal_submit(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LedgerManage)?;
    require_idempotency_key(&headers)?;
    let v = match if_match(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let reason = body.and_then(|b| opt_text(b.0.reason.as_deref()));
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let r0 = load_journal(&mut conn, t, id, false).await?;
    drop(conn);
    require_version(v, r0.version)?;
    if !can_transition(r0.status, ManualJournalStatus::PendingApproval) {
        return Err(illegal("manual journal", r0.status.as_str(), "pending_approval").into());
    }
    let scope = institution_scope(ctx, r0.institution);
    require_institution_writable(&st, &scope, r0.institution).await?;
    let mut vars = BTreeMap::new();
    vars.insert("amount".to_owned(), json!(fmt(r0.total)));
    vars.insert("journalNumber".to_owned(), json!(r0.number));
    let outcome = request_approval(
        &st,
        &scope,
        process::MANUAL_JOURNAL_APPROVAL,
        business_key("manual_journal", id, r0.version),
        vars,
    )
    .await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load_journal(&mut tx, t, id, true).await?;
    require_version(v, r.version)?;
    let pid = outcome.process_instance_id().to_owned();
    let pending = set_status(
        &mut tx,
        &r,
        ManualJournalStatus::PendingApproval,
        reason.as_deref(),
        MANUAL_JOURNAL_SUBMITTED,
        Some(&pid),
        &a,
    )
    .await?;
    sqlx::query("UPDATE finance.manual_journal SET submitted_at = now() WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    // Auto-approval decides in the same transaction; the submitted event then carries the
    // final (posted) state so the inbox rule only asks administrators while it really waits.
    let fin = if let ApprovalOutcome::Approved { .. } = outcome {
        let system = Actor::system(a.correlation_id.clone());
        approve_and_post(&mut tx, &scope, &pending, Some("auto-approved"), &system).await?
    } else {
        pending.clone()
    };
    emit(
        &mut tx,
        &scope,
        Some((MANUAL_JOURNAL_SUBMITTED, MANUAL_JOURNAL_SUBMITTED_SCHEMA)),
        &changed(
            id,
            r.institution,
            Some(r.status.as_str()),
            fin.status.as_str(),
            fin.version,
            Some(r.total),
            Some(&r.number),
            Some(&pid),
            fin.journal_entry_id,
        ),
        "finance.manual_journal.submit",
        "manual_journal",
        json!({"journalNumber": r.number, "amount": fmt(r.total), "processInstanceId": pid}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, id, StatusCode::OK).await
}

/// `approve | reject` decision of a pending manual journal.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionRequest {
    /// `approve` or `reject`.
    pub decision: String,
    pub reason: Option<String>,
}

/// Decides a pending manual journal (`finance.approve` + step-up, `If-Match`,
/// `Idempotency-Key`). Maker-checker: the requester cannot decide their own journal. Approval
/// posts it (the posting date's period must be open); rejection needs a reason and posts nothing.
#[utoipa::path(post, path = "/api/v1/finance/manual-journals/{id}/decision", operation_id = "finance_journal_manual_decide", tag = "finance-ledger",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header), ("Idempotency-Key" = String, Header)), request_body = DecisionRequest, security(("session_cookie" = [])),
    responses((status = 200, body = ManualJournalView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn manual_journal_decide(
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
    let approve = parse_decision(&body.decision)?;
    let reason = opt_text(body.reason.as_deref());
    if !approve && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load_journal(&mut tx, t, id, true).await?;
    require_version(v, r.version)?;
    let scope = institution_scope(ctx, r.institution);
    require_institution_writable(&st, &scope, r.institution).await?;
    if r.status != ManualJournalStatus::PendingApproval {
        return Err(illegal(
            "manual journal",
            r.status.as_str(),
            if approve { "approved" } else { "rejected" },
        )
        .into());
    }
    if r.requested_by.is_some() && r.requested_by == a.id {
        return Err(KernelError::Forbidden(
            "maker-checker: the requester of a manual journal cannot decide it".into(),
        )
        .into());
    }
    if approve {
        approve_and_post(&mut tx, &scope, &r, reason.as_deref(), &a).await?;
    } else {
        let after = set_status(
            &mut tx,
            &r,
            ManualJournalStatus::Rejected,
            reason.as_deref(),
            "sutraerp.finance.manual_journal.rejected",
            None,
            &a,
        )
        .await?;
        sqlx::query(
            "UPDATE finance.manual_journal SET decided_by = $2, decided_at = now(), decision_reason = $3 WHERE id = $1",
        )
        .bind(id)
        .bind(a.id.as_deref())
        .bind(reason.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        emit(
            &mut tx,
            &scope,
            None,
            &changed(
                id,
                r.institution,
                Some(r.status.as_str()),
                after.status.as_str(),
                after.version,
                Some(r.total),
                reason.as_deref(),
                r.process_instance_id.as_deref(),
                None,
            ),
            "finance.manual_journal.reject",
            "manual_journal",
            json!({"journalNumber": r.number, "reason": reason}),
            &a,
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, t, id, StatusCode::OK).await
}

// ================================================================== trial balance

#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct TrialBalanceQuery {
    /// Defaults to today (business date), or the period's end date with `periodId`.
    pub as_of: Option<NaiveDate>,
    pub period_id: Option<Uuid>,
}

/// Trial balance as of a date (`finance.read`; L3-F-030).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/trial-balance", operation_id = "finance_trial_balance_get", tag = "finance-ledger",
    params(("id" = String, Path, format = Uuid), TrialBalanceQuery), security(("session_cookie" = [])),
    responses((status = 200, body = TrialBalanceView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn trial_balance_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<TrialBalanceQuery>,
) -> ApiResult<Json<TrialBalanceView>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let as_of = match (q.as_of, q.period_id) {
        (Some(d), _) => d,
        (None, Some(p)) => sqlx::query_scalar::<_, NaiveDate>(
            "SELECT end_date FROM finance.financial_period WHERE tenant_id = $1 AND institution_id = $2 AND id = $3",
        )
        .bind(t.as_uuid())
        .bind(id)
        .bind(p)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "financial period",
        })?,
        (None, None) => business_today(&st, &scope, inst).await?,
    };
    Ok(Json(compute(&mut conn, t, inst, as_of, q.period_id).await?))
}

// ================================================================== reconciliation

/// One exception of a reconciliation run.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationException {
    pub charge_id: Uuid,
    pub charge_number: String,
    pub charge_status: String,
    #[schema(value_type = String)]
    pub source_amount: String,
    #[schema(value_type = String)]
    pub ledger_amount: String,
    #[schema(value_type = String)]
    pub difference: String,
}

/// A source ↔ ledger reconciliation run (L3-F-029).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationView {
    pub id: LedgerReconciliationId,
    pub institution_id: Uuid,
    /// `receivables`: Σ charge outstanding ↔ the receivable control account.
    pub scope: String,
    pub as_of: NaiveDate,
    pub status: ReconciliationStatus,
    #[schema(value_type = String, example = "25000.00")]
    pub source_total: String,
    #[schema(value_type = String, example = "25000.00")]
    pub ledger_total: String,
    #[schema(value_type = String, example = "0.00")]
    pub difference: String,
    pub currency: String,
    pub exceptions: Vec<ReconciliationException>,
    pub performed_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReconciliationList {
    pub items: Vec<ReconciliationView>,
}

fn recon_from_row(r: &PgRow) -> Result<ReconciliationView, KernelError> {
    let st: String = r.try_get("status").map_err(map_db_error)?;
    let ex: serde_json::Value = r.try_get("exceptions").map_err(map_db_error)?;
    Ok(ReconciliationView {
        id: LedgerReconciliationId(r.try_get("id").map_err(map_db_error)?),
        institution_id: r.try_get("institution_id").map_err(map_db_error)?,
        scope: r.try_get("scope").map_err(map_db_error)?,
        as_of: r.try_get("as_of").map_err(map_db_error)?,
        status: ReconciliationStatus::parse(&st)
            .ok_or_else(|| KernelError::internal_msg(format!("reconciliation status `{st}`")))?,
        source_total: fmt(r.try_get("source_total").map_err(map_db_error)?),
        ledger_total: fmt(r.try_get("ledger_total").map_err(map_db_error)?),
        difference: fmt(r.try_get("difference").map_err(map_db_error)?),
        currency: r.try_get("currency").map_err(map_db_error)?,
        exceptions: serde_json::from_value(ex).unwrap_or_default(),
        performed_by: r.try_get("performed_by").map_err(map_db_error)?,
        created_at: r.try_get("created_at").map_err(map_db_error)?,
    })
}

/// Charge statuses whose outstanding amount sits on the receivable control account.
const RECEIVABLE_STATUSES: &str = "('published', 'active', 'suspended', 'disputed', 'closed')";

/// Runs the receivables reconciliation (`ledger.manage`, `Idempotency-Key`): every charge's
/// outstanding amount against its sub-ledger balance on the receivable control account, and the
/// totals. `matched` when nothing differs, otherwise `exceptions` with one row per charge.
#[utoipa::path(post, path = "/api/v1/finance/institutions/{id}/reconciliations", operation_id = "finance_reconciliation_run", tag = "finance-ledger",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)), security(("session_cookie" = [])),
    responses((status = 201, body = ReconciliationView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn reconciliation_run(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LedgerManage)?;
    require_idempotency_key(&headers)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let ar = resolve(
        &mut tx,
        t,
        inst,
        AccountRef::System(sutra_contracts::finance::ports::SystemAccount::StudentReceivables),
    )
    .await?;
    let rows: Vec<(Uuid, String, String, Decimal, Decimal)> = sqlx::query_as(&format!(
        "WITH l AS (SELECT charge_id, sum(debit - credit) AS bal FROM finance.journal_line \
           WHERE tenant_id = $1 AND ledger_account_id = $3 AND charge_id IS NOT NULL GROUP BY charge_id) \
         SELECT c.id, c.charge_number, c.status, \
           CASE WHEN c.status IN {RECEIVABLE_STATUSES} THEN c.outstanding_amount ELSE 0 END AS src, \
           COALESCE(l.bal, 0) AS led \
         FROM finance.charge c LEFT JOIN l ON l.charge_id = c.id \
         WHERE c.tenant_id = $1 AND c.institution_id = $2 ORDER BY c.charge_number"
    ))
    .bind(t.as_uuid())
    .bind(id)
    .bind(ar.id.as_uuid())
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let ledger_total: Decimal = sqlx::query_scalar(
        "SELECT COALESCE(sum(debit - credit), 0) FROM finance.journal_line WHERE tenant_id = $1 AND ledger_account_id = $2",
    )
    .bind(t.as_uuid())
    .bind(ar.id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let source_total: Decimal = rows.iter().map(|r| r.3).sum();
    let exceptions: Vec<ReconciliationException> = rows
        .iter()
        .filter(|r| r.3 != r.4)
        .map(|r| ReconciliationException {
            charge_id: r.0,
            charge_number: r.1.clone(),
            charge_status: r.2.clone(),
            source_amount: fmt(r.3),
            ledger_amount: fmt(r.4),
            difference: fmt(r.3 - r.4),
        })
        .collect();
    let difference = source_total - ledger_total;
    let status = if exceptions.is_empty() && difference.is_zero() {
        ReconciliationStatus::Matched
    } else {
        ReconciliationStatus::Exceptions
    };
    let rid = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO finance.ledger_reconciliation (id, tenant_id, institution_id, scope, as_of, status, \
         source_total, ledger_total, difference, exceptions, performed_by) \
         VALUES ($1, $2, $3, 'receivables', $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(rid)
    .bind(t.as_uuid())
    .bind(id)
    .bind(today)
    .bind(status.as_str())
    .bind(source_total)
    .bind(ledger_total)
    .bind(difference)
    .bind(serde_json::to_value(&exceptions).map_err(KernelError::internal)?)
    .bind(a.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    emit(
        &mut tx,
        &scope,
        Some((
            LEDGER_RECONCILIATION_COMPLETED,
            LEDGER_RECONCILIATION_COMPLETED_SCHEMA,
        )),
        &changed(
            rid,
            inst,
            None,
            status.as_str(),
            1,
            None,
            Some(&format!("difference {}", fmt(difference))),
            None,
            None,
        ),
        "finance.ledger_reconciliation.run",
        "ledger_reconciliation",
        json!({"status": status, "sourceTotal": fmt(source_total), "ledgerTotal": fmt(ledger_total),
               "exceptions": exceptions.len()}),
        &a,
    )
    .await?;
    let row = sqlx::query("SELECT * FROM finance.ledger_reconciliation WHERE id = $1")
        .bind(rid)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let v = recon_from_row(&row)?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &v, Version(1)))
}

/// Reconciliation runs of an institution, newest first (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/reconciliations", operation_id = "finance_reconciliation_list", tag = "finance-ledger",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ReconciliationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn reconciliation_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ReconciliationList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let rows = sqlx::query(
        "SELECT * FROM finance.ledger_reconciliation WHERE tenant_id = $1 AND institution_id = $2 \
         ORDER BY created_at DESC, id DESC LIMIT 100",
    )
    .bind(tenant(ctx).as_uuid())
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(recon_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(ReconciliationList { items }))
}

/// One reconciliation run (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/reconciliations/{id}", operation_id = "finance_reconciliation_get", tag = "finance-ledger",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ReconciliationView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn reconciliation_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ReconciliationView>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let row =
        sqlx::query("SELECT * FROM finance.ledger_reconciliation WHERE tenant_id = $1 AND id = $2")
            .bind(tenant(ctx).as_uuid())
            .bind(id)
            .fetch_optional(&st.pool)
            .await
            .map_err(map_db_error)?
            .ok_or(KernelError::NotFound {
                entity: "reconciliation",
            })?;
    Ok(Json(recon_from_row(&row)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_journal_machine_and_balance() {
        use ManualJournalStatus as S;
        assert!(can_transition(S::Draft, S::PendingApproval));
        assert!(can_transition(S::PendingApproval, S::Approved));
        assert!(can_transition(S::Approved, S::Posted));
        assert!(can_transition(S::Posted, S::Reversed));
        assert!(!can_transition(S::Draft, S::Posted));
        assert!(!can_transition(S::Reversed, S::Posted));
        assert!(!can_transition(S::Rejected, S::Posted));
        let d = |s: &str| s.parse::<Decimal>().unwrap();
        assert_eq!(
            balanced_total(&[(d("10.00"), d("0")), (d("0"), d("10.00"))]).ok(),
            Some(d("10.00"))
        );
        assert!(matches!(
            balanced_total(&[(d("10.00"), d("0")), (d("0"), d("9.99"))]),
            Err(KernelError::Conflict(_))
        ));
        assert!(balanced_total(&[(d("10.00"), d("0"))]).is_err());
        assert!(balanced_total(&[(d("10.00"), d("1")), (d("0"), d("11.00"))]).is_err());
    }
}
