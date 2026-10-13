//! The **only** write path into the general ledger (contract §3.3 / §6.2): balanced,
//! idempotent, period-guarded double-entry postings, inside the caller's owner-local
//! transaction.
//!
//! [`post`]:
//! 1. validates the request ([`PostingRequest::validate`]: ≥ 2 lines, positive amounts at scale
//!    ≤ 2, Σ debit = Σ credit) → `409` otherwise;
//! 2. returns the existing entry (`already_posted`) when `(source_type, source_id, posting_key)`
//!    was posted before — a retried command never posts twice;
//! 3. asks the period guard (`shared::period::require_open`) for the posting date;
//! 4. resolves every account (seeding the default chart on first use; inactive / foreign /
//!    non-postable accounts → `409`/`404`);
//! 5. inserts the header (`JE-…` number) and its lines; the database re-checks the balance at
//!    commit (deferred constraint trigger) and keeps both tables append-only;
//! 6. emits `journal_entry.posted` and audits `finance.journal_entry.post`.
//!
//! [`reverse`] posts the mirror entry of an existing one (kind `reversal`, same source,
//! posting key `reversal:<entry id>`), at most once per entry.
//!
//! 5A (charges, adjustments, aid) and 5B (payments, refunds) call these in their own
//! transactions; 5C builds manual journals, batches and close on top. Callers outside a finance
//! transaction use `FinanceState::posting` (`PostingPort`).

use crate::shared::coa::resolve;
use crate::shared::numbering::{kind, next};
use crate::shared::period::require_open;
use crate::shared::publish::{audit, event};
use crate::shared::Actor;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde_json::json;
use sqlx::{PgConnection, Row};
use sutra_contracts::finance::events::{
    JournalEntryPostedV1, JOURNAL_ENTRY_POSTED, JOURNAL_ENTRY_POSTED_SCHEMA,
    JOURNAL_ENTRY_REVERSED, JOURNAL_ENTRY_REVERSED_SCHEMA,
};
use sutra_contracts::finance::ids::{ChargeId, JournalEntryId, LedgerAccountId, StudentAccountId};
use sutra_contracts::finance::ports::{AccountRef, PostingLine, PostingReceipt, PostingRequest};
use sutra_contracts::finance::status::{JournalEntryKind, NormalBalance};
use sutra_kernel::error::KernelError;
use sutra_kernel::money::CurrencyCode;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

async fn existing(
    conn: &mut PgConnection,
    tenant: TenantId,
    source_type: &str,
    source_id: Uuid,
    posting_key: &str,
) -> Result<Option<PostingReceipt>, KernelError> {
    let row = sqlx::query(
        "SELECT id, entry_number, financial_period_id FROM finance.journal_entry \
         WHERE tenant_id = $1 AND source_type = $2 AND source_id = $3 AND posting_key = $4",
    )
    .bind(tenant.as_uuid())
    .bind(source_type)
    .bind(source_id)
    .bind(posting_key)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    row.map(|r| {
        Ok(PostingReceipt {
            journal_entry_id: JournalEntryId(r.try_get("id")?),
            entry_number: r.try_get("entry_number")?,
            financial_period_id: r.try_get("financial_period_id")?,
            already_posted: true,
        })
    })
    .transpose()
    .map_err(map_db_error)
}

/// Posts a balanced journal entry in the caller's transaction (see the module docs).
pub async fn post(
    conn: &mut PgConnection,
    tenant: TenantId,
    req: &PostingRequest,
    actor: &Actor,
) -> Result<PostingReceipt, KernelError> {
    insert(conn, tenant, req, None, actor).await
}

