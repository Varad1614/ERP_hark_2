//! ERP_EXTENSIONS 1B: reconciling a bank account against the statement balance (Everything's
//! `reconcile.js`), match suggestions for statement lines (`banking.js` `findMatches`), changes
//! after reconciliation (`reports.js` `reconDiscrepancy`) and the bank reconciliation statement.
//!
//! Statement matching (`/bank-statements/{id}/reconcile`) marks entries *cleared* (the bank has
//! shown them); finishing a reconciliation here marks the ticked entries *reconciled* up to a
//! statement date once the cleared balance equals the statement's ending balance, and carries
//! that balance forward. Records are append-only; undo marks the latest one `undone`.

use super::reconcile::{
    brs, candidates, find_matches, summarize, Brs, MatchLine, OpenDoc, Posting, SuggestionKind,
    Summary,
};
use crate::ledger::common::{changed, emit, invalid, parse_date, parse_signed};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::actor;
use crate::shared::money::fmt;
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::time::business_today;
use crate::shared::FinanceState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, Row};
use std::collections::{HashMap, HashSet};
use sutra_kernel::auth::Permission;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

const MAX_SELECTED: usize = 20_000;
const MAX_LIST: usize = 500;

fn paise(d: Decimal) -> i64 {
    (d * Decimal::ONE_HUNDRED).round().to_i64().unwrap_or(i64::MAX)
}
fn rupees(p: i64) -> String {
    fmt(Decimal::new(p, 2))
}

struct Bank {
    id: Uuid,
    institution: InstitutionId,
    ledger: Uuid,
    status: String,
}