async fn insert(
    conn: &mut PgConnection,
    tenant: TenantId,
    req: &PostingRequest,
    reverses: Option<JournalEntryId>,
    actor: &Actor,
) -> Result<PostingReceipt, KernelError> {
    let total = req.validate()?;
    if let Some(r) = existing(
        conn,
        tenant,
        &req.source_type,
        req.source_id,
        &req.posting_key,
    )
    .await?
    {
        return Ok(r);
    }
    let period = require_open(conn, tenant, req.institution_id, req.posting_date).await?;
    let mut accounts = Vec::with_capacity(req.lines.len());
    for l in &req.lines {
        let a = resolve(conn, tenant, req.institution_id, l.account).await?;
        if a.currency != req.currency.as_str() {
            return Err(KernelError::Conflict(format!(
                "ledger account {} is kept in {}, not {}",
                a.code, a.currency, req.currency
            )));
        }
        accounts.push(a.id);
    }
    let number = next(conn, tenant, req.institution_id, kind::JOURNAL).await?;
    let id: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO finance.journal_entry (tenant_id, institution_id, entry_number, posting_date, \
         financial_period_id, kind, source_type, source_id, posting_key, description, currency, \
         total_debit, total_credit, reverses_entry_id, posted_by, correlation_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $12, $13, $14, $15) \
         ON CONFLICT (tenant_id, source_type, source_id, posting_key) DO NOTHING RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(req.institution_id.as_uuid())
    .bind(&number)
    .bind(req.posting_date)
    .bind(period)
    .bind(req.kind.as_str())
    .bind(&req.source_type)
    .bind(req.source_id)
    .bind(&req.posting_key)
    .bind(&req.description)
    .bind(req.currency.as_str())
    .bind(total)
    .bind(reverses.map(|r| r.as_uuid()))
    .bind(actor.id.as_deref())
    .bind(actor.correlation_id.as_deref())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let Some(id) = id else {
        // Lost a race with an identical posting: report the winner.
        return existing(
            conn,
            tenant,
            &req.source_type,
            req.source_id,
            &req.posting_key,
        )
        .await?
        .ok_or_else(|| KernelError::Conflict("journal entry changed concurrently".into()));
    };
    for (i, (l, account)) in req.lines.iter().zip(&accounts).enumerate() {
        let (debit, credit) = match l.side {
            NormalBalance::Debit => (l.amount, Decimal::ZERO),
            NormalBalance::Credit => (Decimal::ZERO, l.amount),
        };
        sqlx::query(
            "INSERT INTO finance.journal_line (tenant_id, journal_entry_id, line_no, \
             ledger_account_id, debit, credit, student_account_id, charge_id, memo) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(tenant.as_uuid())
        .bind(id)
        .bind(i32::try_from(i + 1).unwrap_or(i32::MAX))
        .bind(account.as_uuid())
        .bind(debit)
        .bind(credit)
        .bind(l.student_account_id.map(|a| a.as_uuid()))
        .bind(l.charge_id.map(|c| c.as_uuid()))
        .bind(l.memo.as_deref())
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    let scope = ExecutionScope::institution(tenant, req.institution_id);
    let payload = JournalEntryPostedV1 {
        journal_entry_id: id,
        institution_id: req.institution_id.as_uuid(),
        entry_number: number.clone(),
        posting_date: req.posting_date,
        kind: req.kind.as_str().to_owned(),
        source_type: req.source_type.clone(),
        source_id: req.source_id,
        posting_key: req.posting_key.clone(),
        amount: crate::shared::money::fmt(total),
        currency: req.currency.as_str().to_owned(),
        financial_period_id: period,
        reverses_entry_id: reverses.map(|r| r.as_uuid()),
    };
    let (event_type, schema, action) = if reverses.is_some() {
        (
            JOURNAL_ENTRY_REVERSED,
            JOURNAL_ENTRY_REVERSED_SCHEMA,
            "finance.journal_entry.reverse",
        )
    } else {
        (
            JOURNAL_ENTRY_POSTED,
            JOURNAL_ENTRY_POSTED_SCHEMA,
            "finance.journal_entry.post",
        )
    };
    event(
        conn,
        &scope,
        event_type,
        schema,
        id.to_string(),
        &payload,
        actor,
    )
    .await?;
    audit(
        conn,
        &scope,
        action,
        "journal_entry",
        id.to_string(),
        json!({"entryNumber": number, "sourceType": req.source_type, "sourceId": req.source_id,
               "postingKey": req.posting_key, "amount": payload.amount, "kind": req.kind}),
        actor,
    )
    .await?;
    Ok(PostingReceipt {
        journal_entry_id: JournalEntryId(id),
        entry_number: number,
        financial_period_id: period,
        already_posted: false,
    })
}

/// Posts the mirror entry of `entry` dated `posting_date` (idempotent; a reversal entry itself
/// cannot be reversed — post a new entry instead).
pub async fn reverse(
    conn: &mut PgConnection,
    tenant: TenantId,
    entry: JournalEntryId,
    posting_date: NaiveDate,
    reason: &str,
    actor: &Actor,
) -> Result<PostingReceipt, KernelError> {
    let head = sqlx::query(
        "SELECT institution_id, kind, source_type, source_id, description, currency \
         FROM finance.journal_entry WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(entry.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "journal entry",
    })?;
    let kind_raw: String = head.try_get("kind").map_err(map_db_error)?;
    if kind_raw == JournalEntryKind::Reversal.as_str() {
        return Err(KernelError::Conflict(
            "a reversal entry cannot be reversed; post a new entry instead".into(),
        ));
    }
    let source_type: String = head.try_get("source_type").map_err(map_db_error)?;
    let source_id: Uuid = head.try_get("source_id").map_err(map_db_error)?;
    let institution: Uuid = head.try_get("institution_id").map_err(map_db_error)?;
    let description: String = head.try_get("description").map_err(map_db_error)?;
    let currency: String = head.try_get("currency").map_err(map_db_error)?;
    let posting_key = format!("reversal:{entry}");
    let lines = sqlx::query(
        "SELECT ledger_account_id, debit, credit, student_account_id, charge_id, memo \
         FROM finance.journal_line WHERE tenant_id = $1 AND journal_entry_id = $2 ORDER BY line_no",
    )
    .bind(tenant.as_uuid())
    .bind(entry.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(|r| {
        let debit: Decimal = r.try_get("debit")?;
        let credit: Decimal = r.try_get("credit")?;
        let account = AccountRef::Id(LedgerAccountId(r.try_get("ledger_account_id")?));
        let mut l = if debit > Decimal::ZERO {
            PostingLine::credit(account, debit)
        } else {
            PostingLine::debit(account, credit)
        };
        l.student_account_id = r
            .try_get::<Option<Uuid>, _>("student_account_id")?
            .map(StudentAccountId);
        l.charge_id = r.try_get::<Option<Uuid>, _>("charge_id")?.map(ChargeId);
        l.memo = r.try_get("memo")?;
        Ok(l)
    })
    .collect::<Result<Vec<_>, sqlx::Error>>()
    .map_err(map_db_error)?;
    let currency = CurrencyCode::try_from(currency).map_err(KernelError::Conflict)?;
    let req = PostingRequest {
        institution_id: InstitutionId(institution),
        posting_date,
        kind: JournalEntryKind::Reversal,
        source_type,
        source_id,
        posting_key,
        description: format!("Reversal of {description}: {reason}"),
        currency,
        lines,
    };
    insert(conn, tenant, &req, Some(entry), actor).await
}