async fn load_bank(
    conn: &mut PgConnection,
    t: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<Bank, KernelError> {
    let r = sqlx::query(&format!(
        "SELECT id, institution_id, ledger_account_id, status FROM finance.bank_account \
         WHERE tenant_id = $1 AND id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    ))
    .bind(t.as_uuid())
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "bank account",
    })?;
    Ok(Bank {
        id: r.try_get("id").map_err(map_db_error)?,
        institution: InstitutionId(r.try_get("institution_id").map_err(map_db_error)?),
        ledger: r.try_get("ledger_account_id").map_err(map_db_error)?,
        status: r.try_get("status").map_err(map_db_error)?,
    })
}

/// An entry on the bank's ledger account with its amount there (debit positive, paise).
#[derive(Clone)]
struct EntryRow {
    id: Uuid,
    date: NaiveDate,
    number: String,
    description: String,
    amount: i64,
}

/// Entries on the bank's ledger account not reconciled by a finished reconciliation, up to a
/// date, in ledger order.
async fn unreconciled_entries(
    conn: &mut PgConnection,
    t: TenantId,
    bank: &Bank,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> Result<Vec<EntryRow>, KernelError> {
    let rows: Vec<(Uuid, NaiveDate, String, String, Decimal)> = sqlx::query_as(
        "SELECT e.id, e.posting_date, e.entry_number, e.description, sum(l.debit - l.credit) \
         FROM finance.journal_line l JOIN finance.journal_entry e ON e.id = l.journal_entry_id \
         WHERE l.tenant_id = $1 AND l.ledger_account_id = $2 \
         AND ($3::date IS NULL OR e.posting_date >= $3) AND ($4::date IS NULL OR e.posting_date <= $4) \
         AND NOT EXISTS (SELECT 1 FROM finance.bank_account_reconciliation_entry re \
             JOIN finance.bank_account_reconciliation r ON r.id = re.reconciliation_id \
             WHERE re.tenant_id = $1 AND re.journal_entry_id = e.id AND r.bank_account_id = $5 AND r.status = 'done') \
         GROUP BY e.id, e.posting_date, e.entry_number, e.description, e.created_at \
         ORDER BY e.posting_date, e.created_at, e.id",
    )
    .bind(t.as_uuid())
    .bind(bank.ledger)
    .bind(from)
    .bind(to)
    .bind(bank.id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(rows
        .into_iter()
        .map(|(id, date, number, description, amount)| EntryRow {
            id,
            date,
            number,
            description,
            amount: paise(amount),
        })
        .collect())
}

/// Entries the bank has shown (matched to a statement line of this account, directly or through
/// a matched bank transaction), with the earliest value date of a line that shows them.
async fn cleared_entries(
    conn: &mut PgConnection,
    t: TenantId,
    bank: &Bank,
) -> Result<HashMap<Uuid, NaiveDate>, KernelError> {
    let rows: Vec<(Uuid, NaiveDate)> = sqlx::query_as(
        "SELECT entry, min(value_date) FROM ( \
           SELECT l.journal_entry_id AS entry, s.value_date FROM finance.bank_statement_line s \
             JOIN finance.bank_statement bs ON bs.id = s.bank_statement_id \
             JOIN finance.journal_line l ON l.id = s.matched_journal_line_id \
             WHERE bs.tenant_id = $1 AND bs.bank_account_id = $2 AND s.match_status = 'matched' \
           UNION ALL \
           SELECT (i->>'journalEntryId')::uuid, s.value_date FROM finance.bank_statement_line s \
             JOIN finance.bank_statement bs ON bs.id = s.bank_statement_id \
             JOIN finance.bank_transaction bt ON bt.id = s.matched_transaction_id \
             CROSS JOIN LATERAL jsonb_array_elements(CASE WHEN jsonb_typeof(bt.items) = 'array' THEN bt.items ELSE '[]'::jsonb END) i \
             WHERE bs.tenant_id = $1 AND bs.bank_account_id = $2 AND s.match_status = 'matched' \
             AND i ? 'journalEntryId' \
         ) x WHERE entry IS NOT NULL GROUP BY entry",
    )
    .bind(t.as_uuid())
    .bind(bank.id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(rows.into_iter().collect())
}

/// Σ of the amounts finished reconciliations recorded (each entry once).
async fn beginning(conn: &mut PgConnection, t: TenantId, bank: &Bank) -> Result<i64, KernelError> {
    let v: Decimal = sqlx::query_scalar(
        "SELECT COALESCE(sum(amount), 0) FROM ( \
           SELECT DISTINCT ON (re.journal_entry_id) re.amount FROM finance.bank_account_reconciliation_entry re \
           JOIN finance.bank_account_reconciliation r ON r.id = re.reconciliation_id \
           WHERE re.tenant_id = $1 AND r.bank_account_id = $2 AND r.status = 'done' \
           ORDER BY re.journal_entry_id, r.finished_at, r.id) x",
    )
    .bind(t.as_uuid())
    .bind(bank.id)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(paise(v))
}

/// One reconciliation of a bank account.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BankReconView {
    pub id: Uuid,
    pub bank_account_id: Uuid,
    pub statement_date: NaiveDate,
    #[schema(value_type = String, example = "97500.00")]
    pub beginning_balance: String,
    #[schema(value_type = String, example = "97500.00")]
    pub ending_balance: String,
    pub cleared_count: i32,
    /// `done` or `undone`.
    pub status: String,
    pub note: Option<String>,
    pub finished_by: Option<String>,
    pub finished_at: DateTime<Utc>,
    pub undone_by: Option<String>,
    pub undone_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BankReconList {
    pub items: Vec<BankReconView>,
}

const RECON_COLS: &str = "id, bank_account_id, statement_date, beginning_balance, ending_balance, \
    cleared_count, status, note, finished_by, finished_at, undone_by, undone_at";

fn recon_from(r: &sqlx::postgres::PgRow) -> Result<BankReconView, KernelError> {
    let g = |e: sqlx::Error| map_db_error(e);
    Ok(BankReconView {
        id: r.try_get("id").map_err(g)?,
        bank_account_id: r.try_get("bank_account_id").map_err(g)?,
        statement_date: r.try_get("statement_date").map_err(g)?,
        beginning_balance: fmt(r.try_get("beginning_balance").map_err(g)?),
        ending_balance: fmt(r.try_get("ending_balance").map_err(g)?),
        cleared_count: r.try_get("cleared_count").map_err(g)?,
        status: r.try_get("status").map_err(g)?,
        note: r.try_get("note").map_err(g)?,
        finished_by: r.try_get("finished_by").map_err(g)?,
        finished_at: r.try_get("finished_at").map_err(g)?,
        undone_by: r.try_get("undone_by").map_err(g)?,
        undone_at: r.try_get("undone_at").map_err(g)?,
    })
}

/// The latest finished reconciliation (statement date, then finish time).
async fn last_done(
    conn: &mut PgConnection,
    t: TenantId,
    bank: Uuid,
) -> Result<Option<BankReconView>, KernelError> {
    sqlx::query(&format!(
        "SELECT {RECON_COLS} FROM finance.bank_account_reconciliation \
         WHERE tenant_id = $1 AND bank_account_id = $2 AND status = 'done' \
         ORDER BY statement_date DESC, finished_at DESC, id DESC LIMIT 1"
    ))
    .bind(t.as_uuid())
    .bind(bank)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .as_ref()
    .map(recon_from)
    .transpose()
}

/// An unreconciled entry on the bank account.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BankReconItem {
    pub journal_entry_id: Uuid,
    pub entry_number: String,
    pub posting_date: NaiveDate,
    pub description: String,
    /// Money into the bank positive, out negative.
    #[schema(value_type = String, example = "-2500.00")]
    pub amount: String,
    /// The bank has shown it on an imported statement (matched line): pre-ticked.
    pub cleared: bool,
}

/// What reconciling a bank account up to a statement date starts from.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BankReconWorkspace {
    pub bank_account_id: Uuid,
    pub statement_date: NaiveDate,
    /// Balance carried forward from finished reconciliations.
    #[schema(value_type = String, example = "97500.00")]
    pub beginning_balance: String,
    /// Closing balance of an imported statement ending on the statement date, if any.
    #[schema(value_type = Option<String>)]
    pub statement_closing_balance: Option<String>,
    pub last_reconciliation: Option<BankReconView>,
    pub items: Vec<BankReconItem>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct WorkspaceQuery {
    /// `YYYY-MM-DD`; defaults to the period end of the latest imported statement, else today.
    pub statement_date: Option<String>,
}

async fn statement_closing(
    conn: &mut PgConnection,
    t: TenantId,
    bank: Uuid,
    date: NaiveDate,
) -> Result<Option<Decimal>, KernelError> {
    let v: Option<Option<Decimal>> = sqlx::query_scalar(
        "SELECT closing_balance FROM finance.bank_statement WHERE tenant_id = $1 AND bank_account_id = $2 \
         AND period_to = $3 AND closing_balance IS NOT NULL ORDER BY created_at DESC LIMIT 1",
    )
    .bind(t.as_uuid())
    .bind(bank)
    .bind(date)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(v.flatten())
}

/// Unreconciled entries on a bank account up to a statement date, the carried-forward balance
/// and the last reconciliation (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/bank-accounts/{id}/reconciliation", operation_id = "finance_bank_recon_workspace", tag = "finance-treasury",
    params(("id" = String, Path, format = Uuid), WorkspaceQuery), security(("session_cookie" = [])),
    responses((status = 200, body = BankReconWorkspace), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn bank_recon_workspace(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<WorkspaceQuery>,
) -> ApiResult<Json<BankReconWorkspace>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let bank = load_bank(&mut conn, t, id, false).await?;
    let scope = institution_scope(ctx, bank.institution);
    require_institution(&st, &scope, bank.institution).await?;
    let date = match q.statement_date.as_deref() {
        Some(raw) => parse_date("statementDate", raw)?,
        None => {
            let latest: Option<NaiveDate> = sqlx::query_scalar(
                "SELECT max(period_to) FROM finance.bank_statement WHERE tenant_id = $1 AND bank_account_id = $2",
            )
            .bind(t.as_uuid())
            .bind(bank.id)
            .fetch_one(&mut *conn)
            .await
            .map_err(map_db_error)?;
            match latest {
                Some(d) => d,
                None => business_today(&st, &scope, bank.institution).await?,
            }
        }
    };
    let entries = unreconciled_entries(&mut conn, t, &bank, None, Some(date)).await?;
    let cleared = cleared_entries(&mut conn, t, &bank).await?;
    let begin = beginning(&mut conn, t, &bank).await?;
    let items = to_items(&entries, &cleared, date);
    Ok(Json(BankReconWorkspace {
        bank_account_id: bank.id,
        statement_date: date,
        beginning_balance: rupees(begin),
        statement_closing_balance: statement_closing(&mut conn, t, bank.id, date)
            .await?
            .map(fmt),
        last_reconciliation: last_done(&mut conn, t, bank.id).await?,
        items,
    }))
}

fn postings_of(entries: &[EntryRow]) -> Vec<Posting> {
    entries
        .iter()
        .map(|e| Posting {
            entry: e.id,
            date: e.date,
            amount: e.amount,
        })
        .collect()
}

fn to_items(
    entries: &[EntryRow],
    cleared: &HashMap<Uuid, NaiveDate>,
    date: NaiveDate,
) -> Vec<BankReconItem> {
    let cleared_set: HashSet<Uuid> = cleared.keys().copied().collect();
    let by_id: HashMap<Uuid, &EntryRow> = entries.iter().map(|e| (e.id, e)).collect();
    candidates(&postings_of(entries), &HashSet::new(), &cleared_set, Some(date))
        .into_iter()
        .map(|c| {
            let e = by_id[&c.entry];
            BankReconItem {
                journal_entry_id: c.entry,
                entry_number: e.number.clone(),
                posting_date: c.date,
                description: e.description.clone(),
                amount: rupees(c.amount),
                cleared: c.cleared,
            }
        })
        .collect()
}

/// Ticked entries against a statement ending balance.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BankReconInput {
    /// `YYYY-MM-DD`, the statement's closing date.
    pub statement_date: String,
    /// Statement ending balance (decimal string; an overdraft is negative).
    pub ending_balance: String,
    /// The ticked journal entries (unreconciled entries on the account up to the date).
    #[serde(default)]
    pub journal_entry_ids: Vec<Uuid>,
    pub note: Option<String>,
}

/// The reconcile screen's figures.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BankReconSummary {
    pub statement_date: NaiveDate,
    #[schema(value_type = String)]
    pub beginning_balance: String,
    /// Σ ticked money in.
    #[schema(value_type = String)]
    pub increases: String,
    /// Σ ticked money out.
    #[schema(value_type = String)]
    pub decreases: String,
    pub increase_count: i64,
    pub decrease_count: i64,
    #[schema(value_type = String)]
    pub cleared_balance: String,
    #[schema(value_type = String)]
    pub ending_balance: String,
    /// Ending − cleared; a reconciliation can finish only at zero.
    #[schema(value_type = String)]
    pub difference: String,
    pub balanced: bool,
}

fn summary_view(date: NaiveDate, s: &Summary) -> BankReconSummary {
    BankReconSummary {
        statement_date: date,
        beginning_balance: rupees(s.beginning),
        increases: rupees(s.increases),
        decreases: rupees(s.decreases),
        increase_count: s.n_in,
        decrease_count: s.n_out,
        cleared_balance: rupees(s.cleared),
        ending_balance: rupees(s.ending),
        difference: rupees(s.difference),
        balanced: s.difference == 0,
    }
}

/// Validates the input against the account's unreconciled entries and computes the summary.
async fn evaluate(
    conn: &mut PgConnection,
    t: TenantId,
    bank: &Bank,
    input: &BankReconInput,
) -> Result<(NaiveDate, Summary, Vec<(Uuid, i64)>, i64), KernelError> {
    let date = parse_date("statementDate", &input.statement_date)?;
    let ending = parse_signed("endingBalance", &input.ending_balance)?;
    if ending.scale() > 2 {
        return Err(invalid("endingBalance", "scale", "at most 2 decimals"));
    }
    if input.journal_entry_ids.len() > MAX_SELECTED {
        return Err(invalid(
            "journalEntryIds",
            "too_many",
            format!("at most {MAX_SELECTED} entries"),
        ));
    }
    let entries = unreconciled_entries(conn, t, bank, None, Some(date)).await?;
    let items = candidates(&postings_of(&entries), &HashSet::new(), &HashSet::new(), Some(date));
    let amount_of: HashMap<Uuid, i64> = items.iter().map(|c| (c.entry, c.amount)).collect();
    let mut selected = HashSet::new();
    let mut picked = Vec::new();
    for (i, id) in input.journal_entry_ids.iter().enumerate() {
        let Some(a) = amount_of.get(id) else {
            return Err(invalid(
                &format!("journalEntryIds[{i}]"),
                "not_candidate",
                "not an unreconciled entry on this bank account up to the statement date",
            ));
        };
        if selected.insert(*id) {
            picked.push((*id, *a));
        }
    }
    let begin = beginning(conn, t, bank).await?;
    let s = summarize(1, begin, &items, &selected, paise(ending));
    Ok((date, s, picked, begin))
}

/// Figures for ticked entries against an ending balance, without saving (`finance.read`).
#[utoipa::path(post, path = "/api/v1/finance/bank-accounts/{id}/reconciliation/preview", operation_id = "finance_bank_recon_preview", tag = "finance-treasury",
    params(("id" = String, Path, format = Uuid)), request_body = BankReconInput, security(("session_cookie" = [])),
    responses((status = 200, body = BankReconSummary), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn bank_recon_preview(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(input): Json<BankReconInput>,
) -> ApiResult<Json<BankReconSummary>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let bank = load_bank(&mut conn, t, id, false).await?;
    let scope = institution_scope(ctx, bank.institution);
    require_institution(&st, &scope, bank.institution).await?;
    let (date, s, _, _) = evaluate(&mut conn, t, &bank, &input).await?;
    Ok(Json(summary_view(date, &s)))
}

/// Finishes a reconciliation (`treasury.manage`): the ticked entries become reconciled once the
/// cleared balance equals the statement ending balance (difference zero). The statement date
/// cannot precede the last reconciliation's or be in the future; the bank account row is locked so
/// two finishes serialize.
#[utoipa::path(post, path = "/api/v1/finance/bank-accounts/{id}/reconciliations", operation_id = "finance_bank_recon_finish", tag = "finance-treasury",
    params(("id" = String, Path, format = Uuid)), request_body = BankReconInput, security(("session_cookie" = [])),
    responses((status = 201, body = BankReconView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn bank_recon_finish(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(input): Json<BankReconInput>,
) -> ApiResult<(StatusCode, Json<BankReconView>)> {
    let ctx = auth.require(Permission::TreasuryManage)?;
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let bank = load_bank(&mut tx, t, id, true).await?;
    let scope = institution_scope(ctx, bank.institution);
    require_institution_writable(&st, &scope, bank.institution).await?;
    if !matches!(bank.status.as_str(), "active" | "suspended") {
        return Err(KernelError::Conflict(format!(
            "a {} bank account cannot be reconciled",
            bank.status
        ))
        .into());
    }
    let (date, s, picked, begin) = evaluate(&mut tx, t, &bank, &input).await?;
    let today = business_today(&st, &scope, bank.institution).await?;
    if date > today {
        return Err(invalid("statementDate", "future", "cannot be after today").into());
    }
    if let Some(last) = last_done(&mut tx, t, bank.id).await? {
        if date < last.statement_date {
            return Err(invalid(
                "statementDate",
                "before_last",
                format!("the last reconciliation is up to {}", last.statement_date),
            )
            .into());
        }
    }
    if picked.is_empty() {
        return Err(invalid("journalEntryIds", "empty", "tick at least one entry").into());
    }
    if s.difference != 0 {
        return Err(invalid(
            "endingBalance",
            "difference",
            format!(
                "the cleared balance {} differs from the ending balance by {}",
                rupees(s.cleared),
                rupees(s.difference)
            ),
        )
        .into());
    }
    let note = input
        .note
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|n| n.chars().take(500).collect::<String>());
    let row = sqlx::query(&format!(
        "INSERT INTO finance.bank_account_reconciliation (tenant_id, institution_id, bank_account_id, \
         ledger_account_id, statement_date, beginning_balance, ending_balance, cleared_count, note, finished_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING {RECON_COLS}"
    ))
    .bind(t.as_uuid())
    .bind(bank.institution.as_uuid())
    .bind(bank.id)
    .bind(bank.ledger)
    .bind(date)
    .bind(Decimal::new(begin, 2))
    .bind(Decimal::new(s.ending, 2))
    .bind(i32::try_from(picked.len()).unwrap_or(i32::MAX))
    .bind(note.as_deref())
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let view = recon_from(&row)?;
    let (ids, amounts): (Vec<Uuid>, Vec<Decimal>) = picked
        .iter()
        .map(|(id, p)| (*id, Decimal::new(*p, 2)))
        .unzip();
    sqlx::query(
        "INSERT INTO finance.bank_account_reconciliation_entry (reconciliation_id, tenant_id, journal_entry_id, amount) \
         SELECT $1, $2, x.id, x.amount FROM unnest($3::uuid[], $4::numeric[]) AS x(id, amount)",
    )
    .bind(view.id)
    .bind(t.as_uuid())
    .bind(&ids)
    .bind(&amounts)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    emit(
        &mut tx,
        &scope,
        None,
        &changed(view.id, bank.institution, None, "done", 1, Some(Decimal::new(s.ending, 2)), None, None, None),
        "finance.bank_account.reconcile",
        "bank_account_reconciliation",
        json!({"bankAccountId": bank.id, "statementDate": date, "cleared": picked.len(),
               "beginningBalance": rupees(begin), "endingBalance": rupees(s.ending)}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// Reconciliation history of a bank account, newest first (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/bank-accounts/{id}/reconciliations", operation_id = "finance_bank_recon_list", tag = "finance-treasury",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = BankReconList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn bank_recon_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<BankReconList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let bank = load_bank(&mut conn, t, id, false).await?;
    let scope = institution_scope(ctx, bank.institution);
    require_institution(&st, &scope, bank.institution).await?;
    let items = sqlx::query(&format!(
        "SELECT {RECON_COLS} FROM finance.bank_account_reconciliation WHERE tenant_id = $1 AND bank_account_id = $2 \
         ORDER BY finished_at DESC, id DESC LIMIT {MAX_LIST}"
    ))
    .bind(t.as_uuid())
    .bind(bank.id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(recon_from)
    .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(BankReconList { items }))
}

/// Which reconciliation to undo (must be the latest finished one).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BankReconUndo {
    pub reconciliation_id: Uuid,
}

/// Undoes the latest finished reconciliation (`treasury.manage`): its entries become
/// unreconciled again and the carried-forward balance steps back. The record stays as `undone`.
#[utoipa::path(post, path = "/api/v1/finance/bank-accounts/{id}/reconciliations/undo-last", operation_id = "finance_bank_recon_undo", tag = "finance-treasury",
    params(("id" = String, Path, format = Uuid)), request_body = BankReconUndo, security(("session_cookie" = [])),
    responses((status = 200, body = BankReconView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn bank_recon_undo(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(input): Json<BankReconUndo>,
) -> ApiResult<Json<BankReconView>> {
    let ctx = auth.require(Permission::TreasuryManage)?;
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let bank = load_bank(&mut tx, t, id, true).await?;
    let scope = institution_scope(ctx, bank.institution);
    require_institution_writable(&st, &scope, bank.institution).await?;
    let last = last_done(&mut tx, t, bank.id)
        .await?
        .ok_or_else(|| KernelError::Conflict("the bank account has no reconciliation to undo".into()))?;
    if last.id != input.reconciliation_id {
        return Err(KernelError::Conflict(
            "only the latest reconciliation can be undone".into(),
        )
        .into());
    }
    let row = sqlx::query(&format!(
        "UPDATE finance.bank_account_reconciliation SET status = 'undone', undone_by = $2, undone_at = clock_timestamp() \
         WHERE id = $1 RETURNING {RECON_COLS}"
    ))
    .bind(last.id)
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let view = recon_from(&row)?;
    emit(
        &mut tx,
        &scope,
        None,
        &changed(view.id, bank.institution, Some("done"), "undone", 2, None, None, None, None),
        "finance.bank_account.reconcile_undo",
        "bank_account_reconciliation",
        json!({"bankAccountId": bank.id, "statementDate": view.statement_date}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(view))
}

/// One item of the bank reconciliation statement.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrsItem {
    pub date: NaiveDate,
    /// Entry number or statement reference.
    pub reference: Option<String>,
    pub description: Option<String>,
    #[schema(value_type = String)]
    pub amount: String,
    pub journal_entry_id: Option<Uuid>,
    pub statement_line_id: Option<Uuid>,
}

/// Bank reconciliation statement as of a date.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrsView {
    pub bank_account_id: Uuid,
    pub as_of: NaiveDate,
    /// Balance as per cash book (the bank's ledger account).
    #[schema(value_type = String)]
    pub book_balance: String,
    #[schema(value_type = String)]
    pub issued_not_presented: String,
    #[schema(value_type = String)]
    pub deposited_not_credited: String,
    #[schema(value_type = String)]
    pub bank_credits_not_booked: String,
    #[schema(value_type = String)]
    pub bank_debits_not_booked: String,
    /// Balance as per pass book (bank) the adjustments arrive at.
    #[schema(value_type = String)]
    pub bank_balance: String,
    #[schema(value_type = Option<String>)]
    pub statement_balance: Option<String>,
    #[schema(value_type = Option<String>)]
    pub difference: Option<String>,
    /// The items behind each adjustment (at most 500 per list).
    pub issued: Vec<BrsItem>,
    pub deposited: Vec<BrsItem>,
    pub bank_credits: Vec<BrsItem>,
    pub bank_debits: Vec<BrsItem>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct BrsQuery {
    /// `YYYY-MM-DD`; defaults to today.
    pub as_of: Option<String>,
}

/// Bank reconciliation statement (`finance.read`): balance as per cash book, plus cheques issued
/// not presented, less deposits not credited, plus / less bank credits / debits not yet booked,
/// gives the balance as per pass book; compared with an imported statement closing that day.
#[utoipa::path(get, path = "/api/v1/finance/bank-accounts/{id}/brs", operation_id = "finance_bank_recon_brs", tag = "finance-treasury",
    params(("id" = String, Path, format = Uuid), BrsQuery), security(("session_cookie" = [])),
    responses((status = 200, body = BrsView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn bank_recon_brs(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<BrsQuery>,
) -> ApiResult<Json<BrsView>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let bank = load_bank(&mut conn, t, id, false).await?;
    let scope = institution_scope(ctx, bank.institution);
    require_institution(&st, &scope, bank.institution).await?;
    let as_of = match q.as_of.as_deref() {
        Some(raw) => parse_date("asOf", raw)?,
        None => business_today(&st, &scope, bank.institution).await?,
    };
    let book: Decimal = sqlx::query_scalar(
        "SELECT COALESCE(sum(l.debit - l.credit), 0) FROM finance.journal_line l \
         JOIN finance.journal_entry e ON e.id = l.journal_entry_id \
         WHERE l.tenant_id = $1 AND l.ledger_account_id = $2 AND e.posting_date <= $3",
    )
    .bind(t.as_uuid())
    .bind(bank.ledger)
    .bind(as_of)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    // Entries reconciled up to a statement on or before the date count as shown by the bank.
    let reconciled: HashSet<Uuid> = sqlx::query_scalar::<_, Uuid>(
        "SELECT re.journal_entry_id FROM finance.bank_account_reconciliation_entry re \
         JOIN finance.bank_account_reconciliation r ON r.id = re.reconciliation_id \
         WHERE re.tenant_id = $1 AND r.bank_account_id = $2 AND r.status = 'done' AND r.statement_date <= $3",
    )
    .bind(t.as_uuid())
    .bind(bank.id)
    .bind(as_of)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .into_iter()
    .collect();
    let cleared = cleared_entries(&mut conn, t, &bank).await?;
    let all = unreconciled_entries(&mut conn, t, &bank, None, Some(as_of)).await?;
    let uncleared: Vec<&EntryRow> = all
        .iter()
        .filter(|e| e.amount != 0)
        .filter(|e| !reconciled.contains(&e.id))
        .filter(|e| cleared.get(&e.id).is_none_or(|d| *d > as_of))
        .collect();
    let unbooked: Vec<(Uuid, NaiveDate, bool, Decimal, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT s.id, s.value_date, s.direction = 'credit', s.amount, s.reference, s.description \
         FROM finance.bank_statement_line s JOIN finance.bank_statement bs ON bs.id = s.bank_statement_id \
         WHERE bs.tenant_id = $1 AND bs.bank_account_id = $2 AND s.match_status = 'unmatched' AND s.value_date <= $3 \
         ORDER BY s.value_date, s.line_no",
    )
    .bind(t.as_uuid())
    .bind(bank.id)
    .bind(as_of)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let closing = statement_closing(&mut conn, t, bank.id, as_of).await?;
    let signed = |credit: bool, a: Decimal| if credit { paise(a) } else { -paise(a) };
    let b: Brs = brs(
        paise(book),
        &uncleared.iter().map(|e| e.amount).collect::<Vec<_>>(),
        &unbooked.iter().map(|l| signed(l.2, l.3)).collect::<Vec<_>>(),
        closing.map(paise),
    );
    let entry_item = |e: &&EntryRow| BrsItem {
        date: e.date,
        reference: Some(e.number.clone()),
        description: Some(e.description.clone()),
        amount: rupees(e.amount.abs()),
        journal_entry_id: Some(e.id),
        statement_line_id: None,
    };
    let line_item = |l: &(Uuid, NaiveDate, bool, Decimal, Option<String>, Option<String>)| BrsItem {
        date: l.1,
        reference: l.4.clone(),
        description: l.5.clone(),
        amount: fmt(l.3),
        journal_entry_id: None,
        statement_line_id: Some(l.0),
    };
    Ok(Json(BrsView {
        bank_account_id: bank.id,
        as_of,
        book_balance: rupees(b.book_balance),
        issued_not_presented: rupees(b.issued_not_presented),
        deposited_not_credited: rupees(b.deposited_not_credited),
        bank_credits_not_booked: rupees(b.bank_credits_not_booked),
        bank_debits_not_booked: rupees(b.bank_debits_not_booked),
        bank_balance: rupees(b.bank_balance),
        statement_balance: b.statement_balance.map(rupees),
        difference: b.difference.map(rupees),
        issued: uncleared.iter().filter(|e| e.amount < 0).take(MAX_LIST).map(entry_item).collect(),
        deposited: uncleared.iter().filter(|e| e.amount > 0).take(MAX_LIST).map(entry_item).collect(),
        bank_credits: unbooked.iter().filter(|l| l.2).take(MAX_LIST).map(line_item).collect(),
        bank_debits: unbooked.iter().filter(|l| !l.2).take(MAX_LIST).map(line_item).collect(),
    }))
}

/// A reconciled entry whose amount on the bank account changed afterwards.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BankReconDiscrepancy {
    pub reconciliation_id: Uuid,
    pub bank_account_id: Uuid,
    pub bank_account_name: String,
    pub statement_date: NaiveDate,
    pub journal_entry_id: Uuid,
    pub entry_number: Option<String>,
    pub posting_date: Option<NaiveDate>,
    #[schema(value_type = String)]
    pub reconciled_amount: String,
    #[schema(value_type = String)]
    pub current_amount: String,
    #[schema(value_type = String)]
    pub change: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BankReconDiscrepancyList {
    pub items: Vec<BankReconDiscrepancy>,
}

/// Changes after reconciliation across an institution's bank accounts (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/bank-reconciliation-discrepancies", operation_id = "finance_bank_recon_discrepancies", tag = "finance-treasury",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = BankReconDiscrepancyList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn bank_recon_discrepancies(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<BankReconDiscrepancyList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    // Same rule as `reconcile::discrepancies`, in SQL so it scales with the journal.
    let rows: Vec<(Uuid, Uuid, String, NaiveDate, Uuid, Option<String>, Option<NaiveDate>, Decimal, Decimal)> = sqlx::query_as(
        "SELECT r.id, r.bank_account_id, b.name, r.statement_date, re.journal_entry_id, e.entry_number, e.posting_date, \
                re.amount, COALESCE(cur.amount, 0) \
         FROM finance.bank_account_reconciliation r \
         JOIN finance.bank_account b ON b.id = r.bank_account_id \
         JOIN finance.bank_account_reconciliation_entry re ON re.reconciliation_id = r.id \
         LEFT JOIN finance.journal_entry e ON e.id = re.journal_entry_id \
         LEFT JOIN LATERAL (SELECT sum(l.debit - l.credit) AS amount FROM finance.journal_line l \
             WHERE l.journal_entry_id = re.journal_entry_id AND l.ledger_account_id = r.ledger_account_id) cur ON true \
         WHERE r.tenant_id = $1 AND r.institution_id = $2 AND r.status = 'done' \
         AND re.amount <> COALESCE(cur.amount, 0) \
         ORDER BY r.statement_date, r.finished_at, re.journal_entry_id LIMIT 500",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(Json(BankReconDiscrepancyList {
        items: rows
            .into_iter()
            .map(|r| BankReconDiscrepancy {
                reconciliation_id: r.0,
                bank_account_id: r.1,
                bank_account_name: r.2,
                statement_date: r.3,
                journal_entry_id: r.4,
                entry_number: r.5,
                posting_date: r.6,
                reconciled_amount: fmt(r.7),
                current_amount: fmt(r.8),
                change: fmt(r.8 - r.7),
            })
            .collect(),
    }))
}

/// A match suggestion for a statement line.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StatementLineSuggestion {
    /// `entry` (a ledger entry on the bank account) or `charge` (an open student charge of
    /// exactly this amount, probably being paid).
    pub kind: String,
    pub id: Uuid,
    pub number: String,
    pub description: String,
    pub date: NaiveDate,
    #[schema(value_type = String)]
    pub amount: String,
    /// Days between the line and the entry date (or the charge's due date).
    pub days_apart: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StatementLineSuggestionList {
    pub items: Vec<StatementLineSuggestion>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct SuggestionQuery {
    /// Days either side of the value date (default 10, at most 60).
    pub window: Option<i64>,
}

/// Up to 8 match suggestions for one statement line (`finance.read`): entries on the bank account
/// with the same amount and direction within the window that no line has taken and no
/// reconciliation has cleared (closest first), then open student charges of exactly the amount
/// for money in.
#[utoipa::path(get, path = "/api/v1/finance/bank-statements/{id}/lines/{lineId}/suggestions", operation_id = "finance_statement_line_suggestions", tag = "finance-treasury",
    params(("id" = String, Path, format = Uuid), ("lineId" = String, Path, format = Uuid), SuggestionQuery), security(("session_cookie" = [])),
    responses((status = 200, body = StatementLineSuggestionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn statement_line_suggestions(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path((id, line_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<SuggestionQuery>,
) -> ApiResult<Json<StatementLineSuggestionList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let t = tenant(ctx);
    let window = q.window.unwrap_or(10);
    if !(0..=60).contains(&window) {
        return Err(invalid("window", "range", "between 0 and 60 days").into());
    }
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let (bank_id, date, credit, amount, matched_jl, matched_bt): (Uuid, NaiveDate, bool, Decimal, Option<Uuid>, Option<Uuid>) =
        sqlx::query_as(
            "SELECT bs.bank_account_id, s.value_date, s.direction = 'credit', s.amount, s.matched_journal_line_id, s.matched_transaction_id \
             FROM finance.bank_statement_line s JOIN finance.bank_statement bs ON bs.id = s.bank_statement_id \
             WHERE bs.tenant_id = $1 AND bs.id = $2 AND s.id = $3",
        )
        .bind(t.as_uuid())
        .bind(id)
        .bind(line_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "bank statement line",
        })?;
    let bank = load_bank(&mut conn, t, bank_id, false).await?;
    let scope = institution_scope(ctx, bank.institution);
    require_institution(&st, &scope, bank.institution).await?;
    let span = chrono::Duration::days(window);
    let entries =
        unreconciled_entries(&mut conn, t, &bank, Some(date - span), Some(date + span)).await?;
    // Entries other lines already explain are taken (this line's own match is not).
    let mut own: HashSet<Uuid> = HashSet::new();
    if let Some(jl) = matched_jl {
        if let Some(e) = sqlx::query_scalar::<_, Uuid>("SELECT journal_entry_id FROM finance.journal_line WHERE id = $1")
            .bind(jl)
            .fetch_optional(&mut *conn)
            .await
            .map_err(map_db_error)?
        {
            own.insert(e);
        }
    }
    if let Some(bt) = matched_bt {
        let items: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT items FROM finance.bank_transaction WHERE id = $1")
                .bind(bt)
                .fetch_optional(&mut *conn)
                .await
                .map_err(map_db_error)?;
        for i in items.as_ref().and_then(|v| v.as_array()).into_iter().flatten() {
            if let Some(e) = i.get("journalEntryId").and_then(|x| x.as_str()).and_then(|s| Uuid::parse_str(s).ok()) {
                own.insert(e);
            }
        }
    }
    let taken: HashSet<Uuid> = cleared_entries(&mut conn, t, &bank)
        .await?
        .into_keys()
        .filter(|e| !own.contains(e))
        .collect();
    let line_amount = if credit { paise(amount) } else { -paise(amount) };
    let mut docs = Vec::new();
    let mut charge_meta: HashMap<Uuid, (String, String, NaiveDate)> = HashMap::new();
    if credit {
        let rows: Vec<(Uuid, String, String, Option<NaiveDate>, NaiveDate, Decimal)> = sqlx::query_as(
            "SELECT id, charge_number, description, due_date, created_at::date, outstanding_amount FROM finance.charge \
             WHERE tenant_id = $1 AND institution_id = $2 AND status IN ('published', 'active') AND outstanding_amount = $3 \
             ORDER BY due_date NULLS LAST, id LIMIT 50",
        )
        .bind(t.as_uuid())
        .bind(bank.institution.as_uuid())
        .bind(amount)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
        for (cid, number, desc, due, created, open) in rows {
            charge_meta.insert(cid, (number, desc, due.unwrap_or(created)));
            docs.push(OpenDoc {
                id: cid,
                receivable: true,
                date: created,
                due,
                voided: false,
                open: paise(open),
            });
        }
    }
    let by_id: HashMap<Uuid, &EntryRow> = entries.iter().map(|e| (e.id, e)).collect();
    let found = find_matches(
        &MatchLine {
            date,
            amount: line_amount,
        },
        &postings_of(&entries),
        window,
        &taken,
        &HashSet::new(),
        &docs,
    );
    let items = found
        .into_iter()
        .filter_map(|s| match s.kind {
            SuggestionKind::Entry => by_id.get(&s.id).map(|e| StatementLineSuggestion {
                kind: "entry".into(),
                id: e.id,
                number: e.number.clone(),
                description: e.description.clone(),
                date: e.date,
                amount: rupees(e.amount.abs()),
                days_apart: s.score / 100,
            }),
            SuggestionKind::Open => charge_meta.get(&s.id).map(|(n, d, when)| StatementLineSuggestion {
                kind: "charge".into(),
                id: s.id,
                number: n.clone(),
                description: d.clone(),
                date: *when,
                amount: fmt(amount),
                days_apart: s.score - 10_000,
            }),
        })
        .collect();
    Ok(Json(StatementLineSuggestionList { items }))
}
